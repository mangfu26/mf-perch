//! MCP 端到端集成测试：真实 HTTP + 真实 MCP 协议 + 真实 SSH。
//!
//! 覆盖阶段二的核心链路：
//! `list_hosts` → `create_terminal` → `run_command` → `get_command_status` → `archive_terminal`
//!
//! 默认忽略，需先准备环境（见 `docs/design/test-environment.md`）：
//!
//! ```bash
//! export MFPERCH_TEST_HOST=127.0.0.1
//! export MFPERCH_TEST_PORT=2222
//! export MFPERCH_TEST_USER=mfperch
//! export MFPERCH_TEST_KEY=<私钥路径>
//! cargo test --test mcp_e2e --features mcp -- --ignored --test-threads=1
//! ```

#![cfg(feature = "mcp")]

use std::sync::Arc;

use mf_perch_lib::domain::credential::{Credential, CredentialKind};
use mf_perch_lib::domain::host::Host;
use mf_perch_lib::mcp::McpManager;
use mf_perch_lib::state::AppState;
use mf_perch_lib::store::{credentials, hosts};

/// 构造一个使用内存数据库、且已注入主密钥的应用状态。
///
/// 刻意不走 `AppState::initialize()`：那会读写用户真实数据目录，
/// 测试必须完全隔离，避免污染真实配置。
fn test_state() -> (Arc<AppState>, [u8; 32]) {
    let conn = mf_perch_lib::store::db::open_in_memory().expect("in-memory db");
    let key = mf_perch_lib::store::crypto::generate_master_key();

    let state = Arc::new(AppState::new_for_test(conn, key));
    (state, key)
}

/// 读取测试目标环境变量；缺失时返回 `None`（测试跳过）。
fn test_target() -> Option<(String, u16, String, String)> {
    Some((
        std::env::var("MFPERCH_TEST_HOST").ok()?,
        std::env::var("MFPERCH_TEST_PORT").ok()?.parse().ok()?,
        std::env::var("MFPERCH_TEST_USER").ok()?,
        std::env::var("MFPERCH_TEST_KEY").ok()?,
    ))
}

/// 在测试状态中创建主机与密钥凭据。
async fn seed_host(state: &AppState, key: &[u8; 32]) -> String {
    let Some((address, port, username, key_path)) = test_target() else {
        panic!("未设置测试环境变量");
    };
    let pem = std::fs::read_to_string(&key_path).expect("读取测试私钥");

    let conn = state.db.lock().await;

    let cred = Credential::new(username, CredentialKind::Key, pem);
    let cred_id = cred.id.clone();
    credentials::insert(&conn, &cred, key).expect("写入凭据");

    let mut host = Host::new(address, port);
    host.name = Some("e2e test host".into());
    host.credential_id = Some(cred_id);
    let host_id = host.id.clone();
    hosts::insert(&conn, &host, None, key).expect("写入主机");

    host_id
}

/// 向 MCP 端点发起一次 JSON-RPC 调用，返回响应 JSON。
async fn mcp_call(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    session_id: Option<&str>,
    body: serde_json::Value,
) -> (serde_json::Value, Option<String>) {
    let mut req = client
        .post(url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream");

    if let Some(sid) = session_id {
        req = req.header("mcp-session-id", sid);
    }

    let resp = req.json(&body).send().await.expect("HTTP 请求应成功");

    let new_sid = resp
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    assert!(
        status.is_success(),
        "MCP 调用应返回成功状态，实际 {status}，响应：{text}"
    );

    (parse_sse_or_json(&text), new_sid)
}

/// MCP Streamable HTTP 可能以 SSE 或纯 JSON 返回，两种都要能解析。
fn parse_sse_or_json(text: &str) -> serde_json::Value {
    let trimmed = text.trim();

    // 纯 JSON 响应。
    if trimmed.starts_with('{') {
        return serde_json::from_str(trimmed).unwrap_or(serde_json::Value::Null);
    }

    // SSE：逐行找 data: 负载。
    for line in trimmed.lines() {
        if let Some(data) = line.strip_prefix("data:") {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(data.trim()) {
                return v;
            }
        }
    }
    serde_json::Value::Null
}

/// 从 tools/call 结果中取出文本内容并解析为 JSON。
fn tool_payload(resp: &serde_json::Value) -> serde_json::Value {
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("工具应返回文本内容，实际：{resp}"));

    serde_json::from_str(text).unwrap_or_else(|_| serde_json::json!({ "raw": text }))
}

