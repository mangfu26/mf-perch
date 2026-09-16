//! 终端运行时：会话池 + 串行命令队列 + 输出泵（Q4 / D3）。
//!
//! 并发语义（Q4 采纳"排队"）：
//! - **同一终端同时只执行一条命令**，后续命令排队；
//! - 队列上限 `DEFAULT_COMMAND_QUEUE_LIMIT`，超出直接拒绝；
//! - 异步命令会占用该终端直到结束，若要并行需创建多个终端。
//!
//! 输出归属：由于串行，任一时刻每个终端最多只有一条"当前命令"，
//! 因此输出泵可以把到达的输出行记到该命令上。
//! **命令结束则按 `command_id` 精确配对**（V5）：结束标记由远端回显应用侧
//! 生成的 id，不依赖两侧各自自增的序号——两者在恢复终端等场景下必然错位。
//!
//! 数据库访问策略：`rusqlite::Connection` 是 `Send` 但**不是** `Sync`，
//! 因此不能把 `&Connection` 借用跨越 `.await`（那样 future 将不再是 `Send`）。
//! 这里统一借用 `&Db`（`Mutex<Connection>`），并**分阶段加锁**：
//! 读取 → 释放锁 → 进行网络操作 → 重新加锁写入。既满足 `Send`，
//! 也避免在 SSH 连接（最长 15 秒）期间把数据库锁住、拖住界面。

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{oneshot, watch, Mutex};

use crate::domain::command::{CommandRecord, CommandStatus};
use crate::domain::host::SudoPolicy;
use crate::domain::terminal::{Terminal, TerminalStatus};
use crate::domain::now_rfc3339;
use crate::error::{AppError, Result};
use crate::ssh::auth::AuthMethod;
use crate::ssh::output::OutputAccumulator;
use crate::ssh::{Session, SessionOutput};
use crate::store::crypto::KEY_LEN;
use crate::store::{commands as cmd_store, credentials, hosts, terminals};

pub mod events;
pub mod sudo;

pub use events::{event_channel, EventSink, TerminalEvent, EVENT_TERMINAL};
pub use sudo::{
    resolve_action, policy_description, validate_for_policy, AskOutcome, SudoAction, SudoContext,
    SudoDecision, SudoRequest,
};

/// 数据库句柄类型（见 [`crate::store::Db`]）。
pub type Db = crate::store::Db;

/// ask 模式下等待用户响应的回调。
///
/// 由上层（Tauri 层）实现：发系统通知并等待用户点"允许/拒绝"。
/// 运行时不直接依赖 Tauri，以便在测试中使用固定决策的替身。
pub type SudoAsker = Arc<dyn Fn(SudoRequest) -> oneshot::Receiver<SudoDecision> + Send + Sync>;

/// 正在执行的命令的共享状态。
struct ActiveCommand {
    command_id: String,
    output: OutputAccumulator,
    /// 命令结束后的退出码；`None` 表示仍在执行。
    exit_code: Option<i32>,
    started: Instant,
    /// 本条命令的 sudo 提权是否已被拒绝（Q36）。
    ///
    /// sudo 密码错误时会重试（默认最多 3 次），每次重试都重新调用 askpass、
    /// 产生一次新的索要。拒绝一经给出便对**本条命令**的后续索要生效，
    /// 人类只被问一次。
    ///
    /// 记忆挂在"当前命令"上，随命令结束（`active` 置回 `None`）自动消失：
    /// 它不是"该终端禁止提权"，下一条命令照常询问。
    sudo_denied: bool,
}

/// 命令结束信号，用于唤醒同步等待者。
///
/// 用 `watch` 而非 `Notify`（V4）：`Notify::notify_waiters()` 不保留许可，
/// 若"置位结束状态"与"通知"发生在等待者调用 `notified()` **之前**，
/// 通知就被丢掉，等待者永久阻塞、执行锁永不释放、该终端后续命令全部堆积。
/// `watch` 会记住最新值，等待者无论何时订阅都能立即读到结束状态，
/// 从根本上消除丢失唤醒。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum FinishSignal {
    #[default]
    Idle,
    /// 已有一条命令结束（值仅用于触发变更通知，不参与业务判断）。
    Done(u64),
    /// 会话已断开。
    Disconnected,
}

/// 结束信号的可共享持有者。
///
/// 单独抽出来（而非直接放在 `TerminalEntry` 上）是为了能**脱离真实 SSH 会话**
/// 对唤醒语义做单元测试——这正是 V4 缺陷过去没被测出来的原因。
#[derive(Clone)]
struct FinishNotifier {
    tx: Arc<watch::Sender<FinishSignal>>,
}

impl FinishNotifier {
    fn new() -> Self {
        Self {
            tx: Arc::new(watch::channel(FinishSignal::Idle).0),
        }
    }

    /// 订阅变更；无论信号何时发出，订阅者都能观察到最新值。
    fn subscribe(&self) -> watch::Receiver<FinishSignal> {
        self.tx.subscribe()
    }

    /// 当前信号值（供测试与诊断使用）。
    fn current(&self) -> FinishSignal {
        *self.tx.borrow()
    }

    /// 会话是否已经断开（D39）。
    ///
    /// `Disconnected` 是终态（`mark_finished` 不会覆盖它），因此这里读到
    /// `Disconnected` 就意味着该条目持有的会话不可再用，必须重建。
    fn is_disconnected(&self) -> bool {
        matches!(self.current(), FinishSignal::Disconnected)
    }

    /// 通知"当前命令已结束"。用单调递增计数作为值：
    /// 每次结束都产生一次**不同的值**，确保 `changed()` 一定被唤醒。
    fn mark_finished(&self) {
        self.tx.send_modify(|s| {
            // 已断开是终态，不再被覆盖为 Done。
            if *s != FinishSignal::Disconnected {
                let next = match s {
                    FinishSignal::Done(n) => n.wrapping_add(1),
                    _ => 1,
                };
                *s = FinishSignal::Done(next);
            }
        });
    }

    /// 通知"会话已断开"：所有等待者都应立即返回失败。
    fn mark_disconnected(&self) {
        let _ = self.tx.send_replace(FinishSignal::Disconnected);
    }
}

/// 一个活跃终端对应的运行时条目。
struct TerminalEntry {
    session: Arc<Session>,
    /// 执行锁：保证同一终端的命令串行（Q4）。
    exec_lock: Arc<Mutex<()>>,
    /// 在途命令数（含正在执行的），用于队列上限判断。
    inflight: Arc<AtomicUsize>,
    /// 当前命令状态，供输出泵与轮询共享。
    active: Arc<Mutex<Option<ActiveCommand>>>,
    /// 命令结束信号（见 [`FinishNotifier`]）：唤醒同步等待者，且不会丢失通知。
    finished: FinishNotifier,
    /// sudo 配置（Q33）；密码在内，会话结束时随条目释放。
    sudo: Arc<SudoContext>,
    /// 主机显示名，用于 ask 模式的通知文案。
    host_label: String,
    /// ask 模式下的用户确认回调。
    asker: SudoAsker,
    /// 待写入命令输出的审计备注（D39）。
    ///
    /// 会话被自动重建后置位，由**下一条真正开始执行的命令**取出并写在输出开头，
    /// 这样人类在命令历史里能看到"这次重连发生过"，
    /// 而 Agent 在自己的输出里也能读到同样的一行。
    pending_note: Mutex<Option<String>>,
    /// 事件出口（D22）：与运行时**共享同一个槽位**，因此外壳层注入后，
    /// 已经在跑的条目（尤其是输出泵）也能发出事件。
    events: Arc<std::sync::Mutex<Option<EventSink>>>,
}

