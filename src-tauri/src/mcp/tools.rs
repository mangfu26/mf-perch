//! MCP 工具实现（D1 / Q4 / Q11 / Q17）。
//!
//! 工具描述与返回内容**固定英文**（D18）——面向 AI Agent，
//! 且 MCP 生态以英文为主，不随界面语言变化。
//!
//! 权限边界（AGENTS.md 0.1）：
//! - Agent 只能看到主机的公开信息（地址/端口/是否可用），**拿不到任何凭据**
//! - Agent 只能归档终端，不能删除；归档终端对它不可见
//! - 人类侧的管理操作（增删改主机与凭据、删除终端）不通过 MCP 暴露

use std::sync::Arc;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ServerCapabilities, ServerInfo, ToolsCapability};
use rmcp::{tool, tool_router, ErrorData as McpError, ServerHandler};
use rmcp::schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::domain::terminal::TerminalStatus;
use crate::error::AppError;
use crate::state::AppState;
use crate::store::{hosts, terminals};

/// 默认同步等待秒数（Q4）。
const DEFAULT_WAIT_SECS: u64 = crate::domain::DEFAULT_SYNC_WAIT_SECS;
/// 同步等待上限，避免撞上 MCP 客户端自身超时（Q4）。
const MAX_WAIT_SECS: u64 = crate::domain::MAX_SYNC_WAIT_SECS;

/// 同步工具的**有效等待秒数**：未指定时用默认值，超过上限则夹紧。
///
/// 提取成独立函数是为了让"夹紧"这一契约能被直接测试——
/// 同一契约还写在入参 schema 里（由 `wait_seconds_schema_declares_maximum` 守住两者一致）。
/// 超过上限的同步等待没有意义：MCP 客户端自己会先超时断开。
fn effective_wait_seconds(requested: Option<u64>) -> u64 {
    requested.unwrap_or(DEFAULT_WAIT_SECS).min(MAX_WAIT_SECS)
}

// ============================ 工具入参 ============================

/// 可空参数的 JSON Schema 包装器（只参与 schema 生成，不参与序列化）。
///
/// schemars 1.x 默认把 `Option<T>` 生成为 `"type": ["integer", "null"]`。
/// 这是合法的 JSON Schema 2020-12，但不少 MCP 客户端只接受 `type` 为单个
/// 字符串，遇到数组形式的 `type` 会告警、丢弃该约束，甚至拒绝整个工具。
/// 这里改用语义等价的 `anyOf` 表达可空，兼容性最好。
///
/// 用法：字段类型保持 `Option<T>`，改用
/// `#[schemars(with = "Nullable<T>")]` 覆盖其 schema；两者需同时
/// 保留 `#[serde(default)]`，否则字段会被判为必填。
struct Nullable<T>(std::marker::PhantomData<T>);

impl<T: JsonSchema> JsonSchema for Nullable<T> {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        format!("Nullable_{}", T::schema_name()).into()
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        format!("Nullable<{}>", T::schema_id()).into()
    }

    fn json_schema(generator: &mut rmcp::schemars::SchemaGenerator) -> rmcp::schemars::Schema {
        let inner = generator.subschema_for::<T>();
        rmcp::schemars::json_schema!({
            "anyOf": [
                inner,
                { "type": "null" }
            ]
        })
    }

    /// 内联到使用处，避免生成 `$defs` + `$ref`：约束直接出现在参数上，
    /// 对只做浅层解析的客户端更友好。
    fn inline_schema() -> bool {
        true
    }
}

/// `run_command.wait_seconds` 的 JSON Schema：可空 + 上下限（①②）。
///
/// 手写而非用 derive 属性：schemars 1.x 的 `range` 在本 crate 组合下报
/// `unknown schemars attribute`（实测），而"把上限写进机器可读的 schema"
/// 正是本次修复的要点——实现的 `min(MAX_WAIT_SECS)` 静默夹紧必须与 schema 一致。
///
/// 数值 50 与 [`MAX_WAIT_SECS`] 的一致性由单测 `wait_seconds_schema_declares_maximum`
/// 守住：谁改了一边而忘了另一边，测试立刻变红。
struct WaitSecondsSchema;

