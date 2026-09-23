# 测试环境方案

> 状态：已确认（Q26 / D27）
> 本文解决一件事：**如何搭建并复用 Windows 侧的真实 SSH 联调环境**。选型理由在 D27，不复述。

## 1. 环境选型

测试目标环境 = **WSL Ubuntu 内的 `openssh-server`**（监听 `127.0.0.1:2223`），
Windows 侧通过**真实 SSH 协议**连接它，不用进程内 mock。
**选型理由与替代方案（Docker / 客户远端主机）的取舍见 [D27](../decisions/D27.md)，不要在此重新评估。**

> 操作提醒：**Git Bash 不是 Linux 测试目标**——它是 Cygwin，`bash` 内建行为差异大，
> 在它里面跑出来的结论不成立；本文的命令一律在 WSL 内执行。

## 2. 一次性准备（需客户执行，团队无法代劳）

`wsl --install` 需要管理员权限且需要重启：

```powershell
wsl --install -d Ubuntu
```

重启后首次进入 Ubuntu 设置用户名与密码，然后告知团队继续配置 SSH 服务端。

## 3. 搭建步骤（客户完成安装后由团队执行）

1. 安装并启动 SSH 服务端：
   ```bash
   sudo apt update && sudo apt install -y openssh-server
   sudo systemctl enable --now ssh   # WSL 需启用 systemd，或手动启动 sshd
   ```
2. 配置端口（避免与 Windows 侧冲突）：
   ```
   Port 2223
   ```
   **不要选 2222**：它是 SSH 的常见替代端口，客户机器上可能已有别的程序在监听；
   撞端口的症状与判读见 [`development-troubleshooting.md`](../development-troubleshooting.md)。
   **本机做法：让 sshd 自己绑定，端口只有 `sshd_config` 一个来源**——
   `systemctl disable --now ssh.socket && systemctl enable --now ssh`。
   新版 Ubuntu 的 ssh 可能由 systemd socket 激活接管，此时最终监听端口未必由 `Port` 决定；
   **换机后不要假设，以 WSL 内 `ss -ltn | grep 2223` 的实际输出为准**。
3. 准备两种认证：
   - 密码认证：创建一个测试用户（如 `mfperch`）并设置密码；
   - 密钥认证：生成测试密钥对，公钥写入 `~/.ssh/authorized_keys`。
4. 配置 sudo 各档策略用于测试：
   - 默认用户有 sudo 密码 → 测 `ask` / `auto`；
   - 另建一个 `NOPASSWD` 用户 → 测免密路径；
   - `deny` 模式无需特殊配置；
   - **特权身份免提权**（D60 的 `credentials.is_privileged`）**需要一台以 root 身份登录的主机**，
     即 sshd 开 `PermitRootLogin`。该选项改动的是测试机的 sshd 配置，**需客户授权后才动**；
     未开时该路径的真实端到端**未实测**（诚实标注见 **D60**，
     进程内不变式由 `only_a_privileged_login_launches_the_privileged_channel_without_sudo` 守住）。
5. 验证 `bash -l` 能加载 profile（写入一个测试用的 `~/.bash_profile`）。

## 4. 环境现状与实测记录

- **环境因机器而异**：本机的取值、私钥路径与重建入口记在仓库根部的
  **`AGENTS.local.md`**（gitignored，规则见 [`AGENTS.md`](../../AGENTS.md) §5.6.2）——
  换开发机时重建它，不要照抄下面这张表的取值。
  参考实测：2026-09-21 一台开发机为 **WSL Ubuntu 26.04.1**（内核 6.18、systemd 已启用、
  bash 5.3.9、OpenSSH 10.2p1），下表各项在该机复验一致。
  选型与客户答复见 **D27 / Q26**，此处不复述。

### 4.1 该环境必须具备的项（换机后逐项核对）

| 项 | 期望 |
| ---- | ---- |
| openssh-server | 监听 `127.0.0.1:2223` |
| 测试用户 `mfperch` | 可密钥登录；在 `sudo` 组且**提权需口令**（`sudo -n id -u` 必须失败） |
| 免密 sudo 用户 `mfperch-nopass` | `sudo -n id -u` 返回 `0`（免密路径） |
| 测试密钥对 | ed25519、**无 passphrase**（russh 直接读 PEM） |
| `~/.bash_profile`（`mfperch`） | 把 `/opt/mfperch-test-bin` 加进 PATH，且**不打印任何内容**；该目录内有可执行 `mfperch-test` |
| `requiretty` | **不得**设置，否则提权通道不可用（能力边界见 [`AGENTS.md`](../../AGENTS.md) §4.4） |
| 搭建脚本 | 幂等、可重跑；本机路径记在 `AGENTS.local.md`（不入库） |
| Windows → WSL SSH 连通性 | Windows 侧可连通（NAT 模式下的实测见 §4.2 ①）。**注意 Windows 上那个端口监听由 WSL 的 localhost 转发代持，随 WSL 实例生命周期消失**——核对要在 WSL 有进程挂着时做，`netstat -ano \| findstr 2223` 为空即实例已停（判读见 [`development-troubleshooting.md`](../development-troubleshooting.md)） |

