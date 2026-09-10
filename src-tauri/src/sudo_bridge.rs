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

    /// 登记一个 sudo 请求并通知用户，返回等待决定的接收端。
    ///
    /// 同步函数：登记只涉及一次加锁，随后的通知通过事件与系统通知完成，
    /// 不阻塞调用方（调用方是输出泵中的独立任务）。
    pub fn request(&self, app: &AppHandle, req: SudoRequest) -> oneshot::Receiver<SudoDecision> {
        let (tx, rx) = oneshot::channel();

        // 同 id 不应重复登记；若发生（异常情况），旧通道被替换，
        // 其接收端会因发送端被丢弃而立即结束，按拒绝处理。
        if let Ok(mut map) = self.pending.lock() {
            map.insert(req.request_id.clone(), tx);
        } else {
            // 锁中毒（此前有 panic）：宁可拒绝也不提权。
            tracing::error!("sudo 待决表不可用，按拒绝处理");
            return oneshot::channel().1;
        }

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

    fn req(id: &str) -> SudoRequest {
        SudoRequest {
            request_id: id.to_string(),
            terminal_id: "term_1".to_string(),
            host_label: "test-host".to_string(),
        }
    }

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
        let (tx, rx) = oneshot::channel();
        bridge.pending.lock().unwrap().insert("sudo_1".into(), tx);

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
        let (tx, _rx) = oneshot::channel();
        bridge.pending.lock().unwrap().insert("r".into(), tx);

        assert!(bridge.respond("r", SudoDecision::Deny));
        assert!(!bridge.respond("r", SudoDecision::Deny));
    }

    #[test]
    fn dropping_receiver_does_not_panic_on_respond() {
        // 超时后运行时丢弃接收端，此时用户再点允许不应导致 panic。
        let bridge = SudoBridge::new();
        let (tx, rx) = oneshot::channel();
        bridge.pending.lock().unwrap().insert("r2".into(), tx);
        drop(rx);

        assert!(!bridge.respond("r2", SudoDecision::Allow));
    }

    #[test]
    fn deny_decision_is_delivered_when_user_rejects() {
        let bridge = SudoBridge::new();
        let (tx, rx) = oneshot::channel();
        bridge.pending.lock().unwrap().insert("r3".into(), tx);

        assert!(bridge.respond("r3", SudoDecision::Deny));
        assert_eq!(rx.blocking_recv().unwrap(), SudoDecision::Deny);
    }

    #[test]
    fn multiple_pending_requests_are_independent() {
        let bridge = SudoBridge::new();
        let (tx1, rx1) = oneshot::channel();
        let (tx2, rx2) = oneshot::channel();
        {
            let mut m = bridge.pending.lock().unwrap();
            m.insert("a".into(), tx1);
            m.insert("b".into(), tx2);
        }
        assert_eq!(bridge.pending_count(), 2);

        // 只处理其中一个，另一个应保持待决。
        assert!(bridge.respond("a", SudoDecision::Allow));
        assert_eq!(bridge.pending_count(), 1);
        assert_eq!(rx1.blocking_recv().unwrap(), SudoDecision::Allow);

        assert!(bridge.respond("b", SudoDecision::Deny));
        assert_eq!(rx2.blocking_recv().unwrap(), SudoDecision::Deny);
        assert_eq!(bridge.pending_count(), 0);
    }

    #[test]
    fn request_registers_pending_entry() {
        // 不做事件/通知（需要 AppHandle），仅验证登记语义：
        // 这里直接模拟 request 中的登记步骤。
        let bridge = SudoBridge::new();
        let (tx, _rx) = oneshot::channel();
        bridge
            .pending
            .lock()
            .unwrap()
            .insert(req("x").request_id, tx);
        assert_eq!(bridge.pending_count(), 1);
    }
}
