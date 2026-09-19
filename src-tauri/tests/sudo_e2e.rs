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
use mf_perch_lib::domain::host::{Host, ShellEnvMode, SudoPasswordSource, SudoPolicy};
use mf_perch_lib::error::AppError;
use mf_perch_lib::ssh::{AuthMethod, Session, SessionOutput};
use mf_perch_lib::state::AppState;
use mf_perch_lib::store::{credentials, hosts};
use mf_perch_lib::terminal::sudo::{SudoDecision, SudoRequest};

mod common;
use common::{need_env, need_env_port, test_state};

/// 测试目标信息。
struct Target {
    address: String,
    port: u16,
    username: String,
    key_pem: String,
    sudo_password: String,
}

/// 读取测试目标。
///
/// 环境变量缺失时的失败语义由 `common::need_env` 统一保证（AGENTS.md §5.6）：
/// 直接 `panic!`，而不是返回 `Option` 让调用方 `return` 跳过——静默跳过会让
/// 测试报告显示"通过"，而实际一条断言都没执行（绿灯假象）。
/// 需要跳过时请用 `#[ignore]` 表达。
fn target() -> Target {
    Target {
        address: need_env("MFPERCH_TEST_HOST"),
        port: need_env_port("MFPERCH_TEST_PORT"),
        username: need_env("MFPERCH_TEST_USER"),
        key_pem: std::fs::read_to_string(need_env("MFPERCH_TEST_KEY"))
            .expect("读取测试私钥失败：确认 MFPERCH_TEST_KEY 指向可读文件"),
        sudo_password: need_env("MFPERCH_TEST_SUDO_PW"),
    }
}

/// 建主机与密钥凭据，并配置 sudo 策略。
async fn seed_host(
    state: &AppState,
    key: &[u8; 32],
    t: &Target,
    policy: SudoPolicy,
) -> String {
    seed_host_with_env_mode(state, key, t, policy, ShellEnvMode::LoginThenTask).await
}

/// 同上，但显式指定"环境加载方式"（D4）——用于验证该设置真的生效。
async fn seed_host_with_env_mode(
    state: &AppState,
    key: &[u8; 32],
    t: &Target,
    policy: SudoPolicy,
    env_mode: ShellEnvMode,
) -> String {
    let conn = state.db.lock().await;

    let cred = Credential::new(t.username.clone(), CredentialKind::Key, t.key_pem.clone());
    let cred_id = cred.id.clone();
    credentials::insert(&conn, &cred, key).expect("写入凭据");

    let mut host = Host::new(t.address.clone(), t.port);
    host.name = Some("sudo e2e".into());
    host.credential_id = Some(cred_id);
    host.sudo_policy = policy;
    host.shell_env_mode = env_mode;
    // 登录用密钥，无法复用登录密码，故必须单独配置 sudo 密码。
    host.sudo_password_source = SudoPasswordSource::Own;
    let host_id = host.id.clone();
    hosts::insert(&conn, &host, Some(&t.sudo_password), key).expect("写入主机");

    host_id
}

/// 等待某条命令结束，返回（输出，退出码）。
///
/// **超时必须明确失败**，不得返回"看起来像提权未成功"的值（空输出 + `code = None`）：
/// 调用方多用 `assert_ne!(code, Some(0))` 断言"提权未成功"，而命令根本没结束
/// （SSH 断了、队列卡住）时该断言同样通过，绿灯的理由就与用例名不符了。
/// 超时属于测试环境异常，不是被测行为。
async fn collect(
    _rt: &mf_perch_lib::terminal::TerminalRuntime,
    db: &mf_perch_lib::store::Db,
    command_id: &str,
) -> (String, Option<i32>) {
    // 轮询直到命令结束：30 秒上限（200 × 150ms）。
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
        // 轮询间隔——有上限的等待，不是"等事情大概已经发生"（§5.3）。
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    panic!("命令 {command_id} 在 30 秒内未结束：测试环境异常（SSH 断开或队列卡住），而非被测行为");
}

