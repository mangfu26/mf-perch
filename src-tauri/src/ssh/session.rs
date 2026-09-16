//! 基于 russh 的终端会话（D3 / D8 / D9）。
//!
//! 一个会话对应一条常驻 SSH channel，远端运行包装脚本（[`protocol::wrapper_script`]），
//! 通过 NUL 分帧下发命令、以结束标记切分输出。
//!
//! 状态保留由远端脚本的 `eval` 在同一 shell 进程内实现，
//! 本模块负责连接、认证、帧收发与事件解析。

use std::sync::Arc;
use std::time::Duration;

use russh::client::{self, Handle};
use russh::ChannelMsg;
use tokio::sync::mpsc;

use crate::domain::host::{Host, ShellEnvMode};
use crate::domain::terminal::EnvSnapshot;
use crate::error::{AppError, Result};
use crate::ssh::auth::{AuthMethod, TofuHandler};
use crate::ssh::protocol::{self, SessionEvent};

/// 连接超时。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// 会话就绪等待超时（远端需要执行 profile 与初始化脚本）。
const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// 会话事件流：由后台任务推送，供上层（终端管理器 / 人类界面）消费。
#[derive(Debug, Clone)]
pub enum SessionOutput {
    /// 命令输出的一行（已剔除协议标记行）。
    Line { line: String },
    /// 某条命令执行结束；按 `command_id` 精确归属（V5）。
    Finished { command_id: String, exit_code: i32 },
    /// sudo 正在索要密码（Q33）。
    ///
    /// `token` 为该次索要的远端 askpass PID，用于定位唯一的应答 FIFO（B2）。
    SudoRequest { token: String },
    /// 提权通道上 sudo 正在**询问密码**（D47 握手）。
    ///
    /// 上层据此把密码写进**该通道的 stdin**；每条通道最多写一次
    /// （见 [`crate::ssh::protocol::SudoAuthHandshake`]）。
    SudoPrompt,
    /// 连接已断开。
    Disconnected { reason: String },
}

/// 一次已建立的终端会话。
pub struct Session {
    /// SSH 句柄。持有它即可保持连接；`disconnect` 显式关闭。
    handle: Handle<TofuHandler>,
    /// 远端 channel 的写入半部。
    writer: russh::ChannelWriteHalf<client::Msg>,
    /// 会话随机串，用于配对结束标记（防误判）。
    nonce: String,
    /// 本会话的终端 ID（便于日志与事件归属）。
    terminal_id: String,
    /// 首次连接时捕获的主机密钥（TOFU），供上层持久化（D10）。
    captured_host_key: Option<(String, String)>,
    /// 会话建立时捕获的环境快照（D4）。
    env_snapshot: Option<EnvSnapshot>,
}

impl Session {
    /// 建立会话：连接、认证（TOFU 校验）、启动包装脚本、等待就绪。
    ///
    /// `sudo_enabled` 决定是否部署 askpass 与 FIFO——`deny` 模式下不部署，
    /// 使 sudo 因无密码而失败（fail-closed，Q33 模式一）。
    pub async fn connect(
        terminal_id: impl Into<String>,
        host: &Host,
        auth: AuthMethod,
        sudo_enabled: bool,
    ) -> Result<(Self, mpsc::Receiver<SessionOutput>)> {
        Self::connect_inner(terminal_id, host, auth, sudo_enabled, None).await
    }

    /// 建立**提权通道**（D47）：用 `sudo -S` 以 root 身份运行**同一套包装脚本**。
    ///
    /// `password` 只在 sudo **确实索要密码时**才写进本通道的 stdin，
    /// 判定由 [`protocol::SudoAuthHandshake`] 负责：看到提示标记才写，
    /// **每条通道最多写一次**；凭证缓存有效时一个字都不写。
    ///
    /// 提权通道**不部署** askpass 与 FIFO（它本身已是 root，无需拦截），
    /// 也不做环境快照——因此比数据面少一次 setup 往返。
    pub async fn connect_privileged(
        terminal_id: impl Into<String>,
        host: &Host,
        auth: AuthMethod,
        password: &str,
    ) -> Result<(Self, mpsc::Receiver<SessionOutput>)> {
        Self::connect_inner(terminal_id, host, auth, false, Some(password)).await
    }

