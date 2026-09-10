//! 更新检查的 IPC 命令（D23）。
//!
//! 人类侧在设置页手动检查更新、查看版本、忽略某版本、配置更新源。
//! 自动检查在应用启动时由 `run()` 触发（后台、静默失败）。

use std::sync::Arc;

use tauri::State;

use crate::state::AppState;
use crate::update;

use super::IpcResult;

/// 更新检查的当前配置与状态（供设置页展示）。
#[derive(Debug, serde::Serialize)]
pub struct UpdateInfo {
    /// 当前应用版本。
    pub current_version: String,
    /// 更新源地址（可能为空，表示未配置）。
    pub source_url: String,
    /// 是否启用启动时自动检查。
    pub auto_check: bool,
    /// 用户忽略的版本。
    pub ignored_version: Option<String>,
    /// 上次检查结果（若有缓存）。
    pub last_result: Option<update::UpdateStatus>,
}

async fn info_inner(state: &AppState) -> crate::error::Result<UpdateInfo> {
    let conn = state.db.lock().await;
    Ok(UpdateInfo {
        current_version: update::current_version(),
        source_url: update::source_url(&conn)?,
        auto_check: update::auto_check_enabled(&conn)?,
        ignored_version: update::ignored_version(&conn)?,
        last_result: update::cached_result(&conn)?,
    })
}

/// 读取更新检查的配置与上次结果。
#[tauri::command]
pub async fn update_info(state: State<'_, Arc<AppState>>) -> Result<IpcResult<UpdateInfo>, ()> {
    Ok(match info_inner(&state).await {
        Ok(v) => IpcResult::ok(v),
        Err(e) => IpcResult::from(e),
    })
}

/// 检查更新。
///
/// `force` 为真表示用户手动触发：忽略 24 小时缓存，且失败时把原因告知用户；
/// 为假则复用缓存、失败静默（供启动时自动检查使用）。
#[tauri::command]
pub async fn update_check(
    force: bool,
    state: State<'_, Arc<AppState>>,
) -> Result<IpcResult<update::UpdateStatus>, ()> {
    // 分阶段加锁：先在锁内读取配置，再在锁外做网络请求，最后写回缓存。
    // 避免网络请求（最长 5 秒）期间占用数据库锁。
    let (url, ignored) = {
        let conn = state.db.lock().await;
        match (update::source_url(&conn), update::ignored_version(&conn)) {
            (Ok(u), Ok(i)) => (u, i),
            (Err(e), _) | (_, Err(e)) => return Ok(IpcResult::from(e)),
        }
    };

    // 缓存命中直接返回（仅自动检查走这条路）。
    if !force {
        let conn = state.db.lock().await;
        if update::cache_is_fresh(&conn).unwrap_or(false) {
            if let Ok(Some(cached)) = update::cached_result(&conn) {
                return Ok(IpcResult::ok(cached));
            }
        }
    }

    let status = match update::fetch_manifest(&url).await {
        Ok(manifest) => {
            let current = update::current_version();
            update::evaluate(&manifest, &current, ignored.as_deref(), update::platform_key())
        }
        Err(e) => {
            // 自动检查失败静默，仅记录；手动检查把原因返回给用户。
            if !force {
                tracing::debug!("自动更新检查失败（已忽略）：{e}");
            }
            return Ok(IpcResult::ok(update::UpdateStatus::Failed {
                reason: e.to_string(),
            }));
        }
    };

    // 写回缓存（短暂加锁）。
    {
        let conn = state.db.lock().await;
        if let Err(e) = crate::store::db::set_setting(
            &conn,
            update::SETTING_LAST_CHECK,
            &chrono::Utc::now().to_rfc3339(),
        ) {
            tracing::warn!("写入更新检查时间失败：{e}");
        }
        if let Ok(json) = serde_json::to_string(&status) {
            let _ = crate::store::db::set_setting(&conn, update::SETTING_CACHED, &json);
        }
    }

    Ok(IpcResult::ok(status))
}

/// 忽略某个版本（同版本不再提示）。
#[tauri::command]
pub async fn update_ignore_version(
    version: String,
    state: State<'_, Arc<AppState>>,
) -> Result<IpcResult<bool>, ()> {
    let conn = state.db.lock().await;
    Ok(match update::ignore_version(&conn, &version) {
        Ok(()) => IpcResult::ok(true),
        Err(e) => IpcResult::from(e),
    })
}

/// 设置更新源地址。
#[tauri::command]
pub async fn update_set_source(
    url: String,
    state: State<'_, Arc<AppState>>,
) -> Result<IpcResult<bool>, ()> {
    let conn = state.db.lock().await;
    Ok(match update::set_source_url(&conn, &url) {
        Ok(()) => IpcResult::ok(true),
        Err(e) => IpcResult::from(e),
    })
}

/// 设置是否启用启动时自动检查。
#[tauri::command]
pub async fn update_set_auto_check(
    enabled: bool,
    state: State<'_, Arc<AppState>>,
) -> Result<IpcResult<bool>, ()> {
    let conn = state.db.lock().await;
    Ok(match update::set_auto_check(&conn, enabled) {
        Ok(()) => IpcResult::ok(true),
        Err(e) => IpcResult::from(e),
    })
}
