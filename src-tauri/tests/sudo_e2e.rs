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
///
/// 超时必须**明确失败**：原先超时返回 `(String::new(), None)`，
/// 而调用方多用 `assert_ne!(code, Some(0))` 断言"提权未成功"——
/// 命令根本没结束（SSH 断了、队列卡住）时该断言同样通过，
/// 于是绿灯的理由与用例名不符。超时属于测试环境异常，不是被测行为。
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
    // 与 deny 用例同一标准：整行等于 "0" 才算拿到 root 的 uid。
    // 原先用 `output.contains('0')`——任何含字符 0 的输出都会通过，等于没验。
    assert!(
        output.trim().lines().any(|l| l.trim() == "0"),
        "sudo id -u 应输出 root 的 uid（整行为 0）。输出：{output}"
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
        output.trim().lines().any(|l| l.trim() == "0"),
        "应输出 root 的 uid（整行为 0）。输出：{output}"
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

/// **Q36 回归**：同一条命令内，人类只被问一次。
///
/// 背景：sudo 在密码错误时会重试（默认最多 3 次），`-A` 下每次重试都会重新
/// 调用 askpass，因此产生新的索要（§7.10 实测 3 次）。若每次索要都弹窗，
/// 人类要在几十秒内连点三次几乎相同的「拒绝」。
///
/// 本用例借**真实 sudo 的重试循环**验证两条不变式：
/// 1. 拒绝一经给出，同一条命令的后续索要自动沿用，不再询问人类；
/// 2. **下一条命令必须重新询问**——否则"拒绝"会退化成"该终端永久禁止提权"，
///    那是把 fail-closed 做成了 fail-broken。
#[tokio::test]
#[ignore = "需要真实 SSH 服务器；设置 MFPERCH_TEST_* 后以 --ignored 运行"]
async fn deny_is_asked_only_once_per_command() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let t = target();

    let (state, key) = test_state();
    let host_id = seed_host(&state, &key, &t, SudoPolicy::Ask).await;

    // 替身回调每被调用一次 = 人类被弹一次窗。
    let asked = Arc::new(AtomicUsize::new(0));
    let asked_for_asker = asked.clone();
    let asker: mf_perch_lib::terminal::SudoAsker = Arc::new(move |_req: SudoRequest| {
        asked_for_asker.fetch_add(1, Ordering::SeqCst);
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
    let asks_after_first = asked.load(Ordering::SeqCst);
    assert_eq!(
        asks_after_first, 1,
        "同一条命令内的 sudo 重试不得重复询问人类，实际询问 {asks_after_first} 次。输出：{output}"
    );

    // 记忆必须随命令结束而失效。
    let again = state
        .terminals
        .run_command(
            &state.db,
            &terminal.id,
            "sudo id -u",
            Some(Duration::from_secs(30)),
        )
        .await
        .expect("命令应能下发");
    let (out2, _) = collect(&state.terminals, &state.db, &again.command_id).await;

    let asks_total = asked.load(Ordering::SeqCst);
    assert_eq!(
        asks_total, 2,
        "新命令应重新询问人类（否则拒绝会变成永久禁止提权），实际累计询问 {asks_total} 次。\
         第二条命令输出：{out2}"
    );

    state
        .terminals
        .delete_terminal(&state.db, &terminal.id)
        .await
        .ok();
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

        // 先批准 B（写入密码），再拒绝 A——按设计意图让"密码写入"早于"空密码写入"。
        let _ = second.send(SudoDecision::Allow);
        // 这 300ms 只是为了让**旧实现**的错配稳定复现（它曾用一条会话级 FIFO 串答）。
        // 它不承担正确性：修复后每个请求各有自己的 FIFO 与令牌，两个应答以任何次序
        // 落地，结论都必须是"A 被拒、B 拿到 root"。因此这里不适合改成轮询——
        // 没有任何可观测信号表示"密码已写入"，而断言本身并不依赖这个次序。
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
    // 两者都是 SudoElevationFailed，只看类型会让"挂住到超时"也算通过
    // （本用例初版就因此假绿灯）。
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