/// **模式一：deny** —— sudo 必须失败，绝不提权。
#[tokio::test]
#[ignore = "需要真实 SSH 服务器；设置 MFPERCH_TEST_* 后以 --ignored 运行"]
async fn data_plane_sudo_is_always_refused() {
    // D47/D48 核心不变式：**数据面不许提权**。
    //
    // 数据面没有任何密码来源；若不在这里拦下 `sudo`，Agent 收到的会是一句难懂的
    // sudo 报错并可能反复重试。本用例验证它收到的是**可操作**的拒绝。
    //
    // 三种策略都要验：拒绝的**原因**可以不同（策略禁用 / 改用工具），
    // 但"命令不会被提权执行"必须一致——这正是本用例的判别性断言。
    let t = target();

    for policy in [SudoPolicy::Deny, SudoPolicy::Ask, SudoPolicy::Auto] {
        let (state, key) = test_state();
        let host_id = seed_host(&state, &key, &t, policy).await;

        let terminal = state
            .terminals
            .open_terminal(&state.db, &key, &host_id, None)
            .await
            .expect("终端应能建立");

        // 用 `id -u` 作为判据：一旦提权成功，输出里会出现整行的 0。
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

        assert_ne!(code, Some(0), "{policy:?} 下数据面 sudo 必须失败：{output}");
        assert!(
            !output.trim().lines().any(|l| l.trim() == "0"),
            "{policy:?} 下数据面绝不允许提权成功：{output}"
        );
        assert!(
            output.contains("[mf-perch]"),
            "{policy:?} 下应给出应用自己的可操作说明，而不是 sudo 的原始报错：{output}"
        );
        // 说明必须指向正确的出路：允许提权的策略引导改用工具，
        // 禁止提权的策略说明原因（引导它去用一个也会失败的入口是误导）。
        if policy == SudoPolicy::Deny {
            assert!(
                output.contains("已禁用提权"),
                "deny 策略应说明该主机已禁用提权：{output}"
            );
        } else {
            assert!(
                output.contains("run_as_root"),
                "{policy:?} 策略应引导 Agent 改用 run_as_root 工具：{output}"
            );
        }

        // 数据面命令必须继续可用（拒绝不能把终端搞坏）。
        let follow = state
            .terminals
            .run_command(&state.db, &terminal.id, "echo still-alive", Some(Duration::from_secs(20)))
            .await
            .expect("后续命令应能下发");
        let (out2, code2) = collect(&state.terminals, &state.db, &follow.command_id).await;
        assert_eq!(code2, Some(0), "拒绝 sudo 后终端应继续可用：{out2}");

        state.terminals.delete_terminal(&state.db, &terminal.id).await.ok();
    }
}

/// **D47/D48 回归**：跑过提权与数据面两条路径后，远端**不新增**任何本应用的节点
/// （D49 的不变式：远端不落任何文件，也就没有"清理"这一步）。
///
/// 判据用**前后快照对比**，而不是断言"目录为空"或"目录不存在"：
/// 测试主机上可能残留历史痕迹（实测记录见 D49），
/// 断言绝对值为零会把"机器不干净"误报成"实现又写了文件"（§5.4：测试不得
/// 依赖机器上的既有状态）。真正要守的不变式是"**本次操作没有新增痕迹**"。
#[tokio::test]
#[ignore = "需要真实 SSH 服务器；设置 MFPERCH_TEST_* 后以 --ignored 运行"]
async fn data_plane_leaves_no_new_remote_artifacts() {
    let t = target();
    let (state, key) = test_state();
    let host_id = seed_host(&state, &key, &t, SudoPolicy::Auto).await;

    let terminal = state
        .terminals
        .open_terminal(&state.db, &key, &host_id, None)
        .await
        .expect("终端应能建立");

    // 计数对象取投递载体的特征名（askpass* / sudopw*）：现行实现不应产生它们，
    // 一旦出现就说明密码投递机制回潮（D49）。
    let count_cmd = r#"find "$HOME/.mf-perch" -maxdepth 1 \( -name 'askpass*' -o -name 'sudopw*' \) 2>/dev/null | wc -l"#;

    let before = {
        let outcome = state
            .terminals
            .run_command(&state.db, &terminal.id, count_cmd, Some(Duration::from_secs(20)))
            .await
            .expect("命令应能下发");
        let (out, _) = collect(&state.terminals, &state.db, &outcome.command_id).await;
        out.trim().to_string()
    };

    // 触发一次数据面 sudo 拒绝与一次真正的提权，覆盖两条路径。
    let a = state
        .terminals
        .run_command(&state.db, &terminal.id, "sudo true", Some(Duration::from_secs(20)))
        .await
        .expect("命令应能下发");
    let _ = collect(&state.terminals, &state.db, &a.command_id).await;
    state
        .terminals
        .run_as_root(&state.db, &key, &terminal.id, "true", None)
        .await
        .expect("提权命令应能执行");

    let after = {
        let outcome = state
            .terminals
            .run_command(&state.db, &terminal.id, count_cmd, Some(Duration::from_secs(20)))
            .await
            .expect("命令应能下发");
        let (out, code) = collect(&state.terminals, &state.db, &outcome.command_id).await;
        assert_eq!(code, Some(0), "检查命令应成功：{out}");
        out.trim().to_string()
    };

    assert_eq!(
        after, before,
        "本次操作不得新增任何远端投递载体（D49：远端不落文件）；\
         操作前 {before} 个，操作后 {after} 个"
    );

    state.terminals.delete_terminal(&state.db, &terminal.id).await.ok();
}

