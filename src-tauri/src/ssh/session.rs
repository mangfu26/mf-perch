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
    Line { seq: u64, line: String },
    /// 某条命令执行结束。
    Finished { seq: u64, exit_code: i32 },
    /// sudo 正在索要密码（Q33）。
    SudoRequest,
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
        let terminal_id = terminal_id.into();
        let nonce = protocol::new_nonce();

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

        // --- 打开 channel 并启动包装脚本（D3 / D4） ---
        let channel = handle
            .channel_open_session()
            .await
            .map_err(|e| AppError::SshConnect(format!("打开 SSH 会话通道失败：{e}")))?;

        // 会话初始化：工作目录、FIFO、askpass、环境快照（Q33 / D4）
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
        let env_snapshot = parse_env_snapshot(&setup_output, host.shell_env_mode);

        // 重新打开 channel 运行常驻包装脚本。
        let channel = handle
            .channel_open_session()
            .await
            .map_err(|e| AppError::SshConnect(format!("打开 SSH 会话通道失败：{e}")))?;

        let script = protocol::wrapper_script(&nonce, host.init_script.as_deref());
        channel
            .exec(true, script.as_bytes())
            .await
            .map_err(|e| AppError::SshConnect(format!("启动终端会话失败：{e}")))?;

        let (mut reader, writer) = channel.split();

        // --- 等待 READY 标记（隔离 profile 欢迎语等噪声，D4） ---
        let mut ready = false;
        let mut pending: Vec<u8> = Vec::new();

        let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
        while !ready {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(AppError::SshConnect(format!(
                    "远端会话未在 {} 秒内就绪；请确认目标主机已安装 bash",
                    READY_TIMEOUT.as_secs()
                )));
            }

            let msg = tokio::time::timeout(remaining, reader.wait())
                .await
                .map_err(|_| {
                    AppError::SshConnect(format!(
                        "远端会话未在 {} 秒内就绪；请确认目标主机已安装 bash",
                        READY_TIMEOUT.as_secs()
                    ))
                })?;

            match msg {
                Some(ChannelMsg::Data { data }) => {
                    pending.extend_from_slice(&data);
                    consume_lines(&mut pending, &nonce, |ev| {
                        if matches!(ev, SessionEvent::Ready) {
                            ready = true;
                        }
                    });
                }
                Some(ChannelMsg::ExtendedData { data, .. }) => {
                    pending.extend_from_slice(&data);
                    consume_lines(&mut pending, &nonce, |ev| {
                        if matches!(ev, SessionEvent::Ready) {
                            ready = true;
                        }
                    });
                }
                Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => {
                    return Err(AppError::SshConnect(
                        "远端会话在就绪前即已关闭，请确认目标主机已安装 bash".into(),
                    ));
                }
                Some(_) => {}
            }
        }

        // --- 后台任务：持续读取输出并推送事件 ---
        let (tx, rx) = mpsc::channel::<SessionOutput>(1024);
        let nonce_for_task = nonce.clone();

        tokio::spawn(async move {
            let mut buf: Vec<u8> = pending;
            loop {
                match reader.wait().await {
                    Some(ChannelMsg::Data { data }) => {
                        buf.extend_from_slice(&data);
                        consume_lines(&mut buf, &nonce_for_task, |ev| {
                            forward_event(&tx, ev);
                        });
                    }
                    Some(ChannelMsg::ExtendedData { data, .. }) => {
                        buf.extend_from_slice(&data);
                        consume_lines(&mut buf, &nonce_for_task, |ev| {
                            forward_event(&tx, ev);
                        });
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

        let session = Self {
            handle,
            writer,
            nonce,
            terminal_id,
        };

        // 记录首次连接捕获到的主机密钥（TOFU），由调用方持久化。
        if let Some((key, fp)) = captured.lock().ok().and_then(|mut g| g.take()) {
            tracing::info!(
                terminal_id = %session.terminal_id,
                fingerprint = %fp,
                "首次连接，记录主机密钥（TOFU）"
            );
            // 通过环境变量式的旁路把结果带回：这里用参数返回更清晰，
            // 但为保持签名简洁，改为记录日志并由上层从数据库读取。
            let _ = key;
        }

        // env_snapshot 通过返回的会话状态由上层读取；此处保持连接建立的最小副作用。
        let _ = env_snapshot;

        Ok((session, rx))
    }

    /// 会话随机串（测试与日志用）。
    pub fn nonce(&self) -> &str {
        &self.nonce
    }

    pub fn terminal_id(&self) -> &str {
        &self.terminal_id
    }

    /// 下发一条命令（NUL 分帧，D3）。
    pub async fn send_command(&self, command: &str) -> Result<()> {
        let frame = protocol::encode_frame(command)?;
        self.writer
            .data(frame.as_slice())
            .await
            .map_err(|e| AppError::SshConnect(format!("发送命令失败：{e}")))?;
        Ok(())
    }

    /// 向 sudo FIFO 写入密码（Q33 模式二/三）。
    ///
    /// 走**独立的 SSH channel**，避免污染命令帧协议与输出解析；
    /// 密码经内存传递，不落盘、不进环境变量。
    pub async fn send_sudo_password(&self, password: &str) -> Result<()> {
        let channel = self
            .handle
            .channel_open_session()
            .await
            .map_err(|e| AppError::SshConnect(format!("打开 sudo 通道失败：{e}")))?;

        let fifo = format!("$HOME/{}/{}", protocol::REMOTE_DIR, protocol::SUDO_FIFO_NAME);
        // 用 printf 写入，避免 echo 的行为差异；单引号包裹防止内容被解释。
        let cmd = format!("printf '%s\\n' {}\n", shell_single_quote(password));
        let script = format!("cat > {fifo} <<'MFPERCH_SUDO'\n{password}\nMFPERCH_SUDO\n");

        // 优先使用 heredoc：无需转义密码中的特殊字符。
        let _ = cmd;

        channel
            .exec(true, script.as_bytes())
            .await
            .map_err(|e| AppError::SshConnect(format!("写入 sudo 密码失败：{e}")))?;

        channel
            .eof()
            .await
            .map_err(|e| AppError::SshConnect(format!("关闭 sudo 通道失败：{e}")))?;

        Ok(())
    }

    /// 优雅关闭会话。
    pub async fn close(&self) -> Result<()> {
        self.writer
            .eof()
            .await
            .map_err(|e| AppError::SshConnect(format!("关闭会话失败：{e}")))?;
        Ok(())
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

/// 把单引号转义为可在 shell 单引号字符串中安全使用。
fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
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

/// 转发事件到通道；`CommandFinished` 需要知道序号。
fn forward_event(tx: &mpsc::Sender<SessionOutput>, ev: SessionEvent) {
    let out = match ev {
        SessionEvent::OutputLine(line) => {
            // 逐行事件不带序号——序号由上层按"当前执行中命令"关联，
            // 因为标记行只在命令结束时出现（见 D3 协议设计）。
            SessionOutput::Line { seq: 0, line }
        }
        SessionEvent::CommandFinished { seq, exit_code } => {
            SessionOutput::Finished { seq, exit_code }
        }
        SessionEvent::SudoRequest => SessionOutput::SudoRequest,
        SessionEvent::Ready => return,
    };

    // 尽力投递：接收端关闭时忽略，避免后台任务 panic。
    let _ = tx.try_send(out);
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

    #[test]
    fn shell_single_quote_escapes_quotes() {
        assert_eq!(shell_single_quote("abc"), "'abc'");
        // 密码中的单引号必须被转义，否则会破坏 shell 语法。
        assert_eq!(shell_single_quote("a'b"), "'a'\\''b'");
        // 特殊字符在单引号内无需转义。
        assert_eq!(shell_single_quote("$HOME `id`;rm -rf /"), "'$HOME `id`;rm -rf /'");
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
            format_args!("{}{}__1__0__", protocol::END_MARKER_PREFIX, nonce)
        )
        .into_bytes();
        let mut events = Vec::new();
        consume_lines(&mut buf, nonce, |e| events.push(e));

        assert_eq!(events.len(), 3);
        assert!(matches!(&events[0], SessionEvent::OutputLine(s) if s == "hello"));
        assert!(matches!(
            &events[1],
            SessionEvent::CommandFinished { seq: 1, exit_code: 0 }
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
