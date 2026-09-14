//! 托盘常驻与窗口关闭行为（D16 / Q14）。
//!
//! 客户在 Q14 明确决定：**关闭窗口最小化到托盘，MCP Server 持续运行**。
//! 理由是"本应用实质是为 AI Agent 提供终端访问代理，关窗即退出会严重影响体验"——
//! 若关窗即退出，Agent 会突然断连，正在执行的长命令也会被中断。
//!
//! 行为约定：
//! - 关闭窗口 → 隐藏到托盘（不退出）
//! - 托盘左键单击 / 菜单"打开主界面" → 恢复并聚焦窗口
//! - 托盘菜单"退出" → 若有活跃终端，**先原生确认**再退出（D16）
//! - `pnpm tauri dev` 下开发者按 Ctrl+C 仍可直接终止进程，不受此处影响
//!
//! 实现要点：用 `EXITING` 标志区分"用户点关闭按钮"与"用户确认退出"——
//! 前者拦截并隐藏，后者放行。若不加区分，`app.exit()` 会被自己的
//! 关闭拦截逻辑挡下，导致无法退出。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, WindowEvent};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

use crate::state::AppState;

/// 正在退出：置位后放行窗口关闭，避免被自己的拦截逻辑挡住。
static EXITING: AtomicBool = AtomicBool::new(false);

/// 托盘是否已成功创建。
///
/// **这道保护必不可少**：若托盘创建失败（例如系统托盘不可用），
/// 此时仍拦截关窗并隐藏窗口，用户将没有任何途径重新打开界面，
/// 只能强制结束进程——比直接退出更糟。因此只有在托盘可用时才隐藏。
static TRAY_READY: AtomicBool = AtomicBool::new(false);

/// 托盘菜单项 ID。
const MENU_OPEN: &str = "tray_open";
const MENU_QUIT: &str = "tray_quit";
/// 主窗口标签（与 tauri.conf.json 的默认窗口一致）。
const MAIN_WINDOW: &str = "main";

/// 创建托盘图标与菜单。
pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    let open_item = MenuItem::with_id(app, MENU_OPEN, "打开主界面", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, MENU_QUIT, "退出 mf-perch", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;

    let menu = Menu::with_items(app, &[&open_item, &separator, &quit_item])?;

    let mut builder = TrayIconBuilder::with_id("main")
        .menu(&menu)
        .tooltip("mf-perch · AI Agent 的 SSH 终端代理")
        // 左键单击直接打开界面；右键（或左键）弹出菜单由 show_menu_on_left_click 控制。
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            MENU_OPEN => reveal_main_window(app),
            MENU_QUIT => request_quit(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // 左键单击（抬起动作）恢复窗口——这是托盘应用最常见的交互习惯。
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                reveal_main_window(tray.app_handle());
            }
        });

    // 复用应用图标；缺失时（理论上不会）仍创建托盘，只是没有图标。
    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }

    builder.build(app)?;
    TRAY_READY.store(true, Ordering::SeqCst);
    Ok(())
}

/// 显示、还原并聚焦主窗口。
pub fn reveal_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        // 顺序有讲究：先取消最小化再显示，否则 Windows 上可能仍是最小化状态。
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// 关闭请求的处理决策（纯逻辑，便于测试）。
#[derive(Debug, PartialEq, Eq)]
pub enum CloseDecision {
    /// 隐藏窗口（到托盘）并拦截关闭。
    HideToTray,
    /// 放行关闭（应用退出）。
    AllowClose,
}

/// 根据退出标志与托盘可用性决定如何处理关闭请求。
///
/// 抽成纯函数是为了能在不构造 `AppHandle` 的前提下测试这两个关键分支。
fn decide_close(exiting: bool, tray_ready: bool) -> CloseDecision {
    if exiting {
        // 用户已确认退出。
        return CloseDecision::AllowClose;
    }
    if !tray_ready {
        // 托盘不可用：隐藏窗口会让用户失去唯一的找回入口，宁可正常关闭。
        return CloseDecision::AllowClose;
    }
    CloseDecision::HideToTray
}