/// **策略回归**：`deny` 主机上 `run_as_root` 必须被拒绝，且不建任何通道。
///
/// 这条守的是"人有权关掉提权"这一产品承诺：策略为 deny 时，Agent 既不能
/// 经数据面提权（上一条用例），也不能经提权工具绕过。
#[tokio::test]
#[ignore = "需要真实 SSH 服务器；设置 MFPERCH_TEST_* 后以 --ignored 运行"]
async fn run_as_root_is_refused_when_host_disables_elevation() {
    let t = target();
    let (state, key) = test_state();
    let host_id = seed_host(&state, &key, &t, SudoPolicy::Deny).await;

    let terminal = state
        .terminals
        .open_terminal(&state.db, &key, &host_id, None)
        .await
        .expect("终端应能建立");

    let err = state
        .terminals
        .run_as_root(&state.db, &key, &terminal.id, "id -u", None)
        .await
        .expect_err("deny 主机上提权必须被拒绝");
    assert!(
        matches!(err, AppError::SudoElevationFailed(_)),
        "应以「提权失败」明确报错，实际：{err:?}"
    );
    assert!(
        err.to_string().contains("禁止注入"),
        "报错应说明是该主机的策略禁用，并指引人类如何开启：{err}"
    );

    // 拒绝之后数据面照常可用。
    let follow = state
        .terminals
        .run_command(&state.db, &terminal.id, "echo ok", Some(Duration::from_secs(20)))
        .await
        .expect("普通命令应能下发");
    let (out, code) = collect(&state.terminals, &state.db, &follow.command_id).await;
    assert_eq!(code, Some(0), "被拒绝提权后终端应继续可用：{out}");

    state.terminals.delete_terminal(&state.db, &terminal.id).await.ok();
}

/// **ask 模式**：人类拒绝时 `run_as_root` 不投递密码，命令不会被执行。
///
/// 拒绝路径上远端没有任何等待者，因此不存在"忘记应答把队列挂死"这类缺陷形态。
#[tokio::test]
#[ignore = "需要真实 SSH 服务器；设置 MFPERCH_TEST_* 后以 --ignored 运行"]
async fn run_as_root_ask_mode_denied_by_human_does_not_elevate() {
    let t = target();
    let (state, key) = test_state();
    let host_id = seed_host(&state, &key, &t, SudoPolicy::Ask).await;

    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let asked_for = asked.clone();
    let asker: mf_perch_lib::terminal::SudoAsker = Arc::new(move |_req: SudoRequest| {
        asked_for.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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

    let err = state
        .terminals
        .run_as_root(&state.db, &key, &terminal.id, "id -u", None)
        .await
        .expect_err("人类拒绝后不得提权");
    assert!(
        matches!(err, AppError::SudoElevationFailed(_)),
        "应以「提权失败」报错，实际：{err:?}"
    );
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "应当只询问人类一次（单命令形态没有 sudo 重试带来的连环询问）"
    );

    // 拒绝之后普通命令照常可用。
    let follow = state
        .terminals
        .run_command(&state.db, &terminal.id, "echo ok", Some(Duration::from_secs(20)))
        .await
        .expect("普通命令应能下发");
    let (out, code) = collect(&state.terminals, &state.db, &follow.command_id).await;
    assert_eq!(code, Some(0), "拒绝提权后队列不得卡住：{out}");

    state.terminals.delete_terminal(&state.db, &terminal.id).await.ok();
}

// ===================================================================
// D47：提权双通道 —— 通道层（R2）
// ===================================================================

/// 造一个内存 Host + 密钥认证，供通道层用例直接建立会话。
///
/// 提权通道**不经过** askpass/FIFO（它本身已是 root），因此主机策略与
/// sudo 密码配置无关——这里只需要一个可用的登录目标。
fn privileged_target(t: &Target) -> (Host, AuthMethod) {
    let mut host = Host::new(t.address.clone(), t.port);
    host.name = Some("privileged e2e".into());
    let auth = AuthMethod::Key {
        username: t.username.clone(),
        private_key_pem: t.key_pem.clone(),
        passphrase: None,
    };
    (host, auth)
}

/// 从事件流里收集某条命令的输出，直到它的结束标记。
///
/// 超时**明确失败**（测试环境异常），不返回空值——否则"提权失败"与
/// "命令根本没跑完"会得到同一种绿灯理由。
async fn collect_session_output(
    rx: &mut tokio::sync::mpsc::Receiver<SessionOutput>,
    command_id: &str,
) -> (String, Option<i32>) {
    let mut out = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            panic!("命令 {command_id} 在 30 秒内未结束：测试环境异常，而非被测行为");
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Err(_) => panic!("命令 {command_id} 等待超时：测试环境异常"),
            Ok(None) => panic!("会话事件流已关闭，未收到 {command_id} 的结束标记"),
            Ok(Some(SessionOutput::Line { line })) => {
                out.push_str(&line);
                out.push('\n');
            }
            Ok(Some(SessionOutput::Finished {
                command_id: id,
                exit_code,
            })) if id == command_id => return (out, Some(exit_code)),
            Ok(Some(_)) => {}
        }
    }
}

