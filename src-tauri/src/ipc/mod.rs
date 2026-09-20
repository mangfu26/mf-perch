//! Tauri IPC 命令层。
//!
//! 人类侧界面通过这些命令管理主机、凭据、终端与 MCP Server。
//!
//! 与 MCP 工具层的边界（AGENTS.md 0.1）：
//! - **IPC（本模块）**：人类可增删改主机与凭据、启停 MCP、删除终端
//! - **MCP（`crate::mcp::tools`）**：Agent 只读主机列表、创建/归档终端、执行命令
//!
//! 两条路径共享同一 `AppState`，因此界面能实时反映 Agent 的操作。
//!
//! 结构约定：每个命令是薄包装，内部实现（`*_inner`）返回
//! `Result<T, AppError>`，由 `IpcResult` 统一转成
//! `{ ok: true, data }` 或 `{ ok: false, code, message }`，
//! 前端据 `code` 做差异化提示。

pub mod mcp;
pub mod settings;
pub mod update;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use tauri::State;

use crate::domain::credential::{Credential, CredentialKind, CredentialSummary};
use crate::domain::host::{Host, HostSummary, ShellEnvMode, SudoPasswordSource, SudoPolicy};
use crate::domain::terminal::Terminal;
use crate::error::AppError;
use crate::state::AppState;
use crate::terminal::validate_for_policy;
use crate::store::crypto::KEY_LEN;
use crate::store::keyring::KeyProvider;
use crate::store::{commands as cmd_store, credentials, hosts, terminals};

/// IPC 统一返回：错误以结构化形式返回，前端不必解析错误字符串。
#[derive(Debug)]
pub enum IpcResult<T> {
    Ok { data: T },
    Err { code: String, message: String },
}

/// 手写序列化以锁定**线上形状**：`{ok:true,data}` / `{ok:false,code,message}`。
///
/// 不能用 `#[serde(tag = "ok")]` 的内部标签枚举：那会把 `ok` 写成变体名**字符串**
/// （`"ok"` / `"err"`），而前端 `src/lib/ipc.ts` 的 `unwrap()` 按 `raw.ok` 的真假分流
/// ——非空字符串 `"err"` 是真值，于是**所有后端错误都会被当成成功**
/// （`data` 取到 `undefined`，界面提示"已添加/已保存"却查无数据）。
/// 形状由 `ipc::tests` 的两条信封用例钉住。
impl<T: Serialize> Serialize for IpcResult<T> {
    fn serialize<S: Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(3))?;
        match self {
            Self::Ok { data } => {
                map.serialize_entry("ok", &true)?;
                map.serialize_entry("data", data)?;
            }
            Self::Err { code, message } => {
                map.serialize_entry("ok", &false)?;
                map.serialize_entry("code", code)?;
                map.serialize_entry("message", message)?;
            }
        }
        map.end()
    }
}

impl<T> IpcResult<T> {
    pub fn ok(data: T) -> Self {
        Self::Ok { data }
    }
}

impl<T> From<AppError> for IpcResult<T> {
    fn from(e: AppError) -> Self {
        Self::Err {
            code: e.code().to_string(),
            message: e.to_string(),
        }
    }
}

/// 把内部实现的结果统一转为 IPC 返回。
///
/// 返回 `Result<_, ()>` 而非裸 `IpcResult`：Tauri 要求含引用的异步命令
/// （如 `State<'_, T>`）必须返回 `Result`。真正的错误信息已由 `IpcResult::Err`
/// 承载，外层 `Err(())` 不会出现。
fn wrap<T>(r: Result<T, AppError>) -> Result<IpcResult<T>, ()> {
    Ok(match r {
        Ok(v) => IpcResult::ok(v),
        Err(e) => IpcResult::from(e),
    })
}

/// 读取主密钥（未解锁时返回明确错误，P1）。
async fn master_key(state: &AppState) -> Result<[u8; KEY_LEN], AppError> {
    let mk = state.master_key.lock().await;
    Ok(*mk.get()?)
}

// ==================== 密钥状态与引导 ====================

#[derive(Debug, Serialize)]
pub struct KeyStatus {
    pub initialized: bool,
    pub unlocked: bool,
    pub provider: Option<String>,
    /// 系统钥匙串是否可用（供界面引导，P1）。
    pub keyring_available: bool,
    /// 已记录的密钥方式当前不可用时的原因（D6：不清空数据，只报告原因）。
    pub unavailable_reason: Option<String>,
}