    async fn connect_inner(
        terminal_id: impl Into<String>,
        host: &Host,
        auth: AuthMethod,
        sudo_enabled: bool,
        privileged_password: Option<&str>,
    ) -> Result<(Self, mpsc::Receiver<SessionOutput>)> {
        let terminal_id = terminal_id.into();
        let nonce = protocol::new_nonce();
        let privileged = privileged_password.is_some();

        /// 会话未就绪时的报错：提权通道与数据面的原因不同，提示也应不同。
        fn not_ready_error(privileged: bool) -> AppError {
            if privileged {
                AppError::SudoElevationFailed(format!(
                    "提权会话未在 {} 秒内就绪：sudo 可能被拒绝（密码不正确，或该主机不允许非交互 sudo）",
                    READY_TIMEOUT.as_secs()
                ))
            } else {
                AppError::SshConnect(format!(
                    "远端会话未在 {} 秒内就绪；请确认目标主机已安装 bash",
                    READY_TIMEOUT.as_secs()
                ))
            }
        }

        // --- 连接与主机密钥校验（D10） ---
        let config = Arc::new(client::Config {
            inactivity_timeout: None,
            keepalive_interval: Some(Duration::from_secs(30)),
            keepalive_max: 3,
            ..Default::default()
        });

        let handler = TofuHandler::new(host.host_key.clone());
        let captured = handler.captured.clone();

        let addr = (host.address.clone(), host.port);
        let mut handle = tokio::time::timeout(
            CONNECT_TIMEOUT,
            client::connect(config, addr, handler),
        )
        .await
        .map_err(|_| {
            AppError::SshConnect(format!(
                "连接 {}:{} 超时（{} 秒）",
                host.address,
                host.port,
                CONNECT_TIMEOUT.as_secs()
            ))
        })?
        .map_err(|e| AppError::SshConnect(super::auth::describe_connect_error(&e)))?;

        // --- 认证 ---
        let authenticated = match &auth {
            AuthMethod::Password { username, password } => {
                let result = handle
                    .authenticate_password(username, password)
                    .await
                    .map_err(|e| AppError::SshAuth(format!("密码认证失败：{e}")))?;
                result.success()
            }
            AuthMethod::Key {
                username,
                private_key_pem,
                passphrase,
            } => {
                let key = super::auth::decode_private_key(
                    private_key_pem,
                    passphrase.as_deref(),
                )?;
                let key_with_alg = russh::keys::PrivateKeyWithHashAlg::new(
                    Arc::new(key),
                    None,
                );
                let result = handle
                    .authenticate_publickey(username, key_with_alg)
                    .await
                    .map_err(|e| AppError::SshAuth(format!("密钥认证失败：{e}")))?;
                result.success()
            }
        };

        if !authenticated {
            return Err(AppError::SshAuth(
                "认证被服务器拒绝，请检查用户名、密码或私钥是否正确".into(),
            ));
        }

        // --- 会话初始化：工作目录、FIFO、askpass、环境快照（Q33 / D4） ---
        //
        // 提权通道**跳过**这一步：它本身已是 root，不需要 sudo 拦截；
        // 环境快照也只在数据面上有意义。跳过可省一次往返。
        let env_snapshot = if privileged {
            None
        } else {
            let channel = handle
                .channel_open_session()
                .await
                .map_err(|e| AppError::SshConnect(format!("打开 SSH 会话通道失败：{e}")))?;

            let setup = protocol::session_setup_script(&nonce, sudo_enabled);
            channel
                .exec(true, setup.as_bytes())
                .await
                .map_err(|e| AppError::SshConnect(format!("初始化远端会话失败：{e}")))?;

            // setup 阶段的写入半部不再需要：后续会重新打开通道运行包装脚本。
            let (mut reader, _writer) = channel.split();

            // 等待 setup 完成：读取直到通道关闭，这样环境快照与 FIFO 一定先就绪。
            let mut setup_output = Vec::new();
            loop {
                match reader.wait().await {
                    Some(ChannelMsg::Data { data }) => setup_output.extend_from_slice(&data),
                    Some(ChannelMsg::ExtendedData { data, .. }) => {
                        setup_output.extend_from_slice(&data)
                    }
                    Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
                    Some(_) => {}
                }
            }

            // 解析环境快照（D4）：失败不影响会话建立，仅记录为空。
            parse_env_snapshot(&setup_output, host.shell_env_mode)
        };

        // 打开 channel 运行常驻包装脚本。
        let channel = handle
            .channel_open_session()
            .await
            .map_err(|e| AppError::SshConnect(format!("打开 SSH 会话通道失败：{e}")))?;

        // sudo_enabled 决定是否注入 sudo 拦截函数（Q33 三模式）。
        let script =
            protocol::wrapper_script(&nonce, host.init_script.as_deref(), sudo_enabled);

        // 提权通道：同一套包装脚本，但**以 sudo -S 启动**（D47）。
        // 这样提权命令的输出与退出码走的是同一套 NUL 分帧 + nonce 标记协议。
        let launch = if privileged {
            protocol::privileged_wrapper_command(&nonce, &script)
        } else {
            script
        };

        channel
            .exec(true, launch.as_bytes())
            .await
            .map_err(|e| AppError::SshConnect(format!("启动终端会话失败：{e}")))?;

        let (mut reader, writer) = channel.split();

        // --- 等待 READY 标记（隔离 profile 欢迎语等噪声，D4） ---
        let mut ready = false;
        let mut pending: Vec<u8> = Vec::new();

        // D47 握手：只在 sudo **确实索要密码**时写，且每条通道最多一次。
        let mut handshake = protocol::SudoAuthHandshake::new();
        let mut ask_for_password = false;
        let mut password_rejected = false;

        let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
        while !ready {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(not_ready_error(privileged));
            }

            let msg = tokio::time::timeout(remaining, reader.wait())
                .await
                .map_err(|_| not_ready_error(privileged))?;

            match msg {
                Some(ChannelMsg::Data { data })
                | Some(ChannelMsg::ExtendedData { data, .. }) => {
                    pending.extend_from_slice(&data);

                    // 1) 密码提示**不带换行**，不能等按行切分（否则死锁：
                    //    应用等提示、sudo 等密码）。直接在字节流里摘标记，
                    //    摘掉即计数一次，避免同一次提示被重复判定。
                    while protocol::take_sudo_prompt(&mut pending, &nonce) {
                        if handshake.on_prompt() == protocol::SudoAuthStep::SendPassword {
                            ask_for_password = true;
                        } else {
                            // 第二次索要 ⇒ 上一次写进去的密码没被接受
                            password_rejected = true;
                        }
                    }

                    // 2) 再按行解析就绪标记（其余事件暂存给后台任务）。
                    consume_lines(&mut pending, &nonce, |ev| {
                        if matches!(ev, SessionEvent::Ready) {
                            ready = true;
                        }
                    });

                    if password_rejected {
                        return Err(AppError::SudoElevationFailed(
                            "sudo 未接受该密码：请检查该主机配置的提权密码，\
                             或该主机是否允许非交互 sudo"
                                .into(),
                        ));
                    }

                    if ask_for_password {
                        ask_for_password = false;
                        let Some(pw) = privileged_password else {
                            // 数据面不该出现密码提示（数据面不跑 `sudo -S`）。
                            // 宁可失败也不要把密码写到别处。
                            return Err(AppError::SudoElevationFailed(
                                "数据面通道收到 sudo 密码提示，已拒绝写入密码".into(),
                            ));
                        };
                        // 密码经**本通道的 stdin** 交给 sudo：不落盘、不进命令行、
                        // 不经过 Agent 可枚举的任何路径（D47）。
                        writer
                            .data(format!("{pw}\n").as_bytes())
                            .await
                            .map_err(|e| {
                                AppError::SudoElevationFailed(format!("写入 sudo 密码失败：{e}"))
                            })?;
                    }
                }
                Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => {
                    return Err(if privileged {
                        AppError::SudoElevationFailed(
                            "提权会话在就绪前即已关闭：sudo 可能被拒绝\
                             （密码不正确，或该主机不允许非交互 sudo）"
                                .into(),
                        )
                    } else {
                        AppError::SshConnect(
                            "远端会话在就绪前即已关闭，请确认目标主机已安装 bash".into(),
                        )
                    });
                }
                Some(_) => {}
            }
        }