impl TerminalEntry {
    /// 发布一条属于本条目的事件（尽力而为，语义见 [`TerminalRuntime::emit`]）。
    fn emit_event(&self, event: TerminalEvent) {
        let Ok(slot) = self.events.lock() else {
            return;
        };
        if let Some(sink) = slot.as_ref() {
            let _ = sink.send(event);
        }
    }
}

/// 终端运行时。
///
/// 输出与队列上限**不在此缓存**：执行命令时从数据库读取（见 [`TerminalRuntime::run_command`]），
/// 这样用户在设置页调整后立即生效，也少一处"忘记同步"的可能。
#[derive(Default)]
pub struct TerminalRuntime {
    entries: Mutex<HashMap<String, Arc<TerminalEntry>>>,
    /// ask 模式的确认回调；未设置时 ask 模式一律按拒绝处理（fail-closed）。
    asker: Mutex<Option<SudoAsker>>,
    /// 事件出口（D22）：由外壳层注入并转发为前端可见的 Tauri 事件。
    ///
    /// 用标准库锁而非 tokio 锁：发送到无界通道不阻塞，
    /// 且事件会在不少同步位置发出（与 `sudo_bridge` 的待决表同理）。
    /// 与每个 [`TerminalEntry`] **共享同一个 `Arc`**，因此注入后条目也能发事件。
    events: Arc<std::sync::Mutex<Option<EventSink>>>,
}

/// 执行命令的结果（对应 Q4 的同步/异步两种返回）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct RunOutcome {
    pub command_id: String,
    pub status: CommandStatus,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u64>,
    pub output: String,
    pub truncated: bool,
    /// 命令仍在执行时为 true，Agent 应改用 `get_command_status` 轮询。
    pub still_running: bool,
    /// 本次执行前是否**重建了已断开的 SSH 会话**（D39）。
    ///
    /// 为 true 时意味着 shell 是全新的：工作目录、环境变量、后台进程
    /// 都已丢失（D3 的"状态保留"不再成立），Agent 需要重新 `cd` / `export`。
    /// 由上层（工具层）在调用 [`TerminalRuntime::run_command`] 前经
    /// `AppState::ensure_terminal_session` 得知并回填。
    #[serde(default)]
    pub session_reconnected: bool,
}

impl TerminalRuntime {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            asker: Mutex::new(None),
            events: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// 设置 ask 模式的用户确认回调（由 Tauri 层在启动时注入）。
    ///
    /// 未设置时 ask 模式一律按拒绝处理——宁可不提权，
    /// 也不能在无人确认的情况下悄悄注入密码（Q33 / P2）。
    pub async fn set_sudo_asker(&self, asker: SudoAsker) {
        *self.asker.lock().await = Some(asker);
    }

    /// 注入事件出口（由外壳层在启动时调用，D22）。
    pub fn set_event_sink(&self, sink: EventSink) {
        if let Ok(mut slot) = self.events.lock() {
            *slot = Some(sink);
        }
    }

    /// 发布一条事件。
    ///
    /// **尽力而为**：没有注入出口（测试）或接收端已关闭（应用退出）时静默跳过。
    /// 事件只影响界面新鲜度，真实状态以数据库为准，因此这里不需要回压或重试。
    fn emit(&self, event: TerminalEvent) {
        let Ok(slot) = self.events.lock() else {
            return;
        };
        if let Some(sink) = slot.as_ref() {
            let _ = sink.send(event);
        }
    }

    /// 该终端是否已建立连接。
    pub async fn is_connected(&self, terminal_id: &str) -> bool {
        self.entries.lock().await.contains_key(terminal_id)
    }

    /// 已连接的终端数量（供界面展示与调试）。
    pub async fn connected_count(&self) -> usize {
        self.entries.lock().await.len()
    }

    /// 建立终端会话（MCP 的"创建终端"）。
    ///
    /// 分阶段执行，避免长时间持有数据库锁：
    /// 1. 加锁：读取主机与凭据、校验配额、落库终端记录 → 释放
    /// 2. 无锁：建立 SSH 会话（可能耗时数秒）
    /// 3. 加锁：写入主机密钥（TOFU）与环境快照 → 释放
    pub async fn open_terminal(
        &self,
        db: &Db,
        key: &[u8; KEY_LEN],
        host_id: &str,
        name: Option<String>,
    ) -> Result<Terminal> {
        // --- 阶段 1：读取所需信息并落库终端记录 ---
        let (terminal, host, credential, sudo_ctx) = {
            let conn = db.lock().await;

            let host = hosts::get(&conn, host_id)?;
            let credential_id = host.credential_id.clone().ok_or_else(|| {
                AppError::CredentialNotFound(format!(
                    "主机 {} 尚未绑定认证信息，请先由人类在应用中配置",
                    host.name.clone().unwrap_or_else(|| host.address.clone())
                ))
            })?;
            let credential = credentials::get(&conn, &credential_id, key)?;

            // 配额校验（Q11）：归档终端不占配额，上限取自可配置设置。
            let settings = crate::settings::load(&conn)?;
            terminals::check_quota(
                &conn,
                host_id,
                settings.quota_per_host,
                settings.quota_global,
            )?;

            // 解析 sudo 密码（Q33）：可能来自主机单独配置，或复用登录密码。
            let sudo_ctx = build_sudo_context(&conn, &host, &credential, key)?;

            let terminal = Terminal::new(host_id, name);
            terminals::insert(&conn, &terminal)?;

            (terminal, host, credential, sudo_ctx)
            // 锁在此释放——SSH 连接期间不占用数据库。
        };

        // askpass 仅在三模式中的 ask/auto 下部署（Q33）。
        let sudo_enabled = sudo_ctx.needs_askpass();
        let auth = AuthMethod::from_credential(&credential);

        // --- 阶段 2：建立会话（无数据库锁） ---
        match Session::connect(&terminal.id, &host, auth, sudo_enabled).await {
            Ok((session, rx)) => {
                // --- 阶段 3：回写 TOFU 主机密钥与环境快照 ---
                let mut terminal = terminal;
                if host.host_key.is_none() {
                    if let Some((hk, fp)) = session.take_captured_host_key() {
                        let conn = db.lock().await;
                        hosts::set_host_key(&conn, &host.id, &hk, &fp)?;
                    }
                }
                if let Some(snapshot) = session.env_snapshot().cloned() {
                    let conn = db.lock().await;
                    terminals::set_env_snapshot(&conn, &terminal.id, &snapshot)?;
                    terminal.env_snapshot = Some(snapshot);
                }

                self.register(db, &terminal.id, session, rx, sudo_ctx, host_label(&host))
                    .await;
                // 通知界面：新终端已就绪（D22）。
                self.emit(TerminalEvent::TerminalCreated {
                    terminal_id: terminal.id.clone(),
                    host_id: terminal.host_id.clone(),
                });
                Ok(terminal)
            }
            Err(e) => {
                // 保留终端记录但标记 broken，人类能看到失败痕迹（D3）。
                let mut failed = terminal;
                failed.mark_broken();
                let conn = db.lock().await;
                terminals::update(&conn, &failed)?;
                drop(conn);
                self.emit(TerminalEvent::SessionChanged {
                    terminal_id: failed.id.clone(),
                    status: events::STATUS_BROKEN.to_string(),
                    reason: format!("终端建立失败：{e}"),
                });
                Err(e)
            }
        }
    }

