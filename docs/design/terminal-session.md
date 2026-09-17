# 终端会话模型设计

> 状态：已确认（Q3）
> 本文解释"方案 C：常驻会话 + 可控 shell 包装"的具体实现逻辑。

## 1. 三种方案对比

| 维度 | 方案 A：裸 PTY + 提示符识别 | 方案 B：无状态执行 | 方案 C：常驻会话 + 结束标记（推荐） |
| ---- | ---- | ---- | ---- |
| 是否常驻会话 | 是 | 否 | 是 |
| 保留 cwd / env | 是 | 否 | 是 |
| 是否申请 PTY | 是 | 否 | 否 |
| 如何判定命令结束 | 识别 shell 提示符 | 进程退出 | 我们自己打印的结束标记 |
| 输出是否干净 | 否（回显、ANSI、分页） | 是 | 是 |
| 交互式全屏程序 | 可用 | 不可用 | 不可用 |
| 实现复杂度 | 高、不确定 | 低 | 中 |
| 判定可靠性 | 低（提示符易被污染） | 高 | 高 |

## 2. 方案 C 机制

### 2.1 建立会话

- 通过 SSH 打开一条连接，并开一个 session channel。
- **不申请 PTY**。因此远端不会出现终端回显、ANSI 控制序列，程序也不会误以为自己连着终端（不会自动上色、不会分页）。
- 远端执行的命令是一个包装脚本；**启动方式由主机的「环境加载方式」设置 `shell_env_mode` 决定**（D4），由 `protocol::wrapper_launch_command`（`src-tauri/src/ssh/protocol.rs`）生成，并在 `src-tauri/src/ssh/session.rs` 里经 `channel.exec(true, launch)` 启动：

  ```bash
  # login（默认）：登录 shell，加载 /etc/profile 与 ~/.bash_profile
  bash -l -c '<包装脚本源码>'
  # clean：干净模式，显式跳过配置文件
  bash --noprofile --norc -c '<包装脚本源码>'
  ```

  脚本作为 `-c` 的**单个参数**传入并做单引号转义，因此脚本里的 `$`、引号、换行都原样到达。该脚本常驻运行，其 stdin 就是 SSH channel 的输入，用于接收命令帧。两种模式的环境差异详见第 7 节。

  > **历史注记（已修复）**：这一对启动参数此前**没有调用方**——生产代码把脚本原样交给 `channel.exec`（等价 `bash -s`，既不是登录 shell，也不是显式的干净模式），于是设置页里的「环境加载方式」开关**完全不生效**（实测：登录模式下 `~/.bash_profile` 里加的 `/opt/...` 不会出现在 `PATH` 中）。现已接到启动路径上，该设置真正生效。

### 2.2 命令帧格式

向 channel 的 stdin 写入一段字节（`protocol::encode_frame`）：

```
<command_id>\n<命令内容原样><NUL 字节 0x00>
```

- 首行是**应用生成的 `command_id`**（V5 / D33），其余为命令正文。
- 以 **NUL 字节（`0x00`）作为帧分隔符**，不用换行、不用 base64。
- 命令内容原样写入，**无需编码、无需转义**，命令中的引号、`$`、分号、换行都不影响。
- 之所以可行：POSIX 的 `execve` 参数以 NUL 结尾，因此**任何 shell 命令都不可能包含 NUL 字节**，NUL 是天然安全的分隔符。
- 约束：`command_id` 不得含换行或 NUL，命令正文不得含 NUL；违反时 `encode_frame` 直接返回参数错误。

### 2.3 包装脚本（示意）

```bash
NONCE='<每会话随机串>'
while IFS= read -r -d '' frame; do
  # 帧首行是应用生成的 command_id，其余是命令正文（可能含换行）
  cmd_id=${frame%%$'\n'*}
  cmd=${frame#*$'\n'}
  eval "$cmd" < /dev/null
  rc=$?
  printf '\n__MF_PERCH_END__%s__%s__%s__\n' "$NONCE" "$cmd_id" "$rc"
done
```

关键点：

- `read -r -d ''` 是 **bash 内置功能**，读取到 NUL 字节为止，赋值给 `frame`。**不需要 base64、不需要任何外部命令。**
- `eval` 在**同一个 shell 进程**中执行 → `cd`、`export` 的效果保留。
- `< /dev/null` 让每条命令的 stdin 断开，防止交互式命令吃掉后续命令帧。
- 结束时打印带 `NONCE`、命令 ID、退出码的标记；ID 来自**帧首行的应用侧 `command_id`**，远端**不再自己编号**（V5 / D33）：会话内自增的序号与数据库历史序号是两套命名空间，终端重建后必然错位，会导致命令永远等不到结束标记。
- 读到 EOF（无 NUL）时 `read` 返回非零，循环自然退出，用于会话结束。

### 2.4 如何判定命令结束

