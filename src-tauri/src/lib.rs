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
//! - [`sudo_bridge`]：ask 模式的用户确认桥接（Q33）
//!
//! 安全边界（AGENTS.md 0.1 / D6）：
//! 认证信息与 sudo 密码对 AI Agent 完全不可见，仅在应用进程内按需解密使用。

pub mod domain;
pub mod error;
pub mod ipc;
pub mod ssh;
pub mod state;
pub mod settings;
pub mod store;
pub mod sudo_bridge;
pub mod terminal;
pub mod tray;
pub mod update;

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

    // ask 模式的用户确认桥接（Q33）。
    let sudo_bridge = sudo_bridge::SudoBridge::new();

    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .manage(state)
        .manage(sudo_bridge.clone());

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
            ipc::reconnect_terminal,
            ipc::delete_terminal,
            // 审计
            ipc::search_history,
            ipc::history_stats,
            // 设置：无通用读写入口（V18），一律走下列类型化命令
            // MCP 管理
            ipc::mcp::mcp_status,
            ipc::mcp::mcp_start,
            ipc::mcp::mcp_stop,
            ipc::mcp::mcp_regenerate_token,
            ipc::mcp::mcp_set_allow_remote,
            ipc::mcp::mcp_set_auto_start,
            ipc::mcp::mcp_client_config,
            // sudo 确认（Q33 ask 模式）
            sudo_bridge::sudo_respond,
            // 更新检查（D23）
            ipc::update::update_info,
            ipc::update::update_check,
            ipc::update::update_ignore_version,
            ipc::update::update_set_source,
            ipc::update::update_set_auto_check,
            // 运行期设置（Q11 / Q12 / Q4）
            ipc::settings::runtime_settings,
            ipc::settings::set_runtime_setting,
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

            // 把 ask 模式的确认桥接注入终端运行时（Q33）。
            // 未注入时 ask 模式一律按拒绝处理（fail-closed），
            // 因此这一步是 ask 模式可用的前提。
            {
                let runtime = {
                    let state = app.state::<Arc<state::AppState>>().inner().clone();
                    state.terminals.clone()
                };
                let asker = sudo_bridge.as_asker(app.handle().clone());
                tauri::async_runtime::spawn(async move {
                    runtime.set_sudo_asker(asker).await;
                });
            }

            // 历史保留期清理（Q12）：启动时执行一次，之后每 24 小时一次。
            // 此前只实现了清理函数却从未调用，导致"30 天保留"实际不生效。
            {
                let state = app.state::<Arc<state::AppState>>().inner().clone();
                tauri::async_runtime::spawn(async move {
                    loop {
                        let retention_hours = {
                            let conn = state.db.lock().await;
                            crate::settings::load(&conn)
                                .map(|s| s.retention_hours)
                                .unwrap_or(crate::settings::DEFAULT_RETENTION_HOURS)
                        };

                        // retention_hours == 0 表示永久保留，不做清理。
                        if retention_hours > 0 {
                            let conn = state.db.lock().await;
                            match crate::store::commands::cleanup_expired(
                                &conn,
                                retention_hours,
                                chrono::Utc::now(),
                            ) {
                                Ok(report) if report.deleted > 0 => {
                                    // 清理行为需留痕（D14），便于确认"历史为何变少"。
                                    tracing::info!(
                                        deleted = report.deleted,
                                        cutoff = %report.cutoff,
                                        "已按保留期清理活跃终端的历史"
                                    );
                                }
                                Ok(_) => tracing::debug!("保留期清理：无需删除的记录"),
                                Err(e) => tracing::warn!("保留期清理失败：{e}"),
                            }
                        }

                        tokio::time::sleep(std::time::Duration::from_secs(24 * 60 * 60)).await;
                    }
                });
            }

            // 启动时的自动更新检查（D23）：后台执行、静默失败、24 小时缓存。
            // 不阻塞启动，也不在失败时打扰用户。
            {
                let state = app.state::<Arc<state::AppState>>().inner().clone();
                tauri::async_runtime::spawn(async move {
                    let (enabled, url, ignored, fresh) = {
                        let conn = state.db.lock().await;
                        (
                            update::auto_check_enabled(&conn).unwrap_or(true),
                            update::source_url(&conn).unwrap_or_default(),
                            update::ignored_version(&conn).unwrap_or(None),
                            update::cache_is_fresh(&conn).unwrap_or(false),
                        )
                    };

                    // 未启用、未配置更新源、或缓存仍有效时都不发请求。
                    if !enabled || url.trim().is_empty() || fresh {
                        tracing::debug!("跳过自动更新检查（未启用、未配置源或缓存有效）");
                        return;
                    }

                    // 网络请求在数据库锁之外进行（最长 5 秒）。
                    match update::fetch_manifest(&url).await {
                        Ok(manifest) => {
                            let current = update::current_version();
                            let status = update::evaluate(
                                &manifest,
                                &current,
                                ignored.as_deref(),
                                update::platform_key(),
                            );
                            if let update::UpdateStatus::Available { latest, .. } = &status {
                                tracing::info!(latest = %latest, "发现新版本");
                            }

                            // 回写缓存（短暂加锁）。
                            let conn = state.db.lock().await;
                            let _ = update::store_cached(&conn, &status);
                        }
                        // 自动检查失败不改动状态、不提示用户：网络不通是常见情况。
                        Err(e) => tracing::debug!("自动更新检查失败（已忽略）：{e}"),
                    }
                });
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