        // --- 后台任务：持续读取输出并推送事件 ---
        let (tx, rx) = mpsc::channel::<SessionOutput>(1024);
        let nonce_for_task = nonce.clone();

        tokio::spawn(async move {
            let mut buf: Vec<u8> = pending;
            let mut ready = false;
            loop {
                match reader.wait().await {
                    Some(ChannelMsg::Data { data })
                    | Some(ChannelMsg::ExtendedData { data, .. }) => {
                        buf.extend_from_slice(&data);

                        // 按换行切分并解析事件。控制事件（结束标记、sudo 请求）
                        // 在通道满时会等待而非丢弃（V13）。
                        if !pump_buffered(&mut buf, &nonce_for_task, &tx, &mut ready).await {
                            break;
                        }

                        // V12：只有遇到 \n 才会消费缓冲，若命令持续输出不含换行的
                        // 数据（如 `yes | tr -d '\n'`、读大二进制文件），缓冲会无界
                        // 增长直至 OOM。超过上限时强制切出一段作为输出交出，
                        // 既不丢数据，也不让内存继续膨胀。
                        if let Some(line) = take_overflow_chunk(&mut buf) {
                            if !forward_event(&tx, SessionEvent::OutputLine(line)).await {
                                break;
                            }
                        }
                    }
                    Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => {
                        let _ = tx
                            .send(SessionOutput::Disconnected {
                                reason: "远端关闭了会话通道".into(),
                            })
                            .await;
                        break;
                    }
                    Some(_) => {}
                }
            }
        });