    /// 注册一个已建立的会话并启动输出泵。
    async fn register(
        &self,
        db: &Db,
        terminal_id: &str,
        session: Session,
        rx: tokio::sync::mpsc::Receiver<SessionOutput>,
        sudo: SudoContext,
        host_label: String,
    ) {
        // ask 模式需要确认回调；未注入时保持 `None`，
        // 届时 resolve_action 会按"无人确认"处理为拒绝（fail-closed）。
        let asker: SudoAsker = self
            .asker
            .lock()
            .await
            .clone()
            .unwrap_or_else(|| Arc::new(|_| oneshot::channel().1));

        let entry = Arc::new(TerminalEntry {
            session: Arc::new(session),
            exec_lock: Arc::new(Mutex::new(())),
            inflight: Arc::new(AtomicUsize::new(0)),
            active: Arc::new(Mutex::new(None)),
            finished: FinishNotifier::new(),
            sudo: Arc::new(sudo),
            host_label,
            asker,
            pending_note: Mutex::new(None),
            events: self.events.clone(),
        });

        spawn_output_pump(db.clone(), entry.clone(), rx);
        self.entries
            .lock()
            .await
            .insert(terminal_id.to_string(), entry);
    }

    /// 注销会话并关闭底层连接。
    ///
    /// 先发出"已断开"信号：否则正在等待命令结束的同步调用
    /// 会一直挂在结束信号上（归档/删除终端时尤其明显，V4）。
    async fn unregister(&self, terminal_id: &str) {
        if let Some(entry) = self.entries.lock().await.remove(terminal_id) {
            entry.finished.mark_disconnected();
            entry.session.close().await.ok();
            entry.session.disconnect().await.ok();
        }
    }

    /// 归档终端（关闭会话，保留历史；归档后 Agent 不可见，D20）。
    pub async fn archive_terminal(&self, db: &Db, terminal_id: &str) -> Result<Terminal> {
        // 先移出运行时（关闭会话），再写数据库，避免持锁做网络操作。
        self.unregister(terminal_id).await;

        let conn = db.lock().await;
        let mut terminal = terminals::get(&conn, terminal_id)?;

        // 未完成的命令标记失败（Q4 状态机）。
        cmd_store::fail_pending_for_terminal(&conn, terminal_id, "终端已归档")?;

        terminal.archive();
        terminals::update(&conn, &terminal)?;
        drop(conn);
        // 通知界面：已归档（Agent 不可见，人类视图需更新状态）。
        self.emit(TerminalEvent::SessionChanged {
            terminal_id: terminal.id.clone(),
            status: events::STATUS_ARCHIVED.to_string(),
            reason: "终端已归档".into(),
        });
        Ok(terminal)
    }

    /// 恢复归档终端：沿用原 ID 与历史，重建底层会话（D20）。
    pub async fn restore_terminal(
        &self,
        db: &Db,
        key: &[u8; KEY_LEN],
        terminal_id: &str,
    ) -> Result<Terminal> {
        // --- 阶段 1：校验并取所需信息 ---
        let (terminal, host, auth, sudo_ctx) = {
            let conn = db.lock().await;

            let terminal = terminals::get(&conn, terminal_id)?;
            if terminal.status != TerminalStatus::Archived {
                return Err(AppError::InvalidArgument(
                    "该终端未处于归档状态，无需恢复".into(),
                ));
            }

            // 恢复前重新校验配额——期间可能已被其他终端占满（Q11）。
            // 归档终端不占配额，因此这里的校验不会被它自己干扰
            // （broken 的重连路径则相反，见 [`Self::reconnect_terminal`]）。
            let settings = crate::settings::load(&conn)?;
            terminals::check_quota(
                &conn,
                &terminal.host_id,
                settings.quota_per_host,
                settings.quota_global,
            )?;

            drop(conn);
            let (host, auth, sudo_ctx) = load_connection_context(db, key, &terminal).await?;
            (terminal, host, auth, sudo_ctx)
        };

        let sudo_enabled = sudo_ctx.needs_askpass();

        // --- 阶段 2：重建会话 ---
        let (session, rx) = Session::connect(&terminal.id, &host, auth, sudo_enabled).await?;
        self.register(db, &terminal.id, session, rx, sudo_ctx, host_label(&host))
            .await;

        // --- 阶段 3：更新状态 ---
        let mut t = terminal;
        t.restore();
        {
            let conn = db.lock().await;
            terminals::update(&conn, &t)?;
        }
        // 通知界面：已恢复（人类视图需把状态改回 active）。
        self.emit(TerminalEvent::SessionChanged {
            terminal_id: t.id.clone(),
            status: events::STATUS_ACTIVE.to_string(),
            reason: "终端已恢复".into(),
        });
        Ok(t)
    }

    /// 该终端当前是否持有**可用**的会话（D39）。
    ///
    /// 注意区分两种"不可用"：条目不存在（应用重启后必然如此）与
    /// 条目在但会话已断开（运行中断线，条目会残留）。
    pub async fn has_live_session(&self, terminal_id: &str) -> bool {
        match self.entries.lock().await.get(terminal_id) {
            Some(entry) => !entry.finished.is_disconnected(),
            None => false,
        }
    }

    /// 重建**已断开**的终端会话，沿用原 ID 与命令历史（D39）。
    ///
    /// 与 [`Self::restore_terminal`]（人类恢复归档终端）的区别：
    ///
    /// - 适用状态不同：这里针对 `broken`（或状态尚未同步、但会话已丢）；
    /// - **不做配额校验**：`broken` 终端本来就算在配额里（`count_active` 只
    ///   排除 `archived`），再校验一次会把它自己数进去，在"刚好占满"时
    ///   误报配额超限，反而让自动重连失败；
    /// - 重建后会留下一条审计备注，由下一条命令写入输出。
    ///
    /// 返回 `(终端记录, 审计备注)`。
    pub async fn reconnect_terminal(
        &self,
        db: &Db,
        key: &[u8; KEY_LEN],
        terminal_id: &str,
    ) -> Result<(Terminal, String)> {
        let terminal = {
            let conn = db.lock().await;
            terminals::get(&conn, terminal_id)?
        };

        if terminal.status == TerminalStatus::Archived {
            // 归档终端对 Agent 不可见，不得经重连路径复活（D20）。
            return Err(AppError::TerminalArchived(terminal_id.to_string()));
        }

        // 旧条目若还在（断线未清理），先注销并关闭，避免留下死会话。
        self.unregister(terminal_id).await;

        let (host, auth, sudo_ctx) = load_connection_context(db, key, &terminal).await?;
        let sudo_enabled = sudo_ctx.needs_askpass();

        let (session, rx) = Session::connect(&terminal.id, &host, auth, sudo_enabled).await?;
        let note = format!(
            "原 SSH 会话已断开，已自动重建（主机 {}）；\
             shell 状态已重置：工作目录、环境变量、后台进程均不再保留",
            host_label(&host)
        );
        self.register(db, &terminal.id, session, rx, sudo_ctx, host_label(&host))
            .await;
        // 备注在注册后写入，确保落在新条目上。
        if let Some(entry) = self.entries.lock().await.get(terminal_id) {
            *entry.pending_note.lock().await = Some(note.clone());
        }

        let mut t = terminal;
        t.restore();
        {
            let conn = db.lock().await;
            terminals::update(&conn, &t)?;
        }

        tracing::info!(terminal = %terminal_id, "会话已断开，已按需重建（D39）");
        // 通知界面：会话已重建（D22）——否则界面会一直停在"连接已断开"，
        // 审核者会误判终端仍不可用。
        self.emit(TerminalEvent::SessionChanged {
            terminal_id: t.id.clone(),
            status: events::STATUS_ACTIVE.to_string(),
            reason: "会话已自动重建（shell 状态已重置）".into(),
        });
        Ok((t, note))
    }

