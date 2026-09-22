//! sudo 提权策略（Q33 三模式）。
//!
//! 客户在 Q33 明确要求三种模式**一期全部实现**，由用户为每个主机选择。
//! 提权走**应用自建的特权通道**（`run_as_root`，D47），三模式的含义是
//! "**是否允许 Agent 经该通道提权**"：
//!
//! | 模式 | 行为 |
//! | ---- | ---- |
//! | `deny` | 不允许提权。`run_as_root` 直接报错，数据面的 sudo 垫片说明原因（默认，fail-closed） |
//! | `ask` | 每次提权经系统通知征得人类同意后才投递密码；拒绝或超时即不提权 |
//! | `auto` | 直接投递该主机已配置的提权密码 |
//!
//! 三种模式共享同一条不变式：**数据面（Agent 的普通命令）永远拿不到密码**。
//! 数据面上的 `sudo` 由 [`crate::ssh::protocol::sudo_reject_shim`] 明确拒绝，
//! 因此这里不再有"拦截是否被绕过"的问题——绕过也无处可取密码。
//!
//! ## 密码的存放与投递
//!
//! - 只在**应用内存**中，包一层 [`Zeroizing`]，会话结束即清零；
//! - 只写进**特权通道的 stdin**，且每条通道最多写一次（见 `SudoAuthHandshake`）；
//! - 远端不落任何文件、不设任何环境变量（D49：远端不得出现本应用的痕迹）。
//!
//! ## 为什么不需要"已拒绝"记忆
//!
//! 单命令形态的提权一次调用只提权一次、也就只问一次，Q36 担心的"连环询问"
//! 在这里无处发生，因此 `AskOutcome` 不设"已拒绝"这一记忆状态。

use zeroize::Zeroizing;

use crate::domain::host::SudoPolicy;
use crate::error::Result;

/// sudo 密码的来源（Q33）。
///
/// `ReuseLogin` 复用 SSH 登录密码，避免用户重复录入。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SudoPasswordSource {
    /// 该主机单独配置的 sudo 密码。
    Own,
    /// 复用该主机的 SSH 登录密码。
    ReuseLogin,
}

/// 终端建立时解析好的 sudo 配置。
///
/// 密码在内存中包一层 [`Zeroizing`]：终端会话结束时自动清零，
/// 降低内存转储或日志误打印带来的泄露风险。
pub struct SudoContext {
    pub policy: SudoPolicy,
    /// 已解析的密码；`None` 表示该主机未配置密码。
    pub password: Option<Zeroizing<String>>,
}

impl SudoContext {

    /// `auto` 模式：可立即注入密码时返回密码。
    ///
    /// 无密码可注入时返回 `None`，调用方应改为让 sudo 失败（fail-closed），
    /// 而不是静默跳过。
    pub fn password_for_auto(&self) -> Option<&str> {
        if self.policy != SudoPolicy::Auto {
            return None;
        }
        self.password.as_deref().map(|s| s.as_str())
    }
}

impl std::fmt::Debug for SudoContext {
    /// 刻意遮蔽密码，避免经日志或 panic 信息泄露（D6 / P2）。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SudoContext")
            .field("policy", &self.policy)
            .field(
                "password",
                &self.password.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

/// 用户在 ask 模式下的决定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SudoDecision {
    /// 允许注入密码。
    Allow,
    /// 拒绝注入（sudo 将失败）。
    Deny,
}

impl SudoDecision {
    /// 超时视为拒绝：无人响应时不应默认提权。
    pub fn on_timeout() -> Self {
        Self::Deny
    }
}

/// ask 模式下一次询问的**来源**（Q33 / D49）。
///
/// 四种来源对提权的动作都是"不投递密码"，但它们**不是同一件事**：
/// 审计（人类事后核对"这次提权是谁拒绝的"）依赖这个区分。把它们折叠成
/// 一个 `Deny`，命令历史就会把"没人应答"写成"用户已拒绝"——那是在审计栏里说假话。
///
/// （原为五种：Q36 时代还有 `DeniedByMemo`（同一命令内已拒绝、自动沿用）。
/// 该记忆随 D49 一并删除——提权改为单命令形态后一次调用只提权一次、
/// 也就只问一次，成因消失。）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskOutcome {
    /// 人类本次点击「允许」。
    Allowed,
    /// 人类本次点击「拒绝」。
    Denied,
    /// 等待人类应答超时，按拒绝处理（fail-closed）。
    TimedOut,
    /// 询问通道不可用（界面 / 桥接异常），按拒绝处理。
    Unavailable,
}

impl AskOutcome {
    /// 对应的提权决策：**只有「允许」会提权**，其余一律 fail-closed。
    pub fn decision(self) -> SudoDecision {
        match self {
            Self::Allowed => SudoDecision::Allow,
            Self::Denied | Self::TimedOut | Self::Unavailable => SudoDecision::Deny,
        }
    }

    /// 写入命令输出的审计备注（人类与 Agent 都读得到）。
    pub fn audit_note(self) -> &'static str {
        match self {
            Self::Allowed => "sudo 提权请求：用户已允许",
            Self::Denied => "sudo 提权请求：用户已拒绝",
            Self::TimedOut => "sudo 提权请求：等待确认超时，已按拒绝处理",
            Self::Unavailable => "sudo 提权请求：确认通道不可用，已按拒绝处理",
        }
    }
}

/// 一次待决的 sudo 请求。
#[derive(Debug, Clone, serde::Serialize)]
pub struct SudoRequest {
    pub request_id: String,
    pub terminal_id: String,
    /// 主机显示名，便于用户判断这是哪台机器。
    pub host_label: String,
}


