use serde::{Deserialize, Serialize};

use super::{new_id, now_rfc3339};

/// 认证方式（Q10）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKind {
    /// 用户名 + 密码。
    Password,
    /// 用户名 + 私钥。
    Key,
}

impl CredentialKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::Key => "key",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "password" => Some(Self::Password),
            "key" => Some(Self::Key),
            _ => None,
        }
    }
}

/// 认证信息：敏感数据，**对 AI Agent 完全不可见**（AGENTS.md 0.1）。
///
/// 本结构体中的 `secret` 与 `passphrase` 在内存中是明文，
/// 仅在建立连接时短暂存在，用后需 `zeroize`（D6）。
/// 持久化时由 `crate::store::crypto` 加密，数据库中不出现明文。
#[derive(Clone, Serialize, Deserialize)]
pub struct Credential {
    pub id: String,
    pub name: Option<String>,
    pub username: String,
    pub kind: CredentialKind,
    /// **由人类声明**：用该身份登录时拿到的本身就是特权用户（uid 0，D60）。
    ///
    /// 之所以落在**凭据**而不是主机上：它说的是"我是谁"，而 `sudo_policy` 那三档
    /// 说的是"允许 Agent 在这台机器上使多大劲"——两份事实的粒度不同，
    /// 一份凭据可被 N 台主机引用，标记跟着身份走才不用逐台改。
    ///
    /// 声明只表达意图，**事实由远端核实**：包装脚本仍打印 `${EUID}`，
    /// 实际 uid 非 0 时如实告警（同一把密钥在 A 机是 root、在 B 机不是，就会走到这条）。
    /// 为真时特权通道不包 `sudo`、不取口令也不向人类确认；`deny` 仍优先（见 D60）。
    pub is_privileged: bool,
    /// 密码，或私钥正文（OpenSSH / PEM 格式）。
    ///
    /// **刻意不参与序列化**（V17）：`Debug` 已脱敏，但若允许 `Serialize`，
    /// 任何对 `Credential` 调用 `serde_json::to_string` 的地方都会绕过脱敏、
    /// 把明文发往前端或写入日志。凭据只应在应用内部按需解密使用，
    /// 需要的字段请用 [`CredentialSummary`] 这类显式脱敏结构对外输出。
    #[serde(skip_serializing)]
    pub secret: String,
    /// 私钥口令（passphrase），仅 `kind == Key` 时可能非空（Q10）。
    #[serde(skip_serializing)]
    pub passphrase: Option<String>,
    /// 公钥指纹（SHA256），供人类界面对比；私钥正文不回显（Q10）。
    pub fingerprint: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl std::fmt::Debug for Credential {
    /// 刻意遮蔽敏感字段，防止凭据经 `{:?}`、日志或 panic 信息泄露（D6 / P2）。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("username", &self.username)
            .field("kind", &self.kind)
            .field("secret", &"<redacted>")
            .field("passphrase", &self.passphrase.as_ref().map(|_| "<redacted>"))
            .field("fingerprint", &self.fingerprint)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

impl Credential {
    pub fn new(
        username: impl Into<String>,
        kind: CredentialKind,
        secret: impl Into<String>,
    ) -> Self {
        let now = now_rfc3339();
        Self {
            id: new_id("cred"),
            name: None,
            username: username.into(),
            kind,
            is_privileged: false,
            secret: secret.into(),
            passphrase: None,
            fingerprint: None,
            created_at: now.clone(),
            updated_at: now,
        }
    }

    /// 该认证信息是否可用于 SSH 认证。
    pub fn is_usable(&self) -> bool {
        !self.username.is_empty() && !self.secret.is_empty()
    }
}

/// 供人类界面列表展示的认证信息摘要——**不含任何敏感内容**。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialSummary {
    pub id: String,
    pub name: Option<String>,
    pub username: String,
    pub kind: CredentialKind,
    /// 该身份是否被声明为登录即特权用户（D60）；供主机表单判断 sudo 策略是否适用。
    pub is_privileged: bool,
    pub fingerprint: Option<String>,
    /// 是否有口令（不解开口令本身）。
    pub has_passphrase: bool,
    /// 被哪些主机引用，便于人类判断删除影响。
    pub used_by_hosts: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl From<&Credential> for CredentialSummary {
    fn from(c: &Credential) -> Self {
        Self {
            id: c.id.clone(),
            name: c.name.clone(),
            username: c.username.clone(),
            kind: c.kind,
            is_privileged: c.is_privileged,
            fingerprint: c.fingerprint.clone(),
            has_passphrase: c.passphrase.is_some(),
            used_by_hosts: Vec::new(),
            created_at: c.created_at.clone(),
            updated_at: c.updated_at.clone(),
        }
    }
}