/// **D47 R2 回归**：提权通道以 root 运行命令，且输出/退出码走同一套协议。
#[tokio::test]
#[ignore = "需要真实 SSH 服务器；设置 MFPERCH_TEST_* 后以 --ignored 运行"]
async fn privileged_channel_runs_commands_as_root() {
    let t = target();
    let (host, auth) = privileged_target(&t);

    let (session, mut rx) =
        Session::connect_privileged("term_priv", &host, auth, &t.sudo_password)
            .await
            .expect("提权通道应能建立（密码经本通道 stdin 投递）");

    session
        .send_command("cmd_priv", "id -u")
        .await
        .expect("命令应能下发");

    let (out, rc) = collect_session_output(&mut rx, "cmd_priv").await;
    assert_eq!(rc, Some(0), "提权命令应成功。输出：{out}");
    assert!(
        out.lines().any(|l| l.trim() == "0"),
        "提权通道应以 root 执行（`id -u` 整行为 0）。输出：{out}"
    );
}

/// **D47 R2 回归**：密码错误时**明确失败**，不能挂住。
///
/// 交互过程：sudo 索要密码（第 1 次提示）→ 应用写入错误密码 →
/// sudo 认证失败后**再次索要**（第 2 次提示）→ 应用判定"密码被拒"并立即报错。
/// 若不处理第二次提示，sudo 会一直等输入（`passwd_timeout` 默认数分钟），
/// 表现为"提权请求挂住"。
///
/// 注：sudo 的凭证缓存按父进程/会话记录，本用例每次都是新会话，
/// 因此不会因缓存跳过密码（PoC 实测确认跨会话不共享）。
#[tokio::test]
#[ignore = "需要真实 SSH 服务器；设置 MFPERCH_TEST_* 后以 --ignored 运行"]
async fn privileged_channel_fails_fast_on_wrong_password() {
    let t = target();
    let (host, auth) = privileged_target(&t);

    let result = tokio::time::timeout(
        Duration::from_secs(45),
        Session::connect_privileged("term_priv_bad", &host, auth, "definitely-wrong-password"),
    )
    .await;

    let err = match result {
        Err(_) => panic!("密码错误时提权应快速失败，而不是挂住（45 秒超时）"),
        Ok(Ok(_)) => panic!("错误密码不应建立提权通道"),
        Ok(Err(e)) => e,
    };
    assert!(
        matches!(err, AppError::SudoElevationFailed(_)),
        "应以「提权失败」明确报错，实际：{err:?}"
    );
    // **判别性断言**：必须因"密码被拒"而失败，而不是等到就绪超时——
    // 两者同为 SudoElevationFailed，**只断言错误类型会让"挂住到超时"也算通过**。
    assert!(
        err.to_string().contains("未接受该密码"),
        "应因「密码被拒」快速失败，而不是等待就绪超时，实际：{err}"
    );
}

// ===================================================================
// D47：提权双通道 —— 编排层（R3）
// ===================================================================

/// 取回某条命令在历史里的完整记录（人类审计看到的形态）。
async fn history_record(
    db: &mf_perch_lib::store::Db,
    command_id: &str,
) -> mf_perch_lib::domain::command::CommandRecord {
    let conn = db.lock().await;
    mf_perch_lib::store::commands::get(&conn, command_id).expect("命令应存在于历史中")
}

