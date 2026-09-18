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
/// 提权通道的**密码提示标记**（D47 握手）。
///
/// 通过 `sudo -S -p '<本标记>'` 传入，sudo 需要密码时会把它打进 stderr
/// （实测：sudo-rs 形如 `[sudo: <标记>] Password: `，经典 sudo 就是标记本身）；
/// **凭证缓存有效时不会出现**——应用据此决定"要不要写密码"，且不依赖 sudo 文案。
pub const SUDO_PROMPT_PREFIX: &str = "__MF_SUDO_PROMPT__";

/// 解析出的会话事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    /// 会话已就绪，可以开始发送命令。
    Ready,
    /// 收到某条命令的结束标记。
    ///
    /// 用**应用侧生成的 command_id** 关联，而不是各自自增的序号（V5）：
    /// 远端序号在会话内从 1 开始，应用侧序号是数据库历史累加，
    /// 两者在恢复终端等场景下必然错位，会导致命令永远等不到结束标记。
    CommandFinished { command_id: String, exit_code: i32 },
    /// 提权通道上 sudo 正在**询问密码**（D47 握手）。
    ///
    /// 应用收到它才把密码写进**该通道的 stdin**。数据面**不会有**这个事件：
    /// 数据面的 sudo 已被 [`sudo_reject_shim`] 明确拒绝（D48）。
    ///
    /// ⚠️ **握手判定不要依赖本变体**：密码提示**不带换行**，按行解析通常看不到它。
    /// 就绪阶段的判定请用 [`take_sudo_prompt`]（直接在字节流上摘标记）；
    /// 本变体只在提示恰好被换行终止时出现，用于让上层观察/告警。
    SudoPrompt,
    /// 普通输出行（命令产生的输出）。
    OutputLine(String),
}

