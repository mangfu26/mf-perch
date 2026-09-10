//! 领域模型：SSH 主机、认证信息、终端、命令记录。
//!
//! 模型只描述数据形状与状态迁移，持久化细节在 `crate::store`，
//! SSH 行为在 `crate::ssh`。

pub mod command;
pub mod credential;
pub mod host;
pub mod terminal;

pub use command::{CommandRecord, CommandStatus};
pub use credential::{Credential, CredentialKind};
pub use host::{Host, SudoPolicy};
pub use terminal::{Terminal, TerminalStatus};

use serde::{Deserialize, Serialize};

/// 生成带前缀的唯一 ID，便于日志与界面辨识。
pub fn new_id(prefix: &str) -> String {
    format!("{}_{}", prefix, uuid::Uuid::new_v4().simple())
}

/// 当前时间的 RFC3339 表示，统一时间格式便于排序与展示。
pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// 每主机默认终端配额（Q11）。
pub const DEFAULT_TERMINAL_LIMIT_PER_HOST: u32 = 5;
/// 全局默认终端配额（Q11）。
pub const DEFAULT_TERMINAL_LIMIT_GLOBAL: u32 = 20;
/// 单终端命令队列上限（Q4 采纳"排队"方案）。
pub const DEFAULT_COMMAND_QUEUE_LIMIT: usize = 10;
/// 单条命令输出上限：字节数（Q4 / Q12）。
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 1024 * 1024;
/// 单条命令输出上限：行数（Q4 / Q12）。
pub const DEFAULT_MAX_OUTPUT_LINES: usize = 20_000;

/// MCP 命令默认同步等待秒数（Q4）。
pub const DEFAULT_SYNC_WAIT_SECS: u64 = 30;
/// MCP 同步等待上限，留出余量避免撞上 MCP 客户端自身超时（Q4）。
pub const MAX_SYNC_WAIT_SECS: u64 = 50;
/// sudo `ask` 模式等待用户响应的超时（Q33）。
pub const SUDO_ASK_TIMEOUT_SECS: u64 = 60;

/// 排序方向，供历史查询使用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortOrder {
    Asc,
    Desc,
}

impl Default for SortOrder {
    fn default() -> Self {
        Self::Desc
    }
}