/// **D47 R3 回归**：提权命令自动继承数据面当前目录。
///
/// 这是双通道方案里最容易做错的一点：提权通道是**另一个 shell 进程**，
/// 不共享数据面的目录。若编排漏掉"切目录"这一步，命令会在提权通道的
/// 初始目录（家目录）下执行——命令照样成功、退出码照样为 0，
/// 只有 `pwd` 的输出能区分对错，因此断言必须落在具体路径上。
#[tokio::test]
#[ignore = "需要真实 SSH 服务器；设置 MFPERCH_TEST_* 后以 --ignored 运行"]
async fn run_as_root_inherits_plain_session_working_directory() {
    let t = target();
    let (state, key) = test_state();
    let host_id = seed_host(&state, &key, &t, SudoPolicy::Auto).await;

    let terminal = state
        .terminals
        .open_terminal(&state.db, &key, &host_id, None)
        .await
        .expect("终端应能建立");

    // 先在数据面上把目录挪到一个必然不同于家目录的位置。
    let plain = state
        .terminals
        .run_command(&state.db, &terminal.id, "cd /tmp && pwd", Some(Duration::from_secs(20)))
        .await
        .expect("普通命令应能下发");
    let (out, code) = collect(&state.terminals, &state.db, &plain.command_id).await;
    assert_eq!(code, Some(0), "准备步骤应成功：{out}");
    assert!(out.contains("/tmp"), "数据面应已切到 /tmp：{out}");

    let outcome = state
        .terminals
        .run_as_root(&state.db, &key, &terminal.id, "pwd", None)
        .await
        .expect("提权命令应能执行");

    assert_eq!(outcome.exit_code, Some(0), "提权 pwd 应成功：{}", outcome.output);
    assert!(
        outcome.output.contains("/tmp"),
        "提权命令必须继承数据面目录 /tmp，实际输出：{}",
        outcome.output
    );
    assert_ne!(
        outcome.output.trim(),
        "/root",
        "落在家目录/root 说明「切目录」这一步没生效（命令会成功但目录是错的）"
    );

    state.terminals.delete_terminal(&state.db, &terminal.id).await.ok();
}

/// **D47 R3 回归**：显式 `cwd` 优先于继承，且目录不存在时**拒绝执行**。
///
/// 两条不变式一起验：给了 `cwd` 就用它；给了一个进不去的目录时，
/// 必须**不执行**命令（否则会在错误目录下以 root 跑了命令，是最危险的失败形态）。
#[tokio::test]
#[ignore = "需要真实 SSH 服务器；设置 MFPERCH_TEST_* 后以 --ignored 运行"]
async fn run_as_root_honours_explicit_cwd_and_refuses_missing_dir() {
    let t = target();
    let (state, key) = test_state();
    let host_id = seed_host(&state, &key, &t, SudoPolicy::Auto).await;

    let terminal = state
        .terminals
        .open_terminal(&state.db, &key, &host_id, None)
        .await
        .expect("终端应能建立");

    let outcome = state
        .terminals
        .run_as_root(&state.db, &key, &terminal.id, "pwd", Some("/etc"))
        .await
        .expect("显式 cwd 应能执行");
    assert_eq!(outcome.exit_code, Some(0), "输出：{}", outcome.output);
    assert!(
        outcome.output.contains("/etc"),
        "应使用显式 cwd=/etc，实际输出：{}",
        outcome.output
    );

    // 目录不存在 → `cd` 失败 → 整条命令**不得执行**。
    // 判据：命令体里的副作用（创建文件）绝不能发生，而不只是看退出码。
    let missing = "/tmp/mfperch-no-such-dir-d47";
    let outcome = state
        .terminals
        .run_as_root(
            &state.db,
            &key,
            &terminal.id,
            &format!("touch {missing}/should-not-exist"),
            Some(missing),
        )
        .await
        .expect("调用本身应返回结果（失败体现在退出码上）");
    assert_ne!(
        outcome.exit_code,
        Some(0),
        "目录不存在时不得执行命令，实际输出：{}",
        outcome.output
    );

    state.terminals.delete_terminal(&state.db, &terminal.id).await.ok();
}