        // 首次连接捕获的主机密钥（TOFU）：存入会话，由上层持久化（D10）。
        let captured_host_key = captured.lock().ok().and_then(|mut g| g.take());
        if let Some((_, fp)) = captured_host_key.as_ref() {
            tracing::info!(
                terminal_id = %terminal_id,
                fingerprint = %fp,
                "首次连接，记录主机密钥（TOFU）"
            );
        }

        let session = Self {
            handle,
            writer,
            nonce,
            terminal_id,
            captured_host_key,
            env_snapshot,
        };

        Ok((session, rx))
    }

    /// 取走首次连接捕获的主机密钥（TOFU），供上层写入数据库（D10）。
    pub fn take_captured_host_key(&self) -> Option<(String, String)> {
        self.captured_host_key.clone()
    }

    /// 会话建立时捕获的环境快照（D4）。
    pub fn env_snapshot(&self) -> Option<&EnvSnapshot> {
        self.env_snapshot.as_ref()
    }

    /// 会话随机串（测试与日志用）。
    pub fn nonce(&self) -> &str {
        &self.nonce
    }

    pub fn terminal_id(&self) -> &str {
        &self.terminal_id
    }

    /// 下发一条命令（NUL 分帧，D3 / V5）。
    ///
    /// `command_id` 由应用生成，远端在结束标记中原样回显，
    /// 从而按 id 精确配对，不依赖两侧各自自增的序号。
    pub async fn send_command(&self, command_id: &str, command: &str) -> Result<()> {
        let frame = protocol::encode_frame(command_id, command)?;
        self.writer
            .data(frame.as_slice())
            .await
            .map_err(|e| AppError::SshConnect(format!("发送命令失败：{e}")))?;
        Ok(())
    }

    /// 向 sudo FIFO 写入密码（Q33 模式二/三的"允许注入"）。
    ///
    /// `token` 来自本次索要的协议标记（远端 askpass 的 PID），
    /// 与会话 nonce 一起唯一定位该次索要的 FIFO（B2）。
    ///
    /// 走**独立的 SSH channel**，避免污染命令帧协议与输出解析；
    /// 密码经内存传递，不落盘、不进环境变量。
    ///
    /// **密码经 stdin 投递，绝不出现在命令行参数中**（V14）：
    /// 远端以 `cat > fifo` 接收，密码走 channel 的 stdin。
    /// 若把密码拼进 heredoc 脚本再 `exec`，sshd 会以
    /// `sh -c '<整段脚本>'` 启动进程，密码将出现在远端 `ps` /
    /// `/proc/<pid>/cmdline` 中，同机用户可读。
    ///
    /// 写端不会阻塞：askpass 已用 `exec 3<>fifo`（O_RDWR）打开 FIFO，
    /// 因此这里的 `cat > fifo`（O_WRONLY）一定能立刻找到读者。
    pub async fn send_sudo_password(&self, token: &str, password: &str) -> Result<()> {
        let fifo = self.fifo_path(token)?;
        // `test -p` 守卫（V1 回归防线）：目标必须是 FIFO。
        // 若 FIFO 因任何原因不存在，`cat > path` 会**创建普通文件**并把密码
        // 明文写入磁盘——这正是 V1。宁可直接失败（fail-closed）。
        let cmd = format!("test -p \"{fifo}\" || exit 1; cat > \"{fifo}\"");

        // askpass 按行读取（read 遇换行返回），故必须补一个换行终止。
        let mut payload = zeroize::Zeroizing::new(Vec::with_capacity(password.len() + 1));
        payload.extend_from_slice(password.as_bytes());
        payload.push(b'\n');

        let result = self.exec_with_stdin(&cmd, &payload, "写入 sudo 密码").await;
        // Zeroizing 会在离开作用域时清零 payload，减少内存中的密码副本（V20）。
        result
    }

    /// 让本次 sudo 索要认证失败（Q33 "拒绝"）。
    ///
    /// 向该次索要专属的 FIFO 写入一个**空行**：askpass 读到空密码交给 sudo，
    /// 认证随即失败，命令正常结束并返回非零退出码。
    ///
    /// 为什么不用"关闭 FIFO 让 read 得到 EOF"：askpass 以 O_RDWR 持有该
    /// FIFO，EOF 不会因外部关闭写端而出现。写入空值是更直接、更可靠的做法。
    ///
    /// 这一步**必不可少**：收到索要却不回应，askpass 会一直阻塞在 read 上，
    /// 而命令串行执行，后续排队命令会全部卡死（askpass 侧另有 120 秒兜底超时）。
    pub async fn deny_sudo(&self, token: &str) -> Result<()> {
        let fifo = self.fifo_path(token)?;
        let script = format!("test -p \"{fifo}\" || exit 1; printf '\\n' > \"{fifo}\"\n");
        self.exec_sudo_helper(&script, "拒绝 sudo 注入").await
    }

    /// 本会话中某次索要专属的 sudo FIFO 路径。
    ///
    /// 由会话 nonce + 索要 token 共同定位，一条 FIFO 上永远只有一个读者（B2）。
    /// token 先经严格校验：它来自远端输出且会被拼进命令。
    fn fifo_path(&self, token: &str) -> Result<String> {
        if !protocol::is_valid_sudo_token(token) {
            return Err(AppError::InvalidArgument(format!(
                "sudo 索要令牌非法：{token:?}"
            )));
        }
        Ok(format!(
            "$HOME/{}/{}",
            protocol::REMOTE_DIR,
            protocol::sudo_fifo_name(&self.nonce, token)
        ))
    }

    /// 在独立 channel 上执行一段 sudo 辅助脚本。
    async fn exec_sudo_helper(&self, script: &str, what: &str) -> Result<()> {
        let channel = self
            .handle
            .channel_open_session()
            .await
            .map_err(|e| AppError::SshConnect(format!("打开 sudo 通道失败：{e}")))?;

        channel
            .exec(true, script.as_bytes())
            .await
            .map_err(|e| AppError::SshConnect(format!("{what}失败：{e}")))?;

        // 等待命令结束，确保写入在返回前完成——
        // 否则调用方可能在 FIFO 尚未写入时就继续，askpass 仍会读到旧值。
        let mut reader = channel.split().0;
        loop {
            match reader.wait().await {
                Some(russh::ChannelMsg::Eof)
                | Some(russh::ChannelMsg::Close)
                | None => break,
                Some(_) => {}
            }
        }

        Ok(())
    }

    /// 执行远端命令并通过 **stdin** 投递数据（V14）。
    ///
    /// 用于传递敏感内容：命令自身不含任何秘密，秘密只走 channel 的 stdin，
    /// 因此不会出现在远端的命令行参数（`ps` / `/proc/*/cmdline）中。
    async fn exec_with_stdin(&self, command: &str, stdin_data: &[u8], what: &str) -> Result<()> {
        let channel = self
            .handle
            .channel_open_session()
            .await
            .map_err(|e| AppError::SshConnect(format!("打开 sudo 通道失败：{e}")))?;

        channel
            .exec(true, command.as_bytes())
            .await
            .map_err(|e| AppError::SshConnect(format!("{what}失败：{e}")))?;

        let (mut reader, writer) = channel.split();

        writer
            .data(stdin_data)
            .await
            .map_err(|e| AppError::SshConnect(format!("{what}失败：{e}")))?;
        // 发送 EOF，让远端的 `cat` 知道输入结束并退出。
        writer
            .eof()
            .await
            .map_err(|e| AppError::SshConnect(format!("{what}失败：{e}")))?;

        // 等待远端命令结束，确保数据在返回前已写入 FIFO。
        loop {
            match reader.wait().await {
                Some(russh::ChannelMsg::Eof)
                | Some(russh::ChannelMsg::Close)
                | None => break,
                Some(_) => {}
            }
        }

        Ok(())
    }

    /// 优雅关闭会话。
    ///
    /// 关闭前先尽力收尾：删除本会话的 FIFO 与 askpass 脚本，
    /// 避免在远端留下可被同机用户读取的残留节点（V1）。
    /// 清理失败不阻断关闭流程——会话要关，残留只是次要问题。
    pub async fn close(&self) -> Result<()> {
        self.cleanup_remote_artifacts().await;

        self.writer
            .eof()
            .await
            .map_err(|e| AppError::SshConnect(format!("关闭会话失败：{e}")))?;
        Ok(())
    }

    /// 删除远端为本会话创建的临时节点（FIFO 与 askpass 脚本）。
    ///
    /// 用独立 channel 执行，不影响命令帧协议；任何失败都只记日志。
    async fn cleanup_remote_artifacts(&self) {
        let script = protocol::session_cleanup_script(&self.nonce);
        if let Err(e) = self.exec_sudo_helper(&script, "清理远端临时文件").await {
            tracing::debug!("清理远端临时文件失败（不影响关闭）：{e}");
        }
    }

    /// 断开底层连接。
    pub async fn disconnect(&self) -> Result<()> {
        self.handle
            .disconnect(russh::Disconnect::ByApplication, "", "en")
            .await
            .map_err(|e| AppError::SshConnect(format!("断开连接失败：{e}")))?;
        Ok(())
    }
}

