//! 终端运行时的领域事件（D22「应用内实时状态更新」）。
//!
//! ## 为什么需要
//!
//! 人类侧界面此前是**纯拉取**：只在页面挂载时取一次数，因此 Agent 执行命令、
//! 会话断开或自动重连后，界面要等人工点刷新才对得上——审核者可能据此误判
//! 终端状态（例如已自动重连却仍显示"连接已断开"）。
//!
//! ## 设计要点
//!
//! - **运行时不依赖 Tauri**：事件经 channel 交给外壳层，由外壳层转成前端可见的
//!   Tauri 事件。这样运行时可以在测试里直接订阅并断言事件序列。
//! - **只用于"界面新鲜度"**：真实状态始终以数据库为准。事件丢了最坏只是界面
//!   晚一拍（外壳层另有窗口重新可见时的校准刷新），不会造成状态错乱，
//!   因此投递采用"尽力而为"，不阻塞、不回压。
//! - **不承载敏感内容**：只带 ID、状态与命令文本；命令输出不在此通道上，
//!   界面需要输出时按 `command_id` 走既有的 IPC 拉取。
//! - 系统通知仍按 D22 留在二期：本模块只解决"应用内状态更新"。

use serde::Serialize;

/// 前端监听的 Tauri 事件名（单一通道，负载带 `kind` 标签）。
///
/// 约定与 `sudo://request` 一致：`<域>://<用途>`。
pub const EVENT_TERMINAL: &str = "terminal://event";

/// 会话状态取值（与 [`crate::domain::terminal::TerminalStatus`] 一致，序列化为字符串）。
pub const STATUS_ACTIVE: &str = "active";
pub const STATUS_BROKEN: &str = "broken";
pub const STATUS_ARCHIVED: &str = "archived";

/// 终端运行时对外发布的事件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TerminalEvent {
    /// 终端已建立（人类界面需要立刻出现这一行）。
    TerminalCreated {
        terminal_id: String,
        host_id: String,
    },
    /// 终端已被删除（人类界面需要移除）。
    TerminalRemoved { terminal_id: String },
    /// 一条命令开始执行（界面可显示"执行中"）。
    CommandStarted {
        terminal_id: String,
        command_id: String,
        command: String,
    },
    /// 一条命令结束（成功或失败；界面据此更新状态、退出码与耗时）。
    CommandFinished {
        terminal_id: String,
        command_id: String,
        status: String,
        exit_code: Option<i32>,
        duration_ms: Option<u64>,
    },
    /// 会话状态变化：建立 / 断线 / 自动重连 / 归档 / 恢复。
    ///
    /// `reason` 为面向人类的简短说明，例如"连接已断开""会话已自动重建"。
    SessionChanged {
        terminal_id: String,
        status: String,
        reason: String,
    },
}

/// 事件出口：由外壳层注入，运行时只管发送（尽力而为）。
pub type EventSink = tokio::sync::mpsc::UnboundedSender<TerminalEvent>;

/// 构造（发送端, 接收端）一对事件通道。
pub fn event_channel() -> (EventSink, tokio::sync::mpsc::UnboundedReceiver<TerminalEvent>) {
    tokio::sync::mpsc::unbounded_channel()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_serialize_with_a_kind_tag() {
        // 前端按 kind 分派，因此标签必须稳定且为 snake_case。
        let ev = TerminalEvent::CommandStarted {
            terminal_id: "term_1".into(),
            command_id: "cmd_1".into(),
            command: "ls -la".into(),
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["kind"], "command_started");
        assert_eq!(json["terminal_id"], "term_1");
        assert_eq!(json["command"], "ls -la");

        let ev = TerminalEvent::SessionChanged {
            terminal_id: "term_1".into(),
            status: STATUS_BROKEN.into(),
            reason: "连接已断开".into(),
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["kind"], "session_changed");
        assert_eq!(json["status"], "broken");
    }

    #[test]
    fn event_channel_delivers_in_order() {
        let (tx, mut rx) = event_channel();
        tx.send(TerminalEvent::TerminalRemoved {
            terminal_id: "t1".into(),
        })
        .unwrap();
        tx.send(TerminalEvent::TerminalRemoved {
            terminal_id: "t2".into(),
        })
        .unwrap();

        assert_eq!(
            rx.try_recv().unwrap(),
            TerminalEvent::TerminalRemoved {
                terminal_id: "t1".into()
            }
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            TerminalEvent::TerminalRemoved {
                terminal_id: "t2".into()
            }
        );
    }
}