- 持续读取 channel 的 stdout，逐行扫描。
- 直到出现 `__MF_PERCH_END__<NONCE>__<command_id>__<rc>__`：
  - 标记之前的全部内容 = 本条命令的输出（可流式实时推给人类界面）；
  - `rc` = 退出码；
  - 标记带 `NONCE` + `command_id`，即使命令输出中恰好出现类似字样也不会误判。
- 结束标记前先输出 `\n`，保证命令最后一行无换行时标记仍另起一行。

### 2.5 状态保留体现在哪

- 常驻 shell 进程 = 一个持续活着的 bash。
- `cd /var/log` 改变的是这个 bash 的工作目录；`export FOO=bar` 改的是它的环境变量。
- 下一条命令仍在同一进程中 `eval`，因此这些状态都还在。
- 会话结束（终端归档 / 连接断开）后状态自然消失，符合预期。

### 2.6 stdout 与 stderr 的区分

两种做法（实现时二选一或结合）：

1. SSH 协议本身把 stderr 作为 channel 的扩展数据流，russh 可分别接收，天然区分。
2. 在包装脚本内给 stderr 每行加前缀标记，再合并到 stdout。

## 3. 与方案 A 的本质区别

方案 A 依赖"看到 shell 提示符"来判断命令结束。提示符是**远端 shell 自己输出**的东西，会被以下情况破坏：

- 命令输出内容里包含类似提示符的文本；
- shell 配置改了 `PS1`、带了颜色转义；
- 交互式程序临时接管终端。

方案 C 判断依据是**我们自己打印的带随机 nonce 的标记**，不受远端配置影响，确定性高。

## 4. 已知限制（诚实记录）

1. **交互式全屏程序不可用**：`vim`、`top`、`less` 等需要 PTY，本方案不支持。这也符合"Agent 不应做这类操作"的定位。
2. **单条命令卡死时不能只杀它**：命令与包装脚本在同一 shell 进程内。处理方式：
   - 简单：超时后终止整条会话，终端标记为 broken，由 Agent 重建；
   - 更好：包装脚本把命令放入独立进程组，需要终止时经同一条 SSH 连接另开 channel 发送 `kill`。需额外实现，MVP 可先用简单方案。
3. **后台进程可能污染后续输出**：`cmd &` 的子进程 stdout 仍连着通道。需在输出层用标记包裹，或建议 Agent 显式重定向。
4. **输出量上限**：需做流式缓冲与单条命令输出上限，避免内存失控。
5. **远端依赖**：需要 `bash` 与 `read -d` 内置能力（bash 3.0+ 均支持）。**不再依赖 base64 等外部命令**。若远端无 bash，当前没有降级方案，只会以就绪超时报错（见第 6 节）。

## 5. 终端生命周期

- **创建**：建立 SSH 连接 → 开 channel → 启动包装脚本 → 握手（等待 ready 标记）→ 终端就绪。
- **执行命令**：写命令帧 → 流式接收输出 → 收到结束标记 → 记录命令历史（命令、输出、退出码、耗时）。
- **连接断开 / 应用重启**：终端状态变为 broken，人类可见；**Agent 下次执行命令时自动重建会话**
  （沿用原 ID 与历史，见 D39）。重建后 shell 状态已重置，结果里会带 `session_reconnected: true` 告知 Agent。
- **归档**：关闭 channel，保留命令历史供人类审计。

## 6. 关于外部命令依赖的说明

> 客户质疑：不能保证目标主机安装了 base64。以下为架构师回应。

**结论：已改为不依赖任何外部命令的实现（NUL 分帧 + bash 内建 `read -d`）。**

依赖对比：

| 实现方式 | 远端依赖 | 结论 |
| ---- | ---- | ---- |
| 行协议 + base64 解码 | `base64` 外部命令 | 已废弃，不可靠 |
| NUL 分帧 + `read -r -d ''` | 仅需 bash 内建能力 | **采用** |

关于 bash 本身的依赖：

- 包装脚本需要 bash（`read -d`、`printf` 为 bash 内建）。POSIX `sh`（dash、busybox ash）不支持 `read -d`，但主流 Linux 服务器默认存在 `/bin/bash`。
- **MVP 要求远端具备 bash**（覆盖率极高）。可选的两条降级路线（二期可选，均未实现）：用 POSIX `sh` + 逐行协议（需转义或改用其他分隔手段，复杂度回升）；或在创建终端时探测 `command -v bash`，缺失则直接报错。

**能力探测尚未实现——现状是"超时报错"而不是"明确错误"：**