/// 生成每会话随机 nonce（128 位）。
///
/// nonce 用十六进制表示，避免与 base64 字符集混淆，也便于在日志中比对。
///
/// **已知残余风险（V2，未能完全消除）**：nonce 是包装脚本内的 shell 变量，
/// 而被 `eval` 的命令运行在同一个 shell 中，因此 Agent 下发的命令可以读到它
/// （`echo "$mfperch_nonce"`）。这只是**防止误判**（命令输出恰好长得像标记），
/// 不是对抗性安全边界。
///
/// 提高位宽可让"盲猜 nonce"不可行，但不能阻止主动读取。真正消除该风险需要
/// 把协议标记改由独立通道传递，属于较大的协议变更，已记录为待评估项。
pub fn new_nonce() -> String {
    use rand::Rng;
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 构造远端包装脚本（D3 第 2.3 节 + D48 sudo 垫片）。
///
/// 关键点：
/// - `read -r -d ''` 读到 NUL 为止——bash 内建，不依赖外部命令；
/// - `eval` 在同一 shell 进程内执行，因此 `cd` / `export` 状态保留；
/// - `< /dev/null` 断开每条命令的 stdin，防止交互式命令吃掉后续命令帧；
/// - **命令号由应用生成、随帧首行下发**，远端在结束标记里原样回显
///   （V5 / D33）。脚本**不再**自己编号——两侧各自自增会在恢复终端等场景下
///   错位，导致命令永远等不到自己的结束标记。
/// - 打印结束标记前显式 `set +e`：`set -e` 会跨帧留在 shell 状态里，
///   否则一条失败命令会让整个包装脚本退出、结束标记永不回来。
///
/// `elevation_allowed` 决定垫片给出的指引：为真时告诉 Agent 改用 `run_as_root`
/// 工具，为假时说明该主机已禁用提权。两种情况**都拒绝执行**——数据面没有
/// 任何拿到密码的途径，放行只会让 Agent 收到一句难以理解的 sudo 报错。
///
/// 启动方式（登录 shell / 干净模式）由 [`shell_invocation`] 决定，
/// 脚本本身不含该差异——保证两种模式下协议行为完全一致。
pub fn wrapper_script(nonce: &str, init_script: Option<&str>, elevation_allowed: bool) -> String {
    // 初始化脚本在包装循环之前执行一次，用于 nvm / conda 等显式加载（D4）。
    let init = match init_script {
        Some(s) if !s.trim().is_empty() => format!("# 主机配置的初始化脚本\n{}\n", s.trim()),
        _ => String::new(),
    };

    // 数据面 sudo 拦截（D47/D48）：提权已改为**独立的特权通道**，
    // 数据面不再具备任何提权能力，因此这里装的是"明确拒绝"而非密码注入。
    let sudo_setup = sudo_reject_shim(elevation_allowed);

    format!(
        r#"set +e
mfperch_nonce='{nonce}'
{init}{sudo_setup}printf '{ready}{nonce}__\n' >&2
while IFS= read -r -d '' mfperch_frame; do
  # 帧格式：第一行是应用侧生成的 command_id，其余为命令正文（可能含换行）。
  mfperch_id=${{mfperch_frame%%$'\n'*}}
  mfperch_cmd=${{mfperch_frame#*$'\n'}}
  eval "$mfperch_cmd" < /dev/null
  mfperch_rc=$?
  # `set -e` 会**跨帧**留在 shell 状态里（它本来就是合法的持久状态）：
  # Agent 先用一条命令打开它，**下一条**命令只要失败，整个包装脚本就当场退出。
  # 打印结束标记这一步因此必须显式 `set +e`，否则结束标记永远不来、
  # 应用侧只能干等到超时。实测形态（WSL Ubuntu，两帧：`set -e` → `/nonexistent`）：
  #   无 `set +e`：只回第一帧的 END，包装脚本退出码 127，第二帧的 END 永不出现；
  #   有 `set +e`：两帧 END 都回，退出码 0。
  # 注意**不能**写成"eval 之后 set -e 就立刻生效"——errexit 在 eval 整串结束后才生效，
  # 所以同一帧里写 `set -e; <失败命令>` 不会触发——**别**按"errexit 立刻生效"写回归用例，
  # 那样构造出的场景根本不存在，用例会假绿。
  # 守住它的是 `wrapper_always_emits_end_marker_even_after_set_e`（单测，查顺序）
  # 与 `run_as_root_returns_when_command_enables_set_e`（真实环境 e2e，查"是否超时"）。
  set +e
  printf '\n{end}%s__%s__%s__\n' "$mfperch_nonce" "$mfperch_id" "$mfperch_rc"
done
"#,
        nonce = nonce,
        init = init,
        sudo_setup = sudo_setup,
        ready = READY_MARKER_PREFIX,
        end = END_MARKER_PREFIX,
    )
}


/// 启动包装脚本时使用的 bash 参数（D4）。
///
/// - `LoginThenTask`：`bash -l -c '<脚本>'` —— 登录 shell，加载 `/etc/profile`
///   与 `~/.bash_profile`，最接近人类 SSH 登录；
/// - `CleanThenTask`：`bash --noprofile --norc -c '<脚本>'` —— 干净环境。
///
/// **必须真正接到远端启动路径上**（由 [`wrapper_launch_command`] 交给远端执行）：
/// 只在类型里存着、却没拼进实际启动命令，效果是设置页里"环境加载方式"这个
/// **用户可见的开关完全没有效果**，而且不报任何错
/// （实测：登录模式下 `~/.bash_profile` 里加的 `/opt/...` 不会出现在 PATH 中）。
///
/// **privileged 通道刻意只用这一对参数、不额外包登录 shell**：那条通道上
/// 命令以 `sudo` 的 `env_reset` 语义运行，登录 shell 加载的 PATH 会被 sudo
/// 重置（见 D47 的 PoC 结论），包了也等于没包，只会多一层难排查的嵌套。
pub fn shell_invocation(env_mode: crate::domain::host::ShellEnvMode) -> (&'static str, &'static [&'static str]) {
    use crate::domain::host::ShellEnvMode;
    match env_mode {
        ShellEnvMode::LoginThenTask => ("bash", &["-l", "-c"]),
        ShellEnvMode::CleanThenTask => ("bash", &["--noprofile", "--norc", "-c"]),
    }
}

/// 构造**数据面**包装脚本的启动命令（D4 + D47）。
///
/// 脚本作为 `-c` 的**单个参数**传入并做单引号转义：内容不被外层 shell 展开，
/// 因此脚本里的 `$`、引号、换行都原样到达，NUL 分帧协议不受影响。
pub fn wrapper_launch_command(env_mode: crate::domain::host::ShellEnvMode, script: &str) -> String {
    let (program, args) = shell_invocation(env_mode);
    // 参数全是静态字面量（`-l` / `-c` 等），无需转义；脚本必须转义。
    format!("{program} {} {}", args.join(" "), shell_single_quote(script))
}

/// 数据面的 sudo 垫片：**明确拒绝**并给出可操作指引（D47 / D48）。

///
/// ## 为什么数据面上必须拒绝，而不是"让它自然失败"
///
/// 提权已改为应用自建的**特权通道**（`run_as_root`）：数据面不再部署 askpass、
/// 不建 FIFO、也没有任何环境变量携带密码。若不在数据面拦下 `sudo`，Agent
/// 得到的会是 sudo 自己的一句难懂报错（`no tty present` / `a password is
/// required` / `sorry, you must have a tty`），它极可能反复重试或误判为环境问题。
///
/// 拦下之后 Agent 收到的是**可操作**的一句话：改用 `run_as_root` 工具。
/// 这并没有新增权限限制——数据面本来就已经没有任何提权能力。
///
/// 用 shell 函数而非 PATH 上的可执行文件：不落任何远端文件、不依赖远端工具、
/// 随会话消失；`export -f` 之后对 `bash -c` 子进程同样生效（Agent 的命令
/// 常常在自己的 bash 里跑）。
pub fn sudo_reject_shim(elevation_allowed: bool) -> String {
    let reason = if elevation_allowed {
        "数据面不允许提权；请改用 run_as_root 工具以特权身份执行该命令"
    } else {
        "该主机已禁用提权（sudo 策略为「禁止注入」）；如需提权请由人类在主机设置中开启"
    };
    format!(
        "sudo() {{\n  printf '%s\\n' '[mf-perch] {reason}' >&2\n  return 1\n}}\nexport -f sudo\n"
    )
}

/// 环境快照命令（D4）：只输出三项，供人类排查"命令找不到"类问题。
///
/// 本函数**只读不写**：不在远端部署任何文件（D49 的不变式——远端不得出现本应用的痕迹）。
///
/// 输出走 stderr，与包装脚本的协议标记一致，便于上层按同一个通道解析。
pub fn env_snapshot_script() -> &'static str {
    "printf 'MFPERCH_PATH=%s\\n' \"$PATH\" >&2\nprintf 'MFPERCH_PWD=%s\\n' \"$PWD\" >&2\nprintf 'MFPERCH_BASH=%s\\n' \"$BASH_VERSION\" >&2\n"
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

    // sudo 的密码提示（D47 握手）。
    //
    // 用**包含**判断而非整行匹配：sudo 会把我们传入的 `-p` 文本包进它自己的文案里
    // （实测 sudo-rs 为 `[sudo: <标记>] Password: `，经典 sudo 就是标记本身）。
    // nonce 是随机值，误判概率可忽略；且**握手只允许写一次密码**（见
    // [`SudoAuthHandshake`]），所以即使标记被伪造，代价也不是密码泄露。
    if trimmed.contains(&sudo_prompt_marker(nonce)) {
        return Some(SessionEvent::SudoPrompt);
    }

    // 结束标记：__MF_PERCH_END__<nonce>__<command_id>__<rc>__
    if let Some(rest) = trimmed.strip_prefix(END_MARKER_PREFIX) {
        // 只有 nonce 匹配才算数——这是防误判的关键。
        if let Some(after_nonce) = rest.strip_prefix(&format!("{nonce}__")) {
            let parts: Vec<&str> = after_nonce.trim_end_matches('_').split("__").collect();
            if parts.len() == 2 {
                if let Ok(rc) = parts[1].parse::<i32>() {
                    let command_id = parts[0].to_string();
                    if !command_id.is_empty() {
                        return Some(SessionEvent::CommandFinished {
                            command_id,
                            exit_code: rc,
                        });
                    }
                }
            }
        }
        // nonce 不匹配：这是命令自己输出的类似文本，按普通输出处理。
        return Some(SessionEvent::OutputLine(trimmed.to_string()));
    }

    Some(SessionEvent::OutputLine(trimmed.to_string()))
}

/// 提权通道传给 `sudo -p` 的密码提示标记（D47）。
pub fn sudo_prompt_marker(nonce: &str) -> String {
    format!("{SUDO_PROMPT_PREFIX}{nonce}__")
}

/// 把一段文本安全地包成**一个** shell 词（单引号形式）。
///
/// 单引号内无法转义，POSIX 的标准做法是"结束单引号 → 插入 `\'` → 重新开启"：
/// `a'b` → `'a'\''b'`。包装脚本与提示标记都要经它嵌入命令行。
///
/// 对上层公开（D47）：提权编排要把数据面的**工作目录**嵌进特权通道的命令，
/// 目录名可能含空格、引号等字符，必须走同一套转义——两处各写一份
/// 迟早会出现"一处修了、另一处没修"的注入缺口。
pub fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// 构造**提权通道**的启动命令（D47）。
///
/// 形如：`sudo -S -p '<提示标记>' bash -c '<包装脚本>'`
///
/// - `-S`：密码从 **stdin** 读——本通道的 stdin 正是应用持有的私有管道，
///   由上层按 [`SudoAuthHandshake`] 的结论写入（看到提示标记才写）；
/// - `-p <提示标记>`：让 sudo 的提示带上**我们自己的随机标记**，
///   应用据此判断"要不要写密码"，不依赖 sudo 的文案（实测两种实现文案不同）；
/// - 包装脚本必须作为**单个参数**传给 `bash -c`，因此做单引号转义
///   （脚本自身含单引号，不能直接拼接）。
///
/// 这样提权通道跑的是**同一套包装循环**（NUL 分帧 + nonce 结束标记），
/// 因此提权命令的输出与退出码能和普通命令一样被精确归属。
pub fn privileged_wrapper_command(nonce: &str, script: &str) -> String {
    format!(
        "sudo -S -p {} bash -c {}",
        shell_single_quote(&sudo_prompt_marker(nonce)),
        shell_single_quote(script)
    )
}

/// 从原始字节流中**摘除第一个**密码提示标记，返回是否摘到（D47）。
///
/// 为什么不能靠按行切分：**密码提示是不带换行的**（sudo 写完提示就等输入），
/// 按 `\n` 分行的解析永远看不到它——那会变成"应用等提示、sudo 等密码"的死锁。
/// 因此握手判定直接在字节流上做，并把标记摘掉，避免同一次提示被重复计数
/// （重复计数会被误判成"密码被拒"）。
pub fn take_sudo_prompt(buf: &mut Vec<u8>, nonce: &str) -> bool {
    let needle = sudo_prompt_marker(nonce);
    let needle = needle.as_bytes();
    if needle.is_empty() || buf.len() < needle.len() {
        return false;
    }
    match buf.windows(needle.len()).position(|w| w == needle) {
        Some(pos) => {
            buf.drain(pos..pos + needle.len());
            true
        }
        None => false,
    }
}

/// 探测数据面当前目录的命令（D47：cwd 自动继承）。
///
/// 刻意**不把 `$PWD` 编进结束标记**：路径可能含 `__` 甚至换行，而标记用 `__`
/// 分隔、又不能用 `base64`（目标机不保证安装），纯 bash 内建做十六进制对多字节
/// 路径不可靠。改为"发一条探测命令、按结束标记界定整段输出"——
/// **天然容忍特殊字符**，且完全不改动标记格式。
pub fn pwd_probe_command() -> &'static str {
    // 用 printf 的 %s（不加换行），避免把换行算进路径。
    "printf '%s' \"$PWD\""
}

/// 提权通道上**核实实际身份**的命令（D47）。
///
/// 为什么不能只靠"提权成功"就标注为 root（uid=0）：
/// sudoers 可以配置成 `user ALL=(someuser) ...`——`sudo -S ... bash` 此时
/// **成功**，但落到的是非 0 的目标用户。若审计一律写 `uid=0`，命令历史里
/// 就会出现一句假话（P2：降级/偏差必须显式告知）。
///
/// `${EUID}` 是 bash 内建变量，不依赖外部 `id` 命令；用户名用 `$(id -un ...)`
/// 尽力而为，取不到就不写名字——数字 uid 才是审计的事实依据。
/// 与命令**同一次提权里连着执行**，因此不需要额外往返。
pub fn privileged_identity_command() -> &'static str {
    "printf '__MF_PERCH_UID__%s|%s\\n' \"${EUID:-?}\" \"$(id -un 2>/dev/null)\""
}

/// 从提权通道的输出里摘出身份行，返回 `(实际 uid, 用户名)`（D47）。
///
/// 同时把该行从输出中**移除**：它属于应用的核实动作，不是 Agent 命令的产出，
/// 不应混进 `run_as_root` 的返回内容里。
pub fn take_privileged_identity(output: &mut String) -> Option<(u32, Option<String>)> {
    const MARK: &str = "__MF_PERCH_UID__";
    let mut found = None;
    let mut kept = String::with_capacity(output.len());

    for line in output.lines() {
        if let Some(rest) = line.trim_end().strip_prefix(MARK) {
            if found.is_none() {
                let (uid, name) = match rest.split_once('|') {
                    Some((u, n)) => (u, n),
                    None => (rest, ""),
                };
                // 只接受纯数字：非数字说明输出不是我们发的那条命令产生的，
                // 宁可当作"身份未知"，也不要把它当成 uid。
                if let Ok(value) = uid.trim().parse::<u32>() {
                    let name = name.trim();
                    found = Some((
                        value,
                        if name.is_empty() {
                            None
                        } else {
                            Some(name.to_string())
                        },
                    ));
                }
            }
            // 无论是否解析成功，这一行都不回传给 Agent。
            continue;
        }
        kept.push_str(line);
        kept.push('\n');
    }

    if found.is_some() {
        // 命令原本可能不以换行结尾；这里统一按行重建，仅在确实摘掉了身份行时替换。
        while kept.ends_with('\n') {
            kept.pop();
        }
        *output = kept;
    }
    found
}

/// 握手时"是否要写密码"的结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SudoAuthStep {
    /// 现在写密码（**每条通道最多一次**）。
    SendPassword,
    /// 不写：已经决定过，或已经认证成功。
    Ignore,
}