impl JsonSchema for WaitSecondsSchema {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "WaitSeconds".into()
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        "WaitSeconds".into()
    }

    fn json_schema(_generator: &mut rmcp::schemars::SchemaGenerator) -> rmcp::schemars::Schema {
        rmcp::schemars::json_schema!({
            "anyOf": [
                {
                    "type": "integer",
                    "format": "uint64",
                    "minimum": 0,
                    "maximum": 50
                },
                { "type": "null" }
            ],
            "default": null,
            "description": "Seconds to wait synchronously before returning a handle. \
                            Defaults to 30, maximum 50. Use `run_command_async` for long tasks."
        })
    }

    fn inline_schema() -> bool {
        true
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EmptyParams {}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateTerminalParams {
    /// Target host ID obtained from `list_hosts`.
    pub host_id: String,
    /// Optional human-readable name for this terminal.
    #[serde(default)]
    #[schemars(with = "Nullable<String>")]
    pub name: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TerminalIdParams {
    /// Terminal ID returned by `create_terminal` or `list_terminals`.
    pub terminal_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunCommandParams {
    /// Terminal ID to run the command on.
    pub terminal_id: String,
    /// The shell command to execute.
    pub command: String,
    /// Seconds to wait synchronously before returning a handle.
    /// Defaults to 30, maximum 50. Use `run_command_async` for long tasks.
    #[serde(default)]
    #[schemars(with = "WaitSecondsSchema")]
    pub wait_seconds: Option<u64>,
}

/// 异步执行的入参。
///
/// **刻意不含 `wait_seconds`**：异步模式立即返回句柄，该参数会被直接忽略。
/// 此前两个工具共用同一个入参结构，于是"等待秒数"出现在异步工具的 schema 里、
/// 描述里还写着"长任务请用 run_command_async"——自相矛盾，容易误导 Agent（①）。
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunCommandAsyncParams {
    /// Terminal ID to run the command on.
    pub terminal_id: String,
    /// The shell command to execute.
    pub command: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CommandStatusParams {
    /// Command ID returned by `run_command` or `run_command_async`.
    pub command_id: String,
    /// Return only the last N lines of output. Omit to get everything.
    #[serde(default)]
    #[schemars(with = "Nullable<usize>")]
    pub tail_lines: Option<usize>,
}

/// 提权执行的入参（D47 的第 8 个工具）。
///
/// 只有**单条命令**形式，没有"进入 / 退出 root 模式"：提权通道按需建立、
/// 一条命令用完即收，因此不存在"忘了退出"而把后续命令都留在特权身份下的风险。
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunAsRootParams {
    /// Terminal ID to run the command on.
    pub terminal_id: String,
    /// The shell command to execute with elevated privileges.
    pub command: String,
    /// Optional absolute working directory. Omit to inherit the terminal's
    /// current working directory (the same directory the plain commands run in).
    #[serde(default)]
    #[schemars(with = "Nullable<String>")]
    pub cwd: Option<String>,
}

// ============================ 工具返回 ============================

#[derive(Debug, Serialize)]
struct ToolError {
    error: String,
    code: String,
}

#[derive(Debug, Serialize)]
struct HostList {
    hosts: Vec<crate::domain::host::HostPublicInfo>,
    /// Number of active (non-archived) terminals per host.
    active_terminals: std::collections::HashMap<String, u32>,
    /// Per-host terminal quota, so the agent knows when it must archive first.
    quota_per_host: u32,
    quota_global: u32,
}

#[derive(Debug, Serialize)]
struct TerminalCreated {
    terminal_id: String,
    host_id: String,
    name: Option<String>,
    status: String,
}

#[derive(Debug, Serialize)]
struct TerminalList {
    terminals: Vec<TerminalSummary>,
}

#[derive(Debug, Serialize)]
struct TerminalSummary {
    terminal_id: String,
    host_id: String,
    host_name: Option<String>,
    name: Option<String>,
    status: String,
    created_at: String,
    last_command: Option<String>,
}

#[derive(Debug, Serialize)]
struct ArchiveResult {
    terminal_id: String,
    status: String,
    message: String,
}

// ============================ 服务实现 ============================

/// MCP 服务：持有应用共享状态，工具方法据此操作终端。
#[derive(Clone)]
pub struct McpService {
    state: Arc<AppState>,
}

#[tool_router]
impl McpService {
    pub fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }

    /// List all configured SSH hosts that the AI agent may use.
    ///
    /// Returns host ID, address, port and whether the host is ready
    /// (i.e. a credential has been attached by the human user).
    /// Credentials themselves are never exposed.
    #[tool(
        name = "list_hosts",
        description = "List all configured SSH hosts available to the agent. \
                       Returns host ID, address, port, and whether the host is ready \
                       (credential configured). Credentials are never exposed.",
        annotations(title = "List SSH hosts", read_only_hint = true)
    )]
    async fn list_hosts(
        &self,
        Parameters(_): Parameters<EmptyParams>,
    ) -> Result<CallToolResult, McpError> {
        match self.list_hosts_impl().await {
            Ok(v) => json_result(&v),
            Err(e) => Ok(error_result(&e)),
        }
    }

    /// Create a new terminal on the given SSH host.
    ///
    /// A terminal is a persistent SSH session that keeps its working
    /// directory and environment variables across commands. Only one
    /// command runs at a time per terminal; create several terminals to
    /// work in parallel.
    #[tool(
        name = "create_terminal",
        description = "Create a new terminal on an SSH host. The terminal keeps its \
                       working directory and environment between commands. Commands run \
                       one at a time per terminal; create multiple terminals for parallel \
                       work. Fails if the host has no credential or the terminal quota is reached.",
        annotations(title = "Create terminal", read_only_hint = false)
    )]
    async fn create_terminal(
        &self,
        Parameters(p): Parameters<CreateTerminalParams>,
    ) -> Result<CallToolResult, McpError> {
        match self.create_terminal_impl(p).await {
            Ok(v) => json_result(&v),
            Err(e) => Ok(error_result(&e)),
        }
    }

    /// List terminals visible to the agent.
    ///
    /// Archived terminals are intentionally omitted; the human user can
    /// still audit their command history in the desktop app.
    ///
    /// `broken` means the SSH session was lost (usually because the desktop
    /// app restarted). Such a terminal is still usable: it is reconnected
    /// automatically by the next `run_command` (D39).
    #[tool(
        name = "list_terminals",
        description = "List terminals visible to the agent. Archived terminals are not \
                       returned. Optionally filter by host_id. Shows terminal status and \
                       the last command executed on each terminal. Status 'broken' means the \
                       SSH session was lost (e.g. the desktop app restarted); the terminal is \
                       still usable and is reconnected automatically by the next run_command.",
        annotations(title = "List terminals", read_only_hint = true)
    )]
    async fn list_terminals(
        &self,
        Parameters(p): Parameters<OptionalHostIdParams>,
    ) -> Result<CallToolResult, McpError> {
        match self.list_terminals_impl(p.host_id).await {
            Ok(v) => json_result(&v),
            Err(e) => Ok(error_result(&e)),
        }
    }

    /// Run a command and wait for it to finish (synchronous mode).
    ///
    /// Waits up to `wait_seconds` (default 30, max 50). If the command
    /// does not finish in time, this returns a `command_id` with status
    /// `running` **without interrupting the command**; poll it with
    /// `get_command_status`. Use `run_command_async` for clearly long tasks.
    ///
    /// A terminal whose SSH session was lost (for example after the desktop
    /// app restarted) is reconnected automatically before the command runs;
    /// the result then carries `session_reconnected: true`, which means the
    /// shell is brand new and its state (working directory, environment
    /// variables, background jobs) was reset — re-issue any `cd` / `export`.
    #[tool(
        name = "run_command",
        description = "Run a shell command on a terminal and wait for completion \
                       (synchronous). Returns output, exit code and duration. If the command \
                       is still running after wait_seconds (default 30, max 50) it returns a \
                       command_id with status 'running' WITHOUT stopping the command; poll \
                       with get_command_status. Commands on the same terminal run one at a time. \
                       If the terminal's SSH session was lost it is reconnected automatically; \
                       `session_reconnected: true` in the result means the shell state \
                       (working directory, environment variables) was reset.",
        annotations(title = "Run command (sync)", read_only_hint = false)
    )]
    async fn run_command(
        &self,
        Parameters(p): Parameters<RunCommandParams>,
    ) -> Result<CallToolResult, McpError> {
        let wait = Some(std::time::Duration::from_secs(effective_wait_seconds(
            p.wait_seconds,
        )));
        match self
            .run_command_impl(&p.terminal_id, &p.command, wait)
            .await
        {
            Ok(v) => json_result(&v),
            Err(e) => Ok(error_result(&e)),
        }
    }

    /// Run a long-running command and return immediately (asynchronous mode).
    ///
    /// Returns a `command_id` right away. Poll progress with
    /// `get_command_status`. The command occupies its terminal until it
    /// finishes, so use a separate terminal for each parallel task.
    #[tool(
        name = "run_command_async",
        description = "Start a long-running shell command and return immediately with a \
                       command_id (asynchronous). Poll progress with get_command_status. \
                       The command occupies the terminal until it finishes; use separate \
                       terminals for parallel tasks.",
        annotations(title = "Run command (async)", read_only_hint = false)
    )]
    async fn run_command_async(
        &self,
        Parameters(p): Parameters<RunCommandAsyncParams>,
    ) -> Result<CallToolResult, McpError> {
        // 异步模式：立即返回句柄，不等待（入参里也就没有 wait_seconds）。
        match self
            .run_command_impl(&p.terminal_id, &p.command, None)
            .await
        {
            Ok(v) => json_result(&v),
            Err(e) => Ok(error_result(&e)),
        }
    }

    /// Run one command with elevated privileges (as root).
    ///
    /// A separate, short-lived privileged channel is used; the password is
    /// delivered only on that channel and is never exposed to the agent.
    /// The command inherits the terminal's current working directory unless
    /// `cwd` is given. Per-host policy applies: if the human disabled
    /// elevation the call fails with `sudo_elevation_failed`.
    #[tool(
        name = "run_as_root",
        description = "Run ONE command with elevated privileges (as root) on a terminal. \
                       Use only when the task truly needs root; prefer run_command otherwise. \
                       The command runs on a separate, short-lived privileged channel: the \
                       elevation password is handled by the app and is never sent to the agent. \
                       The working directory is the same as for run_command unless `cwd` is \
                       given explicitly. Elevation depends on the host's sudo policy set by the \
                       human user; if it is disabled the call fails with code \
                       'sudo_elevation_failed'. There is no interactive root session: each call \
                       elevates, runs one command and closes. Result includes the effective uid \
                       that actually ran the command.",
        annotations(title = "Run as root", read_only_hint = false)
    )]
    async fn run_as_root(
        &self,
        Parameters(p): Parameters<RunAsRootParams>,
    ) -> Result<CallToolResult, McpError> {
        match self.run_as_root_impl(p).await {
            Ok(v) => json_result(&v),
            Err(e) => Ok(error_result(&e)),
        }
    }

    /// Get the current status of a command started earlier.
    ///
    /// Returns status (queued/running/completed/failed), exit code, duration
    /// and accumulated output. Use `tail_lines` to limit output size.
    #[tool(
        name = "get_command_status",
        description = "Get the current status of a command started by run_command or \
                       run_command_async. Returns status (queued/running/completed/failed), \
                       exit code, duration in milliseconds and accumulated output. Use \
                       tail_lines to limit the amount of output returned.",
        annotations(title = "Get command status", read_only_hint = true)
    )]
    async fn get_command_status(
        &self,
        Parameters(p): Parameters<CommandStatusParams>,
    ) -> Result<CallToolResult, McpError> {
        match self.get_command_status_impl(p).await {
            Ok(v) => json_result(&v),
            Err(e) => Ok(error_result(&e)),
        }
    }

    /// Archive a terminal. The SSH session is closed, but its command
    /// history is preserved for human auditing. Archived terminals are no
    /// longer visible to the agent. Only the human user can delete a terminal.
    #[tool(
        name = "archive_terminal",
        description = "Archive a terminal: closes the SSH session but preserves all command \
                       history for human auditing. Archived terminals become invisible to the \
                       agent. This is the correct way to release a terminal slot; the agent \
                       cannot delete terminals.",
        annotations(title = "Archive terminal", read_only_hint = false)
    )]
    async fn archive_terminal(
        &self,
        Parameters(p): Parameters<TerminalIdParams>,
    ) -> Result<CallToolResult, McpError> {
        match self.archive_terminal_impl(p).await {
            Ok(v) => json_result(&v),
            Err(e) => Ok(error_result(&e)),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OptionalHostIdParams {
    /// Optional host ID to filter terminals by.
    #[serde(default)]
    #[schemars(with = "Nullable<String>")]
    pub host_id: Option<String>,
}

// ============================ 实现细节 ============================

impl McpService {
    async fn list_hosts_impl(&self) -> Result<HostList, AppError> {
        let conn = self.state.db.lock().await;
        let list = hosts::list_public(&conn)?;

        let mut active = std::collections::HashMap::new();
        for h in &list {
            active.insert(
                h.id.clone(),
                terminals::count_active(&conn, Some(&h.id))?,
            );
        }

        // 配额以用户设置为准：Agent 需据此判断何时该先归档终端。
        // 若这里返回默认值，用户调高配额后 Agent 仍会误以为已满。
        let settings = crate::settings::load(&conn)?;

        Ok(HostList {
            hosts: list,
            active_terminals: active,
            quota_per_host: settings.quota_per_host,
            quota_global: settings.quota_global,
        })
    }

    async fn create_terminal_impl(
        &self,
        p: CreateTerminalParams,
    ) -> Result<TerminalCreated, AppError> {
        // 只克隆主密钥后立即释放锁：建立 SSH 会话耗时较长，
        // 期间不应占用主密钥锁或数据库锁。
        let key = {
            let mk = self.state.master_key.lock().await;
            *mk.get()?
        };

        let terminal = self
            .state
            .terminals
            .open_terminal(&self.state.db, &key, &p.host_id, p.name.clone())
            .await?;

        Ok(TerminalCreated {
            terminal_id: terminal.id,
            host_id: terminal.host_id,
            name: terminal.name,
            status: terminal.status.as_str().to_string(),
        })
    }

    async fn list_terminals_impl(&self, host_id: Option<String>) -> Result<TerminalList, AppError> {
        let conn = self.state.db.lock().await;
        let list = terminals::list_visible_to_agent(&conn, host_id.as_deref())?;

        let mut out = Vec::new();
        for t in list {
            let host_name = hosts::get(&conn, &t.host_id).ok().and_then(|h| h.name);
            let last = crate::store::commands::list_by_terminal(&conn, &t.id, 1, 0)?
                .into_iter()
                .next()
                .map(|c| c.command);

            out.push(TerminalSummary {
                terminal_id: t.id,
                host_id: t.host_id,
                host_name,
                name: t.name,
                status: t.status.as_str().to_string(),
                created_at: t.created_at,
                last_command: last,
            });
        }

        Ok(TerminalList { terminals: out })
    }

    async fn run_command_impl(
        &self,
        terminal_id: &str,
        command: &str,
        wait: Option<std::time::Duration>,
    ) -> Result<crate::terminal::RunOutcome, AppError> {
        // 归档终端的命令必须被拒绝，且给出可区分的原因。
        {
            let conn = self.state.db.lock().await;
            let t = terminals::get(&conn, terminal_id)?;
            if t.status == TerminalStatus::Archived {
                return Err(AppError::TerminalArchived(terminal_id.to_string()));
            }
        }

        // 会话已断开（应用重启、网络中断）时**按需重建**（D39）：
        // 断开的成因是人类重启或网络，不该让 Agent 先去发现再处理。
        let reconnected = self.state.ensure_terminal_session(terminal_id).await?;

        let mut outcome = self
            .state
            .terminals
            .run_command(&self.state.db, terminal_id, command, wait)
            .await?;

        // 明确告知 Agent：这条命令跑在**全新**的 shell 上，状态已重置。
        outcome.session_reconnected = reconnected.is_some();
        Ok(outcome)
    }

    /// 提权执行一条命令（D47 第 8 个工具）。
    ///
    /// 与 `run_command_impl` 的三处刻意差异：
    ///
    /// 1. **不返回可轮询的句柄**：提权命令没有异步模式，调用即等待到底；
    /// 2. **`cwd` 语义**：不给就继承数据面当前目录，给了就以 `cwd` 为准；
    /// 3. **归档终端同样拒绝**：与普通命令保持同一条边界（D20）。
    async fn run_as_root_impl(
        &self,
        p: RunAsRootParams,
    ) -> Result<crate::terminal::PrivilegedOutcome, AppError> {
        {
            let conn = self.state.db.lock().await;
            let t = terminals::get(&conn, &p.terminal_id)?;
            if t.status == TerminalStatus::Archived {
                return Err(AppError::TerminalArchived(p.terminal_id.clone()));
            }
        }

        // 会话已断开时按需重建（D39），与普通命令一致——否则 Agent 会先
        // 收到一个"终端已断开"的错误，而它本可以自动恢复。
        self.state
            .ensure_terminal_session(&p.terminal_id)
            .await?;

        // 只克隆主密钥后立即释放锁：建立提权通道期间不占主密钥锁。
        let key = {
            let mk = self.state.master_key.lock().await;
            *mk.get()?
        };

        self.state
            .terminals
            .run_as_root(
                &self.state.db,
                &key,
                &p.terminal_id,
                &p.command,
                p.cwd.as_deref(),
            )
            .await
    }

    async fn get_command_status_impl(
        &self,
        p: CommandStatusParams,
    ) -> Result<crate::domain::command::CommandStatusView, AppError> {
        // 归档终端的命令对其不再可见（V3）：与 run_command 的归档拒绝保持一致，
        // 否则 Agent 可在归档前记下 command_id，归档后继续读取该终端的输出。
        let view = self
            .state
            .terminals
            .command_status(&self.state.db, &p.command_id, p.tail_lines)
            .await?;

        {
            let conn = self.state.db.lock().await;
            let t = terminals::get(&conn, &view.terminal_id)?;
            if t.status == TerminalStatus::Archived {
                return Err(AppError::TerminalArchived(view.terminal_id.clone()));
            }
        }

        Ok(view)
    }

    async fn archive_terminal_impl(
        &self,
        p: TerminalIdParams,
    ) -> Result<ArchiveResult, AppError> {
        let t = self
            .state
            .terminals
            .archive_terminal(&self.state.db, &p.terminal_id)
            .await?;
        Ok(ArchiveResult {
            terminal_id: t.id,
            status: t.status.as_str().to_string(),
            message: "Terminal archived. Its command history remains available to the human user."
                .to_string(),
        })
    }
}

/// 把 `#[tool]` 标注的方法接入 MCP 的 `call_tool` / `list_tools` 分发。
///
/// 该宏自动生成路由方法；`get_info` 已手写（含 capabilities 与 instructions），
/// 宏检测到后不会重复生成。
#[rmcp::tool_handler]
impl ServerHandler for McpService {
    fn get_info(&self) -> ServerInfo {
        // 这两个结构体标记了 non_exhaustive，只能用 Default 构造后改字段。
        let mut capabilities = ServerCapabilities::default();
        capabilities.tools = Some(ToolsCapability::default());

        let mut info = ServerInfo::default();
        info.capabilities = capabilities;
        // 服务器标识：客户端会展示这个名字，必须能认出是 mf-perch。
        info.server_info.name = "mf-perch".to_string();
        info.server_info.version = env!("CARGO_PKG_VERSION").to_string();
        info.instructions = Some(
            "mf-perch exposes SSH terminals to AI agents. \
             Typical flow: list_hosts -> create_terminal -> run_command. \
             Use run_command_async plus get_command_status for long-running tasks. \
             Always archive_terminal when finished to release the terminal slot. \
             Hosts and credentials are managed by the human user; credentials are never exposed."
                .to_string(),
        );
        info
    }
}

// ============================ 结果封装 ============================

/// 把成功结果序列化为 JSON 文本内容。
fn json_result<T: Serialize>(value: &T) -> Result<CallToolResult, McpError> {
    let text = serde_json::to_string_pretty(value).map_err(|e| {
        McpError::internal_error(format!("failed to serialize tool result: {e}"), None)
    })?;
    Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
}

/// 把应用错误转为**结构化**的工具结果（Q4：错误要可判断、可恢复）。
///
/// 刻意不使用协议级错误：Agent 需要读到 `code` 才能区分
/// "终端不存在"与"队列已满"等不同情况，并决定是否自行恢复。
fn error_result(e: &AppError) -> CallToolResult {
    let payload = ToolError {
        error: e.to_string(),
        code: e.code().to_string(),
    };
    let text = serde_json::to_string_pretty(&payload)
        .unwrap_or_else(|_| format!(r#"{{"code":"internal_error","error":"{e}"}}"#));

    CallToolResult::error(vec![ContentBlock::text(text)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_result_carries_machine_readable_code() {
        let e = AppError::TerminalArchived("term_1".into());
        let result = error_result(&e);
        assert_eq!(result.is_error, Some(true));

        let text = serde_json::to_string(&result.content).unwrap();
        assert!(text.contains("terminal_archived"), "应包含错误码");
        assert!(text.contains("term_1"), "应包含具体终端的上下文");
    }

    #[test]
    fn quota_error_is_distinguishable_from_not_found() {
        // Agent 需据此决定"归档后重试"还是"换个终端"，故错误码必须不同。
        let quota = error_result(&AppError::TerminalQuotaExceeded { scope: "per_host" });
        let missing = error_result(&AppError::TerminalNotFound("t".into()));

        let q = serde_json::to_string(&quota.content).unwrap();
        let m = serde_json::to_string(&missing.content).unwrap();
        assert!(q.contains("terminal_quota_exceeded"));
        assert!(m.contains("terminal_not_found"));
        assert_ne!(q, m);
    }

    #[test]
    fn json_result_is_not_an_error() {
        let v = ArchiveResult {
            terminal_id: "t1".into(),
            status: "archived".into(),
            message: "ok".into(),
        };
        let r = json_result(&v).unwrap();
        assert_ne!(r.is_error, Some(true));
        let text = serde_json::to_string(&r.content).unwrap();
        assert!(text.contains("archived"));
    }

    /// 同步等待的秒数契约：未指定→默认；超上限→夹紧（Q4）。
    ///
    /// 直接调用生产函数 `effective_wait_seconds`，而不是在测试里重写一遍 `min`——
    /// 后者无论实现怎么改都会通过（断言的是标准库，不是本项目的行为）。
    #[test]
    fn wait_seconds_is_clamped_to_maximum() {
        let cases = [
            (None, DEFAULT_WAIT_SECS, "未指定时应使用默认值"),
            (Some(10), 10, "未超上限时应原样使用"),
            (Some(MAX_WAIT_SECS), MAX_WAIT_SECS, "恰好等于上限时不应改动"),
            (Some(MAX_WAIT_SECS + 1), MAX_WAIT_SECS, "刚超上限即应夹紧"),
            (Some(u64::MAX), MAX_WAIT_SECS, "极端值必须夹紧而不是溢出"),
        ];
        for (requested, expected, why) in cases {
            assert_eq!(
                effective_wait_seconds(requested),
                expected,
                "{why}：requested={requested:?}"
            );
        }
    }

    #[test]
    fn default_wait_seconds_matches_design() {
        assert_eq!(DEFAULT_WAIT_SECS, 30);
    }

    /// 递归查找指定键名，用于确认 schema 中没有出现 `$defs` / `$ref`。
    fn find_key(
        value: &serde_json::Value,
        path: &str,
        keys: &[&str],
        hits: &mut Vec<String>,
    ) {
        match value {
            serde_json::Value::Object(map) => {
                for (k, v) in map {
                    if keys.contains(&k.as_str()) {
                        hits.push(format!("{path}/{k}"));
                    }
                    find_key(v, &format!("{path}/{k}"), keys, hits);
                }
            }
            serde_json::Value::Array(items) => {
                for (i, v) in items.iter().enumerate() {
                    find_key(v, &format!("{path}[{i}]"), keys, hits);
                }
            }
            _ => {}
        }
    }

    /// 递归查找形如 `"type": ["integer", "null"]` 的数组形式 `type`。
    fn find_array_type(value: &serde_json::Value, path: &str, hits: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(map) => {
                if let Some(t) = map.get("type") {
                    if t.is_array() {
                        hits.push(format!("{path}/type = {t}"));
                    }
                }
                for (k, v) in map {
                    find_array_type(v, &format!("{path}/{k}"), hits);
                }
            }
            serde_json::Value::Array(items) => {
                for (i, v) in items.iter().enumerate() {
                    find_array_type(v, &format!("{path}[{i}]"), hits);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn tool_schemas_do_not_use_array_type() {
        // 数组形式的 `type` 会触发 MCP Inspector 等客户端的 schema 告警，
        // 个别客户端还会丢弃约束甚至拒绝工具。可空参数一律用 anyOf 表达。
        let mut hits = Vec::new();
        for t in McpService::tool_router().list_all() {
            find_array_type(
                &serde_json::Value::Object(t.input_schema.as_ref().clone()),
                &t.name,
                &mut hits,
            );
        }
        assert!(hits.is_empty(), "存在数组形式的 type：{hits:#?}");
    }

    /// 断言某个可空参数用 `anyOf` + `null` 表达，且不在 `required` 中。
    fn assert_nullable_anyof(tool: &str, field: &str, inner_type: &str) {
        let router = McpService::tool_router();
        let def = router.get(tool).unwrap_or_else(|| panic!("缺少工具 {tool}"));
        let prop = &def.input_schema["properties"][field];
        let branches = prop["anyOf"]
            .as_array()
            .unwrap_or_else(|| panic!("{tool}.{field} 应为 anyOf：{prop}"));

        assert!(
            branches
                .iter()
                .any(|b| b["type"] == serde_json::json!("null")),
            "{tool}.{field} 的 anyOf 应包含 null 分支：{prop}"
        );
        assert!(
            branches
                .iter()
                .any(|b| b["type"] == serde_json::json!(inner_type)),
            "{tool}.{field} 的 anyOf 应包含 {inner_type} 分支：{prop}"
        );

        // 可空不等于必填：带 default 的字段不应出现在 required 中。
        if let Some(required) = def.input_schema.get("required").and_then(|v| v.as_array()) {
            assert!(
                !required.iter().any(|v| v == field),
                "{tool}.{field} 不应是必填项"
            );
        }
    }

    #[test]
    fn optional_params_use_anyof_with_null() {
        assert_nullable_anyof("create_terminal", "name", "string");
        assert_nullable_anyof("list_terminals", "host_id", "string");
        assert_nullable_anyof("run_command", "wait_seconds", "integer");
        assert_nullable_anyof("get_command_status", "tail_lines", "integer");
    }

    #[test]
    fn async_tool_does_not_expose_wait_seconds() {
        // ①：异步工具立即返回句柄，wait_seconds 会被忽略。
        // 此前两个工具共用入参结构，于是"等待秒数"出现在异步工具的 schema 里，
        // Agent 可能以为它真的会等——契约与实现不符。
        let router = McpService::tool_router();
        let def = router
            .get("run_command_async")
            .expect("应有 run_command_async");

        assert!(
            def.input_schema["properties"].get("wait_seconds").is_none(),
            "异步工具的入参不应包含 wait_seconds：{:?}",
            def.input_schema
        );

        let required: Vec<String> = def.input_schema["required"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            required,
            vec!["terminal_id".to_string(), "command".to_string()],
            "异步工具只应要求 terminal_id 与 command"
        );

        // 便于人工/排查时核对（默认被测试框架捕获，`--nocapture` 可见）。
        println!(
            "run_command_async 入参 schema：{}",
            serde_json::to_string_pretty(&def.input_schema).unwrap_or_default()
        );
    }

    #[test]
    fn wait_seconds_schema_declares_maximum() {
        // ②：实现会把 wait_seconds 夹紧到 MAX_WAIT_SECS，schema 必须把这个上限
        // 写进机器可读层面——只写在描述文字里，客户端校验时会放行 999。
        let router = McpService::tool_router();
        let def = router.get("run_command").expect("应有 run_command");
        let prop = &def.input_schema["properties"]["wait_seconds"];

        let integer = prop["anyOf"]
            .as_array()
            .and_then(|branches| {
                branches
                    .iter()
                    .find(|b| b["type"] == serde_json::json!("integer"))
            })
            .unwrap_or_else(|| panic!("wait_seconds 应有 integer 分支：{prop}"));

        assert_eq!(
            integer["maximum"],
            serde_json::json!(MAX_WAIT_SECS),
            "schema 必须声明 maximum 且与实现用的常量一致：{integer}"
        );
        assert_eq!(
            integer["minimum"],
            serde_json::json!(0),
            "秒数不应为负：{integer}"
        );

        // 便于人工/排查时核对（默认被测试框架捕获，`--nocapture` 可见）。
        println!(
            "run_command.wait_seconds schema：{}",
            serde_json::to_string_pretty(prop).unwrap_or_default()
        );
    }

    #[test]
    fn optional_params_accept_absent_and_null() {
        // 两种写法都必须能反序列化：缺省字段与显式 null 均表示"未提供"。
        let absent: RunCommandParams =
            serde_json::from_value(serde_json::json!({
                "terminal_id": "t1", "command": "ls"
            }))
            .expect("缺省 wait_seconds 应可解析");
        assert_eq!(absent.wait_seconds, None);

        let explicit_null: RunCommandParams =
            serde_json::from_value(serde_json::json!({
                "terminal_id": "t1", "command": "ls", "wait_seconds": null
            }))
            .expect("wait_seconds 为 null 应可解析");
        assert_eq!(explicit_null.wait_seconds, None);

        let value: RunCommandParams =
            serde_json::from_value(serde_json::json!({
                "terminal_id": "t1", "command": "ls", "wait_seconds": 12
            }))
            .expect("应可解析");
        assert_eq!(value.wait_seconds, Some(12));
    }

    #[tokio::test]
    async fn list_hosts_reports_configured_quota() {
        // 用户调高配额后，Agent 必须看到新值；否则它会误以为已达上限，
        // 在明明还能创建终端时提前归档。
        let conn = crate::store::db::open_in_memory().expect("内存库");
        crate::settings::set(&conn, crate::settings::SETTING_QUOTA_PER_HOST, "8")
            .expect("写入配额");
        crate::settings::set(&conn, crate::settings::SETTING_QUOTA_GLOBAL, "50")
            .expect("写入配额");

        let key = crate::store::crypto::generate_master_key();
        let service = McpService::new(Arc::new(AppState::new_for_test(conn, key)));
        let list = service.list_hosts_impl().await.expect("list_hosts 应成功");

        assert_eq!(list.quota_per_host, 8);
        assert_eq!(list.quota_global, 50);
    }

    #[test]
    fn tool_schemas_have_no_refs_or_defs() {
        // 内联生成可避免 $defs/$ref，对只做浅层解析的客户端更稳妥。
        let router = McpService::tool_router();
        let mut hits = Vec::new();
        for t in router.list_all() {
            find_key(
                &serde_json::Value::Object(t.input_schema.as_ref().clone()),
                &t.name,
                &["$defs", "$ref"],
                &mut hits,
            );
        }
        assert!(hits.is_empty(), "schema 中不应出现 $defs / $ref：{hits:#?}");
    }

    /// 工具面必须**恰好**是这 8 个（D47 新增 `run_as_root`）。
    ///
    /// 这条守的是权限边界（AGENTS.md §0.1）：Agent 的能力面一旦被无意扩大
    /// （例如把"删除终端"这类人类侧管理操作加进来），只靠逐个工具的单测
    /// 是发现不了的——它们各自都是对的。工具的**集合**才是契约。
    #[test]
    fn tool_surface_is_exactly_the_documented_eight() {
        let router = McpService::tool_router();
        let mut names: Vec<String> = router.list_all().iter().map(|t| t.name.to_string()).collect();
        names.sort();

        assert_eq!(
            names,
            vec![
                "archive_terminal",
                "create_terminal",
                "get_command_status",
                "list_hosts",
                "list_terminals",
                "run_as_root",
                "run_command",
                "run_command_async",
            ],
            "工具集合变化时必须同步 docs/mcp-tools.md §2/§3 并复核权限边界"
        );
    }

    /// `run_as_root` 的契约（D47）：三个入参、`cwd` 可空且非必填。
    ///
    /// 特别守住"**没有** root 模式"这条设计决定：入参里不得出现
    /// `enter` / `mode` / `keep_alive` 之类会把授权粒度从"一条命令"
    /// 放大成"一段时间"的字段。
    #[test]
    fn run_as_root_schema_is_single_command_only() {
        assert_nullable_anyof("run_as_root", "cwd", "string");

        let router = McpService::tool_router();
        let def = router.get("run_as_root").expect("应有 run_as_root");

        let required: Vec<String> = def.input_schema["required"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            required,
            vec!["terminal_id".to_string(), "command".to_string()],
            "提权工具只应要求 terminal_id 与 command（cwd 可空）"
        );

        let props = def.input_schema["properties"]
            .as_object()
            .expect("应有 properties");
        assert_eq!(
            props.len(),
            3,
            "提权工具只应有 terminal_id / command / cwd，实际：{:?}",
            props.keys().collect::<Vec<_>>()
        );
    }
}
