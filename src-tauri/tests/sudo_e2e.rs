//! sudo 三模式的端到端测试（Q33）。
//!
//! 客户要求三种模式一期全部实现，因此这里逐一验证真实行为：
//!
//! | 模式 | 期望 |
//! | ---- | ---- |
//! | `deny` | sudo 因拿不到密码而失败；**绝不提权** |
//! | `auto` | 自动注入密码，sudo 成功 |
//! | `ask` + 允许 | 注入密码，sudo 成功 |
//! | `ask` + 拒绝 | 不注入，sudo 失败 |
//!
//! 运行前提见 `docs/design/test-environment.md`：
//!
//! ```bash
//! export MFPERCH_TEST_HOST=127.0.0.1
//! export MFPERCH_TEST_PORT=2222
//! export MFPERCH_TEST_USER=mfperch
//! export MFPERCH_TEST_KEY=<私钥路径>
//! export MFPERCH_TEST_SUDO_PW=<测试用户的 sudo 密码>
//! cargo test --test sudo_e2e --features mcp -- --ignored --test-threads=1
//! ```

#![cfg(feature = "mcp")]

use std::sync::Arc;
use std::time::Duration;

use mf_perch_lib::domain::credential::{Credential, CredentialKind};
use mf_perch_lib::domain::host::{Host, SudoPasswordSource, SudoPolicy};
use mf_perch_lib::state::AppState;
use mf_perch_lib::store::{credentials, hosts};
use mf_perch_lib::terminal::sudo::{SudoDecision, SudoRequest};

/// 测试目标信息。
struct Target {
    address: String,
    port: u16,
    username: String,
    key_pem: String,
    sudo_password: String,
}

fn target() -> Option<Target> {
    Some(Target {
        address: std::env::var("MFPERCH_TEST_HOST").ok()?,
        port: std::env::var("MFPERCH_TEST_PORT").ok()?.parse().ok()?,
        username: std::env::var("MFPERCH_TEST_USER").ok()?,
        key_pem: std::fs::read_to_string(std::env::var("MFPERCH_TEST_KEY").ok()?).ok()?,
        sudo_password: std::env::var("MFPERCH_TEST_SUDO_PW").ok()?,
    })
}

fn test_state() -> (Arc<AppState>, [u8; 32]) {
    let conn = mf_perch_lib::store::db::open_in_memory().expect("in-memory db");
    let key = mf_perch_lib::store::crypto::generate_master_key();
    (Arc::new(AppState::new_for_test(conn, key)), key)
}

/// 建主机与密钥凭据，并配置 sudo 策略。
async fn seed_host(
    state: &AppState,
    key: &[u8; 32],
    t: &Target,
    policy: SudoPolicy,
) -> String {
    let conn = state.db.lock().await;

    let cred = Credential::new(t.username.clone(), CredentialKind::Key, t.key_pem.clone());
    let cred_id = cred.id.clone();
    credentials::insert(&conn, &cred, key).expect("写入凭据");

    let mut host = Host::new(t.address.clone(), t.port);
    host.name = Some("sudo e2e".into());
    host.credential_id = Some(cred_id);
    host.sudo_policy = policy;
    // 登录用密钥，无法复用登录密码，故必须单独配置 sudo 密码。
    host.sudo_password_source = SudoPasswordSource::Own;
    let host_id = host.id.clone();
    hosts::insert(&conn, &host, Some(&t.sudo_password), key).expect("写入主机");

    host_id
}

