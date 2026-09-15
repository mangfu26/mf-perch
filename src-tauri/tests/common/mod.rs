//! 集成测试共用设施。
//!
//! **仅用于 `tests/*.rs`**：每个测试文件是各自独立的 crate，用 `mod common;` 引入。
//! `src/` 内的单元测试在生产 crate 内部，拿不到这里的模块——因此
//! `src/ipc/tests.rs`、`src/mcp/tools.rs` 的同名 fixture 只能留在原处。
//!
//! 各测试文件只用到其中一部分，故整体允许 `dead_code`，避免未使用的项刷警告。
#![allow(dead_code)]

use std::sync::Arc;

use mf_perch_lib::state::AppState;

/// 内存数据库 + 主密钥的应用状态。
///
/// 刻意不走 `AppState::initialize()`：那会读写用户真实数据目录，
/// 测试必须完全隔离，避免污染真实配置。
pub fn test_state() -> (Arc<AppState>, [u8; 32]) {
    let conn = mf_perch_lib::store::db::open_in_memory().expect("in-memory db");
    let key = mf_perch_lib::store::crypto::generate_master_key();
    (Arc::new(AppState::new_for_test(conn, key)), key)
}

/// 读取必需的测试环境变量。
///
/// **缺失即失败**（AGENTS.md §5.6）：直接 `panic!`，不返回 `Option` 让调用方
/// `return` 跳过——静默跳过会让报告显示"通过"而实际一条断言都没执行。
/// 需要跳过时请用 `#[ignore]` 表达。
pub fn need_env(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| {
        panic!("未设置环境变量 {key}；联调环境准备见 docs/design/test-environment.md")
    })
}

/// 读取必需的环境变量并解析为端口号。
pub fn need_env_port(key: &str) -> u16 {
    need_env(key)
        .parse()
        .unwrap_or_else(|e| panic!("{key} 应为端口号：{e}"))
}