/// 提权通道的密码握手状态机（D47）。
///
/// 规则只有一条：**每条通道最多写一次密码**。
///
/// 为什么必须一次性：`sudo -S` 从该通道的 stdin 读密码。若允许"见到提示就写"，
/// 那么在 sudo 已经认证成功之后再出现的提示标记（例如提权后的命令自己打印，
/// 或伪造）会让应用把密码再写一次——这些字节会滞留在 stdin 里、**被后续命令帧
/// 当成命令执行并随输出回传给 Agent**，反而造成泄露。
#[derive(Debug, Default)]
pub struct SudoAuthHandshake {
    decided: bool,
}

impl SudoAuthHandshake {
    pub fn new() -> Self {
        Self::default()
    }

    /// 观察到 sudo 的密码提示标记。
    pub fn on_prompt(&mut self) -> SudoAuthStep {
        if self.decided {
            return SudoAuthStep::Ignore;
        }
        self.decided = true;
        SudoAuthStep::SendPassword
    }

    /// 观察到"已经认证通过"（就绪标记、命令开始产出输出、进程结束等）。
    ///
    /// 之后任何提示标记都不再触发写密码。
    pub fn on_authenticated(&mut self) {
        self.decided = true;
    }

    /// 是否已有结论（供上层判定"可以停止等待认证结果了"）。
    pub fn is_decided(&self) -> bool {
        self.decided
    }
}