    /// 删除终端（历史一并删除，D21）。
    pub async fn delete_terminal(&self, db: &Db, terminal_id: &str) -> Result<()> {
        self.unregister(terminal_id).await;
        {
            let conn = db.lock().await;
            terminals::delete(&conn, terminal_id)?;
        }
        // 通知界面：该终端已从列表中消失（D22）。
        self.emit(TerminalEvent::TerminalRemoved {
            terminal_id: terminal_id.to_string(),
        });
        Ok(())
    }

    /// 执行命令（Q4 双模式）。
    ///
    /// `wait` 为同步等待时长：`None` 表示立即返回句柄（异步模式）。
    /// **超时不报错**，而是返回 `still_running` 与 `command_id`，
    /// 让 Agent 无缝转入轮询——这是避免模型误判耗时后重试的关键设计。
    pub async fn run_command(
        &self,
        db: &Db,
        terminal_id: &str,
        command: &str,
        wait: Option<Duration>,
    ) -> Result<RunOutcome> {
        let entry = self
            .entries
            .lock()
            .await
            .get(terminal_id)
            .cloned()
            .ok_or_else(|| AppError::TerminalBroken(terminal_id.to_string()))?;

        // 每次执行都读取一次设置：这样用户在设置页调整后立即生效，
        // 无需额外的"同步配置到运行时"路径（少一处可能忘记同步的地方）。
        // 与该命令记录的写入合并到同一次加锁，避免多占一次锁。
        let (record, cfg) = {
            let conn = db.lock().await;
            let cfg = crate::settings::load(&conn)?;
            let seq = cmd_store::next_seq(&conn, terminal_id)?;
            let r = CommandRecord::new(terminal_id, seq, command);
            cmd_store::insert(&conn, &r)?;
            (r, cfg)
        };

        // 队列上限（Q4）：超出直接拒绝，避免无限堆积。
        let limit = cfg.queue_limit;
        if entry.inflight.load(Ordering::SeqCst) >= limit {
            return Err(AppError::CommandQueueFull { limit });
        }

        entry.inflight.fetch_add(1, Ordering::SeqCst);

        // `seq` 仍写入数据库供人类侧排序展示，但**不再参与协议配对**（V5）。
        let command_id = record.id.clone();

        let exec_lock = entry.exec_lock.clone();
        let entry_for_task = entry.clone();
        let command_owned = command.to_string();
        let command_id_for_task = command_id.clone();
        let db_for_task = db.clone();
        let max_bytes = cfg.max_output_bytes;
        let max_lines = cfg.max_output_lines;

        // 后台执行。关键：**取得执行锁之后**才注册"当前命令"并标记 running。
        //
        // 若在取锁之前注册，排在队列里的命令会覆盖正在执行那条的输出归属，
        // 导致两条命令的输出串到一起（串行语义被破坏）。
        //
        // 终态落库也放在任务内：这样**异步模式**（调用方不等待）同样会
        // 把 completed 与输出写入数据库，否则轮询会永远停留在 running。
        let runner = tokio::spawn(async move {
            let _guard = exec_lock.lock().await;

            let started = Instant::now();

            // 注册当前命令（此时才真正轮到它执行）。
            {
                let mut active = entry_for_task.active.lock().await;
                *active = Some(ActiveCommand {
                    command_id: command_id_for_task.clone(),
                    output: OutputAccumulator::new(max_bytes, max_lines),
                    exit_code: None,
                    started,
                    // 新命令重新开始：提权的询问记忆不跨命令（Q36）。
                    sudo_denied: false,
                });

                // 会话重建的审计备注（D39）：写在**这条**命令的输出开头。
                // 放在这里而不是重连时，是因为队列里可能还排着别的命令——
                // 备注应当跟着"第一条真正在新会话上执行的命令"走，
                // 这样人类在命令历史里看到的上下文才是对的。
                let note = entry_for_task.pending_note.lock().await.take();
                if let (Some(note), Some(a)) = (note, active.as_mut()) {
                    a.output.push_line(&format!("[mf-perch] {note}"));
                }
            }
            // 订阅"结束信号"必须在发送命令之前完成，
            // 否则可能错过命令结束的通知（V4）。
            let mut finished_rx = entry_for_task.finished.subscribe();

            // 通知界面：命令开始执行（D22，界面显示"执行中"）。
            entry_for_task.emit_event(TerminalEvent::CommandStarted {
                terminal_id: entry_for_task.session.terminal_id().to_string(),
                command_id: command_id_for_task.clone(),
                command: command_owned.clone(),
            });

            // 标记为 running 并落库（此前为 queued）。
            {
                let mut running = record.clone();
                running.status = CommandStatus::Running;
                running.started_at = Some(now_rfc3339());
                if let Ok(conn) = db_for_task.try_lock() {
                    let _ = cmd_store::update_status(&conn, &running);
                } else {
                    let conn = db_for_task.lock().await;
                    let _ = cmd_store::update_status(&conn, &running);
                }
            }

            if let Err(e) = entry_for_task
                .session
                .send_command(&command_id_for_task, &command_owned)
                .await
            {
                finish_with_error(&entry_for_task, &e.to_string()).await;
            } else {
                // 等待输出泵置位 exit_code（或连接断开）。
                //
                // 用 `watch` 订阅而非 `Notify`：`Notify` 的许可不保留，
                // 若结束信号在等待者订阅前发出就会丢失唤醒，导致命令永久挂起（V4）。
                //
                // 顺序很关键：**先 `borrow_and_update` 记录当前代次，再检查状态，
                // 最后 `changed()`**。
                //  - 若结束发生在本轮记录之前 → 上面的状态检查已能看到 exit_code；
                //  - 若结束发生在本轮记录之后 → `changed()` 会立即返回（无需等待下一次）。
                // 因此不存在"信号已发出却永远等不到"的窗口。
                loop {
                    // 1) 记录当前代次（把已有变更标记为已读）。
                    let signal = *finished_rx.borrow_and_update();

                    // 2) 检查命令状态。
                    {
                        let active = entry_for_task.active.lock().await;
                        match active.as_ref() {
                            Some(a) if a.command_id == command_id_for_task => {
                                if a.exit_code.is_some() {
                                    break;
                                }
                            }
                            // 已被替换或移除：说明会话中断。
                            _ => break,
                        }
                    }

                    // 3) 会话已断开则无需继续等待。
                    if signal == FinishSignal::Disconnected {
                        break;
                    }

                    // 4) 等待下一次变更（若已变更则立即返回）。
                    if finished_rx.changed().await.is_err() {
                        // 发送端已释放（终端被注销），按中断处理。
                        break;
                    }
                }
            }

            // 取出终态并从"当前命令"移除，让后续排队命令可以接上。
            let (exit_code, output, truncated, total_bytes, duration) = {
                let mut active = entry_for_task.active.lock().await;
                match active.as_ref() {
                    Some(a) if a.command_id == command_id_for_task => {
                        let v = (
                            a.exit_code,
                            a.output.to_display_string(),
                            a.output.is_truncated(),
                            a.output.total_bytes(),
                            a.started.elapsed().as_millis() as u64,
                        );
                        *active = None;
                        v
                    }
                    _ => (Some(-1), String::new(), false, 0, 0),
                }
            };

            // 落库终态：同步与异步两条路径都会执行到这里。
            {
                let mut final_record = record;
                final_record.status = CommandStatus::Completed;
                final_record.exit_code = exit_code;
                final_record.duration_ms = Some(duration);
                final_record.truncated = truncated;
                final_record.output_bytes = Some(total_bytes);
                final_record.finished_at = Some(now_rfc3339());

                let conn = db_for_task.lock().await;
                if let Err(e) = cmd_store::update_status(&conn, &final_record) {
                    tracing::warn!("写入命令终态失败：{e}");
                }
                if let Err(e) = cmd_store::save_output(&conn, &command_id_for_task, &output) {
                    tracing::warn!("写入命令输出失败：{e}");
                }
            }

            entry_for_task.inflight.fetch_sub(1, Ordering::SeqCst);

            // 通知界面：命令结束（D22）。放在落库之后，界面若立刻回查也能读到终态。
            entry_for_task.emit_event(TerminalEvent::CommandFinished {
                terminal_id: entry_for_task.session.terminal_id().to_string(),
                command_id: command_id_for_task.clone(),
                status: CommandStatus::Completed.as_str().to_string(),
                exit_code,
                duration_ms: Some(duration),
            });

            RunOutcome {
                command_id: command_id_for_task,
                status: CommandStatus::Completed,
                exit_code,
                duration_ms: Some(duration),
                output,
                truncated,
                still_running: false,
                // 由工具层在确认"本次执行前重建过会话"后回填（D39）。
                session_reconnected: false,
            }
        });

        // 异步模式：立即返回句柄，任务在后台继续执行并自行落库终态。
        let Some(wait) = wait else {
            tokio::spawn(async move {
                // 单独接管句柄：即使调用方不等待，也要让任务跑完。
                let _ = runner.await;
            });
            return Ok(RunOutcome {
                command_id,
                status: CommandStatus::Queued,
                exit_code: None,
                duration_ms: None,
                output: String::new(),
                truncated: false,
                still_running: true,
                session_reconnected: false,
            });
        };

        match tokio::time::timeout(wait, runner).await {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err(join_err)) => Err(AppError::Internal(format!(
                "命令执行任务异常终止：{join_err}"
            ))),
            Err(_) => {
                // 超时：返回运行中句柄，命令在远端继续执行（不中断）。
                let active = entry.active.lock().await;
                let (partial, truncated, status) = match active.as_ref() {
                    Some(a) if a.command_id == command_id => (
                        a.output.to_display_string(),
                        a.output.is_truncated(),
                        CommandStatus::Running,
                    ),
                    // 仍在排队（尚未轮到执行）。
                    _ => (String::new(), false, CommandStatus::Queued),
                };
                Ok(RunOutcome {
                    command_id,
                    status,
                    exit_code: None,
                    duration_ms: None,
                    output: partial,
                    truncated,
                    still_running: true,
                    session_reconnected: false,
                })
            }
        }
    }

    /// 查询命令执行情况（Q4 轮询）。
    ///
    /// 运行中的命令输出取自内存（最新），已结束的取自数据库。
    pub async fn command_status(
        &self,
        db: &Db,
        command_id: &str,
        tail_lines: Option<usize>,
    ) -> Result<crate::domain::command::CommandStatusView> {
        let record = {
            let conn = db.lock().await;
            cmd_store::get(&conn, command_id)?
        };

        // 运行中：优先取内存中的实时输出。
        if record.status.is_pending() {
            let entries = self.entries.lock().await;
            if let Some(entry) = entries.get(&record.terminal_id) {
                let active = entry.active.lock().await;
                if let Some(a) = active.as_ref().filter(|a| a.command_id == command_id) {
                    let text = a.output.to_display_string();
                    let sliced = match tail_lines {
                        Some(n) => tail(&text, n),
                        None => text,
                    };
                    return Ok(crate::domain::command::CommandStatusView {
                        command_id: record.id,
                        terminal_id: record.terminal_id,
                        command: record.command,
                        status: record.status,
                        exit_code: None,
                        duration_ms: Some(a.started.elapsed().as_millis() as u64),
                        truncated: a.output.is_truncated(),
                        output: Some(sliced),
                        output_omitted: false,
                    });
                }
            }
        }

        // 已结束（或内存中已无记录）：取数据库。
        let conn = db.lock().await;
        cmd_store::status_view(&conn, command_id, true, tail_lines)
    }
}

