//! sudo 密码处理（Q33 三模式）。
//!
//! 客户在 Q33 明确要求三种模式**一期全部实现**，由用户为每个主机选择：
//!
//! | 模式 | 行为 |
//! | ---- | ---- |
//! | `deny` | 不注入。sudo 因无 TTY 且无密码而失败（默认，fail-closed） |
//! | `ask` | 经系统通知征得同意后注入；拒绝或超时则让 sudo 失败 |
//! | `auto` | 检测到请求即自动注入 |
//!
//! ## 防误用的关键设计
//!
//! - **不靠命令改写**：拦截由远端 shell 函数完成（`protocol::sudo_function_def`），
//!   因此 `sh -c 'sudo x'` 这类写法不会像文本匹配那样漏判。
//! - **未被拦截的 sudo 一律失败**：任何绕过拦截的调用都拿不到密码，
//!   属 fail-closed，宁可失败不可越权。
//! - **密码不落盘、不进环境变量**：经 FIFO 从内存传给 askpass，
//!   读完即消失（环境变量会被同机其他进程从 `/proc/*/environ` 读到）。
//!
//! ## ask 模式的"拒绝"如何生效
//!
//! askpass 阻塞在 `read <&3` 上等待密码。**必须给它一个回应**，否则它会永久
//! 阻塞，而命令串行执行，整条队列都会被拖死。
//!
//! 做法是向 FIFO **写入一个空行**：askpass 读到空密码交给 sudo，认证随即失败，
//! 命令正常结束并返回非零退出码。
//!
//! 为什么不用"关闭 FIFO 让 read 得到 EOF"（初版设计）：askpass 已用
//! `exec 3<>fifo` 以 O_RDWR 同时持有读写端，EOF 不会因外部关闭写端而出现。
//! 该结论由 WSL 端到端实测得出，详见 `docs/design/sudo.md` §7.3。

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
    /// 是否需要在远端部署 askpass 与 FIFO。
    ///
    /// `deny` 模式不部署——这是"禁止注入"的实现方式：不装 askpass，
    /// sudo 自然拿不到密码。
    pub fn needs_askpass(&self) -> bool {
        matches!(self.policy, SudoPolicy::Ask | SudoPolicy::Auto)
    }

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

/// 一次待决的 sudo 请求。
#[derive(Debug, Clone, serde::Serialize)]
pub struct SudoRequest {
    pub request_id: String,
    pub terminal_id: String,
    /// 主机显示名，便于用户判断这是哪台机器。
    pub host_label: String,
}

/// 应用一次 sudo 决策后的动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SudoAction {
    /// 向 FIFO 写入密码。
    Inject,
    /// 向 FIFO 写入空行，让 askpass 以空密码应答，从而使 sudo 认证失败。
    Deny,
}

/// 依据策略与决策决定实际动作。
///
/// 抽为纯函数以便覆盖各分支，尤其是"无密码时必须 Deny 而非静默"。
pub fn resolve_action(
    policy: SudoPolicy,
    has_password: bool,
    decision: Option<SudoDecision>,
) -> SudoAction {
    match policy {
        // deny 不会产生请求（未部署 askpass），这里保守处理为拒绝。
        SudoPolicy::Deny => SudoAction::Deny,
        SudoPolicy::Auto => {
            if has_password {
                SudoAction::Inject
            } else {
                // 配置为自动注入却没有密码：不能假装成功，应让 sudo 失败。
                SudoAction::Deny
            }
        }
        SudoPolicy::Ask => match decision {
            Some(SudoDecision::Allow) if has_password => SudoAction::Inject,
            // 用户拒绝、超时未响应、或本就无密码可注入。
            _ => SudoAction::Deny,
        },
    }
}

/// 供界面展示的策略说明。
pub fn policy_description(policy: SudoPolicy) -> &'static str {
    match policy {
        SudoPolicy::Deny => "Agent 执行 sudo 时自动拒绝（最安全）",
        SudoPolicy::Ask => "sudo 需要提权时通知你确认",
        SudoPolicy::Auto => "自动注入密码执行 sudo",
    }
}