/// 等待某条命令结束，返回（输出，退出码）。
async fn collect(
    rt: &mf_perch_lib::terminal::TerminalRuntime,
    db: &mf_perch_lib::store::Db,
    command_id: &str,
) -> (String, Option<i32>) {
    // 轮询直到命令结束（或超时）。
    for _ in 0..200 {
        {
            let conn = db.lock().await;
            if let Ok(rec) = mf_perch_lib::store::commands::get(&conn, command_id) {
                if !rec.status.is_pending() {
                    let out = mf_perch_lib::store::commands::get_output(&conn, command_id)
                        .ok()
                        .flatten()
                        .unwrap_or_default();
                    return (out, rec.exit_code);
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    let _ = rt;
    (String::new(), None)
}

/// **模式一：deny** —— sudo 必须失败，绝不提权。
#[tokio::test]
#[ignore = "需要真实 SSH 服务器；设置 MFPERCH_TEST_* 后以 --ignored 运行"]
async fn deny_mode_makes_sudo_fail() {
    let Some(t) = target() else {
        eprintln!("跳过：未设置 MFPERCH_TEST_HOST / PORT / USER / KEY / SUDO_PW");
        return;
    };

    let (state, key) = test_state();
    let host_id = seed_host(&state, &key, &t, SudoPolicy::Deny).await;

    let terminal = state
        .terminals
        .open_terminal(&state.db, &key, &host_id, None)
        .await
        .expect("终端应能建立");

    // 用 `sudo id -u` 而非 `sudo -n`：后者本身就不读密码，
    // 无法体现"拦截是否生效"。
    let outcome = state
        .terminals
        .run_command(
            &state.db,
            &terminal.id,
            "sudo id -u",
            Some(Duration::from_secs(20)),
        )
        .await
        .expect("命令应能下发");

    let (output, code) = collect(&state.terminals, &state.db, &outcome.command_id).await;

    assert_ne!(
        code,
        Some(0),
        "deny 模式下 sudo 必须失败（fail-closed）。输出：{output}"
    );
    // 关键：不能出现 root 的 uid（0），否则说明真的提权成功了。
    assert!(
        !output.trim().lines().any(|l| l.trim() == "0"),
        "deny 模式下不得提权成功。输出：{output}"
    );

    state.terminals.delete_terminal(&state.db, &terminal.id).await.ok();
}

/// **模式三：auto** —— 自动注入密码，sudo 应成功。
#[tokio::test]
#[ignore = "需要真实 SSH 服务器；设置 MFPERCH_TEST_* 后以 --ignored 运行"]
async fn auto_mode_injects_password_and_succeeds() {
    let Some(t) = target() else {
        return;
    };

    let (state, key) = test_state();
    let host_id = seed_host(&state, &key, &t, SudoPolicy::Auto).await;

    let terminal = state
        .terminals
        .open_terminal(&state.db, &key, &host_id, None)
        .await
        .expect("终端应能建立");

    let outcome = state
        .terminals
        .run_command(
            &state.db,
            &terminal.id,
            "sudo id -u",
            Some(Duration::from_secs(30)),
        )
        .await
        .expect("命令应能下发");

    let (output, code) = collect(&state.terminals, &state.db, &outcome.command_id).await;

    assert_eq!(
        code,
        Some(0),
        "auto 模式应自动注入密码并成功。输出：{output}"
    );
    assert!(
        output.contains('0'),
        "sudo id -u 应输出 0（root）。输出：{output}"
    );

    state.terminals.delete_terminal(&state.db, &terminal.id).await.ok();
}

/// **模式二：ask + 允许** —— 用户同意后注入，sudo 成功。
#[tokio::test]
#[ignore = "需要真实 SSH 服务器；设置 MFPERCH_TEST_* 后以 --ignored 运行"]
async fn ask_mode_with_allow_succeeds() {
    let Some(t) = target() else {
        return;
    };

    let (state, key) = test_state();
    let host_id = seed_host(&state, &key, &t, SudoPolicy::Ask).await;

    // 注入一个"总是允许"的替身回调，替代真实人工确认。
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<SudoRequest>();
    let asker: mf_perch_lib::terminal::SudoAsker = Arc::new(move |req: SudoRequest| {
        // 把请求转出去，测试侧可断言确实收到过请求。
        let _ = tx.send(req);
        let (rtx, rrx) = tokio::sync::oneshot::channel();
        // 立即允许。
        let _ = rtx.send(SudoDecision::Allow);
        rrx
    });
    state.terminals.set_sudo_asker(asker).await;

    let terminal = state
        .terminals
        .open_terminal(&state.db, &key, &host_id, None)
        .await
        .expect("终端应能建立");

    let outcome = state
        .terminals
        .run_command(
            &state.db,
            &terminal.id,
            "sudo id -u",
            Some(Duration::from_secs(30)),
        )
        .await
        .expect("命令应能下发");

    let (output, code) = collect(&state.terminals, &state.db, &outcome.command_id).await;

    assert_eq!(code, Some(0), "允许后 sudo 应成功。输出：{output}");
    assert!(
        output.contains('0'),
        "应输出 root 的 uid。输出：{output}"
    );

    // 应当收到过确认请求（否则说明 ask 流程未被触发）。
    let got = rx.try_recv();
    assert!(got.is_ok(), "ask 模式应向用户发出确认请求");

    state.terminals.delete_terminal(&state.db, &terminal.id).await.ok();
}

/// **模式二：ask + 拒绝** —— 拒绝后 sudo 失败，且**不能卡住队列**。
#[tokio::test]
#[ignore = "需要真实 SSH 服务器；设置 MFPERCH_TEST_* 后以 --ignored 运行"]
async fn ask_mode_with_deny_fails_and_keeps_queue_alive() {
    let Some(t) = target() else {
        return;
    };

    let (state, key) = test_state();
    let host_id = seed_host(&state, &key, &t, SudoPolicy::Ask).await;

    let asker: mf_perch_lib::terminal::SudoAsker = Arc::new(|_req: SudoRequest| {
        let (rtx, rrx) = tokio::sync::oneshot::channel();
        let _ = rtx.send(SudoDecision::Deny);
        rrx
    });
    state.terminals.set_sudo_asker(asker).await;

    let terminal = state
        .terminals
        .open_terminal(&state.db, &key, &host_id, None)
        .await
        .expect("终端应能建立");

    let outcome = state
        .terminals
        .run_command(
            &state.db,
            &terminal.id,
            "sudo id -u",
            Some(Duration::from_secs(30)),
        )
        .await
        .expect("命令应能下发");

    let (output, code) = collect(&state.terminals, &state.db, &outcome.command_id).await;

    assert_ne!(code, Some(0), "拒绝后 sudo 应失败。输出：{output}");
    assert!(
        !output.trim().lines().any(|l| l.trim() == "0"),
        "拒绝后不得提权。输出：{output}"
    );

    // **关键回归**：拒绝必须让 askpass 的 read 得到 EOF 而结束。
    // 若不主动关闭 FIFO，askpass 会永久阻塞，而命令串行执行，
    // 后续命令会全部卡死——这里验证队列仍可用。
    let follow_up = state
        .terminals
        .run_command(
            &state.db,
            &terminal.id,
            "echo queue-still-alive",
            Some(Duration::from_secs(20)),
        )
        .await
        .expect("后续命令应能下发");

    let (out2, code2) = collect(&state.terminals, &state.db, &follow_up.command_id).await;
    assert_eq!(
        code2,
        Some(0),
        "拒绝 sudo 后终端应继续可用（不得卡死队列）。输出：{out2}"
    );
    assert!(out2.contains("queue-still-alive"));

    state.terminals.delete_terminal(&state.db, &terminal.id).await.ok();
}

/// 非 sudo 命令在 ask 模式下不应触发确认（避免打扰）。
#[tokio::test]
#[ignore = "需要真实 SSH 服务器；设置 MFPERCH_TEST_* 后以 --ignored 运行"]
async fn ask_mode_does_not_prompt_for_non_sudo_commands() {
    let Some(t) = target() else {
        return;
    };

    let (state, key) = test_state();
    let host_id = seed_host(&state, &key, &t, SudoPolicy::Ask).await;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<SudoRequest>();
    let asker: mf_perch_lib::terminal::SudoAsker = Arc::new(move |req: SudoRequest| {
        let _ = tx.send(req);
        let (rtx, rrx) = tokio::sync::oneshot::channel();
        let _ = rtx.send(SudoDecision::Allow);
        rrx
    });
    state.terminals.set_sudo_asker(asker).await;

    let terminal = state
        .terminals
        .open_terminal(&state.db, &key, &host_id, None)
        .await
        .expect("终端应能建立");

    let outcome = state
        .terminals
        .run_command(&state.db, &terminal.id, "echo no-sudo", Some(Duration::from_secs(15)))
        .await
        .expect("命令应能下发");

    let (output, code) = collect(&state.terminals, &state.db, &outcome.command_id).await;
    assert_eq!(code, Some(0));
    assert!(output.contains("no-sudo"));

    // 普通命令不应产生确认请求。
    let got = rx.try_recv();
    assert!(
        got.is_err(),
        "非 sudo 命令不应触发确认请求，实际收到：{got:?}"
    );

    state.terminals.delete_terminal(&state.db, &terminal.id).await.ok();
}
