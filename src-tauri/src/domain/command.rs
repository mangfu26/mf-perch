use serde::{Deserialize, Serialize};

use super::new_id;

/// 命令执行状态机（Q4）。
///
/// `queued` → `running` → `completed` | `failed`
/// 终端断开时处于 `running` 的命令转为 `failed`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    /// 已入队，等待同一终端上的前一条命令结束（Q4 采纳"排队"）。
    Queued,
    /// 正在执行。
    Running,
    /// 执行完成。注意：完成不等于成功，退出码可能非零。
    Completed,
    /// 因终端断开或内部错误未能正常完成。
    Failed,
}

impl CommandStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }

    /// 是否仍在进行中（决定 MCP 是否返回轮询句柄）。
    pub fn is_pending(self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }
}

/// 一条命令的执行记录（审计的核心数据，Q17 / Q12）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandRecord {
    pub id: String,
    pub terminal_id: String,
    /// 终端内递增序号，用于与远端结束标记配对（D3）。
    pub seq: u64,
    /// 人类可读的原始命令。**不得包含 sudo 密码等凭据内容**（D11）。
    pub command: String,
    pub status: CommandStatus,
    /// 命令退出码；未结束时为 None。
    pub exit_code: Option<i32>,
    /// 耗时（毫秒，Q4 要求毫秒级）。
    pub duration_ms: Option<u64>,
    /// 是否被截断（超出输出上限，D14）。
    pub truncated: bool,
    /// 输出总字节数，截断时告知真实规模。
    pub output_bytes: Option<u64>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

impl CommandRecord {
    pub fn new(terminal_id: impl Into<String>, seq: u64, command: impl Into<String>) -> Self {
        Self {
            id: new_id("cmd"),
            terminal_id: terminal_id.into(),
            seq,
            command: command.into(),
            status: CommandStatus::Queued,
            exit_code: None,
            duration_ms: None,
            truncated: false,
            output_bytes: None,
            created_at: super::now_rfc3339(),
            started_at: None,
            finished_at: None,
        }
    }

    /// 命令是否成功（已完成且退出码为 0）。
    pub fn is_success(&self) -> bool {
        self.status == CommandStatus::Completed && self.exit_code == Some(0)
    }
}

/// 命令及其输出——输出体量可能很大，因此与记录分表存储，按需加载。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandWithOutput {
    #[serde(flatten)]
    pub record: CommandRecord,
    pub output: String,
}

/// 命令详情（人类审计视图与 MCP 轮询返回共用的形状）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandStatusView {
    pub command_id: String,
    pub terminal_id: String,
    pub command: String,
    pub status: CommandStatus,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u64>,
    pub truncated: bool,
    /// 已累积的输出（轮询时为部分输出，结束时为完整输出）。
    pub output: Option<String>,
    /// 输出是否被省略（轮询时未请求输出时为 true）。
    pub output_omitted: bool,
}