/// 输出泵：消费会话事件，归属到"当前命令"，并在结束时置位退出码。
fn spawn_output_pump(
    db: Db,
    entry: Arc<TerminalEntry>,
    mut rx: tokio::sync::mpsc::Receiver<SessionOutput>,
) {
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            match event {
                SessionOutput::Line { line, .. } => {
                    let mut active = entry.active.lock().await;
                    if let Some(a) = active.as_mut() {
                        a.output.push_line(&line);
                    }
                }
                SessionOutput::Finished {
                    command_id,
                    exit_code,
                } => {
                    let mut active = entry.active.lock().await;
                    // 按 command_id 精确配对（V5）：只有当前正在执行的命令
                    // 才接受该结束标记，避免错位标记污染状态。
                    let matched = active
                        .as_ref()
                        .is_some_and(|a| a.command_id == command_id);
                    if matched {
                        if let Some(a) = active.as_mut() {
                            a.exit_code = Some(exit_code);
                        }
                    } else {
                        tracing::debug!(
                            command_id = %command_id,
                            "收到非当前命令的结束标记，已忽略"
                        );
                    }
                    drop(active);
                    // 无论是否配对成功都通知一次：让等待者重新检查状态，
                    // 避免因标记错位而永久阻塞。
                    entry.finished.mark_finished();
                }
                SessionOutput::SudoRequest { token } => {
                    // Q33：按该主机的策略决定是否注入密码。
                    // 关键词：ask 模式会等待用户确认，因此这里必须
                    // **脱离输出泵的读取循环**去处理，否则会阻塞后续输出。
                    //
                    // 并发索要是安全的：每次索要各有自己的 FIFO（B2），
                    // 应答按 (nonce, token) 精确投递，不会互相抢走。
                    let entry = entry.clone();
                    tokio::spawn(async move {
                        handle_sudo_request(&entry, &token).await;
                    });
                }
                SessionOutput::SudoPrompt => {
                    // D47：密码提示只应出现在**提权通道**上（由提权编排处理）。
                    // 数据面收到它说明状态异常（例如有人在数据面上直接跑了
                    // `sudo -S -p '<标记>'`）。这里没有任何可写的通道，
                    // 因此**不写密码**、仅告警——宁可失败，也不要写错地方。
                    tracing::warn!("数据面收到 sudo 密码提示（D47 仅提权通道处理），已忽略");
                }
                SessionOutput::Disconnected { reason } => {
                    tracing::warn!("终端连接已断开：{reason}");
                    let mut active = entry.active.lock().await;
                    if let Some(a) = active.as_mut() {
                        // 未结束的命令按失败处理，避免轮询永远返回 running。
                        if a.exit_code.is_none() {
                            a.exit_code = Some(-1);
                            a.output
                                .push_line(&format!("[mf-perch] 连接已断开：{reason}"));
                        }
                    }
                    drop(active);
                    entry.finished.mark_disconnected();

                    // 把状态**落库**为 broken（D39 待办 / V-类问题）：
                    // 此前只有"应用重启"才会落 broken，运行中断线时数据库仍是
                    // active，界面会长期显示得偏乐观。
                    // 只覆盖 active：并发发生的归档/删除不应被这里改回去。
                    {
                        let conn = db.lock().await;
                        let tid = entry.session.terminal_id();
                        match terminals::get(&conn, tid) {
                            Ok(mut t) if t.status == TerminalStatus::Active => {
                                t.mark_broken();
                                if let Err(e) = terminals::update(&conn, &t) {
                                    tracing::warn!("落库 broken 状态失败：{e}");
                                }
                            }
                            Ok(_) => {}
                            Err(e) => tracing::debug!("读取终端状态失败（不影响断开处理）：{e}"),
                        }
                    }

                    // 通知界面：会话已断开（人类界面据此显示 broken）。
                    entry.emit_event(TerminalEvent::SessionChanged {
                        terminal_id: entry.session.terminal_id().to_string(),
                        status: events::STATUS_BROKEN.to_string(),
                        reason: format!("连接已断开：{reason}"),
                    });
                    break;
                }
            }
        }
    });
}