/// 完整链路：列主机 → 建终端 → 执行命令 → 查状态 → 归档。
#[tokio::test]
#[ignore = "需要真实 SSH 服务器与 MCP 端点；设置 MFPERCH_TEST_* 后以 --ignored 运行"]
async fn mcp_full_lifecycle_over_real_ssh() {
    if test_target().is_none() {
        eprintln!("跳过：未设置 MFPERCH_TEST_HOST / PORT / USER / KEY");
        return;
    }

    let (state, key) = test_state();
    let host_id = seed_host(&state, &key).await;

    let manager = McpManager::new(state.clone());
    let status = manager.start().await.expect("MCP 端点应能启动");
    let port = status.port.expect("应有端口");
    let token = status.token.expect("应有 Token");
    let url = format!("http://127.0.0.1:{port}/mcp");

    let client = reqwest::Client::new();

    // --- 1) initialize ---
    let (init, sid) = mcp_call(
        &client,
        &url,
        &token,
        None,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "mf-perch-e2e", "version": "0.1.0" }
            }
        }),
    )
    .await;
    assert!(
        init["result"]["serverInfo"].is_object() || init["result"].is_object(),
        "initialize 应返回 serverInfo：{init}"
    );

    // 通知初始化完成（无 id，按 JSON-RPC 通知处理）。
    let _ = client
        .post(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("mcp-session-id", sid.clone().unwrap_or_default())
        .json(&serde_json::json!({
            "jsonrpc": "2.0", "method": "notifications/initialized"
        }))
        .send()
        .await;

    let sid_ref = sid.as_deref();

    // --- 2) tools/list：确认 6 个工具都暴露 ---
    let (tools, _) = mcp_call(
        &client,
        &url,
        &token,
        sid_ref,
        serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
    )
    .await;

    let names: Vec<String> = tools["result"]["tools"]
        .as_array()
        .expect("应返回工具列表")
        .iter()
        .filter_map(|t| t["name"].as_str().map(|s| s.to_string()))
        .collect();

    for expected in [
        "list_hosts",
        "create_terminal",
        "list_terminals",
        "run_command",
        "run_command_async",
        "get_command_status",
        "archive_terminal",
    ] {
        assert!(names.contains(&expected.to_string()), "缺少工具 {expected}，实际：{names:?}");
    }

    // --- 3) list_hosts：应看到已种入的主机，且 ready ---
    let (hosts_resp, _) = mcp_call(
        &client,
        &url,
        &token,
        sid_ref,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": { "name": "list_hosts", "arguments": {} }
        }),
    )
    .await;
    let hosts_payload = tool_payload(&hosts_resp);
    let listed = hosts_payload["hosts"].as_array().expect("应有 hosts 数组");
    assert_eq!(listed.len(), 1, "应返回 1 台主机");
    assert_eq!(listed[0]["id"].as_str(), Some(host_id.as_str()));
    assert_eq!(
        listed[0]["ready"].as_bool(),
        Some(true),
        "已绑定凭据的主机应为 ready"
    );

    // --- 4) create_terminal ---
    let (create_resp, _) = mcp_call(
        &client,
        &url,
        &token,
        sid_ref,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": {
                "name": "create_terminal",
                "arguments": { "host_id": host_id, "name": "e2e terminal" }
            }
        }),
    )
    .await;
    let created = tool_payload(&create_resp);
    let terminal_id = created["terminal_id"]
        .as_str()
        .unwrap_or_else(|| panic!("应返回 terminal_id，实际：{created}"))
        .to_string();
    assert_eq!(created["status"].as_str(), Some("active"));

    // --- 5) run_command（同步）：cd 与 export 应跨命令保留 ---
    let (cd_resp, _) = mcp_call(
        &client,
        &url,
        &token,
        sid_ref,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 5, "method": "tools/call",
            "params": {
                "name": "run_command",
                "arguments": { "terminal_id": terminal_id, "command": "cd /tmp && export MF_E2E=1" }
            }
        }),
    )
    .await;
    let cd_out = tool_payload(&cd_resp);
    assert_eq!(cd_out["exit_code"].as_i64(), Some(0), "cd 应成功：{cd_out}");

    let (pwd_resp, _) = mcp_call(
        &client,
        &url,
        &token,
        sid_ref,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 6, "method": "tools/call",
            "params": {
                "name": "run_command",
                "arguments": { "terminal_id": terminal_id, "command": "pwd; echo var=$MF_E2E" }
            }
        }),
    )
    .await;
    let pwd_out = tool_payload(&pwd_resp);
    let out_text = pwd_out["output"].as_str().unwrap_or_default();
    assert!(out_text.contains("/tmp"), "cd 状态应保留，实际输出：{out_text}");
    assert!(
        out_text.contains("var=1"),
        "export 状态应保留，实际输出：{out_text}"
    );

    // --- 6) run_command_async + get_command_status 轮询 ---
    let (async_resp, _) = mcp_call(
        &client,
        &url,
        &token,
        sid_ref,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": {
                "name": "run_command_async",
                "arguments": { "terminal_id": terminal_id, "command": "sleep 2; echo async-done" }
            }
        }),
    )
    .await;
    let async_out = tool_payload(&async_resp);
    let command_id = async_out["command_id"]
        .as_str()
        .expect("异步应返回 command_id")
        .to_string();
    assert_eq!(async_out["still_running"].as_bool(), Some(true));

    // 立即轮询：应为 running。
    let (poll1, _) = mcp_call(
        &client,
        &url,
        &token,
        sid_ref,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 8, "method": "tools/call",
            "params": { "name": "get_command_status", "arguments": { "command_id": command_id } }
        }),
    )
    .await;
    let poll1_payload = tool_payload(&poll1);
    assert!(
        poll1_payload["status"].as_str().is_some(),
        "轮询应返回状态：{poll1_payload}"
    );

    // 等待完成后再轮询：应为 completed 且输出包含结果。
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    let (poll2, _) = mcp_call(
        &client,
        &url,
        &token,
        sid_ref,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 9, "method": "tools/call",
            "params": { "name": "get_command_status", "arguments": { "command_id": command_id } }
        }),
    )
    .await;
    let poll2_payload = tool_payload(&poll2);
    assert_eq!(
        poll2_payload["status"].as_str(),
        Some("completed"),
        "命令应已完成：{poll2_payload}"
    );
    assert!(
        poll2_payload["output"]
            .as_str()
            .unwrap_or_default()
            .contains("async-done"),
        "应能取到异步命令的输出：{poll2_payload}"
    );

    // --- 7) archive_terminal：归档后不可见于 Agent ---
    let (archive_resp, _) = mcp_call(
        &client,
        &url,
        &token,
        sid_ref,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 10, "method": "tools/call",
            "params": { "name": "archive_terminal", "arguments": { "terminal_id": terminal_id } }
        }),
    )
    .await;
    let archived = tool_payload(&archive_resp);
    assert_eq!(archived["status"].as_str(), Some("archived"));

    let (list_resp, _) = mcp_call(
        &client,
        &url,
        &token,
        sid_ref,
        serde_json::json!({ "jsonrpc": "2.0", "id": 11, "method": "tools/call",
            "params": { "name": "list_terminals", "arguments": {} } }),
    )
    .await;
    let listed_terms = tool_payload(&list_resp);
    assert_eq!(
        listed_terms["terminals"].as_array().map(|a| a.len()),
        Some(0),
        "归档终端对 Agent 不可见"
    );

    // --- 8) 归档后再执行命令应被明确拒绝（错误码可判断）---
    let (reject_resp, _) = mcp_call(
        &client,
        &url,
        &token,
        sid_ref,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 12, "method": "tools/call",
            "params": { "name": "run_command",
                "arguments": { "terminal_id": terminal_id, "command": "pwd" } }
        }),
    )
    .await;
    let rejected = tool_payload(&reject_resp);
    assert_eq!(
        rejected["code"].as_str(),
        Some("terminal_archived"),
        "应返回可判断的错误码：{rejected}"
    );

    manager.stop().await.ok();
}

