//! mf-perch：让 AI Agent 像人类一样通过 MCP 与 SSH 访问和管理主机。
//!
//! 架构分层：
//! - [`domain`]：领域模型与状态定义，不依赖具体实现
//! - [`store`]：持久化（SQLite + 字段级加密 + 主密钥分层管理）
//! - [`ssh`]：SSH 终端引擎（russh + NUL 分帧协议）
//! - [`terminal`]：终端运行时（会话池、串行命令队列、输出泵）
//! - [`mcp`]：MCP Server（Streamable HTTP，供 AI Agent 调用）
//! - [`ipc`]：Tauri IPC（供人类界面管理主机、凭据与 MCP 启停）
//! - [`tray`]：托盘常驻与窗口关闭行为（D16：关窗不退出，MCP 持续运行）
//!
//! 安全边界（AGENTS.md 0.1 / D6）：
//! 认证信息与 sudo 密码对 AI Agent 完全不可见，仅在应用进程内按需解密使用。

pub mod domain;
pub mod error;
pub mod ipc;
pub mod ssh;
pub mod state;
pub mod store;
pub mod terminal;
pub mod tray;

#[cfg(feature = "mcp")]
pub mod mcp;

pub use error::{AppError, Result};

use std::sync::Arc;

use tauri::Manager;

use state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 初始化日志：以 RUST_LOG 控制级别，默认 info。
    // 刻意不打印凭据内容（D6 / P2）。
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    // 打开数据库并完成密钥引导。
    let state = match AppState::initialize() {
        Ok(s) => s,
        Err(e) => {
            // 启动失败必须明确报出原因，而不是静默空白窗口（P1）。
            eprintln!("mf-perch 启动失败：{e}");
            panic!("mf-perch 初始化失败：{e}");
        }
    };

    #[cfg(feature = "mcp")]
    let mcp_manager = Arc::new(mcp::McpManager::new(state.clone()));

    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .manage(state);

    #[cfg(feature = "mcp")]
    let builder = builder.manage(mcp_manager.clone());

    builder
        .invoke_handler(tauri::generate_handler![
            // 密钥状态与引导
            ipc::key_status,
            ipc::init_key_provider,
            ipc::unlock_with_password,
            // 主机
            ipc::list_hosts,
            ipc::save_host,
            ipc::delete_host,
            // 认证信息
            ipc::list_credentials,
            ipc::save_credential,
            ipc::delete_credential,
            // 终端
            ipc::list_terminals,
            ipc::archive_terminal,
            ipc::restore_terminal,
            ipc::delete_terminal,
            // 审计
            ipc::search_history,
            ipc::history_stats,
            // 设置
            ipc::get_setting,
            ipc::set_setting,
            // MCP 管理
            ipc::mcp::mcp_status,
            ipc::mcp::mcp_start,
            ipc::mcp::mcp_stop,
            ipc::mcp::mcp_regenerate_token,
            ipc::mcp::mcp_set_allow_remote,
            ipc::mcp::mcp_set_auto_start,
            ipc::mcp::mcp_client_config,
        ])
        .on_window_event(|window, event| {
            // 关窗隐藏到托盘而非退出（D16）：否则 MCP 会随之下线、Agent 断连。
            tray::on_window_event(window, event);
        })
        .setup(move |app| {
            // 创建托盘图标与菜单（D16）。
            if let Err(e) = tray::setup(app.handle()) {
                // 托盘失败不应阻断应用启动，但必须明确报出原因（P1）。
                tracing::error!("创建托盘图标失败：{e}；关闭窗口可能直接退出应用");
            }

            #[cfg(feature = "mcp")]
            {
                // 若配置为自动启动，则在应用就绪后启动 MCP Server（D16：托盘常驻）。
                let manager = app.state::<Arc<mcp::McpManager>>().inner().clone();
                let state = app.state::<Arc<state::AppState>>().inner().clone();
                tauri::async_runtime::spawn(async move {
                    let should_start = {
                        let conn = state.db.lock().await;
                        mcp::endpoint::auto_start(&conn).unwrap_or(true)
                    };
                    if should_start {
                        match manager.start().await {
                            Ok(s) => tracing::info!(
                                port = s.port,
                                allow_remote = s.allow_remote,
                                "MCP Server 已随应用启动"
                            ),
                            Err(e) => {
                                // 启动失败不应阻断应用——界面仍可手动启动与排错。
                                tracing::error!("MCP Server 自动启动失败：{e}");
                            }
                        }
                    }
                });
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