/// 命令发送失败时置位，让等待方立即返回而不是等满超时。
async fn finish_with_error(entry: &TerminalEntry, reason: &str) {
    {
        let mut active = entry.active.lock().await;
        if let Some(a) = active.as_mut() {
            a.exit_code = Some(-1);
            a.output
                .push_line(&format!("[mf-perch] 命令发送失败：{reason}"));
        }
    }
    entry.finished.mark_finished();
}

/// 取字符串末尾 n 行。
fn tail(s: &str, n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= n {
        return s.to_string();
    }
    lines[lines.len() - n..].join("\n")
}

/// 主机的显示名，用于 sudo 通知文案。
fn host_label(host: &crate::domain::host::Host) -> String {
    host.name
        .clone()
        .unwrap_or_else(|| format!("{}:{}", host.address, host.port))
}

/// 解析主机的 sudo 配置（Q33）。
///
/// 密码来源有两种：主机单独配置，或复用 SSH 登录密码。
/// 复用仅在登录认证为密码方式时可行——密钥登录没有密码可复用，
/// 此时明确报错而不是让 sudo 在后面神秘失败（P1）。
fn build_sudo_context(
    conn: &rusqlite::Connection,
    host: &crate::domain::host::Host,
    credential: &crate::domain::credential::Credential,
    key: &[u8; KEY_LEN],
) -> Result<SudoContext> {
    use crate::domain::host::SudoPasswordSource;
    use crate::domain::credential::CredentialKind;
    use zeroize::Zeroizing;

    // deny 模式不需要密码，直接返回，避免无谓地解密敏感数据。
    if host.sudo_policy == SudoPolicy::Deny {
        return Ok(SudoContext {
            policy: SudoPolicy::Deny,
            password: None,
        });
    }

    let password: Option<Zeroizing<String>> = match host.sudo_password_source {
        SudoPasswordSource::Own => hosts::get_sudo_password(conn, &host.id, key)?
            .map(Zeroizing::new),
        SudoPasswordSource::ReuseLogin => match credential.kind {
            CredentialKind::Password => Some(Zeroizing::new(credential.secret.clone())),
            CredentialKind::Key => None,
        },
    };

    let ctx = SudoContext {
        policy: host.sudo_policy,
        password,
    };

    // 配置不自洽时立即报错：提前在创建终端时暴露，
    // 好过让 Agent 执行 sudo 时收到难以理解的失败。
    validate_for_policy(host.sudo_policy, ctx.password.is_some())?;

    Ok(ctx)
}

/// 取出建立/重建会话所需的全部上下文：主机、认证方式、sudo 配置。
///
/// 由 [`TerminalRuntime::restore_terminal`]（恢复归档）与
/// [`TerminalRuntime::reconnect_terminal`]（重连 broken）共用，
/// 避免两条路径各写一遍解密与校验逻辑而出现行为漂移。
async fn load_connection_context(
    db: &Db,
    key: &[u8; KEY_LEN],
    terminal: &Terminal,
) -> Result<(crate::domain::host::Host, AuthMethod, SudoContext)> {
    let conn = db.lock().await;

    let host = hosts::get(&conn, &terminal.host_id)?;
    let credential_id = host
        .credential_id
        .clone()
        .ok_or_else(|| AppError::CredentialNotFound("该主机尚未绑定认证信息".into()))?;
    let credential = credentials::get(&conn, &credential_id, key)?;

    // 每次都按**当前**主机配置重建 sudo 上下文（Q33）。
    let sudo_ctx = build_sudo_context(&conn, &host, &credential, key)?;
    let auth = AuthMethod::from_credential(&credential);

    Ok((host, auth, sudo_ctx))
}

/// 面对一次"要在这个终端上执行命令"的请求，应当怎么做（D39）。
///
/// 抽成纯函数以便覆盖全部分支——这里的分支直接决定 Agent 是被拒绝
/// 还是被自动恢复，必须逐条可测。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionAction {
    /// 已有可用会话，直接执行。
    UseExisting,
    /// 会话已丢失（broken，或状态未同步但会话已断）：按需重建。
    Reconnect,
    /// 归档终端：命令必须被拒绝（Agent 不可见，D20）。
    RejectArchived,
}

pub(crate) fn decide_session_action(
    has_live_session: bool,
    status: TerminalStatus,
) -> SessionAction {
    // 归档优先：即使内存里还挂着会话，也不能让 Agent 继续用（D20 / V3）。
    if status == TerminalStatus::Archived {
        return SessionAction::RejectArchived;
    }
    if has_live_session {
        SessionAction::UseExisting
    } else {
        // broken 与"状态还是 active 但会话已断"都要重建：
        // 后者是运行中断线、状态尚未落库的情形，对 Agent 而言同样是"连不上"。
        SessionAction::Reconnect
    }
}

/// 等待用户对 sudo 请求的响应超时（Q33：拒绝或超时都让 sudo 失败）。
const SUDO_ASK_TIMEOUT: Duration = Duration::from_secs(crate::domain::SUDO_ASK_TIMEOUT_SECS);

/// 处理一次 sudo 请求：按策略决定注入还是拒绝（Q33）。
///
/// `token` 标识本次索要（远端 askpass PID），应答据此精确定向到该次索要的
/// FIFO——这是并发索要不会互相错配的前提（B2）。
async fn handle_sudo_request(entry: &TerminalEntry, token: &str) {
    let sudo = entry.sudo.clone();
    let has_password = sudo.password.is_some();

    let (decision, audit_note) = match sudo.policy {
        // ask 模式：先取人类决策（本条命令已拒绝过则不再询问，Q36）。
        // 先记下"这次询问属于哪条命令"：拒绝只记在那条命令上。
        SudoPolicy::Ask => {
            let asked_for = current_command_id(entry).await;
            let outcome = ask_human(entry, asked_for.as_deref()).await;
            (Some(outcome.decision()), outcome.audit_note())
        }
        SudoPolicy::Auto => (None, "sudo 提权请求：按主机策略自动注入密码"),
        // deny 模式不会部署 askpass，正常不会走到这里；保守按拒绝记录。
        SudoPolicy::Deny => (None, "sudo 提权请求：该主机已禁用提权注入"),
    };

    match resolve_action(sudo.policy, has_password, decision) {
        SudoAction::Inject => {
            let password = sudo
                .password
                .as_deref()
                .map(|s| s.as_str())
                .unwrap_or_default();

            match entry.session.send_sudo_password(token, password).await {
                Ok(()) => tracing::info!(
                    host = %entry.host_label,
                    "已按策略注入 sudo 密码"
                ),
                Err(e) => {
                    // 注入失败仍必须让 sudo 结束，否则命令会一直挂着。
                    tracing::error!("注入 sudo 密码失败：{e}");
                    let _ = entry.session.deny_sudo(token).await;
                    push_audit_note(entry, "sudo 提权：密码注入失败，本次提权已失败").await;
                    return;
                }
            }
        }
        SudoAction::Deny => {
            // 关键：必须**回应**这次索要——向该次索要的 FIFO 写入空行，让 askpass
            // 以空密码应答、sudo 随即认证失败。若什么都不做，askpass 会一直阻塞在
            // read 上（直到 120 秒兜底超时），而命令串行执行，队列会被拖住
            // （见 docs/design/sudo.md §7.3）。
            if let Err(e) = entry.session.deny_sudo(token).await {
                tracing::error!("回应 sudo 拒绝（写入空密码）失败：{e}");
            }
            tracing::info!(
                host = %entry.host_label,
                policy = ?sudo.policy,
                "已拒绝 sudo 密码注入"
            );
        }
    }

    // 人类侧审计（O3）：提权决策必须留在命令历史里，可被事后核对。
    // 只记决策与来源，绝不记录密码本身。
    push_audit_note(entry, audit_note).await;
}

