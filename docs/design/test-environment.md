# 测试环境方案

> 状态：已确认（Q26 / D27）
> 本文解决一件事：**如何搭建并复用 Windows 侧的真实 SSH 联调环境**。选型理由在 D27，不复述。

## 1. 环境选型

测试目标环境 = **WSL Ubuntu 内的 `openssh-server`**（监听 `127.0.0.1:2222`），
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
   Port 2222
   ```
3. 准备两种认证：
   - 密码认证：创建一个测试用户（如 `mfperch`）并设置密码；
   - 密钥认证：生成测试密钥对，公钥写入 `~/.ssh/authorized_keys`。
4. 配置 sudo 三种模式用于测试：
   - 默认用户有 sudo 密码 → 测 `ask` / `auto`；
   - 另建一个 `NOPASSWD` 用户 → 测免密路径；
   - `deny` 模式无需特殊配置。
5. 验证 `bash -l` 能加载 profile（写入一个测试用的 `~/.bash_profile`）。

## 4. 环境现状与实测记录

- 当前环境：**WSL Ubuntu 26.04**（内核 6.18、systemd 已启用、bash 5.3.9；2026-09-10 记录）。
  选型与客户答复见 **D27 / Q26**，此处不复述。

### 4.1 已搭建的测试环境

| 项 | 状态 |
| ---- | ---- |
| openssh-server | ✅ 监听 `127.0.0.1:2222` |
| 测试用户 `mfperch`（有 sudo 密码 本地测试口令） | ✅ |
| 免密 sudo 用户 `mfperch-nopass` | ✅ |
| 测试密钥对（ed25519，无 passphrase） | ✅ |
| 搭建脚本 | `/home/mf/setup-mfperch-test.sh` |
| Windows → WSL SSH 连通性 | ✅ 已实测 |

### 4.2 实测验证结论（关键）

**① 密钥认证与命令执行**：Windows 侧 `ssh -i ... -p 2222 mfperch@127.0.0.1` 成功登录并执行命令。

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
- 现：数据面包装脚本注入 `sudo` shell 垫片（`protocol::sudo_reject_shim`），**三种模式下都**非 0 返回 +
  一句可操作说明（策略允许提权时指引改用 `run_as_root` 工具；`deny` 时说明该主机已禁用提权）。
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
- sudo `deny` / `ask` / `auto` 三模式；
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
export MFPERCH_TEST_PORT=2222
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

### 4.5 测试规模与门禁（**会随开发变化**）

> 下面只是**量级参考**，不保证与当前代码一致——数字会随开发漂移。
> 需要准确值时以实际输出为准：`cd src-tauri && cargo test -j 2`、`pnpm test`。
> 完整门禁清单见 [`AGENTS.md`](../../AGENTS.md) §5.10。

| 类别 | 规模 | 说明 |
| ---- | ---- | ---- |
| Rust 单测 | 250 项 | 生产文件内的 `#[cfg(test)]`，默认门禁（`cargo test --lib -- --list` 实测） |
| Rust 集成测试 | 32 项 | `mcp_e2e` 6 / `ssh_integration` 6 / `sudo_e2e` 13 / `update_e2e` 7 |
| ↑ 其中 `#[ignore]`（默认门禁不跑） | 23 项 | `mcp_e2e` 4 / `ssh_integration` 6 / `sudo_e2e` 13；其中 **22 项需真实 SSH**，`mcp_e2e` 另 1 项是耗时的本地用例（空闲 310 秒）。`update_e2e` 7 项不依赖真实环境，默认就跑 |
| 前端单测 | 21 项 | vitest，覆盖 `src/lib/` 纯逻辑 |
| 前端契约检查 | 1 道 | `pnpm check:ipc`：命令名在 Rust 定义 / `generate_handler!` 注册 / 前端 `call()` 三处一致 |

覆盖范围：协议解析、输出截断、加解密、密钥分层、仓储与配额、MCP 工具契约与鉴权、
sudo 三模式、更新检查、跨语言 IPC 契约。