/// 处理窗口关闭请求：隐藏到托盘而非退出（D16）。
///
/// 返回 `true` 表示已拦截（窗口应隐藏），`false` 表示放行关闭。
pub fn handle_close_requested(app: &AppHandle) -> bool {
    match decide_close(EXITING.load(Ordering::SeqCst), is_tray_ready()) {
        CloseDecision::AllowClose => {
            if !EXITING.load(Ordering::SeqCst) {
                tracing::warn!(
                    "托盘不可用，无法隐藏到托盘；将按正常关闭处理（应用会退出）"
                );
            }
            false
        }
        CloseDecision::HideToTray => {
            if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
                // 隐藏窗口但保持进程存活，使 MCP Server 与 SSH 终端继续可用。
                let _ = window.hide();
            } else {
                // 拿不到窗口时仍需拦截，避免误退出。
                tracing::warn!("关闭请求时未找到主窗口，已拦截以避免意外退出");
            }
            tracing::info!("窗口已隐藏到托盘，MCP Server 继续运行");
            true
        }
    }
}

/// 托盘是否可用（诊断用）。
pub fn is_tray_ready() -> bool {
    TRAY_READY.load(Ordering::SeqCst)
}

/// 请求退出应用：有活跃终端时先确认（D16）。
fn request_quit(app: &AppHandle) {
    let app = app.clone();

    tauri::async_runtime::spawn(async move {
        let active = count_active_terminals(&app).await;

        if active == 0 {
            exit_now(&app);
            return;
        }

        // 有活跃终端：让窗口可见后再弹确认框，
        // 否则对话框可能出现在已隐藏的窗口之后，用户看不到。
        reveal_main_window(&app);

        let message = format!(
            "当前有 {active} 个活跃的 SSH 终端。\n\n\
             退出后这些连接会被断开，AI Agent 也将无法再通过 MCP 访问终端，\
             正在执行的命令会被中断。\n\n确定要退出吗？"
        );

        let app_for_callback = app.clone();
        app.dialog()
            .message(message)
            .title("确认退出 mf-perch")
            .kind(MessageDialogKind::Warning)
            .buttons(MessageDialogButtons::OkCancelCustom(
                "退出".to_string(),
                "取消".to_string(),
            ))
            .show(move |confirmed| {
                if confirmed {
                    exit_now(&app_for_callback);
                }
            });
    });
}

/// 统计活跃（未归档）终端数，用于退出确认。
async fn count_active_terminals(app: &AppHandle) -> u32 {
    let state = app.state::<Arc<AppState>>();
    let conn = state.db.lock().await;
    crate::store::terminals::count_active(&conn, None).unwrap_or(0)
}

/// 真正退出进程。
fn exit_now(app: &AppHandle) {
    // 先置位，使窗口关闭请求被放行。
    EXITING.store(true, Ordering::SeqCst);
    tracing::info!("正在退出 mf-perch");
    app.exit(0);
}

/// 供 `run()` 注册的窗口事件处理器。
pub fn on_window_event(window: &tauri::Window, event: &WindowEvent) {
    if let WindowEvent::CloseRequested { api, .. } = event {
        if handle_close_requested(window.app_handle()) {
            api.prevent_close();
        }
    }
}

/// 退出标志是否已置位（测试与诊断用）。
pub fn is_exiting() -> bool {
    EXITING.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hides_to_tray_when_normal_close_and_tray_available() {
        // 核心行为（Q14 / D16）：关窗应隐藏到托盘，让 MCP 继续服务 Agent。
        assert_eq!(decide_close(false, true), CloseDecision::HideToTray);
    }

    #[test]
    fn allows_close_when_exiting() {
        // 用户点了"退出"：必须放行，否则关不掉。
        assert_eq!(decide_close(true, true), CloseDecision::AllowClose);
    }

    #[test]
    fn allows_close_when_tray_unavailable() {
        // 关键保护：托盘不可用时若仍隐藏窗口，
        // 用户将失去唯一入口而只能强杀进程——比直接退出更糟。
        assert_eq!(decide_close(false, false), CloseDecision::AllowClose);
    }

    #[test]
    fn exiting_takes_precedence_over_tray_state() {
        assert_eq!(decide_close(true, false), CloseDecision::AllowClose);
    }

    // 关于 `EXITING` / `TRAY_READY` / `MENU_*`：
    // 它们只是进程级标志与私有常量，"写进去再读出来"证明不了任何用户可见行为，
    // 反而因为改动全局单例而与同二进制的其他测试相互干扰（§5.3）。
    // 真正要守的决策逻辑是 `decide_close`（生产路径见 `on_window_event`），
    // 上面四条用例已覆盖它的全部输入组合。
}