/// 鉴权：错误或缺省 Token 必须被拒绝（Q2）。
#[tokio::test]
#[ignore = "需要 MCP 端点；设置环境变量后以 --ignored 运行"]
async fn mcp_rejects_missing_or_wrong_token() {
    let (state, _key) = test_state();
    let manager = McpManager::new(state);
    let status = manager.start().await.expect("端点应能启动");
    let port = status.port.unwrap();
    let url = format!("http://127.0.0.1:{port}/mcp");

    let client = reqwest::Client::new();
    let body = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });

    // 无 Token。
    let resp = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::UNAUTHORIZED);

    // 错误 Token。
    let resp2 = client
        .post(&url)
        .header("Authorization", "Bearer wrong-token")
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp2.status(), reqwest::StatusCode::UNAUTHORIZED);

    manager.stop().await.ok();
}

/// 队列上限：同一终端并发提交超过上限应被拒绝（Q4）。
#[tokio::test]
#[ignore = "需要真实 SSH 服务器；设置 MFPERCH_TEST_* 后以 --ignored 运行"]
async fn mcp_rejects_when_command_queue_is_full() {
    if test_target().is_none() {
        return;
    }

    let (state, key) = test_state();
    let host_id = seed_host(&state, &key).await;
    let manager = McpManager::new(state.clone());
    let status = manager.start().await.unwrap();
    let base = format!("http://127.0.0.1:{}/mcp", status.port.unwrap());
    let token = status.token.unwrap();
    let client = reqwest::Client::new();

    let (init, sid) = mcp_call(
        &client,
        &base,
        &token,
        None,
        serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2025-06-18", "capabilities": {},
                        "clientInfo": { "name": "e2e", "version": "0" } } }),
    )
    .await;
    assert!(init["result"].is_object());
    let sid_ref = sid.as_deref();

    let (create_resp, _) = mcp_call(
        &client,
        &base,
        &token,
        sid_ref,
        serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": { "name": "create_terminal", "arguments": { "host_id": host_id } } }),
    )
    .await;
    let terminal_id = tool_payload(&create_resp)["terminal_id"]
        .as_str()
        .unwrap()
        .to_string();

    // 连续提交 12 条长命令（队列上限为 10）。
    let mut outcomes = Vec::new();
    for i in 0..12 {
        let (resp, _) = mcp_call(
            &client,
            &base,
            &token,
            sid_ref,
            serde_json::json!({ "jsonrpc": "2.0", "id": 100 + i, "method": "tools/call",
                "params": { "name": "run_command_async",
                    "arguments": { "terminal_id": terminal_id, "command": "sleep 8" } } }),
        )
        .await;
        outcomes.push(tool_payload(&resp));
    }

    let rejected = outcomes
        .iter()
        .filter(|o| o["code"].as_str() == Some("command_queue_full"))
        .count();
    assert!(
        rejected > 0,
        "超过队列上限的提交应被拒绝，实际结果：{outcomes:?}"
    );

    manager.stop().await.ok();
}
