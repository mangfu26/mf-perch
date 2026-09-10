//! 终端会话协议（D3）：NUL 分帧 + 自打印结束标记。
//!
//! 为什么不用 base64：客户指出无法保证目标主机安装了 `base64`。
//! 改用 NUL 字节（`0x00`）作分隔符——`execve` 的参数以 NUL 结尾，
//! 因此**任何 shell 命令都不可能包含 NUL**，它是天然安全的分隔符。
//! 远端只需 bash 内建能力（`read -r -d ''`、`printf`），无外部命令依赖。
//!
//! 命令结束判定：远端脚本每执行完一条命令就打印
//! `__MF_PERCH_END__<nonce>__<seq>__<rc>__`。
//! `nonce` 每会话随机生成，即使命令输出恰好包含类似文本也不会误判——
//! 这正是本方案优于"识别 shell 提示符"的关键。

use crate::error::{AppError, Result};

/// 结束标记前缀。
pub const END_MARKER_PREFIX: &str = "__MF_PERCH_END__";
/// 会话就绪标记（包装脚本启动完成后打印）。
pub const READY_MARKER_PREFIX: &str = "__MF_PERCH_READY__";
/// sudo askpass 请求标记（Q33）。**必须写 stderr**——见 [`askpass_script`]。
pub const SUDO_REQUEST_PREFIX: &str = "__MF_SUDO_REQUEST__";
/// 远端工作目录（存放 FIFO 与 askpass 脚本）。
pub const REMOTE_DIR: &str = ".mf-perch";
/// 远端 sudo 密码 FIFO 文件名。
pub const SUDO_FIFO_NAME: &str = "sudopw.fifo";
/// 远端 askpass 脚本文件名。
pub const ASKPASS_NAME: &str = "askpass";

/// 解析出的会话事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    /// 会话已就绪，可以开始发送命令。
    Ready,
    /// 收到某条命令的结束标记。
    CommandFinished { seq: u64, exit_code: i32 },
    /// 有 sudo 正在索要密码（Q33 `ask` / `auto` 模式据此决定是否注入）。
    SudoRequest,
    /// 普通输出行（命令产生的输出）。
    OutputLine(String),
}

