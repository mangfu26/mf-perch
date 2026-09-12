//! 导出 MCP 工具的 `tools/list`（入参 schema）——**开发工具**。
//!
//! 用途：[`docs/mcp-tools.md`](../../docs/mcp-tools.md) 里"入参"一节的权威来源。
//! 改完工具定义后重新导出比对，避免手写文档与实现漂移。
//!
//! ```bash
//! cargo run --features mcp --example mcp_tools_dump
//! ```
//!
//! 与 `mcp_schema_probe` 的区别：那个用于让 Inspector 连接并做 `--strict`
//! 可移植性校验；这个把 schema 直接打到 stdout，便于贴进文档或做 diff。
//!
//! 完全隔离：内存数据库 + 临时主密钥，不读写用户真实数据目录，
//! 也不需要 `MFPERCH_TEST_*` 环境变量。

/// 仅在启用 `mcp` feature 时可用；未启用时给出明确提示而非晦涩的编译错误。
#[cfg(not(feature = "mcp"))]
fn main() {
    eprintln!("此示例需要 `mcp` feature：cargo run --features mcp --example mcp_tools_dump");
}

#[cfg(feature = "mcp")]
#[tokio::main]
async fn main() {
    use std::sync::Arc;

    use mf_perch_lib::mcp::McpManager;
    use mf_perch_lib::state::AppState;

    // 内存库 + 临时主密钥：完全隔离，不会写入用户数据目录。
    let conn = mf_perch_lib::store::db::open_in_memory().expect("in-memory db");
    let key = mf_perch_lib::store::crypto::generate_master_key();
    let state = Arc::new(AppState::new_for_test(conn, key));

    let manager = McpManager::new(state);
    let status = manager.start().await.expect("MCP 端点应能启动");
    let url = format!("http://127.0.0.1:{}/mcp", status.port.expect("应有端口"));
    let token = status.token.expect("应有 Token");

    let client = reqwest::Client::new();

    // ① initialize：拿会话 id（Streamable HTTP 用会话头）。
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "mf-perch-tools-dump", "version": "0.1.0" }
            }
        }))
        .send()
        .await
        .expect("initialize 应能发出");
    assert!(resp.status().is_success(), "initialize 应成功");
    let session_id = resp
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .expect("应返回 mcp-session-id")
        .to_string();

    // ② 协议要求：initialize 之后发一条 initialized 通知。
    let _ = client
        .post(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("mcp-session-id", &session_id)
        .json(&serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
        .send()
        .await;

    // ③ tools/list。
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("mcp-session-id", &session_id)
        .json(&serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }))
        .send()
        .await
        .expect("tools/list 应能发出");
    assert!(resp.status().is_success(), "tools/list 应成功");
    let text = resp.text().await.unwrap_or_default();

    // 响应可能是纯 JSON，也可能是 SSE（逐行 `data:` 负载），两种都要能解。
    let payload: serde_json::Value = serde_json::from_str(text.trim())
        .ok()
        .or_else(|| {
            text.lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .find_map(|d| serde_json::from_str(d.trim()).ok())
        })
        .expect("应能从响应中解析出 JSON");

    let tools = payload["result"]["tools"]
        .as_array()
        .expect("应返回工具数组");

    println!("===== tools/list（共 {} 个工具）=====", tools.len());
    for tool in tools {
        println!(
            "\n## {}\n{}\n参数 schema:\n{}\n",
            tool["name"].as_str().unwrap_or_default(),
            tool["description"].as_str().unwrap_or_default(),
            serde_json::to_string_pretty(&tool["inputSchema"]).unwrap_or_default()
        );
    }

    manager.stop().await.ok();
}
