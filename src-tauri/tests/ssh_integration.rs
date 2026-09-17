//! 端到端集成测试：连接真实的 SSH 服务器验证终端会话。
//!
//! 这些测试需要一台可连的 SSH 服务器，因此**默认忽略**。
//! 运行方式（以本项目在 Windows + WSL 的测试环境为例）：
//!
//! ```bash
//! # 先在 WSL 中确保证书已部署到 mfperch 用户，然后：
//! export MFPERCH_TEST_HOST=127.0.0.1
//! export MFPERCH_TEST_PORT=2222
//! export MFPERCH_TEST_USER=mfperch
//! export MFPERCH_TEST_KEY=/path/to/id_test
//! cargo test --test ssh_integration -- --ignored --test-threads=1
//! ```
//!
//! 前置条件不满足时测试**明确失败**（`need_env` 会 panic），不会静默跳过——
//! 静默跳过会制造"绿灯假象"（AGENTS.md §5.6）。需要跳过时只能用 `#[ignore]` 表达。

use std::time::Duration;

use mf_perch_lib::domain::host::{Host, ShellEnvMode, SudoPasswordSource, SudoPolicy};
use mf_perch_lib::ssh::auth::AuthMethod;
use mf_perch_lib::ssh::{Session, SessionOutput};

/// 从环境变量读取测试目标。
///
/// **缺失即失败**（AGENTS.md §5.6）：直接 `panic!` 而不是返回 `None` 让调用方
/// `return` 跳过——静默跳过会让报告显示"通过"而实际没跑任何断言。
/// 需要跳过时请用 `#[ignore]` 表达。
fn test_target() -> (Host, AuthMethod) {
    fn need(key: &str) -> String {
        std::env::var(key).unwrap_or_else(|_| {
            panic!("未设置环境变量 {key}；联调环境准备见 docs/design/test-environment.md")
        })
    }

    let address = need("MFPERCH_TEST_HOST");
    let port: u16 = need("MFPERCH_TEST_PORT")
        .parse()
        .expect("MFPERCH_TEST_PORT 应为端口号");
    let username = need("MFPERCH_TEST_USER");
    let key_path = need("MFPERCH_TEST_KEY");

    let private_key_pem = std::fs::read_to_string(&key_path)
        .expect("读取测试私钥失败：确认 MFPERCH_TEST_KEY 指向可读文件");

    let mut host = Host::new(address, port);
    host.name = Some("integration test host".into());
    host.sudo_policy = SudoPolicy::Deny;
    host.sudo_password_source = SudoPasswordSource::ReuseLogin;
    host.shell_env_mode = ShellEnvMode::LoginThenTask;

    let auth = AuthMethod::Key {
        username,
        private_key_pem,
        passphrase: None,
    };

    (host, auth)
}

/// 等待并收集某个序号命令的输出，直到收到结束标记或超时。
async fn wait_for_command(
    rx: &mut tokio::sync::mpsc::Receiver<SessionOutput>,
    command_id: &str,
    timeout: Duration,
) -> (String, Option<i32>) {
    let mut output = String::new();
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return (output, None);
        }

        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(SessionOutput::Line { line, .. })) => {
                output.push_str(&line);
                output.push('\n');
            }
            Ok(Some(SessionOutput::Finished {
                command_id: done_id,
                exit_code,
            })) if done_id == command_id => {
                return (output, Some(exit_code));
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => return (output, None),
        }
    }
}

/// 密钥认证 + 建立会话 + 执行命令，并验证退出码回传。
#[tokio::test]
#[ignore = "需要真实 SSH 服务器；设置 MFPERCH_TEST_* 环境变量后以 --ignored 运行"]
async fn connect_and_execute_simple_command() {
    let (host, auth) = test_target();

    let (session, mut rx) = Session::connect("term_test", &host, auth, false)
        .await
        .expect("会话应能建立");

    session.send_command("c1", "echo hello-mfperch").await.unwrap();

    let (out, code) = wait_for_command(&mut rx, "c1", Duration::from_secs(10)).await;
    assert_eq!(code, Some(0), "退出码应为 0，实际 {code:?}");
    assert!(out.contains("hello-mfperch"), "输出应包含命令结果，实际：{out}");

    session.disconnect().await.ok();
}

/// 验证方案 C 的核心语义：**状态保留**（D3）。
///
/// 这是整个终端模型的关键前提——`cd` 与 `export` 必须跨命令生效。
#[tokio::test]
#[ignore = "需要真实 SSH 服务器；设置 MFPERCH_TEST_* 环境变量后以 --ignored 运行"]
async fn session_preserves_cwd_and_env() {
    let (host, auth) = test_target();

    let (session, mut rx) = Session::connect("term_state", &host, auth, false)
        .await
        .expect("会话应能建立");

    // 1) cd 到 /tmp
    session.send_command("c1", "cd /tmp").await.unwrap();
    let (_, code) = wait_for_command(&mut rx, "c1", Duration::from_secs(10)).await;
    assert_eq!(code, Some(0));

    // 2) pwd 应输出 /tmp —— 证明工作目录被保留
    session.send_command("c2", "pwd").await.unwrap();
    let (out, code) = wait_for_command(&mut rx, "c2", Duration::from_secs(10)).await;
    assert_eq!(code, Some(0));
    assert!(
        out.contains("/tmp"),
        "cd 状态应保留：pwd 应输出 /tmp，实际：{out}"
    );

    // 3) export 一个变量
    session.send_command("c3", "export MFPERCH_IT_VAR=preserved").await.unwrap();
    let (_, code) = wait_for_command(&mut rx, "c3", Duration::from_secs(10)).await;
    assert_eq!(code, Some(0));

    // 4) 读取该变量 —— 证明环境变量被保留
    session.send_command("c4", "echo \"var=$MFPERCH_IT_VAR\"").await.unwrap();
    let (out, code) = wait_for_command(&mut rx, "c4", Duration::from_secs(10)).await;
    assert_eq!(code, Some(0));
    assert!(
        out.contains("var=preserved"),
        "export 状态应保留，实际：{out}"
    );

    session.disconnect().await.ok();
}