### 4.2 实测验证结论（关键）

**① 密钥认证与命令执行**：Windows 侧 `ssh -i ... -p 2223 mfperch@127.0.0.1` 成功登录并执行命令。

**② 登录 shell 环境加载（验证 D4）**：

| 启动方式 | `MFPERCH_PROFILE_LOADED` | 自定义 PATH 生效 | `mfperch-test` 可执行 |
| ---- | ---- | ---- | ---- |
| 默认（非登录 shell） | 空 | ❌ | ❌ command not found |
| **`bash -l`（方案采用）** | **1** | **✅** | **✅** |

→ **直接证实了 D4 决策的必要性**：不加 `-l`，Agent 将无法使用用户自定义 PATH 中的命令。

**③ NUL 分帧协议（验证 D3）**：发送 7 个命令帧（含 `cd`、`export`、`false`、引号），实测：
- `cd /tmp` 后 `pwd` 输出 `/tmp` → 状态保留 ✅
- `export MF_VAR=hello` 后能读到 → 状态保留 ✅
- `false` 的结束标记为 `rc=1` → 退出码正确回传 ✅
- 每条命令均有带 nonce 的独立标记 → 输出边界清晰、防误判 ✅

> **历史实测快照（2026-09-10）**：以下 ④⑤⑥ 三项验证的是**当时的**旧投递机制
> （会话建立时 `sudo -n` 免密探测、`sudo -A` 无 askpass 即失败、askpass + FIFO 投递）。
> 其中与**提权投递**相关的结论**已被 D47 / D49 取代**，不再代表现行实现；
> 现行机制见 [`sudo.md`](sudo.md) §0 与 [`decisions.md`](../decisions.md) **D47 / D48 / D49**。
> ④ 描述的环境事实仍然成立（可按下面的命令手工复现），但"应用在会话建立时探测"这一步已不存在。

**④ 免密 sudo 的存在性（环境事实，现行仍可复现）**：
- `mfperch`：`sudo -n true` 失败（`interactive authentication is required`）→ 该用户提权需要密码；
- `mfperch-nopass`：`sudo -n id -u` 返回 `0` → 免密路径成立。
- **现行差异**：应用**不再**在会话建立时做这项探测（`session_setup_script` 已随 D49 移除）；
  数据面上的 `sudo` 一律被垫片拒绝，提权只经 `run_as_root` 的独立通道。

**⑤ ~~fail-closed 验证~~（结论已被 D47 / D49 取代）**：
- 旧：`sudo -A -p ''` 无 askpass 时报 `sudo: No askpass program specified in SUDO_ASKPASS`，退出码 1 → 天然拒绝提权。
- 现：数据面包装脚本注入 `sudo` shell 垫片（`protocol::sudo_reject_shim`），**三档策略 × 两种身份
  都**非 0 返回 + 一句可操作说明（普通身份允许提权时指引改用 `run_as_root` 工具；`deny` 时说明
  该主机已禁用提权；凭据声明特权身份时说明"已是特权身份，直接执行即可"，D60）。
  现行验证：单测 `wrapper_script_always_installs_reject_shim` 与 `reject_shim_refuses_and_guides_to_the_tool`；
  真实环境见 `src-tauri/tests/sudo_e2e.rs`。

**⑥ ~~askpass + FIFO 密码投递~~（机制已随 D47 / D49 整体移除）**：
- 旧结论保留作教训：**`sudo -A` 会把 askpass 程序的 stdout 第一行当作密码**，因此"请求标记"必须写到
  **stderr**，密码才写 stdout；标记误走 stdout 时 sudo 会把标记当密码，认证必然失败。
  该修正当时同步到了 [`docs/design/sudo.md`](sudo.md) 第 3.3 与 6.2 节（那两节现属**历史记录**）。
- 现：远端不部署 askpass、不建 FIFO、不设环境变量，**不产生任何本应用的文件**；密码只在提权通道
  建立后写一次该通道的 stdin（`sudo -S -p '<自有提示标记>'` 握手，见 D47 的 PoC 与 `sudo.md` §0）。
  上述 stdout / stderr 的坑随之不再适用。

### 4.3 后续可复用的验证清单