/// 取人类对本次提权的决定（Q36 / Q33）。
///
/// 本条命令此前已被拒绝时**不再询问**，直接沿用——这正是 Q36 的目的：
/// sudo 密码错误会重试（默认 3 次），每次都弹窗等于让人在几十秒内
/// 连点三次几乎相同的「拒绝」。
///
/// `asked_for` 是发起询问时的命令标识：拒绝只记在那条命令上（见
/// [`remember_denial`]）。
async fn ask_human(entry: &TerminalEntry, asked_for: Option<&str>) -> AskOutcome {
    if denial_is_remembered(entry).await {
        tracing::info!(
            host = %entry.host_label,
            "本条命令已拒绝过 sudo 提权，自动沿用拒绝（不再询问）"
        );
        return AskOutcome::DeniedByMemo;
    }

    let request = SudoRequest {
        request_id: crate::domain::new_id("sudo"),
        terminal_id: entry.session.terminal_id().to_string(),
        host_label: entry.host_label.clone(),
    };

    // 注意：等待人类应答期间**不得持有** `active` 锁——输出泵要用它记录输出，
    // 持锁等待会让整条命令的输出在弹窗期间停止积累。
    let rx = (entry.asker)(request);
    let outcome = match tokio::time::timeout(SUDO_ASK_TIMEOUT, rx).await {
        Ok(Ok(SudoDecision::Allow)) => AskOutcome::Allowed,
        Ok(Ok(SudoDecision::Deny)) => AskOutcome::Denied,
        // 通道异常（桥接不可用）：无法询问，等同拒绝。
        Ok(Err(_)) => AskOutcome::Unavailable,
        Err(_) => {
            tracing::info!("sudo 确认请求超时，按拒绝处理");
            AskOutcome::TimedOut
        }
    };

    if outcome.should_remember() {
        remember_denial(entry, asked_for).await;
    }
    outcome
}

/// 当前正在执行的命令标识（没有则为 `None`）。
async fn current_command_id(entry: &TerminalEntry) -> Option<String> {
    let active = entry.active.lock().await;
    active.as_ref().map(|a| a.command_id.clone())
}

/// 本条命令此前是否已被拒绝过提权（Q36）。
///
/// 记忆挂在"当前命令"上（见 [`ActiveCommand::sudo_denied`]）：命令一结束
/// 记忆即消失，下一条命令照常询问。当前没有执行中的命令时按"未拒绝"处理
/// （例如上一条命令留下的后台进程又触发了一次 sudo）——宁可按常规询问，
/// 也不要让一次旧拒绝静默吞掉新问题的确认机会。
async fn denial_is_remembered(entry: &TerminalEntry) -> bool {
    let active = entry.active.lock().await;
    active.as_ref().is_some_and(|a| a.sudo_denied)
}

/// 记住"**这条**命令的提权已被拒绝"（Q36）。
///
/// `asked_for` 是发起询问时的命令标识，必须与当前命令一致才记——
/// 人类可能在弹窗期间等上几十秒，而提问那条命令可能早已结束
/// （例如后台进程触发的索要）。不校验就会把拒绝记到**下一条**命令上，
/// 使它的确认被静默跳过：方向虽然仍是"拒绝"，但人类会莫名失去一次确认机会。
async fn remember_denial(entry: &TerminalEntry, asked_for: Option<&str>) {
    let mut active = entry.active.lock().await;
    if let Some(a) = active.as_mut() {
        if denial_applies_to(Some(a.command_id.as_str()), asked_for) {
            a.sudo_denied = true;
        }
    }
}

/// 这次拒绝是否应记在当前命令上（Q36）。
///
/// 抽成纯函数，是因为它守的竞态分支在端到端里无法稳定复现
/// （要求命令恰好在人类应答期间结束），见 [`remember_denial`]。
fn denial_applies_to(current: Option<&str>, asked_for: Option<&str>) -> bool {
    match (current, asked_for) {
        // 有当前命令才能记；且必须是**发起这次询问的那条**命令。
        (Some(current), Some(asked_for)) => current == asked_for,
        // 没有当前命令（命令已结束）：无处可记，也不该记到别的命令上。
        _ => false,
    }
}

