use serde::{Deserialize, Serialize};

use super::new_id;

/// 命令执行状态机（Q4）。
///
/// `queued` → `running` → `completed` | `failed` | `connection_lost`
///
/// **`failed` 与 `connection_lost` 刻意分开**（客户 2026-09-16 要求）：
/// 两者对审计的含义完全不同——
/// - `failed`：命令**跑过并自己失败**（退出码非零），结果已知；
/// - `connection_lost`：**连接断了，命令结局未知**（可能已执行完、可能跑了一半），
///   人类需要据此决定"要不要上去核对"。
///
/// 混为一谈会让审计出现最坏的一种误导：把"没跑完/不知道"写成"执行完成"，
/// 或者把"传输中断"写成"命令失败"。两者都会让人类得出错误结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    /// 已入队，等待同一终端上的前一条命令结束（Q4 采纳"排队"）。
    Queued,
    /// 正在执行。
    Running,
    /// 执行完成。注意：完成不等于成功，退出码可能非零。
    Completed,
    /// 命令**执行过并失败**（退出码非零，或发送/内部错误）。
    Failed,
    /// **连接断开导致结局未知**（命令可能已执行完、也可能只跑了一半）。
    ///
    /// 与 `Failed` 的区别是审计语义，不是严重程度：这里没有"命令失败"的证据，
    /// 只有"我们不知道结果"的事实。
    ConnectionLost,
}

impl CommandStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::ConnectionLost => "connection_lost",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "connection_lost" => Some(Self::ConnectionLost),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `connection_lost` 是**前后端共享的字符串契约**：前端按 `status` 上色与显示标签，
    /// 数据库 CHECK 约束也认这个值。三处必须一致，改动时不能只改一边。
    #[test]
    fn status_strings_round_trip() {
        let all = [
            CommandStatus::Queued,
            CommandStatus::Running,
            CommandStatus::Completed,
            CommandStatus::Failed,
            CommandStatus::ConnectionLost,
        ];
        for s in all {
            assert_eq!(
                CommandStatus::parse(s.as_str()),
                Some(s),
                "{s:?} 的字符串往返失败（值为 {}）",
                s.as_str()
            );
        }
        assert_eq!(CommandStatus::ConnectionLost.as_str(), "connection_lost");
        // 未知字符串不得被猜成某个状态——否则库里出现脏值时会静默显示成"已完成"。
        assert_eq!(CommandStatus::parse("unknown"), None);
    }

    /// **区分"命令失败"与"因断线而未完成"**（客户 2026-09-16 要求）。
    ///
    /// 判别性：把 `connection_lost` 当成 `failed`（或反过来当成 `completed`）
    /// 都会让这条断言失败——而这两类混淆正是审计里最要命的误导。
    #[test]
    fn connection_lost_is_distinct_from_failed_and_completed() {
        assert_ne!(CommandStatus::ConnectionLost, CommandStatus::Failed);
        assert_ne!(CommandStatus::ConnectionLost, CommandStatus::Completed);
        assert_ne!(CommandStatus::ConnectionLost.as_str(), CommandStatus::Failed.as_str());
    }

    /// 只有 `queued` / `running` 算"进行中"：`connection_lost` 是**终态**
    /// （连接断了就不会再有结果），否则轮询会永远等下去。
    #[test]
    fn connection_lost_is_terminal_not_pending() {
        assert!(!CommandStatus::ConnectionLost.is_pending());
        assert!(CommandStatus::Queued.is_pending());
        assert!(CommandStatus::Running.is_pending());
    }

    /// 断线的命令**不算成功**——即便它的退出码恰好是 0 也不能算。
    #[test]
    fn connection_lost_is_never_success() {
        let mut rec = CommandRecord::new("term_1", 1, "ls");
        rec.status = CommandStatus::ConnectionLost;
        rec.exit_code = Some(0);
        assert!(
            !rec.is_success(),
            "连接断开的命令结局未知，不得显示为成功"
        );
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
