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

/// 发起一次 MCP 调用但**不**断言成功，返回（状态码，响应体，会话 id）。
///
/// 用于验证会话被回收后的行为（期望拿到 404），因此不能用会断言成功的
/// [`mcp_call`]。
async fn mcp_call_raw(
    client: &reqwest::Client,
    url: &str,
    session_id: Option<&str>,
    body: serde_json::Value,
) -> (reqwest::StatusCode, String, Option<String>) {
    let mut req = client
        .post(url)
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
    (status, text, new_sid)
}

/// 用**指定的会话空闲超时**起一个裸 MCP 端点（不带鉴权），返回（地址，关闭句柄）。
///
/// 仅供测试：用于复现"空闲超时把会话回收掉"的行为。
async fn spawn_raw_mcp_endpoint(
    state: Arc<AppState>,
    keep_alive: Option<std::time::Duration>,
) -> (String, tokio::task::JoinHandle<()>) {
    use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
    use rmcp::transport::streamable_http_server::tower::{
        StreamableHttpServerConfig, StreamableHttpService,
    };

    let mut manager = LocalSessionManager::default();
    manager.session_config.keep_alive = keep_alive;

    let service = StreamableHttpService::new(
        {
            let state = state.clone();
            move || Ok(mf_perch_lib::mcp::McpService::new(state.clone()))
        },
        Arc::new(manager),
        StreamableHttpServerConfig::default(),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑定测试端口");
    let addr = listener.local_addr().expect("读取测试地址");
    let router = axum::Router::new().route("/mcp", axum::routing::any_service(service));

    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    (format!("http://{addr}/mcp"), handle)
}

/// 完成一次 initialize 握手，返回会话 id。
async fn initialize_session(
    client: &reqwest::Client,
    url: &str,
    token: Option<&str>,
) -> String {
    let mut req = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream");
    if let Some(t) = token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }
    let resp = req
        .json(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "mf-perch-e2e", "version": "0.1.0" }
            }
        }))
        .send()
        .await
        .expect("initialize 应能发出");
    assert!(resp.status().is_success(), "initialize 应成功");
    resp.headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .expect("initialize 应返回 mcp-session-id")
        .to_string()
}

/// **机制复现（红例）**：会话空闲超时一旦到期，客户端再带原会话 id 请求
/// 就会被判定为"会话不存在"（404），这正是用户用 MCP Inspector 实测到的现象。
///
/// 这里把空闲超时压到 1 秒，以便几秒内复现 rmcp 的默认行为
/// （默认值为 5 分钟，见 `mcp::server` 的单测）。
#[tokio::test]
async fn expired_session_is_reported_as_not_found() {
    let (state, _key) = test_state();
    let (url, handle) = spawn_raw_mcp_endpoint(state, Some(std::time::Duration::from_secs(1))).await;
    let client = reqwest::Client::new();

    let sid = initialize_session(&client, &url, None).await;

    // 空闲超过 1 秒 → 会话 worker 退出、会话被移除。
    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;

    let (status, body, _) = mcp_call_raw(
        &client,
        &url,
        Some(&sid),
        serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
    )
    .await;

    assert_eq!(
        status,
        reqwest::StatusCode::NOT_FOUND,
        "空闲超时后应报会话不存在，实际 {status}：{body}"
    );
    assert!(
        body.contains("Session not found") || body.contains("session not found"),
        "响应应说明会话不存在（用户看到的即此错误）：{body}"
    );

    handle.abort();
}

