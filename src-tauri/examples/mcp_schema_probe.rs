//! 临时验证用：启动一个**使用内存数据库**的真实 MCP 端点，
//! 供 MCP Inspector 等客户端连接、检查工具 schema。
//!
//! 用途：不启动桌面应用、不触碰用户真实数据目录的前提下，验证
//! `tools/list` 返回的 schema 是否被客户端接受（例如检查可空参数是否
//! 还有数组形式 `type` 的兼容性告警，见 D32）。
//!
//! 用法：
//!
//! ```bash
//! cargo run --features mcp --example mcp_schema_probe
//! ```
//!
//! 启动后会在 stdout 打印地址与 Bearer Token（三行以 `READY` 结束），
//! 然后持续运行直到进程被杀。示例：
//!
//! ```bash
//! npx -y @modelcontextprotocol/inspector --cli \
//!   --transport http --server-url http://127.0.0.1:50001/mcp \
//!   --header "Authorization: Bearer <token>" \
//!   --method tools/list --strict
//! ```
//!
//! `--strict` 报告 schema 可移植性问题；无输出即表示没有问题。

/// 示例仅在有 `mcp` feature 时可用；关闭时给出明确提示而非晦涩的编译错误。
#[cfg(not(feature = "mcp"))]
fn main() {
    eprintln!("此示例需要 `mcp` feature：cargo run --features mcp --example mcp_schema_probe");
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

    println!("MCP_URL=http://127.0.0.1:{}/mcp", status.port.unwrap());
    println!("MCP_TOKEN={}", status.token.unwrap_or_default());
    println!("READY");

    // 保持进程存活，直到被外部中断或杀掉。
    std::future::pending::<()>().await;
}