/// 从字节缓冲中切分完整行，逐行解析为会话事件。
///
/// 未以换行结尾的残留会留在缓冲中，等待后续数据——
/// 避免把半行当作完整事件处理。
fn consume_lines<F: FnMut(SessionEvent)>(buf: &mut Vec<u8>, nonce: &str, mut f: F) {
    while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
        let line_bytes: Vec<u8> = buf.drain(..=pos).collect();
        let line = String::from_utf8_lossy(&line_bytes);
        let line = line.trim_end_matches(['\r', '\n']);
        if let Some(ev) = protocol::parse_line(line, nonce) {
            // 跳过空输出行，减少无意义事件。
            if let SessionEvent::OutputLine(ref s) = ev {
                if s.is_empty() {
                    continue;
                }
            }
            f(ev);
        }
    }
}

/// 未换行的残留缓冲上限（V12）。
///
/// 只按 `\n` 切分意味着：若命令持续输出不含换行的数据，
/// 缓冲会无界增长直至 OOM。超过上限时强制按当前内容切出一"行"，
/// 既保住内存，又不丢数据（内容仍会作为输出交给上层）。
const MAX_PENDING_LINE_BYTES: usize = 1024 * 1024;

/// 超过未换行缓冲上限时，切出超出的部分作为一行输出交出（V12）。
///
/// 返回 `None` 表示**不该切**：要么没超限，要么缓冲里已经有 `\n`
/// （此时应交给正常分行路径处理，强行切分会把一整行拆散）。
///
/// 提取成独立函数是为了让这条内存护栏能被直接测试——
/// 它原先内联在读取循环里，测试只能自己重写一遍 `drain` 逻辑，
/// 那样无论生产代码怎么写都会通过。
fn take_overflow_chunk(buf: &mut Vec<u8>) -> Option<String> {
    if buf.len() <= MAX_PENDING_LINE_BYTES || buf.contains(&b'\n') {
        return None;
    }
    let over = buf.len() - MAX_PENDING_LINE_BYTES;
    let chunk: Vec<u8> = buf.drain(..over).collect();
    let line = String::from_utf8_lossy(&chunk).to_string();
    if line.is_empty() {
        None
    } else {
        Some(line)
    }
}