编码阶段可直接用该环境验证：
- 密码认证 / 密钥认证 / passphrase 密钥；
- 方案 C 的包装脚本（NUL 分帧、状态保留、结束标记、退出码）；
- `bash -l` vs 干净模式；
- sudo `deny` / `ask` / `auto` 三档；**特权身份免提权（D60）不在其列**——它需要以 root 登录，
  而本环境未开 `PermitRootLogin`（改它需客户授权，见上面第 3 节第 4 条与 **D60**）；
- 免密 sudo 快速路径；
- 长命令异步执行与轮询；
- 连接断开、终端 broken 状态、归档与恢复。

### 4.4 阶段一端到端实测结果（快照：2026-09-10）

阶段一的 SSH 会话层已在该环境完成端到端验证：`src-tauri/tests/ssh_integration.rs`
的 5 项集成测试全部通过（走真实 SSH 协议，非 mock）。

| 测试 | 验证内容 | 结果 |
| ---- | ---- | ---- |
| `connect_and_execute_simple_command` | 连接、认证、建会话、执行命令、退出码回传 | ✅ |
| `session_preserves_cwd_and_env` | **`cd` 与 `export` 跨命令保留**（方案 C 核心语义，D3） | ✅ |
| `session_reports_nonzero_exit_code` | 非零退出码正确回传，且会话仍可继续使用 | ✅ |
| `session_handles_quoting_and_special_chars` | 含引号与 `$` 的命令无需转义（NUL 分帧的价值） | ✅ |
| `session_is_not_confused_by_marker_like_output` | 输出中出现形似结束标记的文本时**不误判**（nonce 机制） | ✅ |

运行方式（需先导出目标主机信息）：

```bash
export MFPERCH_TEST_HOST=127.0.0.1
export MFPERCH_TEST_PORT=2223
export MFPERCH_TEST_USER=mfperch
export MFPERCH_TEST_KEY=<测试私钥路径>
# sudo_e2e 必需：缺失时按 AGENTS.md §5.6 直接失败（不静默跳过）
export MFPERCH_TEST_SUDO_PW=<测试用户的 sudo 密码>
# -j 2：避免默认并发耗尽 Windows 页面文件、把 target 产物写坏
#       （现象与恢复见 ../development-troubleshooting.md）
cargo test -j 2 --test ssh_integration -- --ignored --test-threads=1
```

> 上面 5 个变量与 `src-tauri/tests/*.rs` 实际读取的一致（`sudo_e2e` / `mcp_e2e` 走 `common::need_env`，
> `ssh_integration` 用同语义的本地 `need()`）——**缺失一律 `panic!`**，不静默跳过（AGENTS.md §5.6）。
> 另有 2 个**可选**覆盖项，都有默认值，缺失不影响运行：`MFPERCH_TEST_SESSION_IDLE_SECS`
> （`mcp_e2e` 的会话空闲超时）与 `MFPERCH_TEST_IDLE_SECS`（`mcp_e2e` 那条耗时用例的空闲时长）。
> 真实环境用例的运行方式（含 `sudo_e2e` / 全部 `--ignored`）见 [`AGENTS.md`](../../AGENTS.md) §5.10。

> 测试私钥位于 `.tmp-test/`（已被 `.gitignore` 排除，**绝不入库**）。

### 4.5 门禁口径（规模数字不登记，见 **D57 / D59**）

> 本文**不写"当前有多少条测试"**：那种数字是代码的副本，一次与本文无关的提交就会失真。
> 需要量级时现取——`cd src-tauri && cargo test -j 2`（默认门禁）、
> `cargo test --lib -- --list`（仅单测清单）、`pnpm test`（前端）。
> 完整门禁清单见 [`AGENTS.md`](../../AGENTS.md) §5.10。

口径（结构，不随用例增删而变）：

- **默认门禁就跑**：生产文件内 `#[cfg(test)]` 的单测 + 不依赖真实环境的集成测试
  （`version_check_e2e` 用本地 HTTP 服务，属这一类）；
- **默认不跑、要 `--ignored`**：`ssh_integration` / `sudo_e2e` / `mcp_e2e` 里标了
  `#[ignore]` 的真实环境用例，目标信息一律从 `MFPERCH_TEST_*` 读（见 §2、§3）；
- **前端**：`pnpm check:ipc` / `pnpm test` / `pnpm typecheck` 三道（分工见 AGENTS.md §5.9），
  外加 `pnpm check:secrets` 安全自检（AGENTS.md §2.6）；
- `mcp_e2e` 里有一条**只等空闲超时**的慢用例（约数分钟，不需要 `MFPERCH_TEST_*`）——
  真实环境用例整组跑可能超过一条命令的时限，分组更稳。

覆盖范围：协议解析、输出截断、加解密、密钥分层、仓储与配额、MCP 工具契约与鉴权、
sudo 提权（`deny` / `ask` / `auto` 走真实环境；特权身份免提权仅进程内，见 §3 第 4 条与 **D60**）、
更新检查、跨语言 IPC 契约。