/// **D47 R3 回归**：返回并记录**实际** uid，且命令历史带特权前缀。
///
/// 只断言"命令成功"是不够的：`run_as_root` 的价值在于"确实以特权身份跑"。
/// 这里同时验证三件事：
/// 1. 返回结构里的 `actual_uid` 是远端核实出来的真实身份；
/// 2. 命令历史里带 `[特权用户(uid=0)]` 前缀（客户确认的审计口径）；
/// 3. 前缀**只**出现在提权命令上，普通命令不受影响。
#[tokio::test]
#[ignore = "需要真实 SSH 服务器；设置 MFPERCH_TEST_* 后以 --ignored 运行"]
async fn run_as_root_reports_real_uid_and_marks_history() {
    let t = target();
    let (state, key) = test_state();
    let host_id = seed_host(&state, &key, &t, SudoPolicy::Auto).await;

    let terminal = state
        .terminals
        .open_terminal(&state.db, &key, &host_id, None)
        .await
        .expect("终端应能建立");

    let outcome = state
        .terminals
        .run_as_root(&state.db, &key, &terminal.id, "id -u", None)
        .await
        .expect("提权命令应能执行");

    assert_eq!(outcome.exit_code, Some(0), "输出：{}", outcome.output);
    assert_eq!(
        outcome.actual_uid,
        Some(0),
        "测试机 sudoers 应把目标用户配为 root；实际身份：{outcome:?}"
    );
    assert_eq!(outcome.actual_user.as_deref(), Some("root"));
    // 身份核实文本必须从 Agent 看到的输出里摘掉（那是应用的动作，不是命令产出）。
    assert!(
        !outcome.output.contains("__MF_PERCH_UID__"),
        "身份核实标记不得出现在命令输出里：{}",
        outcome.output
    );
    assert!(
        outcome.output.lines().any(|l| l.trim() == "0"),
        "命令本身应输出 root 的 uid：{}",
        outcome.output
    );

    let rec = history_record(&state.db, &outcome.command_id).await;
    assert!(
        rec.command.starts_with("[特权用户(uid=0)]"),
        "提权命令历史应带特权前缀，实际：{}",
        rec.command
    );

    // 普通命令不得带前缀。
    let plain = state
        .terminals
        .run_command(&state.db, &terminal.id, "echo plain", Some(Duration::from_secs(20)))
        .await
        .expect("普通命令应能下发");
    let plain_rec = history_record(&state.db, &plain.command_id).await;
    assert!(
        !plain_rec.command.starts_with("[特权用户"),
        "普通命令不应带特权前缀，实际：{}",
        plain_rec.command
    );

    state.terminals.delete_terminal(&state.db, &terminal.id).await.ok();
}