/// 验证非零退出码能正确回传（Q4 要求返回状态码）。
#[tokio::test]
#[ignore = "需要真实 SSH 服务器；设置 MFPERCH_TEST_* 环境变量后以 --ignored 运行"]
async fn session_reports_nonzero_exit_code() {
    let (host, auth) = test_target();

    let (session, mut rx) = Session::connect("term_rc", &host, auth, false)
        .await
        .expect("会话应能建立");

    session.send_command("c1", "false").await.unwrap();
    let (_, code) = wait_for_command(&mut rx, "c1", Duration::from_secs(10)).await;
    assert_eq!(code, Some(1), "false 的退出码应为 1");

    // 会话应仍可用（串行执行未破坏状态机）。
    session.send_command("c2", "echo still-alive").await.unwrap();
    let (out, code) = wait_for_command(&mut rx, "c2", Duration::from_secs(10)).await;
    assert_eq!(code, Some(0));
    assert!(out.contains("still-alive"));

    session.disconnect().await.ok();
}

/// 验证复杂命令无需转义即可执行（NUL 分帧的价值，D3）。
#[tokio::test]
#[ignore = "需要真实 SSH 服务器；设置 MFPERCH_TEST_* 环境变量后以 --ignored 运行"]
async fn session_handles_quoting_and_special_chars() {
    let (host, auth) = test_target();

    let (session, mut rx) = Session::connect("term_quote", &host, auth, false)
        .await
        .expect("会话应能建立");

    // 含引号与 $ 的命令：若用行协议 + base64 需额外编码，NUL 分帧则原样可用。
    session
        .send_command("c1", r#"echo "quoted $HOME" && echo 'single'"#)
        .await
        .unwrap();
    let (out, code) = wait_for_command(&mut rx, "c1", Duration::from_secs(10)).await;
    assert_eq!(code, Some(0));
    assert!(out.contains("quoted /"), "$HOME 应被展开，实际：{out}");
    assert!(out.contains("single"), "单引号内容应输出，实际：{out}");

    session.disconnect().await.ok();
}

/// 主机密钥与记录不一致时，必须返回**独立**的错误码 `host_key_mismatch`（D10）。
///
/// 守的是一条**安全信号的可判读性**：密钥变更可能意味着中间人，Agent 必须
/// "停止并报告人类"，而不是当成普通连接失败去重试（`docs/mcp-tools.md` 的错误码表）。
/// 因此断言的是**错误码**，不是"连接失败了"——后者在实现退化成
/// `ssh_connect_failed` 时同样成立，区分不出对错实现（P3）。
#[tokio::test]
#[ignore = "需要真实 SSH 服务器；设置 MFPERCH_TEST_* 环境变量后以 --ignored 运行"]
async fn host_key_mismatch_has_its_own_error_code() {
    let (mut host, auth) = test_target();

    // 预置一把**合法但不是本机**的 ed25519 公钥，模拟"主机换了密钥"。
    // 该公钥非机密，只用于触发比对分支（与 `src/ssh/auth.rs` 单测里的测试密钥同源）。
    host.host_key = Some(
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAINiIsvulZy36SSkIjC1O05QrsH5BESNWTPFIDzkZ6CZt testB"
            .into(),
    );

    let err = match Session::connect("term_tofu", &host, auth, false).await {
        Ok(_) => panic!("主机密钥与记录不一致时必须拒绝连接，实际却连接成功"),
        Err(e) => e,
    };

    assert_eq!(
        err.code(),
        "host_key_mismatch",
        "密钥不一致必须是独立错误码（可能意味着中间人），实际错误：{err:?}"
    );
}

/// 验证命令输出中若出现与结束标记相似的文本，不会造成误判（nonce 防误判）。
#[tokio::test]
#[ignore = "需要真实 SSH 服务器；设置 MFPERCH_TEST_* 环境变量后以 --ignored 运行"]
async fn session_is_not_confused_by_marker_like_output() {
    let (host, auth) = test_target();

    let (session, mut rx) = Session::connect("term_spoof", &host, auth, false)
        .await
        .expect("会话应能建立");

    // 故意输出形似结束标记的内容（但 nonce 不同）。
    session
        .send_command("c1", "echo '__MF_PERCH_END__fake__1__0__'")
        .await
        .unwrap();

    let (out, code) = wait_for_command(&mut rx, "c1", Duration::from_secs(10)).await;
    assert_eq!(
        code,
        Some(0),
        "形似标记的输出不应被误判为结束标记"
    );
    assert!(
        out.contains("__MF_PERCH_END__fake"),
        "该文本应作为普通输出返回，实际：{out}"
    );

    session.disconnect().await.ok();
}