/// 把一条命令编码为 NUL 结尾的帧（V5）。
///
/// 帧格式：`<command_id>\n<command>`。首行的 command_id 由应用生成，
/// 远端原样回显在结束标记中，从而**按 id 精确配对**，不依赖两侧各自自增的序号。
pub fn encode_frame(command_id: &str, command: &str) -> Result<Vec<u8>> {
    if command_id.is_empty() {
        return Err(AppError::InvalidArgument("command_id 不能为空".into()));
    }
    // id 是首行分隔符，自身不能含换行；NUL 会破坏分帧，两者都不允许。
    if command_id.contains('\n') || command_id.as_bytes().contains(&0) {
        return Err(AppError::InvalidArgument(
            "command_id 不能包含换行或 NUL 字节".into(),
        ));
    }
    if command.as_bytes().contains(&0) {
        return Err(AppError::InvalidArgument(
            "命令不能包含 NUL 字节".into(),
        ));
    }

    let mut frame = Vec::with_capacity(command_id.len() + command.len() + 2);
    frame.extend_from_slice(command_id.as_bytes());
    frame.push(b'\n');
    frame.extend_from_slice(command.as_bytes());
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
        assert_eq!(a.len(), 32, "32 个十六进制字符表示 16 字节（128 位）");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b, "每次生成的 nonce 应不同");
    }

    #[test]
    fn encode_frame_prefixes_command_id_then_nul() {
        let f = encode_frame("c1", "ls -la").unwrap();
        assert_eq!(f.last(), Some(&0));
        // 帧 = <command_id>\n<command>\0
        assert_eq!(&f[..f.len() - 1], b"c1\nls -la");
    }

    #[test]
    fn encode_frame_keeps_newlines_quotes_and_dollars() {
        // NUL 分帧的价值：这些字符无需任何转义。
        let cmd = "cd /tmp && echo \"a $HOME\" ; ls\npwd";
        let f = encode_frame("c9", cmd).unwrap();
        let body = &f[..f.len() - 1];
        let expected = format!("c9\n{cmd}");
        assert_eq!(body, expected.as_bytes());
    }

    #[test]
    fn encode_frame_rejects_nul_in_command() {
        assert!(encode_frame("c1", "echo \0 bad").is_err());
    }

    #[test]
    fn encode_frame_rejects_empty_or_multiline_command_id() {
        assert!(encode_frame("", "ls").is_err());
        assert!(encode_frame("a\nb", "ls").is_err());
    }

    #[test]
    fn parse_ready_marker() {
        let nonce = "abc123";
        let line = format!("{READY_MARKER_PREFIX}{nonce}__");
        assert_eq!(parse_line(&line, nonce), Some(SessionEvent::Ready));
    }

    #[test]
    fn parse_command_finished_marker() {
        let nonce = "deadbeef";
        let line = format!("{END_MARKER_PREFIX}{nonce}__cmd3__0__");
        assert_eq!(
            parse_line(&line, nonce),
            Some(SessionEvent::CommandFinished {
                command_id: "cmd3".into(),
                exit_code: 0
            })
        );

        let fail = format!("{END_MARKER_PREFIX}{nonce}__cmd7__1__");
        assert_eq!(
            parse_line(&fail, nonce),
            Some(SessionEvent::CommandFinished {
                command_id: "cmd7".into(),
                exit_code: 1
            })
        );
    }

    #[test]
    fn parse_negative_exit_code() {
        // 被信号终止的命令退出码可能为负。
        let nonce = "cafe";
        let line = format!("{END_MARKER_PREFIX}{nonce}__cmd1__-9__");
        assert_eq!(
            parse_line(&line, nonce),
            Some(SessionEvent::CommandFinished {
                command_id: "cmd1".into(),
                exit_code: -9
            })
        );
    }

    #[test]
    fn marker_with_wrong_nonce_is_treated_as_output() {
        // 关键防误判场景：命令输出恰好包含类似标记，但 nonce 不同。
        let nonce = "realnonce";
        let spoofed = format!("{END_MARKER_PREFIX}othernonce__cmd1__0__");
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

    /// 守的不变式：sudo 把我们的提示标记包进它自己的文案时，仍能被识别（D47 握手）。
    ///
    /// 实测两种实现的形态不同——sudo-rs 为 `[sudo: <标记>] Password: `，
    /// 经典 sudo 就是标记本身——因此这里对**两种形态**都必须成立。
    #[test]
    fn sudo_prompt_marker_is_recognized_in_both_sudo_flavors() {
        let nonce = "abc123";
        let marker = sudo_prompt_marker(nonce);
        let cases = [
            (format!("[sudo: {marker}] Password: "), "sudo-rs 形态"),
            (marker.clone(), "经典 sudo 形态"),
        ];
        for (line, why) in cases {
            assert_eq!(
                parse_line(&line, nonce),
                Some(SessionEvent::SudoPrompt),
                "{why} 下应识别出密码提示：{line:?}"
            );
        }
    }

    /// 提示标记不是万能的：nonce 不匹配时不得触发握手，
    /// 否则任何输出都能诱导应用去写密码。
    #[test]
    fn sudo_prompt_with_other_nonce_is_not_a_prompt() {
        let line = "[sudo: __MF_SUDO_PROMPT__othernonce__] Password: ";
        assert_eq!(
            parse_line(line, "realnonce"),
            Some(SessionEvent::OutputLine(line.to_string())),
            "nonce 不匹配的提示文本应按普通输出处理"
        );
    }

    /// 守的不变式：**每条提权通道最多写一次密码**（D47）。
    ///
    /// 若允许重复写：sudo 认证成功之后再出现的提示标记（伪造，或提权后的命令自己打印）
    /// 会让密码又一次进入该通道的 stdin，被当成命令帧执行并随输出回传给 Agent——
    /// 本想防泄露，反而造成泄露。
    #[test]
    fn handshake_sends_password_at_most_once_per_channel() {
        let mut hs = SudoAuthHandshake::new();
        assert_eq!(hs.on_prompt(), SudoAuthStep::SendPassword, "首次提示应写密码");
        assert!(hs.is_decided());
        assert_eq!(
            hs.on_prompt(),
            SudoAuthStep::Ignore,
            "再次提示不得重复写密码"
        );
    }

    /// 认证成功之后，任何提示都不得再触发写密码。
    #[test]
    fn handshake_ignores_prompts_after_authentication() {
        let mut hs = SudoAuthHandshake::new();
        hs.on_authenticated();
        assert_eq!(
            hs.on_prompt(),
            SudoAuthStep::Ignore,
            "已认证成功后不得再写密码"
        );
    }

    /// 守的不变式：转义后的文本必须仍是**一个** shell 词，且内容原样可还原。
    ///
    /// 这是提权通道能启动的前提——包装脚本自身含单引号（`mfperch_nonce='…'`），
    /// 若转义写错，脚本会被 shell 拆成多个词，远端直接报语法错误。
    #[test]
    fn shell_single_quote_keeps_text_as_one_word() {
        let cases = [
            ("abc", "'abc'"),
            ("", "''"),
            ("a'b", r"'a'\''b'"),
            ("a'b'c", r"'a'\''b'\''c'"),
        ];
        for (input, expected) in cases {
            assert_eq!(
                shell_single_quote(input),
                expected,
                "输入 {input:?} 的单引号转义不符"
            );
        }
    }

    /// 提权通道的启动命令：必须用 `-S`、带上本会话的提示标记，且包装脚本是一个参数。
    #[test]
    fn privileged_command_uses_stdin_and_prompt_marker() {
        let nonce = "n1";
        let cmd = privileged_wrapper_command(nonce, "echo hi");
        assert!(
            cmd.starts_with("sudo -S -p '"),
            "必须用 `-S` 且设置提示标记：{cmd}"
        );
        assert!(
            cmd.contains(&sudo_prompt_marker(nonce)),
            "应包含本会话的提示标记：{cmd}"
        );
        assert!(
            cmd.ends_with(r"bash -c 'echo hi'"),
            "包装脚本应作为单个参数传给 bash -c：{cmd}"
        );
    }

    /// 守的不变式：提示标记必须能从**不带换行**的字节流里摘出来（D47）。
    ///
    /// 若只能按行识别，就会出现"应用等提示、sudo 等密码"的死锁——
    /// 这正是 R2 首次实现时踩到的缺陷（潜伏到 e2e 才暴露）。
    #[test]
    fn take_sudo_prompt_finds_marker_without_newline_and_removes_it() {
        let nonce = "n9";
        let marker = sudo_prompt_marker(nonce);

        // sudo-rs 形态：标记被包在它自己的文案里，且**没有换行**
        let mut buf = format!("[sudo: {marker}] Password: ").into_bytes();
        assert!(take_sudo_prompt(&mut buf, nonce), "无换行也应能摘到标记");
        assert!(
            !String::from_utf8_lossy(&buf).contains(&marker),
            "摘除后不应残留标记：{:?}",
            String::from_utf8_lossy(&buf)
        );

        // 没有标记时不得误判
        let mut none = b"\xe4\xb8\xad\xe6\x96\x87 output\n".to_vec();
        assert!(!take_sudo_prompt(&mut none, nonce), "无标记时不应摘到");

        // 两次提示要能数出两次（用于识别"密码被拒"），且摘完不再重复计数
        let mut twice = format!("{marker}{marker}").into_bytes();
        assert!(take_sudo_prompt(&mut twice, nonce), "第一次应摘到");
        assert!(take_sudo_prompt(&mut twice, nonce), "第二次应摘到");
        assert!(!take_sudo_prompt(&mut twice, nonce), "摘完不应再摘到");
    }

    /// 守的不变式：**身份核实行必须从 Agent 看到的输出里摘掉**，
    /// 且只有本应用真的打印了那一行时才改写输出（D47）。
    ///
    /// 若摘不掉：`run_as_root` 的返回里会多出一条与应用无关的协议文本；
    /// 若误摘：Agent 命令自己打印的相似文本会被吞掉。
    #[test]
    fn privileged_identity_is_extracted_and_stripped() {
        // 正常形态：身份行夹在命令输出中间。
        let mut out = "before\n__MF_PERCH_UID__0|root\nafter".to_string();
        assert_eq!(
            take_privileged_identity(&mut out),
            Some((0, Some("root".to_string()))),
            "应解析出实际 uid 与用户名"
        );
        assert_eq!(out, "before\nafter", "身份行必须被摘掉");

        // 非 root：sudoers 把目标用户配成别人时，uid 必须如实报告（不能假装 0）。
        let mut other = "__MF_PERCH_UID__1002|ops\ncmd".to_string();
        assert_eq!(
            take_privileged_identity(&mut other),
            Some((1002, Some("ops".to_string())))
        );
        assert_eq!(other, "cmd");

        // 用户名取不到（无 id 命令）：只报 uid，不算失败。
        let mut noname = "__MF_PERCH_UID__0|\ncmd".to_string();
        assert_eq!(take_privileged_identity(&mut noname), Some((0, None)));

        // 非数字 uid：**不接受**，宁可"身份未知"也不要把别的东西当 uid。
        let mut garbage = "__MF_PERCH_UID__notanumber|x\ncmd".to_string();
        assert_eq!(take_privileged_identity(&mut garbage), None);

        // 完全没有该行：输出原样返回，不得改动（否则会吞掉 Agent 的输出）。
        let mut plain = "line1\nline2".to_string();
        assert_eq!(take_privileged_identity(&mut plain), None);
        assert_eq!(plain, "line1\nline2");
    }

    /// 守的不变式：**结束标记必须无条件打出来**（`set +e` 要在它之前）。
    ///
    /// 背景（R3 实测踩到）：Agent 的命令如果真的失败到让 shell 改变状态——
    /// 例如命令名不存在触发 `set -e`——那么"打印结束标记"这一步会被跳过，
    /// 于是应用侧永远等不到结束标记，命令**卡到超时**（表现为"提权命令
    /// 30 秒未结束"，而远端其实早已返回 127）。
    ///
    /// 这条断言刻意检查**顺序**而不只是"脚本里出现过 set +e"：
    /// 语句存在但位置不对（在 `eval` 之前、或在 `while` 之外）同样防不住。
    #[test]
    fn wrapper_always_emits_end_marker_even_after_set_e() {
        let script = wrapper_script("nonce_x", None, false);

        let eval_at = script
            .find("eval \"$mfperch_cmd\"")
            .expect("包装脚本应 eval 命令正文");
        let reset_at = script[eval_at..]
            .find("set +e")
            .map(|i| i + eval_at)
            .expect("打印结束标记之前必须显式关掉 set -e");
        let marker_at = script
            .find(&format!("printf '\\n{END_MARKER_PREFIX}"))
            .expect("包装脚本应打印结束标记");

        assert!(
            reset_at > eval_at && reset_at < marker_at,
            "`set +e` 必须夹在 eval 与结束标记之间（eval@{eval_at} reset@{reset_at} marker@{marker_at}）"
        );
    }

    #[test]
    fn crlf_is_normalized() {
        // 注意：`nonce` 与 `line` 都在此处定义——它是本用例的输入。
        let nonce = "n2";
        let line = format!("{END_MARKER_PREFIX}{nonce}__cmd1__0__\r\n");
        assert_eq!(
            parse_line(&line, nonce),
            Some(SessionEvent::CommandFinished {
                command_id: "cmd1".into(),
                exit_code: 0
            })
        );
    }

    #[test]
    fn wrapper_script_contains_key_elements() {
        let nonce = "testnonce";
        let script = wrapper_script(nonce, None, false);

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
        let script = wrapper_script("n", Some("export FOO=bar"), false);
        assert!(script.contains("export FOO=bar"));

        let without = wrapper_script("n", None, false);
        assert!(!without.contains("export FOO=bar"));

        // 空白（或仅空白字符）的初始化脚本必须与"未提供"完全等价：
        // 不得往脚本里插入空的初始化段落。这里比对**整份脚本**，
        // 而不是"某句注释文本是否出现"——后者一改文案就失效，也抓不到行为回归。
        let blank = wrapper_script("n", Some("   \n  "), false);
        assert_eq!(blank, without, "空白初始化脚本不应改变生成的脚本");
    }

    #[test]
    fn wrapper_script_always_installs_reject_shim() {
        // D47/D48 核心：数据面**不许提权**——不论主机策略如何，都必须装上垫片，
        // 让 `sudo` 明确失败并告诉 Agent 改用 run_as_root。
        for allowed in [true, false] {
            let script = wrapper_script("n", None, allowed);
            assert!(script.contains("sudo()"), "应定义 sudo 垫片：{allowed}");
            assert!(script.contains("export -f sudo"), "应导出给子 bash 进程");
        }
        // 数据面不得出现任何密码投递载体——重新引入就恢复了 V1 的攻击面（D49）。
        let script = wrapper_script("n", None, true);
        for forbidden in ["SUDO_ASKPASS", "askpass", "sudo -A", "mkfifo", ".mf-perch"] {
            assert!(
                !script.contains(forbidden),
                "数据面脚本不得出现密码投递载体 {forbidden:?}：\n{script}"
            );
        }
    }

    /// 守的不变式：垫片必须**真的拒绝**（非 0 返回），并给出可操作指引。
    ///
    /// 只断言"含有某句话"是不够的：Agent 需要的是"这条命令不会被执行"。
    /// 因此这里同时检查返回码与两种策略下的文案差异。
    #[test]
    fn reject_shim_refuses_and_guides_to_the_tool() {
        let allowed = sudo_reject_shim(true);
        assert!(allowed.contains("return 1"), "垫片必须以非 0 返回：{allowed}");
        assert!(
            allowed.contains("run_as_root"),
            "策略允许提权时应指引改用 run_as_root：{allowed}"
        );
        assert!(allowed.contains(">&2"), "拒绝说明应走 stderr，不污染命令输出");

        let denied = sudo_reject_shim(false);
        assert!(denied.contains("return 1"), "垫片必须以非 0 返回：{denied}");
        assert!(
            denied.contains("已禁用提权"),
            "策略禁止提权时应说明原因：{denied}"
        );
        // 禁止时不得给出"改用 run_as_root"的指引——那条路同样会被拒绝，
        // 指引它只会让 Agent 白试一次。
        assert!(
            !denied.contains("run_as_root"),
            "禁用提权时不应引导去用一个也会失败的入口：{denied}"
        );
        assert_ne!(allowed, denied, "两种策略的文案必须不同");
    }

    #[test]
    fn env_snapshot_script_emits_the_three_variables() {
        // D4：这三项是人类排查"命令找不到"类问题的依据。
        let s = env_snapshot_script();
        assert!(s.contains("MFPERCH_PATH="));
        assert!(s.contains("MFPERCH_PWD="));
        assert!(s.contains("MFPERCH_BASH="));
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
