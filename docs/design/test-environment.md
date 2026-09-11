# 测试环境方案

> 状态：已确认（Q26）
> 客户建议：使用本机 WSL 进行测试。

## 1. 本机探测结果（2026-09-10）

| 项 | 状态 | 说明 |
| ---- | ---- | ---- |
| WSL 二进制 | ✅ 存在 | `C:\Windows\System32\wsl.exe` |
| WSL 发行版 | ❌ **无** | `wsl -l -v` 返回帮助文本；`HKCU:\...\Lxss` 注册表为空；`AppData\Local\Packages` 无 Linux 发行版包 |
| Docker | ❌ 未安装 | `docker: command not found` |
| Podman | ❌ 未安装 | `podman: command not found` |
| OpenSSH 客户端 | ✅ 有 | `OpenSSH_10.3p1`（Windows 侧） |
| bash | ✅ 有 | Git Bash（Cygwin 5.3.9）——注意：**不是 Linux**，行为差异大，不能替代 |

**结论：本机当前没有可用的 Linux 测试环境。** WSL 已启用（二进制在），但未安装任何发行版。

## 2. 可选路径

| 方案 | 操作 | 优点 | 缺点 |
| ---- | ---- | ---- | ---- |
| **W1 安装 WSL 发行版**（推荐） | 以管理员身份执行 `wsl --install -d Ubuntu`，重启后创建用户 | 真实 Linux 内核、真实 bash/sudo/sshd；可跑 systemd；一次配置长期可用 | 需要管理员权限 + 重启；占用约 1–2 GB 磁盘 |
| W2 安装 Docker Desktop | 安装 Docker Desktop 后跑 `openssh-server` 容器 | 环境可重置、适合自动化 | 需安装较大软件；容器内 sudo/systemd 行为与真机有差异 |
| W3 客户提供真实主机 | 客户提供 1–2 台带 sudo 的 Linux | 最真实 | 依赖客户提供；网络与环境不可控 |

## 3. 团队建议：W1（WSL + Ubuntu）

理由：

1. **最贴近真实目标环境**：我们的设计依赖 bash 内建 `read -d`、`sudo -A`/`SUDO_ASKPASS`、登录 shell 环境加载——这些在真实 Linux 上才可靠验证。
2. **可测试 sudo 三模式**：WSL 里可创建普通用户并配置 sudo 密码，完整验证 `deny` / `ask` / `auto`。
3. **可作为 SSH 服务端**：WSL 内安装 `openssh-server` 并启动，Windows 侧通过 `127.0.0.1:<port>` 连接，完整走真实 SSH 协议。
4. 一次配置，后续自动化测试可复用。

### 3.1 需要客户配合的操作

请以**管理员身份**打开 PowerShell 执行：

```powershell
wsl --install -d Ubuntu
```

然后**重启电脑**，首次启动 Ubuntu 时设置用户名与密码。完成后告知我们，我们继续配置 SSH 服务端。

> 注意：`wsl --install` 需要管理员权限，且需要重启，团队无法代为执行。

## 4. 在 WSL 中搭建测试目标的步骤（客户完成安装后，团队负责）

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

## 5. 已确认结论与实测结果（Q26，2026-09-10）

- 客户答复：本机为 Windows 10，**使用 WSL 进行测试**。
- 环境状态：客户已安装 **WSL Ubuntu 26.04**（内核 6.18、systemd 已启用、bash 5.3.9）。

### 5.1 已搭建的测试环境

| 项 | 状态 |
| ---- | ---- |
| openssh-server | ✅ 监听 `127.0.0.1:2222` |
| 测试用户 `mfperch`（有 sudo 密码 本地测试口令） | ✅ |
| 免密 sudo 用户 `mfperch-nopass` | ✅ |
| 测试密钥对（ed25519，无 passphrase） | ✅ |
| 搭建脚本 | `/home/mf/setup-mfperch-test.sh` |
| Windows → WSL SSH 连通性 | ✅ 已实测 |

### 5.2 实测验证结论（关键）

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

**④ sudo 免密探测（验证 Q33）**：
- `mfperch`：`sudo -n true` 失败（`interactive authentication is required`）→ 需注入；
- `mfperch-nopass`：`sudo -n id -u` 返回 `0` → 免密路径成立，无需注入。

**⑤ fail-closed 验证（验证 Q33 模式一）**：`sudo -A -p ''` 无 askpass 时报
`sudo: No askpass program specified in SUDO_ASKPASS`，退出码 1 → 天然拒绝提权。

**⑥ askpass + FIFO 密码投递（验证 Q33 模式二/三）**：实测发现并修复一个**设计缺陷**：

> **`sudo -A` 会把 askpass 程序的 stdout 第一行当作密码。**
> 因此"请求标记"必须写到 **stderr**，密码才写 stdout。
> 若标记误走 stdout，sudo 会把标记当密码，认证必然失败（实测报 `Authentication failed`）。
> 修正后（标记 `>&2`）实测成功：stdout 返回 `root`，退出码 0。

该修正已同步到 [`docs/design/sudo.md`](sudo.md) 第 3.3 与 6.2 节。

### 5.3 后续可复用的验证清单

编码阶段可直接用该环境验证：
- 密码认证 / 密钥认证 / passphrase 密钥；
- 方案 C 的包装脚本（NUL 分帧、状态保留、结束标记、退出码）；
- `bash -l` vs 干净模式；
- sudo `deny` / `ask` / `auto` 三模式；
- 免密 sudo 快速路径；
- 长命令异步执行与轮询；
- 连接断开、终端 broken 状态、归档与恢复。

### 5.4 阶段一端到端实测结果（2026-09-10）

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
cargo test --test ssh_integration -- --ignored --test-threads=1
```

> 测试私钥位于 `.tmp-test/`（已被 `.gitignore` 排除，**绝不入库**）。

单元测试合计 **104 项**，覆盖协议解析、输出截断、加解密、密钥分层、仓储与配额等。