/// 把事件投递到通道。
///
/// - 普通输出行尽力投递（`try_send`）：丢几行输出可接受，
///   换取"绝不因消费者变慢而卡住读取循环"。
/// - **控制事件（结束标记、sudo 请求、密码提示）不可丢弃**（V13）：
///   丢弃 `Finished` 会让命令永久悬挂，丢弃 `SudoRequest` 会让
///   远端 askpass 阻塞、拖死整条串行队列；丢弃 `SudoPrompt`（D47）会让
///   提权通道等不到密码而超时失败。通道满时等待接收端腾出空间。
///
/// 返回 `false` 表示接收端已关闭，调用方应停止读取循环。
async fn forward_event(tx: &mpsc::Sender<SessionOutput>, ev: SessionEvent) -> bool {
    let out = match ev {
        SessionEvent::OutputLine(line) => {
            // 逐行事件不带序号——归属由上层按"当前执行中命令"决定。
            SessionOutput::Line { line }
        }
        SessionEvent::CommandFinished {
            command_id,
            exit_code,
        } => SessionOutput::Finished {
            command_id,
            exit_code,
        },
        SessionEvent::SudoRequest { token } => SessionOutput::SudoRequest { token },
        SessionEvent::SudoPrompt => SessionOutput::SudoPrompt,
        SessionEvent::Ready => return true,
    };

    let critical = matches!(
        out,
        SessionOutput::Finished { .. }
            | SessionOutput::SudoRequest { .. }
            | SessionOutput::SudoPrompt
    );

    if let Err(err) = tx.try_send(out) {
        match err {
            // 接收端已关闭：后台任务结束，无需再投递。
            mpsc::error::TrySendError::Closed(_) => return false,
            mpsc::error::TrySendError::Full(out) if critical => {
                // 控制事件必须送达；等待消费者腾出空间（不会丢失）。
                if tx.send(out).await.is_err() {
                    return false;
                }
            }
            // 非控制事件丢弃：这是有意的取舍，不影响正确性。
            mpsc::error::TrySendError::Full(_) => {
                tracing::debug!("输出事件通道已满，丢弃一行输出");
            }
        }
    }
    true
}