/// **D39 回归（客户实测反馈）**：应用重启后，旧终端的 SSH 会话已不存在
/// （启动时被标记为 `broken`），此时 Agent 继续用原 `terminal_id` 执行命令，
/// 必须**自动重连**并成功执行，而不是报"终端连接已断开，请重建终端"
/// ——后者 Agent 根本没有手段完成（工具列表里没有重连工具，
/// 只能另建新终端，而且 broken 占配额，几次重启就会撞满配额）。
///
/// 编排刻意贴近真实：
/// 1. 用**同一份数据库文件**开两个 `AppState`，模拟"应用重启"——
///    第二个实例的终端运行时里没有任何会话，且启动时把 active 标成 broken；
/// 2. 把每主机配额压到 **1**：重连**不得**重新校验配额
///    （broken 本来就算在配额里，再校验会把它自己数进去而误报超限）；
/// 3. 走真实 MCP HTTP 调用（与客户用 Inspector 测的路径一致）。
#[tokio::test]
#[ignore = "需要真实 SSH 服务器与 MCP 端点；设置 MFPERCH_TEST_* 后以 --ignored 运行"]
async fn run_command_auto_reconnects_broken_terminal_after_restart() {
    if test_target().is_none() {
        eprintln!("跳过：未设置 MFPERCH_TEST_HOST / PORT / USER / KEY");
        return;
    }

    let dir = tempfile::tempdir().expect("临时目录");
    let db_path = dir.path().join("mf-perch-reconnect.sqlite");
    let key = mf_perch_lib::store::crypto::generate_master_key();

    // ---- 第一段生命周期：建立终端并成功执行一条命令 ----
    let state1 = Arc::new(AppState::new_for_test(
        mf_perch_lib::store::db::open(&db_path).expect("打开测试库"),
        key,
    ));
    let host_id = seed_host(&state1, &key).await;

    // 配额压到 1：让"重连时若误做配额校验"必然失败（该终端自己就占了 1 个名额）。
    {
        let conn = state1.db.lock().await;
        mf_perch_lib::store::db::set_setting(
            &conn,
            mf_perch_lib::settings::SETTING_QUOTA_PER_HOST,
            "1",
        )
        .expect("设置每主机配额");
    }

    let manager1 = McpManager::new(state1.clone());
    let status = manager1.start().await.expect("MCP 端点应能启动");
    let port = status.port.expect("应有端口");
    let token = status.token.expect("应有 Token");
    let url = format!("http://127.0.0.1:{port}/mcp");
    let client = reqwest::Client::new();
    let sid = initialize_session(&client, &url, Some(&token)).await;

    let (created, _) = mcp_call(
        &client,
        &url,
        &token,
        Some(&sid),
        serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": { "name": "create_terminal",
                "arguments": { "host_id": host_id, "name": "reconnect e2e" } }
        }),
    )
    .await;
    let terminal_id = tool_payload(&created)["terminal_id"]
        .as_str()
        .expect("create_terminal 应返回 terminal_id")
        .to_string();

    let (first, _) = mcp_call(
        &client,
        &url,
        &token,
        Some(&sid),
        serde_json::json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": { "name": "run_command",
                "arguments": { "terminal_id": terminal_id, "command": "echo before-restart" } }
        }),
    )
    .await;
    let before = tool_payload(&first);
    assert_eq!(before["exit_code"], 0, "重启前命令应成功：{before}");
    assert_eq!(
        before["session_reconnected"], false,
        "首次执行不应报告重连：{before}"
    );

    manager1.stop().await.ok();

    // ---- 模拟应用重启：同一份库、全新的终端运行时 ----
    let state2 = Arc::new(AppState::new_for_test(
        mf_perch_lib::store::db::open(&db_path).expect("重新打开测试库"),
        key,
    ));
    {
        let conn = state2.db.lock().await;
        let n = mf_perch_lib::store::terminals::mark_all_broken_on_startup(&conn)
            .expect("模拟启动时标记 broken");
        assert_eq!(n, 1, "重启后应把原 active 终端标记为 broken");
    }

    let manager2 = McpManager::new(state2.clone());
    let status2 = manager2.start().await.expect("重启后的 MCP 端点应能启动");
    let port2 = status2.port.expect("应有端口");
    let token2 = status2.token.expect("应有 Token");
    let url2 = format!("http://127.0.0.1:{port2}/mcp");
    let sid2 = initialize_session(&client, &url2, Some(&token2)).await;

    // Agent 看到的是 broken 状态（与客户截图一致）。
    let (listed, _) = mcp_call(
        &client,
        &url2,
        &token2,
        Some(&sid2),
        serde_json::json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": { "name": "list_terminals", "arguments": { "host_id": host_id } }
        }),
    )
    .await;
    let listed = tool_payload(&listed);
    let status_field = listed["terminals"][0]["status"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_eq!(status_field, "broken", "重启后终端应为 broken：{listed}");

    // 关键：用**同一个 terminal_id** 继续执行命令。
    let (after, _) = mcp_call(
        &client,
        &url2,
        &token2,
        Some(&sid2),
        serde_json::json!({
            "jsonrpc": "2.0", "id": 5, "method": "tools/call",
            "params": { "name": "run_command",
                "arguments": { "terminal_id": terminal_id, "command": "echo after-restart" } }
        }),
    )
    .await;
    let after = tool_payload(&after);

    assert_eq!(
        after["exit_code"], 0,
        "broken 终端上的命令应自动重连后成功执行：{after}"
    );
    assert_eq!(
        after["session_reconnected"], true,
        "必须明确告知 Agent：会话已重建、shell 状态已重置：{after}"
    );
    let output = after["output"].as_str().unwrap_or_default();
    assert!(
        output.contains("after-restart"),
        "命令本身应正常执行：{output}"
    );
    assert!(
        output.contains("[mf-perch]") && output.contains("重建"),
        "命令历史里应留有可审计的重连说明：{output}"
    );

    // 终端应已回到 active，且不因配额（=1）而失败。
    let (listed2, _) = mcp_call(
        &client,
        &url2,
        &token2,
        Some(&sid2),
        serde_json::json!({
            "jsonrpc": "2.0", "id": 6, "method": "tools/call",
            "params": { "name": "list_terminals", "arguments": { "host_id": host_id } }
        }),
    )
    .await;
    let listed2 = tool_payload(&listed2);
    assert_eq!(
        listed2["terminals"][0]["status"], "active",
        "重连后状态应回到 active，且不受配额上限（1）影响：{listed2}"
    );

    manager2.stop().await.ok();
}

