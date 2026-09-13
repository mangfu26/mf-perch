//! MCP Server 的 IPC 命令（人类侧启停与配置）。
//!
//! MCP 的**工具调用**由 AI Agent 经 HTTP 直接访问端点，不经过 IPC；
//! 这里只暴露人类需要的管理操作：启停、端口与 Token 查看、远程连接开关。

use std::sync::Arc;

use tauri::State;

use crate::mcp::{endpoint, McpManager, McpStatus};
use crate::state::AppState;

use super::IpcResult;

/// 查询 MCP 运行状态与配置。
#[tauri::command]
pub async fn mcp_status(mcp: State<'_, Arc<McpManager>>) -> Result<IpcResult<McpStatus>, ()> {
    match mcp.status().await {
        Ok(s) => Ok(IpcResult::ok(s)),
        Err(e) => Ok(IpcResult::from(e)),
    }
}

/// 启动 MCP Server。
#[tauri::command]
pub async fn mcp_start(mcp: State<'_, Arc<McpManager>>) -> Result<IpcResult<McpStatus>, ()> {
    match mcp.start().await {
        Ok(s) => Ok(IpcResult::ok(s)),
        Err(e) => Ok(IpcResult::from(e)),
    }
}

/// 停止 MCP Server。
#[tauri::command]
pub async fn mcp_stop(mcp: State<'_, Arc<McpManager>>) -> Result<IpcResult<McpStatus>, ()> {
    match mcp.stop().await {
        Ok(s) => Ok(IpcResult::ok(s)),
        Err(e) => Ok(IpcResult::from(e)),
    }
}

/// 重新生成访问令牌（旧令牌立即失效，需更新客户端配置）。
#[tauri::command]
pub async fn mcp_regenerate_token(
    mcp: State<'_, Arc<McpManager>>,
) -> Result<IpcResult<String>, ()> {
    match mcp.regenerate_token().await {
        Ok(t) => Ok(IpcResult::ok(t)),
        Err(e) => Ok(IpcResult::from(e)),
    }
}

/// 设置是否允许远程连接（会重启端点以应用监听范围）。
///
/// 界面在开启前必须展示安全提示（P2：安全降级必须显式告知）。
#[tauri::command]
pub async fn mcp_set_allow_remote(
    allow: bool,
    mcp: State<'_, Arc<McpManager>>,
) -> Result<IpcResult<McpStatus>, ()> {
    match mcp.set_allow_remote(allow).await {
        Ok(s) => Ok(IpcResult::ok(s)),
        Err(e) => Ok(IpcResult::from(e)),
    }
}

/// 设置是否随应用启动自动运行 MCP Server。
#[tauri::command]
pub async fn mcp_set_auto_start(
    enabled: bool,
    mcp: State<'_, Arc<McpManager>>,
) -> Result<IpcResult<bool>, ()> {
    match mcp.set_auto_start(enabled).await {
        Ok(()) => Ok(IpcResult::ok(true)),
        Err(e) => Ok(IpcResult::from(e)),
    }
}

/// 生成供用户复制到 MCP 客户端的配置片段（仅 Streamable HTTP，D1）。
#[tauri::command]
pub async fn mcp_client_config(
    state: State<'_, Arc<AppState>>,
    mcp: State<'_, Arc<McpManager>>,
) -> Result<IpcResult<String>, ()> {
    let status = match mcp.status().await {
        Ok(s) => s,
        Err(e) => return Ok(IpcResult::from(e)),
    };

    let Some(port) = status.port else {
        return Ok(IpcResult::from(crate::error::AppError::Mcp(
            "MCP Server 尚未启动，暂无接入地址".into(),
        )));
    };

    let token = match &status.token {
        Some(t) => t.clone(),
        None => {
            let key = {
                let guard = state.master_key.lock().await;
                guard.get().ok().copied()
            };
            let conn = state.db.lock().await;
            match endpoint::ensure_token(&conn, key.as_ref()) {
                Ok(t) => t,
                Err(e) => return Ok(IpcResult::from(e)),
            }
        }
    };

    let url = endpoint::endpoint_url(port, status.allow_remote);
    Ok(IpcResult::ok(build_client_config(&url, &token)))
}

/// 构造标准 MCP 客户端配置片段。
///
/// **输出美化后的 JSON（多行缩进）**：这段文本的用途是被人类读、复制进客户端
/// 配置文件，紧凑单行既难读、在界面上还会被卡片右缘裁断。
fn build_client_config(url: &str, token: &str) -> String {
    let config = serde_json::json!({
        "mcpServers": {
            "mf-perch": {
                "type": "streamable-http",
                "url": url,
                "headers": {
                    "Authorization": format!("Bearer {token}")
                }
            }
        }
    });

    // 注意：serde_json 只在**自由函数**上提供美化输出，`Value` 没有
    // `to_string_pretty` 方法。该结构恒可序列化，万一失败也退回紧凑文本，
    // 保证返回的始终是合法 JSON，绝不 panic。
    serde_json::to_string_pretty(&config).unwrap_or_else(|_| config.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_config_uses_streamable_http_only() {
        // D1：只支持 Streamable HTTP，配置片段中不应出现 stdio 相关字段。
        let cfg = build_client_config("http://127.0.0.1:50001/mcp", "abc123");
        assert!(cfg.contains("streamable-http"));
        assert!(cfg.contains("http://127.0.0.1:50001/mcp"));
        assert!(cfg.contains("Bearer abc123"));
        assert!(!cfg.contains("command"), "不应包含 stdio 的 command 字段");
    }

    #[test]
    fn client_config_is_valid_json() {
        let cfg = build_client_config("http://127.0.0.1:50001/mcp", "t");
        let v: serde_json::Value = serde_json::from_str(&cfg).expect("应为合法 JSON");
        assert_eq!(v["mcpServers"]["mf-perch"]["type"], "streamable-http");
    }

    #[test]
    fn client_config_is_pretty_printed_for_humans() {
        // 客户反馈：紧凑单行既难读、界面上还会被卡片右缘裁断。
        let cfg = build_client_config("http://127.0.0.1:50001/mcp", "t");
        assert!(
            cfg.contains('\n') && cfg.contains("  \"mcpServers\""),
            "配置应为缩进多行，便于阅读与复制：\n{cfg}"
        );
    }
}