/// 校验 ask 模式所需的配置是否齐备。
///
/// 返回 `Err` 时调用方应把原因告知用户——静默失效会让用户以为
/// 策略已生效，而实际上 sudo 一直失败（P1：明确报错）。
pub fn validate_for_policy(policy: SudoPolicy, has_password: bool) -> Result<()> {
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

    #[test]
    fn deny_policy_never_enables_askpass() {
        let ctx = SudoContext {
            policy: SudoPolicy::Deny,
            password: Some(Zeroizing::new("pw".to_string())),
        };
        // 即使配置了密码，deny 也不部署 askpass——这是模式一的实现方式。
        assert!(!ctx.needs_askpass());
    }

    #[test]
    fn ask_and_auto_policies_enable_askpass() {
        for policy in [SudoPolicy::Ask, SudoPolicy::Auto] {
            let ctx = SudoContext {
                policy,
                password: Some(Zeroizing::new("pw".to_string())),
            };
            assert!(ctx.needs_askpass(), "{policy:?} 应部署 askpass");
        }
    }

    #[test]
    fn auto_injects_when_password_present() {
        assert_eq!(
            resolve_action(SudoPolicy::Auto, true, None),
            SudoAction::Inject
        );
    }

    #[test]
    fn auto_fails_closed_without_password() {
        // 关键：没有密码时不能假装成功，否则用户以为策略生效而实际一直失败。
        assert_eq!(
            resolve_action(SudoPolicy::Auto, false, None),
            SudoAction::Deny
        );
    }

    #[test]
    fn ask_injects_only_when_user_allows() {
        assert_eq!(
            resolve_action(SudoPolicy::Ask, true, Some(SudoDecision::Allow)),
            SudoAction::Inject
        );
        assert_eq!(
            resolve_action(SudoPolicy::Ask, true, Some(SudoDecision::Deny)),
            SudoAction::Deny
        );
    }

    #[test]
    fn ask_without_response_defaults_to_deny() {
        // 无人响应时不得默认提权。
        assert_eq!(
            resolve_action(SudoPolicy::Ask, true, None),
            SudoAction::Deny
        );
    }

    #[test]
    fn timeout_decision_is_deny() {
        assert_eq!(SudoDecision::on_timeout(), SudoDecision::Deny);
    }

    #[test]
    fn ask_with_allow_but_no_password_still_denies() {
        // 用户点了"允许"但主机没配密码——不能凭空提权。
        assert_eq!(
            resolve_action(SudoPolicy::Ask, false, Some(SudoDecision::Allow)),
            SudoAction::Deny
        );
    }

    #[test]
    fn validate_passes_for_deny_without_password() {
        assert!(validate_for_policy(SudoPolicy::Deny, false).is_ok());
    }

    #[test]
    fn validate_rejects_ask_without_password() {
        let err = validate_for_policy(SudoPolicy::Ask, false).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("sudo 密码"), "应给出可操作的提示：{msg}");
    }

    #[test]
    fn validate_passes_when_password_provided() {
        for policy in [SudoPolicy::Ask, SudoPolicy::Auto] {
            assert!(validate_for_policy(policy, true).is_ok());
        }
    }

    #[test]
    fn debug_hides_password() {
        let ctx = SudoContext {
            policy: SudoPolicy::Auto,
            password: Some(Zeroizing::new("SUPER-SECRET".to_string())),
        };
        let debug = format!("{ctx:?}");
        assert!(
            !debug.contains("SUPER-SECRET"),
            "Debug 输出不得包含密码：{debug}"
        );
        assert!(debug.contains("redacted"));
    }

    #[test]
    fn password_for_auto_only_returns_in_auto_mode() {
        let auto = SudoContext {
            policy: SudoPolicy::Auto,
            password: Some(Zeroizing::new("pw".to_string())),
        };
        assert_eq!(auto.password_for_auto(), Some("pw"));

        let ask = SudoContext {
            policy: SudoPolicy::Ask,
            password: Some(Zeroizing::new("pw".to_string())),
        };
        assert_eq!(ask.password_for_auto(), None, "ask 模式不应直接取密码");
    }
}
