//! 运行期设置的 IPC 命令（Q11 / Q12 / Q4）。
//!
//! 让界面能真正配置配额、保留期与命令上限——客户在对应问题中
//! 明确要求这些值**可配置**，而不是写死在代码里。

use std::sync::Arc;

use tauri::State;

use crate::settings::{self, RuntimeSettings};
use crate::state::AppState;

use super::IpcResult;

/// 读取全部运行期设置。
#[tauri::command]
pub async fn runtime_settings(
    state: State<'_, Arc<AppState>>,
) -> Result<IpcResult<RuntimeSettings>, ()> {
    let conn = state.db.lock().await;
    Ok(match settings::load(&conn) {
        Ok(v) => IpcResult::ok(v),
        Err(e) => IpcResult::from(e),
    })
}

/// 写入一项运行期设置，返回**夹紧后的实际生效值**。
///
/// 返回值让界面能提示"你填 0，实际生效 1"，而不是静默改写用户输入。
#[tauri::command]
pub async fn set_runtime_setting(
    key: String,
    value: String,
    state: State<'_, Arc<AppState>>,
) -> Result<IpcResult<String>, ()> {
    let conn = state.db.lock().await;
    Ok(match settings::set(&conn, &key, &value) {
        Ok(effective) => IpcResult::ok(effective),
        Err(e) => IpcResult::from(e),
    })
}