/// 供界面展示的策略说明。
///
/// 措辞随 D47/D48 更新：提权已改为独立通道的 `run_as_root` 工具，
/// 数据面上的 `sudo` 命令**在任何策略下都被拒绝**（它拿不到密码），
/// 因此这里的说明必须讲"是否允许 Agent 提权"，而不是"如何注入密码"。
pub fn policy_description(policy: SudoPolicy) -> &'static str {
    match policy {
        SudoPolicy::Deny => "不允许 Agent 提权（默认，最安全）",
        SudoPolicy::Ask => "Agent 请求提权时通知你确认",
        SudoPolicy::Auto => "Agent 请求提权时自动使用该主机的提权密码",
        SudoPolicy::NotNeeded => "该主机以特权身份登录，无需提权（不使用任何密码）",
    }
}

/// 校验 ask 模式所需的配置是否齐备。
///
/// 返回 `Err` 时调用方应把原因告知用户——静默失效会让用户以为
/// 策略已生效，而实际上 sudo 一直失败（P1：明确报错）。
pub fn validate_for_policy(policy: SudoPolicy, has_password: bool) -> Result<()> {
    // `deny` 与 `not_needed` 都不涉及口令，无需校验（D60：后者连"提权"这一步都没有）。
    if !matches!(policy, SudoPolicy::Ask | SudoPolicy::Auto) {
        return Ok(());
    }
    if has_password {
        return Ok(());
    }
    Err(crate::error::AppError::Config(format!(
        "主机配置为「{}」但未提供 sudo 密码；\
         sudo 命令会因缺少密码而失败。请为该主机配置 sudo 密码，\
         或选择「复用 SSH 登录密码」并确保登录认证为密码方式。",
        policy_description(policy)
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 守的是一条**权限边界不变式**：除「允许」外一律不得提权。
    ///
    /// 超时与"询问通道不可用"都必须按拒绝处理（fail-closed）——
    /// 无人应答时默认提权，等于把"人类确认"这道闸门变成摆设。
    #[test]
    fn only_allow_may_escalate() {
        let cases = [
            (AskOutcome::Allowed, SudoDecision::Allow),
            (AskOutcome::Denied, SudoDecision::Deny),
            (AskOutcome::TimedOut, SudoDecision::Deny),
            (AskOutcome::Unavailable, SudoDecision::Deny),
        ];
        for (outcome, decision) in cases {
            assert_eq!(outcome.decision(), decision, "{outcome:?} 的决策不符");
            assert!(!outcome.audit_note().is_empty(), "{outcome:?} 必须有审计备注");
        }
    }

    /// 四种来源的审计备注两两不同。
    ///
    /// 守的是审计的可核对性：命令历史里读到哪一条备注，就唯一对应一种经过。
    /// 若"当场拒绝"与"等待超时"用了同一句话，人类事后核对时会误以为
    /// 自己点过拒绝。
    #[test]
    fn audit_notes_distinguish_every_outcome() {
        let notes = [
            AskOutcome::Allowed,
            AskOutcome::Denied,
            AskOutcome::TimedOut,
            AskOutcome::Unavailable,
        ]
        .map(AskOutcome::audit_note);

        for (i, a) in notes.iter().enumerate() {
            for (j, b) in notes.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "第 {i} 与第 {j} 种来源的审计备注相同，无法区分经过");
                }
            }
        }
    }

    #[test]
    fn timeout_decision_is_deny() {
        assert_eq!(SudoDecision::on_timeout(), SudoDecision::Deny);
    }

    #[test]
    fn validate_passes_for_deny_without_password() {
        assert!(validate_for_policy(SudoPolicy::Deny, false).is_ok());
    }

    #[test]
    fn validate_rejects_ask_without_password() {
        // 配置为需要提权却没有密码：必须在建立终端时就明确报错，
        // 而不是让 Agent 在提权时收到一句莫名其妙的话（P1）。
        assert!(validate_for_policy(SudoPolicy::Ask, false).is_err());
        assert!(validate_for_policy(SudoPolicy::Auto, false).is_err());
    }

    #[test]
    fn validate_passes_when_password_provided() {
        assert!(validate_for_policy(SudoPolicy::Ask, true).is_ok());
        assert!(validate_for_policy(SudoPolicy::Auto, true).is_ok());
    }

    #[test]
    fn debug_hides_password() {
        // D6 / P2：密码不得经日志或 panic 信息泄露。
        let ctx = SudoContext {
            policy: SudoPolicy::Auto,
            password: Some(Zeroizing::new("s3cret".to_string())),
        };
        let shown = format!("{ctx:?}");
        assert!(!shown.contains("s3cret"), "调试输出泄露了密码：{shown}");
        assert!(shown.contains("redacted"), "应显示占位符：{shown}");
    }

    #[test]
    fn password_for_auto_only_returns_in_auto_mode() {
        let auto = SudoContext {
            policy: SudoPolicy::Auto,
            password: Some(Zeroizing::new("pw".to_string())),
        };
        assert_eq!(auto.password_for_auto(), Some("pw"));

        // ask 模式必须走人类确认，不得被任何路径直接取走密码。
        let ask = SudoContext {
            policy: SudoPolicy::Ask,
            password: Some(Zeroizing::new("pw".to_string())),
        };
        assert_eq!(ask.password_for_auto(), None, "ask 模式不应直接取密码");
    }
}