async fn key_status_inner(state: &AppState) -> Result<KeyStatus, AppError> {
    let unlocked = state.master_key.lock().await.is_unlocked();
    let keyring_available = crate::store::keyring::is_keyring_available();

    let conn = state.db.lock().await;
    match crate::store::keyring::inspect(&conn)? {
        crate::store::keyring::KeySetupState::NotInitialized => Ok(KeyStatus {
            initialized: false,
            unlocked,
            provider: None,
            keyring_available,
            unavailable_reason: None,
        }),
        crate::store::keyring::KeySetupState::Ready(p) => Ok(KeyStatus {
            initialized: true,
            // K1/K3 在启动时即已解锁；K2 需要用户输入。
            unlocked: unlocked || p != KeyProvider::MasterPassword,
            provider: Some(p.as_str().to_string()),
            keyring_available,
            unavailable_reason: None,
        }),
        crate::store::keyring::KeySetupState::ProviderUnavailable { provider, reason } => {
            Ok(KeyStatus {
                initialized: true,
                unlocked: false,
                provider: Some(provider.as_str().to_string()),
                keyring_available,
                unavailable_reason: Some(reason),
            })
        }
    }
}

/// 查询密钥保护状态。
#[tauri::command]
pub async fn key_status(state: State<'_, Arc<AppState>>) -> Result<IpcResult<KeyStatus>, ()> {
    wrap(key_status_inner(&state).await)
}

