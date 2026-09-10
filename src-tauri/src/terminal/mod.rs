//! 终端运行时：会话池 + 串行命令队列 + 输出泵（Q4 / D3）。
//!
//! 并发语义（Q4 采纳"排队"）：
//! - **同一终端同时只执行一条命令**，后续命令排队；
//! - 队列上限 `DEFAULT_COMMAND_QUEUE_LIMIT`，超出直接拒绝；
//! - 异步命令会占用该终端直到结束，若要并行需创建多个终端。
//!
//! 输出归属：由于串行，任一时刻每个终端最多只有一条"当前命令"，
//! 因此输出泵可以把到达的输出行记到该命令上，无需在协议层携带命令 ID。
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

use tokio::sync::{Mutex, Notify};

use crate::domain::command::{CommandRecord, CommandStatus};
use crate::domain::host::SudoPolicy;
use crate::domain::terminal::{Terminal, TerminalStatus};
use crate::domain::{
    now_rfc3339, DEFAULT_COMMAND_QUEUE_LIMIT, DEFAULT_MAX_OUTPUT_BYTES,
    DEFAULT_MAX_OUTPUT_LINES,
};
use crate::error::{AppError, Result};
use crate::ssh::auth::AuthMethod;
use crate::ssh::output::OutputAccumulator;
use crate::ssh::{Session, SessionOutput};
use crate::store::crypto::KEY_LEN;
use crate::store::{commands as cmd_store, credentials, hosts, terminals};

/// 数据库句柄类型（见 [`crate::store::Db`]）。
pub type Db = crate::store::Db;

/// 正在执行的命令的共享状态。
struct ActiveCommand {
    command_id: String,
    seq: u64,
    output: OutputAccumulator,
    /// 命令结束后的退出码；`None` 表示仍在执行。
    exit_code: Option<i32>,
    started: Instant,
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
    /// 命令结束时唤醒等待者（同步调用用）。
    finished: Arc<Notify>,
}

/// 终端运行时。
#[derive(Default)]
pub struct TerminalRuntime {
    entries: Mutex<HashMap<String, Arc<TerminalEntry>>>,
    /// 命令输出上限（字节 / 行），可配置。
    max_output_bytes: AtomicUsize,
    max_output_lines: AtomicUsize,
    queue_limit: AtomicUsize,
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
}