- 代码里**没有任何 `command -v bash` 探测**（也没有等价的检查）；创建终端时不会先验证远端是否有 bash。
- `AppError::BashNotAvailable`（错误码 `bash_not_available`）虽然定义在 `src-tauri/src/error.rs`，但**从未被构造**——没有任何代码路径会返回它。
- 远端确实没有 bash 时的实际表现：包装脚本起不来 → 应用等不到 READY 标记 → **就绪超时**（30 秒，`READY_TIMEOUT`）后以 `ssh_connect_failed` 报错，文案为"远端会话未在 30 秒内就绪；请确认目标主机已安装 bash"（见 `src-tauri/src/ssh/session.rs`）。
- 总结：**MVP 要求远端具备 bash** 这条前提仍然成立；但"缺失时给出明确错误"目前只兑现了"不静默"——文案确实提示了 bash，代价却是等满 30 秒，错误码也不是 `bash_not_available`。真正的能力探测是待补项。

## 7. 环境变量加载策略

> 客户提问：我们创建的终端，会加载目标主机的环境变量吗？

### 7.1 先说结论

**取决于主机的「环境加载方式」设置（`shell_env_mode`，D4）：默认 `login` 走登录 shell（`bash -l -c`），会加载 `/etc/profile` 与 `~/.bash_profile`，与人类 SSH 登录基本一致；切到 `clean` 才只继承 sshd 提供的基础环境。** `clean` 模式会导致一个实际问题：用户写在 `~/.bashrc` / `~/.profile` 里的 PATH 扩展（如 `nvm`、`conda`、`pyenv`、自定义 `bin` 目录）在 Agent 的终端里**不存在**，于是 `node`、`conda`、`docker` 等命令会报 "command not found"。

> 该设置此前**不生效**（无论选哪个都按非登录、非交互启动，见 §2.1 的历史注记）；现已修复，两种模式按 §7.2 的表所述生效。

### 7.2 机制说明

| 环境来源 | 是否继承 | 说明 |
| ---- | ---- | ---- |
| sshd 设置的基础变量（`HOME`、`USER`、`LOGNAME`、`SHELL`、`TZ`、`SSH_CONNECTION` 等） | ✅ | 所有 SSH 会话都有 |
| `/etc/environment`、`pam_env`（若启用 PAM） | ✅ | 由 sshd 的 PAM 阶段注入 |
| sshd 自身的 `PATH` | ✅ | 通常为 `/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin` |
| `/etc/profile` | login ✅ / clean ❌ | 仅登录 shell 读取 |
| `~/.bash_profile` / `~/.profile` | login ✅ / clean ❌ | 仅登录 shell 读取 |
| `~/.bashrc` | ❌ | 两种模式都是非交互式 shell，默认不读取 |

原因：`clean` 模式用 `bash --noprofile --norc -c '<脚本>'` 显式跳过配置文件；`login` 模式（默认）用 `bash -l -c '<脚本>'`，属于**登录 shell**，会读取 `/etc/profile` 与 `~/.bash_profile`。两种模式都是**非交互**会话，因此 `~/.bashrc` 都不会被读取。

### 7.3 三种可选策略

| 策略 | 启动方式 | 优点 | 缺点 |
| ---- | ---- | ---- | ---- |
| S1 干净环境 | `bash --noprofile --norc -c`（设置值 `clean`） | 可预测、无副作用、启动快 | 缺少用户自定义 PATH |
| S2 登录 shell 环境（当前默认） | `bash -l -c`（设置值 `login`） | 加载 `/etc/profile` + `~/.bash_profile`，最接近人类 SSH 登录 | 可能输出欢迎语、执行耗时脚本、受 profile 内容影响 |
| S3 干净环境 + 显式初始化脚本 | S1 + 主机配置的初始化命令（`init_script`） | 精确可控，只加载需要的（如 `source ~/.nvm/nvm.sh`） | 需要用户手工配置 |

> S1 / S2 是每主机**逐台可选**的启动方式（设置项「环境加载方式」，即 `shell_env_mode`）；S3 是叠加在任一种之上的 `init_script`。

### 7.4 团队建议

采用 **S2 为默认 + S3 作为可选项**：

1. 默认用 `bash -l -c`（登录 shell，即「环境加载方式」的 `login`），让 Agent 拿到的环境和人类 SSH 登录后基本一致，减少"找不到命令"的困惑。该设置**逐主机保存、并真正作用于会话启动**（此前只写进设置、对会话没有影响，见 §2.1 的历史注记）。
2. 每个 SSH 主机可选配置**初始化脚本**（多行 shell），在包装脚本启动时先执行，用于 `nvm`/`conda` 等特殊环境。
3. **创建终端时捕获一次初始环境快照**（至少 `PATH`、`PWD`、`bash --version`），存入终端元数据，便于排查"命令找不到"类问题，也便于人类审计。
4. 输出解析上做保护：**READY 标记之前的所有输出一律丢弃**，避免 profile 的欢迎语污染第一条命令的输出。

### 7.5 与"状态保留"的关系

- 环境变量加载发生在会话建立时（profile 或初始化脚本），之后同一 shell 进程内的 `export`、`cd` 都会保留到后续命令。
- 若 Agent 执行 `export PATH=$PATH:/opt/bin`，这条改动同样保留，直到终端归档或断开。