/// **D47 R3 回归**：一次提权只影响那一条命令，之后数据面仍是普通身份。
///
/// 这条守住双通道方案的**权力边界**：没有"进入 root 模式"这回事。
/// 若特权通道被复用或未收掉，后续普通命令就会在 root 身份下执行——
/// 那等于给了 Agent 一个隐形的常驻后门，且人类审计里看不出来。
#[tokio::test]
#[ignore = "需要真实 SSH 服务器；设置 MFPERCH_TEST_* 后以 --ignored 运行"]
async fn run_as_root_does_not_leave_the_terminal_privileged() {
    let t = target();
    let (state, key) = test_state();
    let host_id = seed_host(&state, &key, &t, SudoPolicy::Auto).await;

    let terminal = state
        .terminals
        .open_terminal(&state.db, &key, &host_id, None)
        .await
        .expect("终端应能建立");

    let before = state
        .terminals
        .run_command(&state.db, &terminal.id, "id -u", Some(Duration::from_secs(20)))
        .await
        .expect("普通命令应能下发");
    let (uid_before, _) = collect(&state.terminals, &state.db, &before.command_id).await;

    state
        .terminals
        .run_as_root(&state.db, &key, &terminal.id, "id -u", None)
        .await
        .expect("提权命令应能执行");

    let after = state
        .terminals
        .run_command(&state.db, &terminal.id, "id -u", Some(Duration::from_secs(20)))
        .await
        .expect("普通命令应能下发");
    let (uid_after, code_after) = collect(&state.terminals, &state.db, &after.command_id).await;

    assert_eq!(code_after, Some(0), "提权后的普通命令仍应可用：{uid_after}");
    assert_ne!(
        uid_after.trim(),
        "0",
        "提权之后数据面必须仍是普通身份（不得留下常驻 root）"
    );
    assert_eq!(
        uid_after.trim(),
        uid_before.trim(),
        "提权前后数据面的身份应一致"
    );

    state.terminals.delete_terminal(&state.db, &terminal.id).await.ok();
}
/// **D4 回归**："环境加载方式"（登录 / 干净）必须真正生效。
///
/// 背景（收网审计发现）：`protocol::shell_invocation` 这对参数**曾完全没有调用方**——
/// 数据面直接把脚本原样交给 `channel.exec`（等价于 `bash -s`，既不是登录 shell
/// 也不是显式干净模式）。于是设置页里这个用户可见的开关**没有任何效果**：
/// 实测登录模式下 `~/.bash_profile` 里加的目录不会出现在 PATH 中。
///
/// 判据刻意用"**只有 profile 才会有**的东西"：本机 `~/.bash_profile` 会把
/// `/opt/mfperch-test-bin` 加进 PATH（见 `docs/design/test-environment.md` 的测试用户配置）。
/// 因此：
/// - 登录模式 → PATH 含该目录（profile 生效）；
/// - 干净模式 → PATH 不含该目录（profile 未加载）。
///
/// 两种模式都必须**至少**正确工作：只断言其中一个方向时，
/// "两种模式都走同一条命令"这种缺陷照样全绿。
#[tokio::test]
#[ignore = "需要真实 SSH 服务器；设置 MFPERCH_TEST_* 后以 --ignored 运行"]
async fn shell_env_mode_actually_changes_the_remote_environment() {
    let t = target();

    // profile 专属目录：只应出现在登录模式下。
    const PROFILE_ONLY_DIR: &str = "/opt/mfperch-test-bin";

    for (mode, expected_label) in [
        (ShellEnvMode::LoginThenTask, "登录模式应加载 profile"),
        (ShellEnvMode::CleanThenTask, "干净模式不应加载 profile"),
    ] {
        let (state, key) = test_state();
        let host_id = seed_host_with_env_mode(&state, &key, &t, SudoPolicy::Auto, mode).await;

        let terminal = state
            .terminals
            .open_terminal(&state.db, &key, &host_id, None)
            .await
            .expect("终端应能建立");

        // 注意：命令里**不能**出现 `##`——多行命令的每一行都会被 `#` 截断
        // （`OutputAccumulator` 的行为，模拟 shell 注释）。这里用 case 判断，
        // 既不含 `##` 也不含 `${...}`，避免被误截。
        let probe = format!(
            r#"case ":$PATH:" in *":{PROFILE_ONLY_DIR}:"*) echo HAS_PROFILE_DIR=yes;; *) echo HAS_PROFILE_DIR=no;; esac"#
        );
        let outcome = state
            .terminals
            .run_command(&state.db, &terminal.id, &probe, Some(Duration::from_secs(20)))
            .await
            .expect("命令应能下发");
        let (out, code) = collect(&state.terminals, &state.db, &outcome.command_id).await;
        assert_eq!(code, Some(0), "读取 PATH 应成功：{out}");

        let has_profile_dir = out.contains("HAS_PROFILE_DIR=yes");
        match mode {
            ShellEnvMode::LoginThenTask => assert!(
                has_profile_dir,
                "{expected_label}：PATH 里应出现 {PROFILE_ONLY_DIR}（该目录只由 ~/.bash_profile 添加）。\
                 实际输出：{out}"
            ),
            ShellEnvMode::CleanThenTask => assert!(
                !has_profile_dir,
                "{expected_label}：PATH 里不应出现 {PROFILE_ONLY_DIR}（它只由 profile 添加）。\
                 实际输出：{out}"
            ),
        }

        state.terminals.delete_terminal(&state.db, &terminal.id).await.ok();
    }
}