impl TerminalRuntime {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            max_output_bytes: AtomicUsize::new(DEFAULT_MAX_OUTPUT_BYTES),
            max_output_lines: AtomicUsize::new(DEFAULT_MAX_OUTPUT_LINES),
            queue_limit: AtomicUsize::new(DEFAULT_COMMAND_QUEUE_LIMIT),
        }
    }

    /// 配置输出与队列上限（供设置页调整）。
    pub fn configure(&self, max_bytes: usize, max_lines: usize, queue_limit: usize) {
        self.max_output_bytes.store(max_bytes, Ordering::Relaxed);
        self.max_output_lines.store(max_lines, Ordering::Relaxed);
        self.queue_limit.store(queue_limit, Ordering::Relaxed);
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
        let (terminal, host, credential, sudo_enabled) = {
            let conn = db.lock().await;

            let host = hosts::get(&conn, host_id)?;
            let credential_id = host.credential_id.clone().ok_or_else(|| {
                AppError::CredentialNotFound(format!(
                    "主机 {} 尚未绑定认证信息，请先由人类在应用中配置",
                    host.name.clone().unwrap_or_else(|| host.address.clone())
                ))
            })?;
            let credential = credentials::get(&conn, &credential_id, key)?;

            // 配额校验（Q11）：归档终端不占配额。
            terminals::check_quota_default(&conn, host_id)?;

            let terminal = Terminal::new(host_id, name);
            terminals::insert(&conn, &terminal)?;

            // `deny` 模式下不部署 askpass，使 sudo 天然失败（Q33 模式一）。
            let sudo_enabled = matches!(host.sudo_policy, SudoPolicy::Ask | SudoPolicy::Auto);

            (terminal, host, credential, sudo_enabled)
            // 锁在此释放——SSH 连接期间不占用数据库。
        };

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

                self.register(&terminal.id, session, rx).await;
                Ok(terminal)
            }
            Err(e) => {
                // 保留终端记录但标记 broken，人类能看到失败痕迹（D3）。
                let mut failed = terminal;
                failed.mark_broken();
                let conn = db.lock().await;
                terminals::update(&conn, &failed)?;
                Err(e)
            }
        }
    }

    /// 注册一个已建立的会话并启动输出泵。
    async fn register(
        &self,
        terminal_id: &str,
        session: Session,
        rx: tokio::sync::mpsc::Receiver<SessionOutput>,
    ) {
        let entry = Arc::new(TerminalEntry {
            session: Arc::new(session),
            exec_lock: Arc::new(Mutex::new(())),
            inflight: Arc::new(AtomicUsize::new(0)),
            active: Arc::new(Mutex::new(None)),
            finished: Arc::new(Notify::new()),
        });

        spawn_output_pump(entry.clone(), rx);
        self.entries
            .lock()
            .await
            .insert(terminal_id.to_string(), entry);
    }

    /// 注销会话并关闭底层连接。
    async fn unregister(&self, terminal_id: &str) {
        if let Some(entry) = self.entries.lock().await.remove(terminal_id) {
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
        let (terminal, host, credential, sudo_enabled) = {
            let conn = db.lock().await;

            let terminal = terminals::get(&conn, terminal_id)?;
            if terminal.status != TerminalStatus::Archived {
                return Err(AppError::InvalidArgument(
                    "该终端未处于归档状态，无需恢复".into(),
                ));
            }

            // 恢复前重新校验配额——期间可能已被其他终端占满（Q11）。
            terminals::check_quota_default(&conn, &terminal.host_id)?;

            let host = hosts::get(&conn, &terminal.host_id)?;
            let credential_id = host
                .credential_id
                .clone()
                .ok_or_else(|| AppError::CredentialNotFound("该主机尚未绑定认证信息".into()))?;
            let credential = credentials::get(&conn, &credential_id, key)?;

            let sudo_enabled = matches!(host.sudo_policy, SudoPolicy::Ask | SudoPolicy::Auto);

            (terminal, host, credential, sudo_enabled)
        };

        let auth = AuthMethod::from_credential(&credential);

        // --- 阶段 2：重建会话 ---
        let (session, rx) = Session::connect(&terminal.id, &host, auth, sudo_enabled).await?;
        self.register(&terminal.id, session, rx).await;

        // --- 阶段 3：更新状态 ---
        let mut t = terminal;
        t.restore();
        let conn = db.lock().await;
        terminals::update(&conn, &t)?;
        Ok(t)
    }

    /// 删除终端（历史一并删除，D21）。
    pub async fn delete_terminal(&self, db: &Db, terminal_id: &str) -> Result<()> {
        self.unregister(terminal_id).await;
        let conn = db.lock().await;
        terminals::delete(&conn, terminal_id)
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

        // 队列上限（Q4）：超出直接拒绝，避免无限堆积。
        let limit = self.queue_limit.load(Ordering::Relaxed);
        if entry.inflight.load(Ordering::SeqCst) >= limit {
            return Err(AppError::CommandQueueFull { limit });
        }

        // 落库命令记录。初始状态为 `queued`——同一终端串行执行，
        // 真正开始跑时（取得执行锁后）才置为 `running`（Q4）。
        let record = {
            let conn = db.lock().await;
            let seq = cmd_store::next_seq(&conn, terminal_id)?;
            let r = CommandRecord::new(terminal_id, seq, command);
            cmd_store::insert(&conn, &r)?;
            r
        };

        entry.inflight.fetch_add(1, Ordering::SeqCst);

        let seq = record.seq;
        let command_id = record.id.clone();

        let exec_lock = entry.exec_lock.clone();
        let entry_for_task = entry.clone();
        let command_owned = command.to_string();
        let command_id_for_task = command_id.clone();
        let db_for_task = db.clone();
        let max_bytes = self.max_output_bytes.load(Ordering::Relaxed);
        let max_lines = self.max_output_lines.load(Ordering::Relaxed);

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
                    seq,
                    output: OutputAccumulator::new(max_bytes, max_lines),
                    exit_code: None,
                    started,
                });
            }

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

            if let Err(e) = entry_for_task.session.send_command(&command_owned).await {
                finish_with_error(&entry_for_task, &e.to_string()).await;
            } else {
                // 等待输出泵置位 exit_code（或连接断开）。
                loop {
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
                    entry_for_task.finished.notified().await;
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

            RunOutcome {
                command_id: command_id_for_task,
                status: CommandStatus::Completed,
                exit_code,
                duration_ms: Some(duration),
                output,
                truncated,
                still_running: false,
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
fn spawn_output_pump(entry: Arc<TerminalEntry>, mut rx: tokio::sync::mpsc::Receiver<SessionOutput>) {
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            match event {
                SessionOutput::Line { line, .. } => {
                    let mut active = entry.active.lock().await;
                    if let Some(a) = active.as_mut() {
                        a.output.push_line(&line);
                    }
                }
                SessionOutput::Finished { seq, exit_code } => {
                    let mut active = entry.active.lock().await;
                    if let Some(a) = active.as_mut() {
                        if a.seq == seq {
                            a.exit_code = Some(exit_code);
                        }
                    }
                    drop(active);
                    // 唤醒等待该命令结束的同步调用。
                    entry.finished.notify_waiters();
                }
                SessionOutput::SudoRequest => {
                    // sudo 注入逻辑在阶段三实现（Q33）。
                    tracing::debug!("收到 sudo 密码请求，等待注入策略处理");
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
                    entry.finished.notify_waiters();
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
    entry.finished.notify_waiters();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_returns_last_lines() {
        let text = "1\n2\n3\n4\n5";
        assert_eq!(tail(text, 2), "4\n5");
        assert_eq!(tail(text, 10), text);
        assert_eq!(tail(text, 0), "");
    }

    #[test]
    fn queue_limit_defaults_to_ten() {
        let rt = TerminalRuntime::new();
        assert_eq!(
            rt.queue_limit.load(Ordering::Relaxed),
            DEFAULT_COMMAND_QUEUE_LIMIT
        );
    }

    #[test]
    fn configure_updates_limits() {
        let rt = TerminalRuntime::new();
        rt.configure(2048, 100, 3);
        assert_eq!(rt.max_output_bytes.load(Ordering::Relaxed), 2048);
        assert_eq!(rt.max_output_lines.load(Ordering::Relaxed), 100);
        assert_eq!(rt.queue_limit.load(Ordering::Relaxed), 3);
    }

    #[tokio::test]
    async fn disconnected_terminal_is_not_connected() {
        let rt = TerminalRuntime::new();
        assert!(!rt.is_connected("term_none").await);
        assert_eq!(rt.connected_count().await, 0);
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
        match AuthMethod::from_credential(&key) {
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