/// 初始化密钥保护方式（首次引导）。
#[tauri::command]
pub async fn init_key_provider(
    provider: String,
    password: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<IpcResult<KeyStatus>, ()> {
    let Some(p) = KeyProvider::parse(&provider) else {
        return Ok(IpcResult::from(AppError::InvalidArgument(format!(
            "无法识别的密钥保护方式：{provider}"
        ))));
    };

    if let Err(e) = state.initialize_key_provider(p, password.as_deref()).await {
        return Ok(IpcResult::from(e));
    }

    wrap(key_status_inner(&state).await)
}

/// 以主密码解锁。
#[tauri::command]
pub async fn unlock_with_password(
    password: String,
    state: State<'_, Arc<AppState>>,
) -> Result<IpcResult<bool>, ()> {
    wrap(
        state
            .unlock_with_master_password(&password)
            .await
            .map(|_| true),
    )
}

// ==================== 主机 ====================

#[derive(Debug, serde::Deserialize)]
pub struct HostInput {
    pub id: Option<String>,
    pub name: Option<String>,
    pub address: String,
    pub port: u16,
    pub credential_id: Option<String>,
    pub proxy_jump_host_id: Option<String>,
    pub sudo_policy: String,
    pub sudo_password_source: String,
    /// 留空表示不修改（更新场景）。
    pub sudo_password: Option<String>,
    pub shell_env_mode: String,
    pub init_script: Option<String>,
}

/// 列主机摘要（人类界面：含终端计数与是否已绑定凭据）。
#[tauri::command]
pub async fn list_hosts(state: State<'_, Arc<AppState>>) -> Result<IpcResult<Vec<HostSummary>>, ()> {
    let conn = state.db.lock().await;
    wrap(hosts::list_summaries(&conn))
}

/// 该主机要复用的登录凭据是否为密码方式（Q33 / O1）。
///
/// "复用 SSH 登录密码"只在登录认证为密码时成立：密钥登录没有密码可复用。
/// 返回 `false` 时调用方会经 [`validate_for_policy`] 报出可操作的错误，
/// 而不是等到建终端时才神秘失败（P1：明确报错）。
fn credential_is_password(
    conn: &rusqlite::Connection,
    input: &HostInput,
    key: &[u8; KEY_LEN],
) -> Result<bool, AppError> {
    let Some(cid) = input.credential_id.as_deref() else {
        return Ok(false);
    };
    if !credentials::exists(conn, cid)? {
        return Err(AppError::CredentialNotFound(cid.to_string()));
    }
    let cred = credentials::get(conn, cid, key)?;
    Ok(cred.kind == CredentialKind::Password)
}

async fn save_host_inner(state: &AppState, input: HostInput) -> Result<String, AppError> {
    let key = master_key(state).await?;

    let policy = SudoPolicy::parse(&input.sudo_policy)
        .ok_or_else(|| AppError::InvalidArgument("sudo 策略取值非法".into()))?;
    let source = SudoPasswordSource::parse(&input.sudo_password_source)
        .ok_or_else(|| AppError::InvalidArgument("sudo 密码来源取值非法".into()))?;
    let env_mode = ShellEnvMode::parse(&input.shell_env_mode)
        .ok_or_else(|| AppError::InvalidArgument("环境加载方式取值非法".into()))?;

    let conn = state.db.lock().await;

    // 校验绑定关系：凭据必须存在，避免留下悬空引用。
    if let Some(cid) = &input.credential_id {
        if !credentials::exists(&conn, cid)? {
            return Err(AppError::CredentialNotFound(cid.clone()));
        }
    }

    match &input.id {
        Some(id) => {
            let mut host = hosts::get(&conn, id)?;
            host.name = input.name.clone();
            host.address = input.address.clone();
            host.port = input.port;
            host.credential_id = input.credential_id.clone();
            host.proxy_jump_host_id = input.proxy_jump_host_id.clone();
            host.sudo_policy = policy;
            host.sudo_password_source = source;
            host.shell_env_mode = env_mode;
            host.init_script = input.init_script.clone();

            // 保存前就校验配置自洽（O1）：策略要密码却没有可用密码时，
            // 应当在**保存这一刻**告诉用户，而不是等他建终端才失败。
            // 已存的 sudo 密码算数（编辑时留空表示保留原值）。
            let has_password = match source {
                SudoPasswordSource::Own => {
                    input
                        .sudo_password
                        .as_deref()
                        .is_some_and(|s| !s.is_empty())
                        || hosts::get_sudo_password(&conn, id, &key)?.is_some()
                }
                SudoPasswordSource::ReuseLogin => credential_is_password(&conn, &input, &key)?,
            };
            validate_for_policy(policy, has_password)?;

            hosts::update(&conn, &host)?;

            // 只有显式提供了新密码才覆盖；留空表示保留原值
            // （避免"改个名字把 sudo 密码清掉"）。
            if let Some(pw) = input.sudo_password.as_deref().filter(|s| !s.is_empty()) {
                hosts::set_sudo_password(&conn, id, Some(pw), &key)?;
            }
            Ok(id.clone())
        }
        None => {
            // 新建时同样先校验自洽（O1）：此刻密码只可能来自本次输入。
            let has_password = match source {
                SudoPasswordSource::Own => input
                    .sudo_password
                    .as_deref()
                    .is_some_and(|s| !s.is_empty()),
                SudoPasswordSource::ReuseLogin => credential_is_password(&conn, &input, &key)?,
            };
            validate_for_policy(policy, has_password)?;

            let host = Host {
                id: crate::domain::new_id("host"),
                name: input.name.clone(),
                address: input.address.clone(),
                port: input.port,
                credential_id: input.credential_id.clone(),
                proxy_jump_host_id: input.proxy_jump_host_id.clone(),
                sudo_policy: policy,
                sudo_password_source: source,
                shell_env_mode: env_mode,
                init_script: input.init_script.clone(),
                host_key: None,
                host_key_fingerprint: None,
                created_at: crate::domain::now_rfc3339(),
                updated_at: crate::domain::now_rfc3339(),
            };
            hosts::insert(&conn, &host, input.sudo_password.as_deref(), &key)?;
            Ok(host.id)
        }
    }
}

/// 新增或更新主机。
#[tauri::command]
pub async fn save_host(
    input: HostInput,
    state: State<'_, Arc<AppState>>,
) -> Result<IpcResult<String>, ()> {
    wrap(save_host_inner(&state, input).await)
}

async fn delete_host_inner(state: &AppState, id: &str) -> Result<bool, AppError> {
    // 先关闭该主机下所有活跃会话，避免留下孤儿连接。
    let terminal_ids: Vec<String> = {
        let conn = state.db.lock().await;
        terminals::list_by_host(&conn, id)?
            .into_iter()
            .map(|t| t.id)
            .collect()
    };
    for tid in terminal_ids {
        state.terminals.delete_terminal(&state.db, &tid).await.ok();
    }

    let conn = state.db.lock().await;
    hosts::delete(&conn, id)?;
    Ok(true)
}

/// 删除主机（其下终端与命令历史一并删除，Q19）。
#[tauri::command]
pub async fn delete_host(id: String, state: State<'_, Arc<AppState>>) -> Result<IpcResult<bool>, ()> {
    wrap(delete_host_inner(&state, &id).await)
}

// ==================== 认证信息 ====================

#[derive(Debug, serde::Deserialize)]
pub struct CredentialInput {
    pub id: Option<String>,
    pub name: Option<String>,
    pub username: String,
    pub kind: String,
    /// 留空表示保持不变（更新场景）。
    pub secret: Option<String>,
    pub passphrase: Option<String>,
}

/// 列认证信息摘要（**不含任何敏感内容**，Q10）。
#[tauri::command]
pub async fn list_credentials(
    state: State<'_, Arc<AppState>>,
) -> Result<IpcResult<Vec<CredentialSummary>>, ()> {
    let conn = state.db.lock().await;
    wrap(
        credentials::list_summaries(&conn).and_then(|mut s| {
            hosts::fill_credential_usage(&conn, &mut s)?;
            Ok(s)
        }),
    )
}

async fn save_credential_inner(
    state: &AppState,
    input: CredentialInput,
) -> Result<String, AppError> {
    let key = master_key(state).await?;

    let kind = CredentialKind::parse(&input.kind)
        .ok_or_else(|| AppError::InvalidArgument("认证方式取值非法".into()))?;

    // 私钥指纹：仅对密钥类且提供了新正文时计算（Q10：正文不回显）。
    let fingerprint = input
        .secret
        .as_deref()
        .filter(|s| !s.is_empty() && kind == CredentialKind::Key)
        .and_then(compute_key_fingerprint);

    let conn = state.db.lock().await;

    match &input.id {
        Some(id) => {
            let existing = credentials::get(&conn, id, &key)?;
            // 未提供新正文时保留原值（界面不回显正文，留空即不修改）。
            let replace = input.secret.as_ref().is_some_and(|s| !s.is_empty());

            let mut cred = Credential {
                id: id.clone(),
                name: input.name.clone(),
                username: input.username.clone(),
                kind,
                secret: input.secret.clone().unwrap_or_default(),
                passphrase: input.passphrase.clone(),
                fingerprint: fingerprint.or_else(|| existing.fingerprint.clone()),
                created_at: existing.created_at.clone(),
                updated_at: crate::domain::now_rfc3339(),
            };

            if !replace {
                cred.secret = existing.secret.clone();
                if input.passphrase.is_none() {
                    cred.passphrase = existing.passphrase.clone();
                }
            }

            credentials::update(&conn, &cred, replace, &key)?;
            Ok(id.clone())
        }
        None => {
            let secret = input.secret.clone().unwrap_or_default();
            if secret.is_empty() {
                return Err(AppError::InvalidArgument(
                    "新增认证信息时必须提供密码或私钥".into(),
                ));
            }
            let mut cred = Credential::new(input.username.clone(), kind, secret);
            cred.name = input.name.clone();
            cred.passphrase = input.passphrase.clone();
            cred.fingerprint = fingerprint;
            credentials::insert(&conn, &cred, &key)?;
            Ok(cred.id)
        }
    }
}

/// 新增或更新认证信息。
#[tauri::command]
pub async fn save_credential(
    input: CredentialInput,
    state: State<'_, Arc<AppState>>,
) -> Result<IpcResult<String>, ()> {
    wrap(save_credential_inner(&state, input).await)
}

/// 删除认证信息。
#[tauri::command]
pub async fn delete_credential(
    id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<IpcResult<bool>, ()> {
    let conn = state.db.lock().await;
    wrap(credentials::delete(&conn, &id).map(|_| true))
}

/// 计算私钥 SHA256 指纹（与 OpenSSH 显示格式一致，Q10）。
fn compute_key_fingerprint(pem: &str) -> Option<String> {
    let key = russh::keys::decode_secret_key(pem, None).ok()?;
    Some(crate::ssh::auth::fingerprint(key.public_key()))
}

// ==================== 终端 ====================

#[derive(Debug, Serialize)]
pub struct TerminalView {
    #[serde(flatten)]
    pub terminal: Terminal,
    pub host_name: Option<String>,
    pub command_count: i64,
}

/// 列终端（含归档——人类需要审计归档终端，D20）。
#[tauri::command]
pub async fn list_terminals(
    host_id: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<IpcResult<Vec<TerminalView>>, ()> {
    let conn = state.db.lock().await;
    let result = (|| -> Result<Vec<TerminalView>, AppError> {
        let list = match &host_id {
            Some(h) => terminals::list_by_host(&conn, h)?,
            None => terminals::list_by_status(&conn, None)?,
        };

        let mut out = Vec::with_capacity(list.len());
        for t in list {
            let host_name = hosts::get(&conn, &t.host_id).ok().and_then(|h| h.name);
            let command_count = cmd_store::list_by_terminal(&conn, &t.id, 1000, 0)
                .map(|v| v.len() as i64)
                .unwrap_or(0);
            out.push(TerminalView {
                terminal: t,
                host_name,
                command_count,
            });
        }
        Ok(out)
    })();

    wrap(result)
}

/// 归档终端（人类侧也可归档以释放配额，Q11）。
#[tauri::command]
pub async fn archive_terminal(
    id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<IpcResult<Terminal>, ()> {
    wrap(state.terminals.archive_terminal(&state.db, &id).await)
}

/// 恢复归档终端（沿用原 ID 与历史，D20）。
#[tauri::command]
pub async fn restore_terminal(
    id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<IpcResult<Terminal>, ()> {
    let result = async {
        let key = master_key(&state).await?;
        state
            .terminals
            .restore_terminal(&state.db, &key, &id)
            .await
    }
    .await;
    wrap(result)
}

/// 删除终端及其命令历史（不可恢复，Q19）。
#[tauri::command]
pub async fn delete_terminal(
    id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<IpcResult<bool>, ()> {
    wrap(
        state
            .terminals
            .delete_terminal(&state.db, &id)
            .await
            .map(|_| true),
    )
}

/// 重连已断开的终端（人类侧手动触发，D39）。
///
/// 与 Agent 侧"执行命令时自动重连"走同一条路（[`AppState::ensure_terminal_session`]），
/// 因此行为一致：沿用原终端 ID 与历史，但 shell 状态会重置。
#[tauri::command]
pub async fn reconnect_terminal(
    id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<IpcResult<bool>, ()> {
    wrap(
        state
            .ensure_terminal_session(&id)
            .await
            .map(|_| true),
    )
}

// ==================== 命令历史（审计，Q17） ====================

#[derive(Debug, serde::Deserialize)]
pub struct HistoryQuery {
    pub terminal_id: Option<String>,
    pub host_id: Option<String>,
    pub query: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct HistoryItem {
    #[serde(flatten)]
    pub record: crate::domain::command::CommandRecord,
    pub output: Option<String>,
}

/// 查询命令历史（时间线 + 全文搜索 + 筛选）。
#[tauri::command]
pub async fn search_history(
    params: HistoryQuery,
    state: State<'_, Arc<AppState>>,
) -> Result<IpcResult<Vec<HistoryItem>>, ()> {
    let conn = state.db.lock().await;
    let result = (|| -> Result<Vec<HistoryItem>, AppError> {
        let filter = cmd_store::HistoryFilter {
            terminal_id: params.terminal_id.clone(),
            host_id: params.host_id.clone(),
            query: params.query.clone().filter(|q| !q.trim().is_empty()),
            status: None,
            limit: params.limit.or(Some(200)),
            offset: params.offset,
            order: crate::domain::SortOrder::Desc,
        };

        let mut out = Vec::new();
        for r in cmd_store::search_history(&conn, &filter)? {
            let output = if r.status.is_pending() {
                // 未结束的命令输出在内存中，由终端运行时提供。
                None
            } else {
                cmd_store::get_output(&conn, &r.id).ok().flatten()
            };
            out.push(HistoryItem { record: r, output });
        }
        Ok(out)
    })();

    wrap(result)
}

/// 历史占用统计（设置页展示，D14）。
#[tauri::command]
pub async fn history_stats(state: State<'_, Arc<AppState>>) -> Result<IpcResult<cmd_store::HistoryStats>, ()> {
    let conn = state.db.lock().await;
    wrap(cmd_store::stats(&conn))
}

// ==================== 设置 ====================
//
// 这里**刻意不提供**通用的 `get_setting` / `set_setting` 命令（V18）。
//
// 通用读写会绕过各设置项的语义校验，并且可以直接读出 `mcp_token`
// （等价于拿到全部主机的命令执行权）、或改写 `key_provider` / `mcp_allow_remote`
// 等安全相关项。此前这两个命令没有任何前端调用方，属"无人使用但敞开高危面"，
// 因此直接移除；设置一律走类型化入口：
// - 运行期限额：`ipc::settings::runtime_settings` / `set_runtime_setting`
// - 更新源等：`ipc::update::*`
// - MCP 配置：`ipc::mcp::*`
