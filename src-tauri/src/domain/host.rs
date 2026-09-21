use serde::{Deserialize, Serialize};

use super::{new_id, now_rfc3339};

/// sudo 密码处理策略（Q33，客户要求一期全部实现）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SudoPolicy {
    /// 禁止注入：sudo 因无密码而失败，应用翻译为明确提示。默认值（fail-closed）。
    Deny,
    /// 每次询问：经系统通知征求用户同意后注入密码，拒绝或超时则失败。
    Ask,
    /// 自动注入：收到请求标记后立即注入密码。
    Auto,
}

impl Default for SudoPolicy {
    fn default() -> Self {
        // 最安全默认值：不允许提权，由用户显式放宽。
        Self::Deny
    }
}

impl SudoPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deny => "deny",
            Self::Ask => "ask",
            Self::Auto => "auto",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "deny" => Some(Self::Deny),
            "ask" => Some(Self::Ask),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }
}

/// 终端环境加载策略（D4）。
///
/// JSON 词表**逐变体写死**而不是用 `rename_all` 推导：变体名（`LoginThenTask`）
/// 的 snake_case 与 `as_str()` 不同，而 JSON、DB 列、前端 `<select>` 选项值必须同一套词。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShellEnvMode {
    /// 登录 shell（`bash -l`）：加载 profile，接近人类 SSH 登录环境。默认。
    #[serde(rename = "login")]
    LoginThenTask,
    /// 干净模式：不加载 profile，可预测、无副作用。
    #[serde(rename = "clean")]
    CleanThenTask,
}

impl Default for ShellEnvMode {
    fn default() -> Self {
        Self::LoginThenTask
    }
}

impl ShellEnvMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LoginThenTask => "login",
            Self::CleanThenTask => "clean",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "login" => Some(Self::LoginThenTask),
            "clean" => Some(Self::CleanThenTask),
            _ => None,
        }
    }
}

/// SSH 主机。
///
/// 由人类创建与管理；AI Agent 只读（见 AGENTS.md 0.1 权限边界）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Host {
    pub id: String,
    /// 名称，人类可读，可选。
    pub name: Option<String>,
    /// 主机地址：IP 或域名。
    pub address: String,
    pub port: u16,
    /// 绑定的认证信息 ID。Agent 永远看不到凭据内容，只由应用内部据此查找。
    pub credential_id: Option<String>,
    /// 跳板机（Q9：MVP 唯一的 SSH 高级能力）。
    pub proxy_jump_host_id: Option<String>,
    /// sudo 策略（Q33）。
    pub sudo_policy: SudoPolicy,
    /// sudo 密码来源：`own` 使用独立密码，`reuse_login` 复用 SSH 登录密码（Q33）。
    pub sudo_password_source: SudoPasswordSource,
    /// 终端环境加载模式（D4）。
    pub shell_env_mode: ShellEnvMode,
    /// 每主机可选的初始化脚本，用于 nvm / conda 等需显式加载的环境（D4）。
    pub init_script: Option<String>,
    /// 已信任的主机密钥（TOFU，D10）。不一致时必须阻止连接。
    pub host_key: Option<String>,
    /// 主机密钥指纹，供人类界面对比确认。
    pub host_key_fingerprint: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// sudo 密码的来源（Q33）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SudoPasswordSource {
    /// 为该主机单独配置一个 sudo 密码。
    Own,
    /// 复用该主机的 SSH 登录密码，免去重复录入。
    ReuseLogin,
}

impl Default for SudoPasswordSource {
    fn default() -> Self {
        Self::ReuseLogin
    }
}

impl SudoPasswordSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Own => "own",
            Self::ReuseLogin => "reuse_login",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "own" => Some(Self::Own),
            "reuse_login" => Some(Self::ReuseLogin),
            _ => None,
        }
    }
}

impl Host {
    /// 新建主机，套用安全默认值。
    pub fn new(address: impl Into<String>, port: u16) -> Self {
        let now = now_rfc3339();
        Self {
            id: new_id("host"),
            name: None,
            address: address.into(),
            port,
            credential_id: None,
            proxy_jump_host_id: None,
            sudo_policy: SudoPolicy::Deny,
            sudo_password_source: SudoPasswordSource::ReuseLogin,
            shell_env_mode: ShellEnvMode::LoginThenTask,
            init_script: None,
            host_key: None,
            host_key_fingerprint: None,
            created_at: now.clone(),
            updated_at: now,
        }
    }
}