/// **修复验证**：使用**生产配置**（`McpManager::start`）时，会话必须能挺过
/// 远长于 rmcp 默认 5 分钟的空闲，之后仍能正常调用工具。
///
/// 空闲时长默认 310 秒（刚好越过 5 分钟默认值），可用
/// `MFPERCH_TEST_IDLE_SECS` 调整；不依赖 SSH，无需 MFPERCH_TEST_HOST。
#[tokio::test]
#[ignore = "耗时较长（默认空闲 310 秒）；用 --ignored 运行"]
async fn mcp_session_survives_idle_longer_than_rmcp_default() {
    let idle = std::env::var("MFPERCH_TEST_IDLE_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(310);

    let (state, _key) = test_state();
    let manager = McpManager::new(state.clone());
    let status = manager.start().await.expect("MCP 端点应能启动");
    let port = status.port.expect("应有端口");
    let token = status.token.expect("应有 Token");
    let url = format!("http://127.0.0.1:{port}/mcp");

    let client = reqwest::Client::new();
    let sid = initialize_session(&client, &url, Some(&token)).await;

    eprintln!("[MCP 空闲测试] 保持 {idle} 秒不发任何请求……");
    tokio::time::sleep(std::time::Duration::from_secs(idle)).await;

    // 同一个会话 id 继续用：修复前这里会拿到 404 Session not found。
    let (resp, _) = mcp_call(
        &client,
        &url,
        &token,
        Some(&sid),
        serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
    )
    .await;

    let names: Vec<String> = resp["result"]["tools"]
        .as_array()
        .expect("空闲后仍应返回工具列表")
        .iter()
        .filter_map(|t| t["name"].as_str().map(|s| s.to_string()))
        .collect();
    assert!(
        names.contains(&"run_command".to_string()),
        "空闲 {idle} 秒后会话应仍然可用，实际工具：{names:?}"
    );

    manager.stop().await.ok();
}

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

/// 递归检查 JSON Schema 中是否存在数组形式的 `type`。
fn contains_array_type(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => map
            .iter()
            .any(|(k, v)| (k == "type" && v.is_array()) || contains_array_type(v)),
        serde_json::Value::Array(items) => items.iter().any(contains_array_type),
        _ => false,
    }
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

    // 工具 schema 必须是客户端普遍能接受的形态：`type` 不能是数组
    // （MCP Inspector 等会告警，个别客户端会丢弃约束甚至拒绝工具）。
    // 可空参数应表达为 `anyOf: [{...}, {"type": "null"}]`。
    let tools_json = tools["result"]["tools"].as_array().expect("工具数组");
    for tool in tools_json {
        let schema = &tool["inputSchema"];
        assert_eq!(schema["type"], serde_json::json!("object"), "{tool}");
        assert!(
            !contains_array_type(schema),
            "工具 {} 的 inputSchema 含数组形式 type：{schema}",
            tool["name"]
        );
    }
    assert_eq!(
        tools_json
            .iter()
            .find(|t| t["name"] == "run_command")
            .expect("run_command")["inputSchema"]["properties"]["wait_seconds"]["anyOf"][1]["type"],
        serde_json::json!("null"),
        "wait_seconds 应以 anyOf + null 表达可空"
    );

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