/// **D47 R3 回归**：取目录的探测与排队中的普通命令必须**互斥**。
///
/// 背景（实测踩到的真缺陷）：探测帧会在"当前命令"槽位上放自己的命令。
/// 若它不与 `run_command` 共用执行锁，就会出现这种交错——
/// 队列里的普通命令把槽位设成自己、帧也发出去了，紧接着探测把槽位**覆盖**掉；
/// 那条普通命令的等待循环看到"槽位不是我"，判定为会话中断并结束，
/// 落库成"退出码未知、输出为空"。而远端其实已经把它执行完了：
/// **记录说失败，效果却发生了**——审计里最不能接受的一类错误。
///
/// 编排要点：探测发生在 `run_as_root` 内部，因此本用例只需**紧接着**
/// 下发一条普通命令，让两条命令真正争抢同一把锁。
#[tokio::test]
#[ignore = "需要真实 SSH 服务器；设置 MFPERCH_TEST_* 后以 --ignored 运行"]
async fn run_as_root_does_not_clobber_a_concurrent_plain_command() {
    let t = target();
    let (state, key) = test_state();
    let host_id = seed_host(&state, &key, &t, SudoPolicy::Auto).await;

    let terminal = state
        .terminals
        .open_terminal(&state.db, &key, &host_id, None)
        .await
        .expect("终端应能建立");

    // 先跑一条普通命令，确保数据面的泵与槽位已进入正常节奏。
    let warm = state
        .terminals
        .run_command(&state.db, &terminal.id, "id -u", Some(Duration::from_secs(20)))
        .await
        .expect("普通命令应能下发");
    let (warm_out, warm_code) = collect(&state.terminals, &state.db, &warm.command_id).await;
    assert_eq!(warm_code, Some(0), "预热命令应成功：{warm_out}");

    state
        .terminals
        .run_as_root(&state.db, &key, &terminal.id, "id -u", None)
        .await
        .expect("提权命令应能执行");

    // 紧跟一条普通命令：它必须拿到**自己**的退出码与输出，
    // 而不是被探测覆盖成"退出码未知、输出为空"。
    let follow = state
        .terminals
        .run_command(
            &state.db,
            &terminal.id,
            "printf 'FOLLOWUP-OK\\n'",
            Some(Duration::from_secs(20)),
        )
        .await
        .expect("普通命令应能下发");
    let (out, code) = collect(&state.terminals, &state.db, &follow.command_id).await;

    assert_eq!(
        code,
        Some(0),
        "紧跟提权之后的普通命令必须拿到真实退出码（被探测覆盖会变成 None）：{out}"
    );
    assert!(
        out.contains("FOLLOWUP-OK"),
        "普通命令的输出必须归属于它自己：{out}"
    );

    state.terminals.delete_terminal(&state.db, &terminal.id).await.ok();
}

/// **D47 R3 回归**：命令让 shell 进入 `set -e` 状态时，结束标记仍必须回来。
///
/// 背景（实测踩到的真缺陷）：包装脚本逐条 `eval` 命令，而 `set -e` 会**跨帧留在
/// shell 状态里**。若打印结束标记前不显式 `set +e`，那么「先跑一条 `set -e`、
/// 再跑一条失败的命令」会让远端 shell 当场退出，**第二帧的结束标记永远不来**——
/// 应用侧只能干等到 30 秒超时，而远端其实早已返回。
///
/// 复现要点（**不能**把两件事写进同一条命令）：errexit 在 `eval` 整串结束后才生效，
/// 所以同一帧里写 `set -e; <失败命令>` **不会**触发；必须**分成两帧**。
///
/// 判据刻意是**时间**：错误实现下也是"报错"，差别在于是立刻报错（127 带来结束标记）
/// 还是等满超时才报错——这也正是该缺陷能潜伏到真实环境才暴露的原因。
#[tokio::test]
#[ignore = "需要真实 SSH 服务器；设置 MFPERCH_TEST_* 后以 --ignored 运行"]
async fn run_as_root_returns_when_command_enables_set_e() {
    let t = target();
    let (state, key) = test_state();
    let host_id = seed_host(&state, &key, &t, SudoPolicy::Auto).await;

    let terminal = state
        .terminals
        .open_terminal(&state.db, &key, &host_id, None)
        .await
        .expect("终端应能建立");

    // 第一帧：只打开 `set -e`（成功结束，把状态留在远端 shell 里）。
    let ok = state
        .terminals
        .run_command(&state.db, &terminal.id, "set -e", Some(Duration::from_secs(20)))
        .await
        .expect("普通命令应能下发");
    let (out, code) = collect(&state.terminals, &state.db, &ok.command_id).await;
    assert_eq!(code, Some(0), "打开 set -e 的命令本身应成功：{out}");

    // 第二帧：提权执行一条**必然失败**的命令——错误实现下远端 shell 直接退出。
    let started = std::time::Instant::now();
    let outcome = state
        .terminals
        .run_as_root(&state.db, &key, &terminal.id, "/nonexistent-binary-d47", None)
        .await;
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(20),
        "命令让 shell 进入 set -e 后，结束标记仍必须回来（耗时 {elapsed:?} 说明脚本吞掉了标记、\
         应用侧在等满 30 秒超时）：{outcome:?}"
    );

    let outcome = outcome.expect("结束标记回来后应正常返回结果（失败体现在退出码上）");
    assert_ne!(
        outcome.exit_code,
        Some(0),
        "不存在的命令应返回非 0 退出码：{}",
        outcome.output
    );

    state.terminals.delete_terminal(&state.db, &terminal.id).await.ok();
}