/// 供人类界面展示的主机摘要。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostSummary {
    pub id: String,
    pub name: Option<String>,
    pub address: String,
    pub port: u16,
    pub has_credential: bool,
    pub sudo_policy: SudoPolicy,
    pub active_terminals: u32,
    pub archived_terminals: u32,
    // ── 以下字段供人类侧「编辑主机」表单**原样回填**（B5）──
    //
    // 表单提交时整体覆盖主机配置，因此摘要里必须带上这些可编辑字段；
    // 否则用户"只改个名字"就会把它们静默清空（凭据绑定、跳板机、
    // 环境加载方式、初始化脚本、sudo 密码来源）。
    // 注意：它们只出现在**人类侧 IPC**，不进入 [`HostPublicInfo`]（Agent 可见），
    // 权限边界（AGENTS.md 0.1）不受影响。
    pub credential_id: Option<String>,
    pub proxy_jump_host_id: Option<String>,
    pub sudo_password_source: SudoPasswordSource,
    pub shell_env_mode: ShellEnvMode,
    pub init_script: Option<String>,
}

/// 供 AI Agent 列出主机时使用的最小信息集。
///
/// 刻意**不包含**凭据 ID、跳板机密钥、sudo 配置等敏感或无关字段，
/// 避免 Agent 借由列表接口推断出可用凭据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostPublicInfo {
    pub id: String,
    pub name: Option<String>,
    pub address: String,
    pub port: u16,
    pub ready: bool,
}

impl From<&Host> for HostPublicInfo {
    fn from(h: &Host) -> Self {
        Self {
            id: h.id.clone(),
            name: h.name.clone(),
            address: h.address.clone(),
            port: h.port,
            ready: h.credential_id.is_some(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 守的契约：领域枚举的**线上词汇表只有一个来源**——`as_str()`（写进 DB 列、
    /// 也写进环境快照）与 serde 序列化出的 JSON 必须是同一个字符串。
    ///
    /// 前端表单的 `<select>` 选项值、`HostSummary` 回填与 `ipc::HostInput` 的
    /// `parse` 全部按 `as_str()` 那一套词表工作；JSON 一旦由变体名推导出另一个
    /// 拼写，编辑表单就匹配不到选项而显示空白（`ShellEnvMode` 曾如此）。
    #[test]
    fn serde_json_matches_as_str_for_every_domain_enum() {
        let sudo_policy = [
            (SudoPolicy::Deny, "deny"),
            (SudoPolicy::Ask, "ask"),
            (SudoPolicy::Auto, "auto"),
        ];
        for (variant, want) in sudo_policy {
            assert_eq!(variant.as_str(), want, "SudoPolicy 的 DB 词表变了");
            assert_eq!(
                serde_json::to_value(variant).unwrap(),
                json!(want),
                "SudoPolicy 的 JSON 应为 {want:?}，实际与 as_str() 不一致",
            );
            assert_eq!(
                serde_json::from_value::<SudoPolicy>(json!(want)).unwrap(),
                variant
            );
            assert_eq!(SudoPolicy::parse(want), Some(variant));
        }

        let shell_env_mode = [
            (ShellEnvMode::LoginThenTask, "login"),
            (ShellEnvMode::CleanThenTask, "clean"),
        ];
        for (variant, want) in shell_env_mode {
            assert_eq!(variant.as_str(), want, "ShellEnvMode 的 DB 词表变了");
            assert_eq!(
                serde_json::to_value(variant).unwrap(),
                json!(want),
                "ShellEnvMode 的 JSON 应为 {want:?}（前端选项值与 DB 列都用它），\
                 实际与 as_str() 不一致会让编辑主机的「环境加载方式」回填空白",
            );
            assert_eq!(
                serde_json::from_value::<ShellEnvMode>(json!(want)).unwrap(),
                variant,
            );
            assert_eq!(ShellEnvMode::parse(want), Some(variant));
        }

        let sudo_password_source = [
            (SudoPasswordSource::Own, "own"),
            (SudoPasswordSource::ReuseLogin, "reuse_login"),
        ];
        for (variant, want) in sudo_password_source {
            assert_eq!(variant.as_str(), want, "SudoPasswordSource 的 DB 词表变了");
            assert_eq!(
                serde_json::to_value(variant).unwrap(),
                json!(want),
                "SudoPasswordSource 的 JSON 应为 {want:?}，实际与 as_str() 不一致",
            );
            assert_eq!(
                serde_json::from_value::<SudoPasswordSource>(json!(want)).unwrap(),
                variant,
            );
            assert_eq!(SudoPasswordSource::parse(want), Some(variant));
        }
    }
}
