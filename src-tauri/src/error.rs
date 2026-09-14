/// 应用统一错误类型。
///
/// 面向 MCP / IPC 的返回需要可读的中文原因，因此每个变体都携带足够上下文，
/// 避免上层只能返回"操作失败"这类模糊信息（见 docs/design/principles.md P1）。
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("数据库错误：{0}")]
    Database(#[from] rusqlite::Error),

    #[error("SSH 连接失败：{0}")]
    SshConnect(String),

    #[error("SSH 认证失败：{0}")]
    SshAuth(String),

    /// 主机密钥与已记录的不一致——按 D10 必须阻止连接，等待人类确认。
    #[error("主机密钥已变更，连接已阻止：{0}")]
    HostKeyMismatch(String),

    #[error("终端不存在：{0}")]
    TerminalNotFound(String),

    #[error("主机不存在：{0}")]
    HostNotFound(String),

    #[error("认证信息不存在：{0}")]
    CredentialNotFound(String),

    #[error("命令不存在：{0}")]
    CommandNotFound(String),

    #[error("终端已被归档，Agent 不可见：{0}")]
    TerminalArchived(String),

    #[error("终端连接已断开（broken），请重建终端：{0}")]
    TerminalBroken(String),

    #[error("终端数量已达上限（{scope}），请先归档不再使用的终端")]
    TerminalQuotaExceeded { scope: &'static str },

    #[error("终端命令队列已满（上限 {limit} 条），请稍后重试")]
    CommandQueueFull { limit: usize },

    #[error("远端未安装 bash，无法建立终端。请在该主机安装 bash 后重试")]
    BashNotAvailable,

    #[error("凭据无法解密：{0}。请检查密钥提供方式是否可用")]
    CredentialUndecryptable(String),

    #[error("加密错误：{0}")]
    Crypto(String),

    #[error("密钥服务不可用：{0}")]
    KeyServiceUnavailable(String),

    #[error("配置错误：{0}")]
    Config(String),

    #[error("MCP 服务错误：{0}")]
    Mcp(String),

    #[error("IO 错误：{0}")]
    Io(#[from] std::io::Error),

    #[error("序列化错误：{0}")]
    Serde(#[from] serde_json::Error),

    #[error("参数无效：{0}")]
    InvalidArgument(String),

    #[error("内部错误：{0}")]
    Internal(String),
}

impl AppError {
    /// 供 MCP 工具返回的错误码，便于 Agent 判断是否能自行恢复。
    pub fn code(&self) -> &'static str {
        match self {
            AppError::HostNotFound(_) => "host_not_found",
            AppError::CredentialNotFound(_) => "credential_not_found",
            AppError::TerminalNotFound(_) => "terminal_not_found",
            AppError::CommandNotFound(_) => "command_not_found",
            AppError::TerminalArchived(_) => "terminal_archived",
            AppError::TerminalBroken(_) => "terminal_broken",
            AppError::TerminalQuotaExceeded { .. } => "terminal_quota_exceeded",
            AppError::CommandQueueFull { .. } => "command_queue_full",
            AppError::BashNotAvailable => "bash_not_available",
            AppError::SshAuth(_) => "ssh_auth_failed",
            AppError::SshConnect(_) => "ssh_connect_failed",
            AppError::HostKeyMismatch(_) => "host_key_mismatch",
            AppError::CredentialUndecryptable(_) => "credential_undecryptable",
            AppError::InvalidArgument(_) => "invalid_argument",
            _ => "internal_error",
        }
    }
}

pub type Result<T> = std::result::Result<T, AppError>;

// 说明（D43）：这里曾有一个 `ErrorPayload { code, message }` 结构，
// 但从未被任何代码使用，而 MCP 工具实际返回的是 `{ error, code }`
// （见 `mcp/tools.rs` 的 `ToolError`）。两份"错误形状"并存的后果是：
// 后来者可能照抄那份没被使用的，从而与线上契约不一致。
// 已删除未使用者，保留线上形状 —— 契约以 `docs/mcp-tools.md` 为准。
