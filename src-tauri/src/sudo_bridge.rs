//! ask 模式的用户确认桥接（Q33）。
//!
//! 终端运行时在 `ask` 策略下发出 `SudoRequest`，需要由**人类**决定
//! 是否注入密码。本模块负责把请求送到界面，并等回决定。
//!
//! 流程：
//! 1. 运行时调用 asker 回调 → 本模块登记 `request_id` 对应的应答通道
//! 2. 向前端发 `sudo://request` 事件；窗口若在托盘中则先唤出（否则用户看不到）
//! 3. 同时发一条系统通知，提示用户去处理
//! 4. 用户点「允许」/「拒绝」→ 前端调用 `sudo_respond` → 本模块回送决定
//!
//! 超时由运行时控制（`SUDO_ASK_TIMEOUT_SECS`）。超时或通道异常均按拒绝处理，
//! 因此即使界面完全无响应，也不会出现"无人确认却悄悄提权"（fail-closed）。
//!
//! 实现说明：待决表用 `std::sync::Mutex` 而非 `tokio::Mutex`。
//! 原因是 asker 是**同步接口**（运行时把它当普通闭包调用），
//! 而 tokio 的锁必须 `.await`。临界区仅是一次 HashMap 插入/删除，
//! 用标准库锁足够且不会阻塞运行时。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::oneshot;
use tauri::{AppHandle, Emitter};
use tauri_plugin_notification::NotificationExt;

use crate::terminal::sudo::{SudoDecision, SudoRequest};

/// 前端监听的 sudo 请求事件名。
pub const EVENT_SUDO_REQUEST: &str = "sudo://request";

/// 待决的 sudo 请求表。
#[derive(Default)]
pub struct SudoBridge {
    /// `request_id` → 应答通道。
    pending: Mutex<HashMap<String, oneshot::Sender<SudoDecision>>>,
}

impl SudoBridge {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 把请求登记进待决表，返回等待用户决定的接收端。
    ///
    /// 与 [`Self::request`] 分离：`request` 还要唤出窗口、发事件与系统通知
    /// （都依赖 `AppHandle`，单测无法构造），而**登记语义本身**必须能被直接测试。
    ///
    /// 锁中毒（此前有 panic）时返回 `None`：调用方应**宁可拒绝也不提权**，
    /// 并且不要再打扰用户——一个注定无法被应答的确认框只会让人困惑。
    fn register_pending(&self, request_id: String) -> Option<oneshot::Receiver<SudoDecision>> {
        let (tx, rx) = oneshot::channel();
        match self.pending.lock() {
            Ok(mut map) => {
                // 同 id 不应重复登记；若发生（异常情况），旧通道被替换，
                // 其接收端会因发送端被丢弃而立即结束，按拒绝处理。
                map.insert(request_id, tx);
                Some(rx)
            }
            Err(_) => {
                tracing::error!("sudo 待决表不可用，按拒绝处理");
                None
            }
        }
    }

    /// 登记一个 sudo 请求并通知用户，返回等待决定的接收端。
    ///
    /// 同步函数：登记只涉及一次加锁，随后的通知通过事件与系统通知完成，
    /// 不阻塞调用方（调用方是输出泵中的独立任务）。
    pub fn request(&self, app: &AppHandle, req: SudoRequest) -> oneshot::Receiver<SudoDecision> {
        let Some(rx) = self.register_pending(req.request_id.clone()) else {
            // 锁中毒：返回一个已关闭的通道（调用方立即得到"未获允许"），
            // 且不再唤出窗口/发通知——这次请求注定无法被应答。
            return oneshot::channel().1;
        };

        // 窗口可能被隐藏到托盘（D16）。若不唤出，用户看不到确认界面，
        // 请求只能等到超时被拒绝——功能等于不可用。
        crate::tray::reveal_main_window(app);

        // 发给前端展示确认界面。
        if let Err(e) = app.emit(EVENT_SUDO_REQUEST, &req) {
            tracing::error!("发送 sudo 确认事件失败：{e}");
        }

        // 系统通知：即使用户没盯着应用，也能注意到有请求待处理。
        let body = format!(
            "主机 {} 上的 sudo 请求等待你确认。请在 mf-perch 中选择「允许」或「拒绝」。",
            req.host_label
        );
        if let Err(e) = app
            .notification()
            .builder()
            .title("mf-perch · sudo 提权请求")
            .body(body)
            .show()
        {
            // 通知失败不致命——界面里仍能看到确认框。
            tracing::warn!("发送系统通知失败：{e}");
        }

        rx
    }

    /// 提交用户决定。返回是否成功送达（超时或重复提交时为 false）。
    pub fn respond(&self, request_id: &str, decision: SudoDecision) -> bool {
        let tx = self.pending.lock().ok().and_then(|mut m| m.remove(request_id));
        match tx {
            Some(tx) => {
                // 接收端可能已因超时被丢弃，此时 send 失败属正常。
                tx.send(decision).is_ok()
            }
            None => {
                tracing::debug!("sudo 请求 {request_id} 已超时或已处理");
                false
            }
        }
    }

