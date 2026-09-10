//! mf-perch：让 AI Agent 像人类一样通过 MCP 与 SSH 访问和管理主机。
//!
//! 架构分层：
//! - [`domain`]：领域模型与状态定义，不依赖具体实现
//! - [`store`]：持久化（SQLite + 字段级加密 + 主密钥分层管理）
//! - [`ssh`]：SSH 终端引擎（russh + NUL 分帧协议）
//! - [`mcp`]：MCP Server（Streamable HTTP，供 AI Agent 调用）
//!
//! 安全边界（AGENTS.md 0.1 / D6）：
//! 认证信息与 sudo 密码对 AI Agent 完全不可见，仅在应用进程内按需解密使用。

pub mod domain;
pub mod error;
pub mod ssh;
pub mod store;

#[cfg(feature = "mcp")]
pub mod mcp;

pub use error::{AppError, Result};

/// 供 Tauri 前端调用的示例命令（脚手架保留，后续由 IPC 层替换）。
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .invoke_handler(tauri::generate_handler![greet])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