/// 生成每会话随机 nonce。
///
/// nonce 用十六进制表示，避免与 base64 字符集混淆，也便于在日志中比对。
pub fn new_nonce() -> String {
    use rand::Rng;
    let mut bytes = [0u8; 8];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 构造远端包装脚本（D3 第 2.3 节）。
///
/// 关键点：
/// - `read -r -d ''` 读到 NUL 为止——bash 内建，不依赖外部命令；
/// - `eval` 在同一 shell 进程内执行，因此 `cd` / `export` 状态保留；
/// - `< /dev/null` 断开每条命令的 stdin，防止交互式命令吃掉后续命令帧；
/// - 序号由脚本自增，无需客户端下发。
///
/// 启动方式（登录 shell / 干净模式）由 [`shell_invocation`] 决定，
/// 脚本本身不含该差异——保证两种模式下协议行为完全一致。
pub fn wrapper_script(nonce: &str, init_script: Option<&str>) -> String {
    // 初始化脚本在包装循环之前执行一次，用于 nvm / conda 等显式加载（D4）。
    let init = match init_script {
        Some(s) if !s.trim().is_empty() => format!(
            "# 主机配置的初始化脚本\n{}\n",
            s.trim()
        ),
        _ => String::new(),
    };

    format!(
        r#"set +e
mfperch_nonce='{nonce}'
{init}printf '{ready}{nonce}__\n' >&2
mfperch_seq=0
while IFS= read -r -d '' mfperch_cmd; do
  mfperch_seq=$((mfperch_seq + 1))
  eval "$mfperch_cmd" < /dev/null
  mfperch_rc=$?
  printf '\n{end}%s__%s__%s__\n' "$mfperch_nonce" "$mfperch_seq" "$mfperch_rc"
done
"#,
        nonce = nonce,
        init = init,
        ready = READY_MARKER_PREFIX,
        end = END_MARKER_PREFIX,
    )
}

/// 启动包装脚本时使用的 bash 参数。
pub fn shell_invocation(env_mode: crate::domain::host::ShellEnvMode) -> (&'static str, &'static [&'static str]) {
    use crate::domain::host::ShellEnvMode;
    match env_mode {
        ShellEnvMode::LoginThenTask => ("bash", &["-l", "-s"]),
        ShellEnvMode::CleanThenTask => ("bash", &["--noprofile", "--norc", "-s"]),
    }
}

/// 构造 askpass 脚本（Q33）。
///
/// **实现要点（WSL 实测发现）**：`sudo -A` 会把 askpass 程序的 **stdout 第一行
/// 当作密码**。因此"请求标记"必须写到 **stderr**（`>&2`），密码才写 stdout。
/// 若标记误走 stdout，sudo 会把标记当密码，认证必然失败。
pub fn askpass_script(nonce: &str, fifo_path: &str) -> String {
    format!(
        r#"#!/bin/sh
printf '{prefix}%s__\n' '{nonce}' >&2
IFS= read -r mfperch_pw < '{fifo}'
printf '%s\n' "$mfperch_pw"
"#,
        prefix = SUDO_REQUEST_PREFIX,
        nonce = nonce,
        fifo = fifo_path,
    )
}

/// 构造会话初始化命令：建立工作目录、FIFO、askpass 脚本，并输出环境快照（D4）。
///
/// `enable_sudo` 为 `false` 时（`deny` 模式）不创建 askpass——
/// sudo 因拿不到密码而失败，这正是"禁止注入"的天然实现（fail-closed）。
pub fn session_setup_script(nonce: &str, enable_sudo: bool) -> String {
    let dir = format!("$HOME/{REMOTE_DIR}");
    let fifo = format!("{dir}/{SUDO_FIFO_NAME}");
    let askpass = format!("{dir}/{ASKPASS_NAME}");

    let sudo_part = if enable_sudo {
        format!(
            r#"
# sudo askpass 与 FIFO（Q33）：密码只在被索要时经内存传递，不落盘
rm -f '{fifo}'
mkfifo '{fifo}' 2>/dev/null || true
chmod 600 '{fifo}' 2>/dev/null || true
EXPORT_ASKPASS=1
"#,
            fifo = fifo,
        )
    } else {
        // deny 模式：不设置 askpass，sudo 将因无密码而失败。
        "EXPORT_ASKPASS=0\n".to_string()
    };

    let askpass_write = if enable_sudo {
        let script = askpass_script(nonce, &fifo);
        // 用 heredoc 写入 askpass 脚本，避免引号转义问题。
        format!(
            "cat > '{askpass}' <<'MFPERCH_ASKPASS'\n{script}MFPERCH_ASKPASS\nchmod 700 '{askpass}'\n",
            askpass = askpass,
            script = script,
        )
    } else {
        String::new()
    };

    format!(
        r#"mkdir -p '{dir}' 2>/dev/null
{sudo_part}{askpass_write}
# 环境快照，便于排查"命令找不到"类问题（D4）
printf 'MFPERCH_PATH=%s\n' "$PATH" >&2
printf 'MFPERCH_PWD=%s\n' "$PWD" >&2
printf 'MFPERCH_BASH=%s\n' "$BASH_VERSION" >&2
"#,
        dir = dir,
        sudo_part = sudo_part,
        askpass_write = askpass_write,
    )
}

/// 生成 sudo 拦截函数定义（Q33）。
///
/// 用 shell 函数而非改写命令：函数是**语义级**拦截，
/// `sh -c 'sudo x'`、变量拼接等情况不会像文本匹配那样漏判。
/// `command` 关键字跳过函数查找，避免无限递归。
pub fn sudo_function_def(askpass_path: &str) -> String {
    format!(
        "sudo() {{ SUDO_ASKPASS='{askpass}' command sudo -A -p '' \"$@\"; }}\nexport -f sudo\n",
        askpass = askpass_path
    )
}

/// 从一行输出中解析会话事件。
///
/// 返回 `None` 表示该行不含协议标记，应作为普通输出处理。
pub fn parse_line(line: &str, nonce: &str) -> Option<SessionEvent> {
    let trimmed = line.trim_end_matches(['\r', '\n']);

    // 就绪标记。
    let ready = format!("{READY_MARKER_PREFIX}{nonce}__");
    if trimmed == ready {
        return Some(SessionEvent::Ready);
    }

    // sudo 请求标记（来自 askpass 的 stderr）。
    let sudo_req = format!("{SUDO_REQUEST_PREFIX}{nonce}__");
    if trimmed == sudo_req {
        return Some(SessionEvent::SudoRequest);
    }

    // 结束标记：__MF_PERCH_END__<nonce>__<seq>__<rc>__
    if let Some(rest) = trimmed.strip_prefix(END_MARKER_PREFIX) {
        // 只有 nonce 匹配才算数——这是防误判的关键。
        if let Some(after_nonce) = rest.strip_prefix(&format!("{nonce}__")) {
            let parts: Vec<&str> = after_nonce.trim_end_matches('_').split("__").collect();
            if parts.len() == 2 {
                if let (Ok(seq), Ok(rc)) = (parts[0].parse::<u64>(), parts[1].parse::<i32>()) {
                    return Some(SessionEvent::CommandFinished {
                        seq,
                        exit_code: rc,
                    });
                }
            }
        }
        // nonce 不匹配：这是命令自己输出的类似文本，按普通输出处理。
        return Some(SessionEvent::OutputLine(trimmed.to_string()));
    }

    Some(SessionEvent::OutputLine(trimmed.to_string()))
}

/// 把命令编码为 NUL 结尾的帧。
pub fn encode_frame(command: &str) -> Result<Vec<u8>> {
    if command.as_bytes().contains(&0) {
        return Err(AppError::InvalidArgument(
            "命令不能包含 NUL 字节".into(),
        ));
    }
    let mut frame = command.as_bytes().to_vec();
    frame.push(0);
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonce_is_hex_and_unique() {
        let a = new_nonce();
        let b = new_nonce();
        assert_eq!(a.len(), 16, "16 个十六进制字符表示 8 字节");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b, "每次生成的 nonce 应不同");
    }

    #[test]
    fn encode_frame_appends_nul() {
        let f = encode_frame("ls -la").unwrap();
        assert_eq!(f.last(), Some(&0));
        assert_eq!(&f[..f.len() - 1], b"ls -la");
    }

    #[test]
    fn encode_frame_keeps_newlines_quotes_and_dollars() {
        // NUL 分帧的价值：这些字符无需任何转义。
        let cmd = "cd /tmp && echo \"a $HOME\" ; ls\npwd";
        let f = encode_frame(cmd).unwrap();
        assert_eq!(&f[..f.len() - 1], cmd.as_bytes());
    }

    #[test]
    fn encode_frame_rejects_nul_in_command() {
        assert!(encode_frame("echo \0 bad").is_err());
    }

    #[test]
    fn parse_ready_marker() {
        let nonce = "abc123";
        let line = format!("{READY_MARKER_PREFIX}{nonce}__");
        assert_eq!(parse_line(&line, nonce), Some(SessionEvent::Ready));
    }

    #[test]
    fn parse_sudo_request_marker() {
        let nonce = "abc123";
        let line = format!("{SUDO_REQUEST_PREFIX}{nonce}__");
        assert_eq!(parse_line(&line, nonce), Some(SessionEvent::SudoRequest));
    }

    #[test]
    fn parse_command_finished_marker() {
        let nonce = "deadbeef";
        let line = format!("{END_MARKER_PREFIX}{nonce}__3__0__");
        assert_eq!(
            parse_line(&line, nonce),
            Some(SessionEvent::CommandFinished {
                seq: 3,
                exit_code: 0
            })
        );

        let fail = format!("{END_MARKER_PREFIX}{nonce}__7__1__");
        assert_eq!(
            parse_line(&fail, nonce),
            Some(SessionEvent::CommandFinished {
                seq: 7,
                exit_code: 1
            })
        );
    }

    #[test]
    fn parse_negative_exit_code() {
        // 被信号终止的命令退出码可能为负。
        let nonce = "cafe";
        let line = format!("{END_MARKER_PREFIX}{nonce}__1__-9__");
        assert_eq!(
            parse_line(&line, nonce),
            Some(SessionEvent::CommandFinished {
                seq: 1,
                exit_code: -9
            })
        );
    }

    #[test]
    fn marker_with_wrong_nonce_is_treated_as_output() {
        // 关键防误判场景：命令输出恰好包含类似标记，但 nonce 不同。
        let nonce = "realnonce";
        let spoofed = format!("{END_MARKER_PREFIX}othernonce__1__0__");
        assert_eq!(
            parse_line(&spoofed, nonce),
            Some(SessionEvent::OutputLine(spoofed.clone())),
            "nonce 不匹配时不得当作结束标记"
        );
    }

    #[test]
    fn ordinary_output_is_passed_through() {
        let nonce = "n1";
        assert_eq!(
            parse_line("total 184", nonce),
            Some(SessionEvent::OutputLine("total 184".into()))
        );
        assert_eq!(
            parse_line("", nonce),
            Some(SessionEvent::OutputLine("".into()))
        );
    }

    #[test]
    fn crlf_is_normalized() {
        let nonce = "n2";
        let line = format!("{END_MARKER_PREFIX}{nonce}__1__0__\r\n");
        assert_eq!(
            parse_line(&line, nonce),
            Some(SessionEvent::CommandFinished {
                seq: 1,
                exit_code: 0
            })
        );
    }

    #[test]
    fn wrapper_script_contains_key_elements() {
        let nonce = "testnonce";
        let script = wrapper_script(nonce, None);

        // 必须用 NUL 读取，且不依赖任何外部命令。
        assert!(script.contains("read -r -d ''"));
        assert!(!script.contains("base64"), "不得依赖 base64");
        // 状态保留：在同一 shell 内 eval。
        assert!(script.contains("eval \"$mfperch_cmd\""));
        // 断开命令 stdin，防止交互式命令吃掉后续帧。
        assert!(script.contains("< /dev/null"));
        // 打印带 nonce 的结束标记。
        assert!(script.contains(END_MARKER_PREFIX));
        assert!(script.contains(nonce));
        // 就绪标记走 stderr。
        assert!(script.contains(READY_MARKER_PREFIX));
    }

    #[test]
    fn wrapper_script_includes_init_script_when_provided() {
        let script = wrapper_script("n", Some("export FOO=bar"));
        assert!(script.contains("export FOO=bar"));

        let without = wrapper_script("n", None);
        assert!(!without.contains("export FOO=bar"));

        // 空白初始化脚本应被忽略。
        let blank = wrapper_script("n", Some("   \n  "));
        assert!(!blank.contains("初始化脚本"));
    }

    #[test]
    fn askpass_script_writes_marker_to_stderr() {
        // 这是 WSL 实测发现的缺陷修正：标记必须走 stderr。
        let script = askpass_script("nonce1", "/home/u/.mf-perch/sudopw.fifo");
        assert!(
            script.contains(">&2"),
            "请求标记必须写 stderr，否则 sudo 会把标记当密码"
        );
        assert!(script.contains(SUDO_REQUEST_PREFIX));
        assert!(script.contains("/home/u/.mf-perch/sudopw.fifo"));
        // 密码走 stdout。
        assert!(script.contains("printf '%s\\n' \"$mfperch_pw\""));
    }

    #[test]
    fn session_setup_creates_fifo_only_when_sudo_enabled() {
        let enabled = session_setup_script("n", true);
        assert!(enabled.contains("mkfifo"));
        assert!(enabled.contains(SUDO_FIFO_NAME));
        assert!(enabled.contains("EXPORT_ASKPASS=1"));
        assert!(enabled.contains("chmod 700"));

        let disabled = session_setup_script("n", false);
        assert!(
            !disabled.contains("mkfifo"),
            "deny 模式下不应创建 FIFO（fail-closed）"
        );
        assert!(disabled.contains("EXPORT_ASKPASS=0"));
    }

    #[test]
    fn session_setup_emits_env_snapshot() {
        let s = session_setup_script("n", false);
        assert!(s.contains("MFPERCH_PATH="));
        assert!(s.contains("MFPERCH_PWD="));
        assert!(s.contains("MFPERCH_BASH="));
    }

    #[test]
    fn sudo_function_uses_command_and_askpass() {
        let d = sudo_function_def("/home/u/.mf-perch/askpass");
        // command 避免无限递归。
        assert!(d.contains("command sudo"));
        assert!(d.contains("-A"));
        assert!(d.contains("SUDO_ASKPASS"));
        // 空提示语，避免污染输出解析。
        assert!(d.contains("-p ''"));
        // 函数需导出给子 bash 进程。
        assert!(d.contains("export -f sudo"));
    }

    #[test]
    fn shell_invocation_matches_env_mode() {
        use crate::domain::host::ShellEnvMode;
        let (prog, args) = shell_invocation(ShellEnvMode::LoginThenTask);
        assert_eq!(prog, "bash");
        assert!(args.contains(&"-l"), "登录模式必须带 -l（D4）");

        let (prog2, args2) = shell_invocation(ShellEnvMode::CleanThenTask);
        assert_eq!(prog2, "bash");
        assert!(args2.contains(&"--noprofile"));
        assert!(args2.contains(&"--norc"));
    }
}