/// 按当前缓冲内容解析事件并逐条投递；返回 `false` 表示接收端已关闭。
async fn pump_buffered(
    buf: &mut Vec<u8>,
    nonce: &str,
    tx: &mpsc::Sender<SessionOutput>,
    ready: &mut bool,
) -> bool {
    // consume_lines 是同步闭包，这里先把事件收集出来，
    // 再在异步上下文里逐条投递（控制事件需要 await 背压）。
    let mut events = Vec::new();
    consume_lines(buf, nonce, |ev| events.push(ev));

    for ev in events {
        if matches!(ev, SessionEvent::Ready) {
            *ready = true;
        }
        if !forward_event(tx, ev).await {
            return false;
        }
    }
    true
}

/// 解析会话初始化输出的环境快照（D4）。
fn parse_env_snapshot(
    raw: &[u8],
    mode: ShellEnvMode,
) -> Option<crate::domain::terminal::EnvSnapshot> {
    let text = String::from_utf8_lossy(raw);

    let mut path = None;
    let mut pwd = None;
    let mut bash_version = None;

    for line in text.lines() {
        if let Some(v) = line.strip_prefix("MFPERCH_PATH=") {
            path = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("MFPERCH_PWD=") {
            pwd = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("MFPERCH_BASH=") {
            bash_version = Some(v.to_string());
        }
    }

    if path.is_none() && pwd.is_none() && bash_version.is_none() {
        return None;
    }

    Some(crate::domain::terminal::EnvSnapshot {
        path,
        pwd,
        bash_version,
        shell_env_mode: mode.as_str().to_string(),
        captured_at: crate::domain::now_rfc3339(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// V12：不含换行的持续输出不得让缓冲无界增长。
    ///
    /// 直接调用生产函数 `take_overflow_chunk`——切出的内容会作为输出交给上层，
    /// 属可观察行为；而不是"常量看起来落在合理区间"这种断言。
    #[test]
    fn overflow_chunk_is_taken_when_buffer_exceeds_cap() {
        let mut buf: Vec<u8> = vec![b'x'; MAX_PENDING_LINE_BYTES + 4096];

        let chunk = take_overflow_chunk(&mut buf).expect("超限时应切出超出的部分");
        assert_eq!(chunk.len(), 4096, "切出的应是超出上限的那一段");
        assert_eq!(buf.len(), MAX_PENDING_LINE_BYTES, "缓冲应回落到上限之内");
        assert!(chunk.bytes().all(|b| b == b'x'), "切出的内容不得被改动");

        assert!(
            take_overflow_chunk(&mut buf).is_none(),
            "恰好等于上限时不应再切分"
        );
    }

    /// 缓冲里已有换行时必须走正常分行路径，不得被上限强行拆行。
    #[test]
    fn overflow_chunk_does_not_split_complete_lines() {
        let mut buf = vec![b'x'; MAX_PENDING_LINE_BYTES + 10];
        buf.push(b'\n');
        assert!(
            take_overflow_chunk(&mut buf).is_none(),
            "有换行时不应按上限切分（会把一整行拆散）"
        );
    }

    /// 未超限时不得切分，也不得改动缓冲。
    #[test]
    fn overflow_chunk_is_not_taken_below_cap() {
        let mut buf = b"partial line".to_vec();
        assert!(take_overflow_chunk(&mut buf).is_none());
        assert_eq!(buf, b"partial line", "未超限时缓冲不应被改动");
    }

    #[test]
    fn consume_lines_handles_partial_line() {
        let nonce = "n1";
        let mut buf = b"partial".to_vec();
        let mut events = Vec::new();
        consume_lines(&mut buf, nonce, |e| events.push(e));
        assert!(events.is_empty(), "未以换行结尾时不应产生事件");
        assert_eq!(buf, b"partial", "残留应保留在缓冲中");

        // 补全该行后才产生事件。
        buf.extend_from_slice(b" line\n");
        consume_lines(&mut buf, nonce, |e| events.push(e));
        assert_eq!(events.len(), 1);
        assert!(buf.is_empty());
    }

    #[test]
    fn consume_lines_parses_multiple_lines() {
        let nonce = "abc";
        let mut buf = format!(
            "hello\n{}\nworld\n",
            format_args!("{}{}__cmdA__0__", protocol::END_MARKER_PREFIX, nonce)
        )
        .into_bytes();
        let mut events = Vec::new();
        consume_lines(&mut buf, nonce, |e| events.push(e));

        assert_eq!(events.len(), 3);
        assert!(matches!(&events[0], SessionEvent::OutputLine(s) if s == "hello"));
        assert!(matches!(
            &events[1],
            SessionEvent::CommandFinished { command_id, exit_code: 0 } if command_id == "cmdA"
        ));
        assert!(matches!(&events[2], SessionEvent::OutputLine(s) if s == "world"));
    }

    #[test]
    fn consume_lines_skips_empty_lines() {
        let nonce = "n";
        let mut buf = b"\n\n\n".to_vec();
        let mut events = Vec::new();
        consume_lines(&mut buf, nonce, |e| events.push(e));
        assert!(events.is_empty(), "空行不产生事件，避免噪声");
    }

    #[test]
    fn consume_lines_ignores_marker_with_wrong_nonce() {
        let nonce = "real";
        let mut buf = format!("{}wrong__1__0__\n", protocol::END_MARKER_PREFIX).into_bytes();
        let mut events = Vec::new();
        consume_lines(&mut buf, nonce, |e| events.push(e));
        // 应作为普通输出行，而非结束标记。
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], SessionEvent::OutputLine(_)));
    }

    #[test]
    fn parse_env_snapshot_extracts_values() {
        let raw = b"MFPERCH_PATH=/usr/local/bin:/usr/bin\nMFPERCH_PWD=/home/deploy\nMFPERCH_BASH=5.2.21\n";
        let snap = parse_env_snapshot(raw, ShellEnvMode::LoginThenTask).unwrap();
        assert_eq!(snap.path.as_deref(), Some("/usr/local/bin:/usr/bin"));
        assert_eq!(snap.pwd.as_deref(), Some("/home/deploy"));
        assert_eq!(snap.bash_version.as_deref(), Some("5.2.21"));
        assert_eq!(snap.shell_env_mode, "login");
    }

    #[test]
    fn parse_env_snapshot_returns_none_when_absent() {
        assert!(parse_env_snapshot(b"unrelated output\n", ShellEnvMode::CleanThenTask).is_none());
        assert!(parse_env_snapshot(b"", ShellEnvMode::CleanThenTask).is_none());
    }

    #[test]
    fn parse_env_snapshot_handles_partial_fields() {
        // 只拿到部分字段时也应记录，便于排查。
        let raw = b"MFPERCH_PWD=/root\n";
        let snap = parse_env_snapshot(raw, ShellEnvMode::LoginThenTask).unwrap();
        assert_eq!(snap.pwd.as_deref(), Some("/root"));
        assert!(snap.path.is_none());
    }
}