    /// 当前待决请求数（诊断与测试用）。
    pub fn pending_count(&self) -> usize {
        self.pending.lock().map(|m| m.len()).unwrap_or(0)
    }

    /// 构造注入给终端运行时的 asker 回调。
    pub fn as_asker(self: &Arc<Self>, app: AppHandle) -> crate::terminal::SudoAsker {
        let bridge = self.clone();
        Arc::new(move |req: SudoRequest| bridge.request(&app, req))
    }
}

/// 前端提交 sudo 决定的 IPC 命令。
#[tauri::command]
pub async fn sudo_respond(
    request_id: String,
    allow: bool,
    bridge: tauri::State<'_, Arc<SudoBridge>>,
) -> Result<bool, ()> {
    let decision = if allow {
        SudoDecision::Allow
    } else {
        SudoDecision::Deny
    };
    Ok(bridge.respond(&request_id, decision))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn respond_returns_false_for_unknown_request() {
        let bridge = SudoBridge::new();
        // 未知或已超时的请求应安全返回 false，而不是 panic。
        assert!(!bridge.respond("sudo_nope", SudoDecision::Allow));
        assert_eq!(bridge.pending_count(), 0);
    }

    #[tokio::test]
    async fn respond_delivers_decision_to_waiter() {
        let bridge = SudoBridge::new();
        let rx = bridge
            .register_pending("sudo_1".into())
            .expect("登记应成功");

        assert_eq!(bridge.pending_count(), 1);
        assert!(bridge.respond("sudo_1", SudoDecision::Allow));

        assert_eq!(rx.await.unwrap(), SudoDecision::Allow);
        // 处理后应从待决表移除，避免内存泄漏。
        assert_eq!(bridge.pending_count(), 0);
    }

    #[test]
    fn respond_is_idempotent_for_same_request() {
        // 用户可能重复点击；第二次应安全失败而非 panic。
        let bridge = SudoBridge::new();
        // 接收端必须存活，否则第一次 respond 就会因发送失败而返回 false。
        let _rx = bridge.register_pending("r".into()).expect("登记应成功");

        assert!(bridge.respond("r", SudoDecision::Deny));
        assert!(!bridge.respond("r", SudoDecision::Deny));
    }

    #[test]
    fn dropping_receiver_does_not_panic_on_respond() {
        // 超时后运行时丢弃接收端，此时用户再点允许不应导致 panic。
        let bridge = SudoBridge::new();
        let rx = bridge.register_pending("r2".into()).expect("登记应成功");
        drop(rx);

        assert!(!bridge.respond("r2", SudoDecision::Allow));
    }

    #[test]
    fn deny_decision_is_delivered_when_user_rejects() {
        let bridge = SudoBridge::new();
        let rx = bridge.register_pending("r3".into()).expect("登记应成功");

        assert!(bridge.respond("r3", SudoDecision::Deny));
        assert_eq!(rx.blocking_recv().unwrap(), SudoDecision::Deny);
    }

    #[test]
    fn multiple_pending_requests_are_independent() {
        let bridge = SudoBridge::new();
        let rx1 = bridge.register_pending("a".into()).expect("登记应成功");
        let rx2 = bridge.register_pending("b".into()).expect("登记应成功");
        assert_eq!(bridge.pending_count(), 2);

        // 只处理其中一个，另一个应保持待决。
        assert!(bridge.respond("a", SudoDecision::Allow));
        assert_eq!(bridge.pending_count(), 1);
        assert_eq!(rx1.blocking_recv().unwrap(), SudoDecision::Allow);

        assert!(bridge.respond("b", SudoDecision::Deny));
        assert_eq!(rx2.blocking_recv().unwrap(), SudoDecision::Deny);
        assert_eq!(bridge.pending_count(), 0);
    }

    /// 登记语义：登记后进入待决表，用户提交决定后即被取走并送达。
    #[test]
    fn register_pending_adds_entry_and_respond_removes_it() {
        let bridge = SudoBridge::new();
        let rx = bridge
            .register_pending("sudo_x".into())
            .expect("正常状态下登记应成功");
        assert_eq!(bridge.pending_count(), 1, "登记后应有一个待决请求");

        assert!(bridge.respond("sudo_x", SudoDecision::Allow));
        assert_eq!(bridge.pending_count(), 0, "提交决定后应从待决表移除");
        assert_eq!(rx.blocking_recv().unwrap(), SudoDecision::Allow);
    }

    /// 同 id 重复登记时旧通道被替换：旧接收端按拒绝处理，待决表不重复计数。
    #[test]
    fn register_pending_replaces_entry_with_same_id() {
        let bridge = SudoBridge::new();
        let first = bridge
            .register_pending("dup".into())
            .expect("正常状态下登记应成功");
        let _second = bridge
            .register_pending("dup".into())
            .expect("正常状态下登记应成功");

        assert_eq!(bridge.pending_count(), 1, "同 id 不应重复计数");
        assert!(
            first.blocking_recv().is_err(),
            "被替换的请求应按拒绝处理（接收端随发送端丢弃而结束）"
        );
    }
}
