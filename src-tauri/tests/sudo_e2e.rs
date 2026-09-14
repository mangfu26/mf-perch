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

/// 读取测试目标。
///
/// **缺失即失败**（AGENTS.md §5.6）：这里直接 `panic!`，而不是返回 `Option`
/// 让调用方 `return` 跳过——静默跳过会让测试报告显示"通过"，
/// 而实际一条断言都没执行（绿灯假象）。需要跳过时请用 `#[ignore]` 表达。
fn target() -> Target {
    fn need(key: &str) -> String {
        std::env::var(key).unwrap_or_else(|_| {
            panic!("未设置环境变量 {key}；联调环境准备见 docs/design/test-environment.md")
        })
    }

    Target {
        address: need("MFPERCH_TEST_HOST"),
        port: need("MFPERCH_TEST_PORT")
            .parse()
            .expect("MFPERCH_TEST_PORT 应为端口号"),
        username: need("MFPERCH_TEST_USER"),
        key_pem: std::fs::read_to_string(need("MFPERCH_TEST_KEY"))
            .expect("读取测试私钥失败：确认 MFPERCH_TEST_KEY 指向可读文件"),
        sudo_password: need("MFPERCH_TEST_SUDO_PW"),
    }
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
    let t = target();

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
    let t = target();

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
    let t = target();

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

    // **关键回归**：拒绝必须让 askpass 的 read 得到**应答**而结束。
    // 若对该次索要不作任何回应，askpass 会一直阻塞在 read 上（直到 120 秒兜底
    // 超时），而命令串行执行，后续命令会被长时间拖住——这里验证队列仍可用。
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
    let t = target();

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

/// **V1 回归（严重）**：sudo 密码绝不能以普通文件形式落在远端。
///
/// 背景：曾经 setup 脚本先 `mkfifo`、后通配 `rm -f "$dir"/sudopw.fifo.*`，
/// 把刚建好的 FIFO 自己删掉；随后 `cat > fifo` 退化为"创建普通文件并写入"，
/// 密码明文落盘且同机可读。
///
/// 单纯断言"sudo 成功"**测不出**这个缺陷——普通文件同样支持写读。
/// 因此这里直接检查远端文件类型与残留：这是唯一能区分"真 FIFO"与
/// "普通文件"的判据。
#[tokio::test]
#[ignore = "需要真实 SSH 服务器；设置 MFPERCH_TEST_* 后以 --ignored 运行"]
async fn sudo_password_never_lands_in_a_regular_file() {
    let t = target();

    let (state, key) = test_state();
    let host_id = seed_host(&state, &key, &t, SudoPolicy::Auto).await;

    let terminal = state
        .terminals
        .open_terminal(&state.db, &key, &host_id, None)
        .await
        .expect("终端应能建立");

    // 1) 会话建立后：存在的 sudopw.fifo.* 必须**全是 FIFO**，不能有普通文件。
    //    用 `find -type f` 精确判定普通文件（`-p` 判 FIFO）。
    let check_cmd = r#"find "$HOME/.mf-perch" -maxdepth 1 -name 'sudopw.fifo.*' -type f 2>/dev/null | wc -l"#;
    let outcome = state
        .terminals
        .run_command(&state.db, &terminal.id, check_cmd, Some(Duration::from_secs(15)))
        .await
        .expect("命令应能下发");
    let (out, code) = collect(&state.terminals, &state.db, &outcome.command_id).await;
    assert_eq!(code, Some(0), "检查命令应成功：{out}");
    assert_eq!(
        out.trim(),
        "0",
        "setup 后不得存在普通文件形态的 sudopw.fifo.*（FIFO 被删会导致密码落盘）"
    );

    // 2) 真正触发一次密码注入。
    let outcome = state
        .terminals
        .run_command(&state.db, &terminal.id, "sudo id -u", Some(Duration::from_secs(30)))
        .await
        .expect("命令应能下发");
    let (out, code) = collect(&state.terminals, &state.db, &outcome.command_id).await;
    assert_eq!(code, Some(0), "auto 模式应成功提权：{out}");

    // 3) 注入后仍不得出现普通文件（密码只经 FIFO 内存传递）。
    let outcome = state
        .terminals
        .run_command(&state.db, &terminal.id, check_cmd, Some(Duration::from_secs(15)))
        .await
        .expect("命令应能下发");
    let (out, _) = collect(&state.terminals, &state.db, &outcome.command_id).await;
    assert_eq!(
        out.trim(),
        "0",
        "注入密码后不得留下普通文件（说明密码可能已明文落盘）"
    );

    // 4) 归档会话后，本会话的临时节点应被清理：
    //    不变式是"一个会话只留下自己那一套"。归档 t1 再开 t2，
    //    若 t1 的残留被清理，总数应与归档前相同（而不是累加）。
    let count_cmd = r#"find "$HOME/.mf-perch" -maxdepth 1 \( -name 'sudopw.fifo.*' -o -name 'askpass.*' \) 2>/dev/null | wc -l"#;
    let before = {
        let outcome = state
            .terminals
            .run_command(&state.db, &terminal.id, count_cmd, Some(Duration::from_secs(15)))
            .await
            .expect("命令应能下发");
        let (out, _) = collect(&state.terminals, &state.db, &outcome.command_id).await;
        out.trim().to_string()
    };

    state
        .terminals
        .archive_terminal(&state.db, &terminal.id)
        .await
        .expect("归档应成功");

    let t2 = state
        .terminals
        .open_terminal(&state.db, &key, &host_id, None)
        .await
        .expect("新终端应能建立");
    let outcome = state
        .terminals
        .run_command(&state.db, &t2.id, count_cmd, Some(Duration::from_secs(15)))
        .await
        .expect("命令应能下发");
    let (after, _) = collect(&state.terminals, &state.db, &outcome.command_id).await;
    assert_eq!(
        after.trim(),
        before,
        "归档后本会话临时节点应被清理；若残留则数量会增加（归档前 {before}，归档后 {}）",
        after.trim()
    );

    state.terminals.delete_terminal(&state.db, &t2.id).await.ok();
}

/// **B2 回归（错配）**：并发的 sudo 索要必须各自收到自己的应答。
///
/// 背景：曾用**一条会话级 FIFO** 承载所有索要。FIFO 上一次写入只会被
/// **其中任意一个**正在阻塞的读者取走，因此当两次索要同时在等，
/// 应答就可能错配——表现是"用户点了允许的那次失败、点了拒绝的那次反而提权"。
///
/// 编排要点（都是为了让"旧实现必然错、新实现必然对"，而不是碰运气）：
///
/// 1. 第 1 条 sudo 先起，1.5 秒后再起第 2 条 —— 保证索要顺序确定：
///    请求 #1 = 进程 A，请求 #2 = 进程 B；
/// 2. 先批准 **#2**（后弹出的对话框），隔 300ms 再拒绝 **#1** ——
///    于是"密码写入"发生在"空密码写入"之前。旧实现下密码会被
///    **最先阻塞**的读者（A，已拒绝）取走 → A 越权成为 root、B 反而失败；
/// 3. 之后到达的索要一律拒绝 —— sudo 在密码错误时可能重试 askpass，
///    重试会产生新的令牌与新索要，必须同样应答，否则该次 sudo 会挂住。
#[tokio::test]
#[ignore = "需要真实 SSH 服务器；设置 MFPERCH_TEST_* 后以 --ignored 运行"]
async fn concurrent_sudo_requests_do_not_cross_route() {
    let t = target();

    let (state, key) = test_state();
    let host_id = seed_host(&state, &key, &t, SudoPolicy::Ask).await;

    // asker → 协调器：按**到达顺序**把应答通道交给协调器。
    let (tx_req, mut rx_req) = tokio::sync::mpsc::unbounded_channel::<
        tokio::sync::oneshot::Sender<SudoDecision>,
    >();
    let asker: mf_perch_lib::terminal::SudoAsker = Arc::new(move |_req: SudoRequest| {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = tx_req.send(tx);
        rx
    });
    state.terminals.set_sudo_asker(asker).await;

    // 协调器：收集请求并观察"总共有几次索要"（诊断重试行为）。
    let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen_for_task = seen.clone();
    let coordinator = tokio::spawn(async move {
        // #1 = 进程 A（先启动）。
        let first = rx_req.recv().await.expect("应有第 1 次索要");
        seen_for_task.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        // #2 = 进程 B。
        let second = rx_req.recv().await.expect("应有第 2 次索要");
        seen_for_task.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        // 先批准 B（写入密码），让"密码写入"早于"空密码写入"。
        let _ = second.send(SudoDecision::Allow);
        tokio::time::sleep(Duration::from_millis(300)).await;
        // 再拒绝 A。
        let _ = first.send(SudoDecision::Deny);

        // 其余（sudo 重试产生的新索要）一律拒绝，避免有索要得不到应答而挂住。
        while let Some(tx) = rx_req.recv().await {
            seen_for_task.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let _ = tx.send(SudoDecision::Deny);
        }
    });

    let terminal = state
        .terminals
        .open_terminal(&state.db, &key, &host_id, None)
        .await
        .expect("终端应能建立");

    // 注意：只重定向 stdout。askpass 的**协议标记走 stderr**，
    // 若把子 shell 的 stderr 也重定向进文件，标记就到不了应用，索要不会被创建。
    let cmd = r#"rm -f /tmp/mfperch-b2-a.out /tmp/mfperch-b2-b.out
( sudo id -un > /tmp/mfperch-b2-a.out ) &
sleep 1.5
( sudo id -un > /tmp/mfperch-b2-b.out ) &
wait
printf 'A=%s\n' "$(cat /tmp/mfperch-b2-a.out)"
printf 'B=%s\n' "$(cat /tmp/mfperch-b2-b.out)"
"#;

    // 异步模式下发：命令会一直挂着等我们应答，因此**不能**用同步等待
    // （同步会阻塞到命令结束，而命令结束又依赖我们应答 → 自我死锁）。
    let outcome = state
        .terminals
        .run_command(&state.db, &terminal.id, cmd, None)
        .await
        .expect("命令应能下发");

    let (out, code) = collect(&state.terminals, &state.db, &outcome.command_id).await;
    coordinator.abort();

    eprintln!(
        "[B2 诊断] 本次共产生 {} 次索要；命令输出：{out}",
        seen.load(std::sync::atomic::Ordering::SeqCst)
    );
    assert_eq!(code, Some(0), "命令本身应正常结束：{out}");

    // 被拒绝的第 1 次索要：不得提权。旧实现下它会拿到本属于 B 的密码。
    assert!(
        !out.contains("A=root"),
        "被拒绝的索要不得提权（应答错配会让它拿到别人的密码）：{out}"
    );
    // 被允许的第 2 次索要：必须提权成功。旧实现下它会拿到空密码而失败。
    assert!(
        out.contains("B=root"),
        "被允许的索要必须提权成功（应答错配会让它拿到空密码）：{out}"
    );

    state
        .terminals
        .delete_terminal(&state.db, &terminal.id)
        .await
        .ok();
}