/// 把一条审计备注写进当前命令的输出（O3）。
///
/// 人类侧的终端审计视图按命令展示输出，因此提权决策写在这里就能被看到，
/// 无需新增存储结构。当前没有执行中的命令时静默跳过。
async fn push_audit_note(entry: &TerminalEntry, note: &str) {
    let mut active = entry.active.lock().await;
    if let Some(a) = active.as_mut() {
        a.output.push_line(&format!("[mf-perch] {note}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_session_is_used_as_is() {
        assert_eq!(
            decide_session_action(true, TerminalStatus::Active),
            SessionAction::UseExisting
        );
    }

    #[test]
    fn broken_terminal_is_reconnected() {
        // D39 的核心：应用重启后终端被标记 broken，下一次命令必须自动重建，
        // 而不是把 Agent 拒之门外（此前只能另建新终端，还会撞配额）。
        assert_eq!(
            decide_session_action(false, TerminalStatus::Broken),
            SessionAction::Reconnect
        );
    }

    #[test]
    fn active_but_session_lost_is_reconnected() {
        // 运行中断线、状态尚未落库的情形：对 Agent 而言同样是"连不上"。
        assert_eq!(
            decide_session_action(false, TerminalStatus::Active),
            SessionAction::Reconnect
        );
    }

    #[test]
    fn archived_terminal_is_never_reconnected() {
        // 归档终端对 Agent 不可见，自动重连不得成为绕过归档的后门（D20）。
        for live in [true, false] {
            assert_eq!(
                decide_session_action(live, TerminalStatus::Archived),
                SessionAction::RejectArchived,
                "归档优先于会话状态（has_live={live}）"
            );
        }
    }

    #[test]
    fn tail_returns_last_lines() {
        let text = "1\n2\n3\n4\n5";
        assert_eq!(tail(text, 2), "4\n5");
        assert_eq!(tail(text, 10), text);
        assert_eq!(tail(text, 0), "");
    }

    /// 守的不变式：人类对 sudo 的拒绝**只对发起询问的那条命令**生效（Q36）。
    ///
    /// 记错命令的后果不是"更安全"，而是让别的命令的确认被静默跳过——
    /// 人类以为自己会被问，实际没有。
    #[test]
    fn denial_is_recorded_only_for_the_command_that_asked() {
        let cases = [
            (Some("cmd_1"), Some("cmd_1"), true, "同一条命令：应记住"),
            (Some("cmd_2"), Some("cmd_1"), false, "命令已切换：不得记到下一条上"),
            (None, Some("cmd_1"), false, "命令已结束：无处可记"),
            (Some("cmd_1"), None, false, "发起询问时没有命令：不得记"),
        ];
        for (current, asked_for, expected, why) in cases {
            assert_eq!(
                denial_applies_to(current, asked_for),
                expected,
                "{why}（current={current:?}, asked_for={asked_for:?}）"
            );
        }
    }

    #[tokio::test]
    async fn disconnected_terminal_is_not_connected() {
        let rt = TerminalRuntime::new();
        assert!(!rt.is_connected("term_none").await);
        assert_eq!(rt.connected_count().await, 0);
    }

    #[tokio::test]
    async fn finish_signal_is_not_lost_when_sent_before_waiting() {
        // V4 回归：结束信号若在等待者订阅**之前**发出，用 Notify 会丢失唤醒，
        // 等待者永久阻塞。watch 会保留最新值，等待者仍能立即观察到变化。
        let notifier = FinishNotifier::new();
        let mut rx = notifier.subscribe();

        // 先发信号（模拟"命令极快结束"，早于等待者注册）。
        notifier.mark_finished();

        // 再等待：不应挂起。
        let got = tokio::time::timeout(Duration::from_millis(200), rx.changed()).await;
        assert!(got.is_ok(), "先发出的结束信号不得丢失");
        assert!(matches!(*rx.borrow_and_update(), FinishSignal::Done(_)));
    }

    #[tokio::test]
    async fn repeated_finishes_each_produce_a_change() {
        // 每次结束都必须产生一次可观察的变更，否则第二条命令会等不到通知。
        let notifier = FinishNotifier::new();
        let mut rx = notifier.subscribe();

        notifier.mark_finished();
        assert!(rx.changed().await.is_ok());
        rx.borrow_and_update();

        notifier.mark_finished();
        let second = tokio::time::timeout(Duration::from_millis(200), rx.changed()).await;
        assert!(second.is_ok(), "第二次结束也应触发变更");
    }

    #[tokio::test]
    async fn archive_and_delete_emit_terminal_events() {
        // D22：人类界面的实时性依赖这些事件。归档/删除是最容易漏发的两处
        // （它们不经过命令路径），且漏发会让界面长期显示陈旧状态。
        let conn = crate::store::db::open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO hosts (id, address, port, created_at, updated_at) \
             VALUES ('host_evt', '10.0.0.1', 22, 'now', 'now')",
            [],
        )
        .unwrap();
        let terminal = Terminal::new("host_evt", None);
        crate::store::terminals::insert(&conn, &terminal).unwrap();
        let db: Db = std::sync::Arc::new(Mutex::new(conn));

        let runtime = TerminalRuntime::new();
        let (tx, mut rx) = events::event_channel();
        runtime.set_event_sink(tx);

        runtime.archive_terminal(&db, &terminal.id).await.unwrap();
        match rx.try_recv().expect("归档应发出事件") {
            TerminalEvent::SessionChanged {
                terminal_id,
                status,
                ..
            } => {
                assert_eq!(terminal_id, terminal.id);
                assert_eq!(status, events::STATUS_ARCHIVED);
            }
            other => panic!("期望 SessionChanged，实际 {other:?}"),
        }

        runtime.delete_terminal(&db, &terminal.id).await.unwrap();
        match rx.try_recv().expect("删除应发出事件") {
            TerminalEvent::TerminalRemoved { terminal_id } => {
                assert_eq!(terminal_id, terminal.id);
            }
            other => panic!("期望 TerminalRemoved，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn events_are_optional_for_the_runtime() {
        // 没有注入出口（单元测试、无界面环境）时不得 panic——事件只影响界面新鲜度。
        let conn = crate::store::db::open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO hosts (id, address, port, created_at, updated_at) \
             VALUES ('host_noevt', '10.0.0.1', 22, 'now', 'now')",
            [],
        )
        .unwrap();
        let terminal = Terminal::new("host_noevt", None);
        crate::store::terminals::insert(&conn, &terminal).unwrap();
        let db: Db = std::sync::Arc::new(Mutex::new(conn));

        let runtime = TerminalRuntime::new();
        runtime.archive_terminal(&db, &terminal.id).await.unwrap();
    }

    #[tokio::test]
    async fn concurrent_waiters_all_wake_up() {
        // 多个等待者订阅同一信号时，都应被唤醒（例如同步调用与轮询同时等待）。
        let notifier = FinishNotifier::new();
        let mut a = notifier.subscribe();
        let mut b = notifier.subscribe();

        notifier.mark_finished();

        for (name, rx) in [("a", &mut a), ("b", &mut b)] {
            let got = tokio::time::timeout(Duration::from_millis(200), rx.changed()).await;
            assert!(got.is_ok(), "等待者 {name} 应被唤醒");
        }
    }

    #[tokio::test]
    async fn disconnect_signal_wakes_waiter() {
        let notifier = FinishNotifier::new();
        let mut rx = notifier.subscribe();
        notifier.mark_disconnected();
        let got = tokio::time::timeout(Duration::from_millis(200), rx.changed()).await;
        assert!(got.is_ok(), "断开信号应唤醒等待者");
        assert_eq!(*rx.borrow_and_update(), FinishSignal::Disconnected);
    }

    #[tokio::test]
    async fn disconnect_is_terminal_and_not_overwritten_by_finish() {
        // 断开后即使再有结束标记，也应保持 Disconnected，便于等待者判断中断。
        let notifier = FinishNotifier::new();
        notifier.mark_disconnected();
        notifier.mark_finished();
        assert_eq!(notifier.current(), FinishSignal::Disconnected);
    }

    #[test]
    fn auth_from_credential_maps_both_kinds() {
        use crate::domain::credential::{Credential, CredentialKind};

        let pw = Credential::new("root", CredentialKind::Password, "hunter2");
        assert!(matches!(
            AuthMethod::from_credential(&pw),
            AuthMethod::Password { .. }
        ));

        let mut key = Credential::new(
            "git",
            CredentialKind::Key,
            "-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n-----END OPENSSH PRIVATE KEY-----",
        );
        key.passphrase = Some("pp".into());
        // 注意：AuthMethod 刻意不实现 Debug（会泄露密码/私钥），
        // 因此这里用 matches! 判断，而非打印其内容。
        assert!(matches!(
            AuthMethod::from_credential(&key),
            AuthMethod::Key { .. }
        ));
        // AuthMethod 实现了 Drop（析构清零），因此只能按引用匹配、不能移动字段。
        match &AuthMethod::from_credential(&key) {
            AuthMethod::Key {
                username,
                passphrase,
                ..
            } => {
                assert_eq!(username, "git");
                assert_eq!(passphrase.as_deref(), Some("pp"));
            }
            AuthMethod::Password { .. } => panic!("密钥类凭据不应构造为密码认证"),
        }
    }

    #[tokio::test]
    async fn run_command_on_unknown_terminal_reports_broken() {
        let rt = TerminalRuntime::new();
        let db: Db = Arc::new(Mutex::new(crate::store::db::open_in_memory().unwrap()));
        let err = rt
            .run_command(&db, "term_missing", "ls", Some(Duration::from_millis(10)))
            .await
            .unwrap_err();
        match err {
            AppError::TerminalBroken(id) => assert_eq!(id, "term_missing"),
            other => panic!("期望 TerminalBroken，实际 {other:?}"),
        }
    }
}
