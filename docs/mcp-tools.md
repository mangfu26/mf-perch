# MCP 工具契约（Agent 可见面）

> 状态：现行
> 列出 Agent 可调用的全部工具、入参 schema、输出结构与错误码，供客户核对与后续维护。
> 入参部分由真实端点导出（见文末 §6），**不以手写为准**；改代码后请重新导出比对。

---

## 1. 通用约定

| 项 | 约定 |
| ---- | ---- |
| 传输 | MCP **Streamable HTTP**，单一路径 `/mcp`（D1 / Q1，不做 stdio） |
| 鉴权 | HTTP 头 `Authorization: Bearer <token>`，缺省或错误返回 `401`（Q2） |
| 会话 | 服务端会话空闲超时 **24 小时**（D38），`404 Session not found` 表示会话已被回收，客户端应重新 `initialize` |
| 成功返回 | `CallToolResult.isError = false`，内容为 `content[0].text` 中的 **JSON 文本**（格式化后的字符串） |
| 失败返回 | `CallToolResult.isError = true`，内容同样是 JSON 文本：`{"error": "<中文说明>", "code": "<机器可读码>"}`。**刻意不用协议级错误**，Agent 需要读 `code` 才能决定如何恢复（Q4） |
| 结构化输出 | 未使用 MCP 的 `structuredContent` / `outputSchema`：好处是兼容性最好，代价是客户端需自行解析文本 |
| 字段命名 | 一律 `snake_case`，并尽量把单位写进名字（`wait_seconds` 秒 / `duration_ms` 毫秒） |
| 可空参数 | `anyOf: [{type: T}, {type: "null"}]` + `default: null`，且**不出现在 `required`** 中（D32：避免数组形式的 `type` 被部分客户端拒绝） |
| 工具总数 | **8 个**（不含工具面之外的任何写操作；人类侧的管理能力不暴露给 Agent） |

---

## 2. 工具清单

| 工具 | 作用 | 只读 | 入参必填 |
| ---- | ---- | ---- | ---- |
| `list_hosts` | 列出可用的 SSH 主机与配额 | ✅ | — |
| `create_terminal` | 在主机上创建终端 | ✗ | `host_id` |
| `list_terminals` | 列出 Agent 可见的终端 | ✅ | — |
| `run_command` | 执行命令并同步等待 | ✗ | `terminal_id`、`command` |
| `run_command_async` | 执行命令并立即返回句柄 | ✗ | `terminal_id`、`command` |
| `get_command_status` | 查询命令状态与输出 | ✅ | `command_id` |
| `run_as_root` | 以**特权身份**执行一条命令（D47） | ✗ | `terminal_id`、`command` |
| `archive_terminal` | 归档终端（释放配额） | ✗ | `terminal_id` |

---

## 3. 逐个工具

### 3.1 `list_hosts`

**入参**：无（schema 为 `{"type": "object"}`）

**输出**

| 字段 | 类型 | 说明 |
| ---- | ---- | ---- |
| `hosts[]` | array | 每项 `{id, name, address, port, ready}`；`ready=false` 表示人类尚未绑定凭据 |
| `active_terminals` | object | `host_id → 未归档终端数`，便于 Agent 自行判断配额 |
| `quota_per_host` | number | 每主机终端配额（取用户设置，非默认值） |
| `quota_global` | number | 全局终端配额 |

```json
{
  "hosts": [
    { "id": "host_8ad7afe77f9c468d8b82eee97d9970b9", "name": "个人小主机",
      "address": "myhost.example.net", "port": 22, "ready": true }
  ],
  "active_terminals": { "host_8ad7afe77f9c468d8b82eee97d9970b9": 1 },
  "quota_per_host": 5,
  "quota_global": 20
}
```

> 凭据内容（用户名、密码、私钥）**永不出现**在任何工具返回中（AGENTS.md §0.1 权限边界）。

### 3.2 `create_terminal`

**入参**

| 字段 | 类型 | 必填 | 说明 |
| ---- | ---- | ---- | ---- |
| `host_id` | string | ✅ | 来自 `list_hosts` |
| `name` | string \| null | ✗ | 人类可读名称，默认 `null` |

**输出**：`{terminal_id, host_id, name, status}`（新建成功时 `status` 为 `"active"`）

```json
{ "terminal_id": "term_2b044b76a6424f549c6c31af8cc338f5",
  "host_id": "host_8ad7afe77f9c468d8b82eee97d9970b9",
  "name": "测试终端", "status": "active" }
```

**可能失败**：`host_not_found`、`credential_not_found`（主机未绑定凭据）、
`terminal_quota_exceeded`（`scope` 会说明是 `per_host` 还是 `global`）、
`ssh_auth_failed`、`ssh_connect_failed`、`host_key_mismatch`（TOFU 不一致，D10）。
**失败时仍会保留一条 `broken` 终端记录**供人类排查。

> **远端没有 bash 时**：本产品要求目标主机具备 bash（见
> [`docs/design/terminal-session.md`](design/terminal-session.md) §6）。缺失时**不会**返回
> `bash_not_available`（该错误码已定义但当前没有任何代码路径会构造它），
> 实际表现是**就绪超时 30 秒**后返回 `ssh_connect_failed`，文案提示"请确认目标主机已安装 bash"。
> 即：不静默，但慢且错误码不专用；真正的能力探测是待补项。

### 3.3 `list_terminals`

**入参**

| 字段 | 类型 | 必填 | 说明 |
| ---- | ---- | ---- | ---- |
| `host_id` | string \| null | ✗ | 可选，按主机过滤 |

**输出**：`{terminals: [...]}`，每项：

| 字段 | 类型 | 说明 |
| ---- | ---- | ---- |
| `terminal_id` | string | 终端 ID |
| `host_id` / `host_name` | string / string\|null | 所属主机 |
| `name` | string \| null | 人类起的名字 |
| `status` | string | `active` / `broken`；**归档终端不会返回** |
| `created_at` | string | RFC 3339 |
| `last_command` | string \| null | 该终端最后一条命令 |

```json
{ "terminals": [
  { "terminal_id": "term_2b044b76a6424f549c6c31af8cc338f5",
    "host_id": "host_8ad7afe77f9c468d8b82eee97d9970b9", "host_name": "个人小主机",
    "name": "测试终端", "status": "broken",
    "created_at": "2026-09-11T12:58:44.514250100+00:00", "last_command": "ls -la" } ] }
```

> `broken`（会话已断，通常因应用重启）**不代表终端不可用**：下一次 `run_command`
> 会自动重连（D39），结果里会带 `session_reconnected: true`。

### 3.4 `run_command`

**入参**

| 字段 | 类型 | 必填 | 说明 |
| ---- | ---- | ---- | ---- |
| `terminal_id` | string | ✅ | 目标终端 |
| `command` | string | ✅ | 要执行的 shell 命令（在常驻会话中 `eval`，工作目录与环境变量保留） |
| `wait_seconds` | integer \| null | ✗ | 同步等待秒数：默认 30，**schema 声明 `minimum: 0` / `maximum: 50`**；超时不打断命令，改为返回句柄。超出上限会被夹到 50 |

**输出**（`RunOutcome`）

| 字段 | 类型 | 说明 |
| ---- | ---- | ---- |
| `command_id` | string | 后续 `get_command_status` 用 |
| `status` | string | `queued` / `running` / `completed` / `failed` / `connection_lost`（**连接断开、结局未知**，与「命令失败」区分；见 D50） |
| `exit_code` | number \| null | 已结束时为退出码（被信号终止可能为负） |
| `duration_ms` | number \| null | 耗时（毫秒） |
| `output` | string | 已累积的输出；未结束时可能只是部分 |
| `truncated` | boolean | 输出是否因上限被截断 |
| `still_running` | boolean | `true` 表示命令仍在远端执行，应改用 `get_command_status` 轮询 |
| `session_reconnected` | boolean | `true` 表示**执行前重建过会话**：shell 是全新的，工作目录/环境变量/后台进程已丢失（D39） |

```json
{ "command_id": "cmd_9f1c...", "status": "completed", "exit_code": 0,
  "duration_ms": 42, "output": "hello\n", "truncated": false,
  "still_running": false, "session_reconnected": false }
```

**超时示例**（`wait_seconds: 5`，命令仍在跑）：

```json
{ "command_id": "cmd_9f1c...", "status": "running", "exit_code": null,
  "duration_ms": null, "output": "…部分输出…", "truncated": false,
  "still_running": true, "session_reconnected": false }
```

**自动重连示例**：

```json
{ "command_id": "cmd_7a2b...", "status": "completed", "exit_code": 0,
  "duration_ms": 31,
  "output": "[mf-perch] 原 SSH 会话已断开，已自动重建（主机 个人小主机）；shell 状态已重置：工作目录、环境变量、后台进程均不再保留\nnginx.service - A high performance web server\n…",
  "truncated": false, "still_running": false, "session_reconnected": true }
```

**可能失败**：`terminal_not_found`、`terminal_archived`（归档终端对 Agent 不可见，D20）、
`command_queue_full`（该终端在途命令达上限）、`terminal_broken`（会话无法建立，
通常是主机不可达，错误里会给 `ssh_connect_failed` 之类的原因）。

### 3.5 `run_command_async`

**入参**

| 字段 | 类型 | 必填 | 说明 |
| ---- | ---- | ---- | ---- |
| `terminal_id` | string | ✅ | 目标终端 |
| `command` | string | ✅ | 要执行的命令 |

> **不含 `wait_seconds`**：异步模式立即返回句柄，该参数无意义。
> 早期两个工具共用入参结构，导致"等待秒数"出现在异步工具的 schema 里、
> 描述里还写着"长任务请用 run_command_async"（自相矛盾），已在 D43 修正。

**输出**：立即返回 `status: "queued"`、`still_running: true`、`output: ""` 的同一结构；
命令在后台执行并自行落库终态，用 `get_command_status` 轮询。

```json
{ "command_id": "cmd_a1b2...", "status": "queued", "exit_code": null,
  "duration_ms": null, "output": "", "truncated": false,
  "still_running": true, "session_reconnected": false }
```

> 同一终端同时只跑一条命令；要并行请在**不同终端**上发起。

### 3.6 `get_command_status`

**入参**

| 字段 | 类型 | 必填 | 说明 |
| ---- | ---- | ---- | ---- |
| `command_id` | string | ✅ | `run_command` / `run_command_async` 返回的 ID |
| `tail_lines` | integer \| null | ✗ | 只返回最后 N 行；省略则返回全部 |

**输出**（`CommandStatusView`）

| 字段 | 类型 | 说明 |
| ---- | ---- | ---- |
| `command_id` / `terminal_id` / `command` | string | 命令归属信息 |
| `status` | string | `queued` / `running` / `completed` / `failed` / `connection_lost`（**连接断开、结局未知**，与「命令失败」区分；见 D50） |
| `exit_code` / `duration_ms` | number \| null | 结束前为 `null` |
| `truncated` | boolean | 输出是否被上限截断 |
| `output` | string \| null | 轮询中为**部分**输出，结束时为完整输出 |
| `output_omitted` | boolean | 本次是否未请求输出（例如只看状态） |

```json
{ "command_id": "cmd_a1b2...", "terminal_id": "term_2b04...",
  "command": "apt-get update", "status": "running", "exit_code": null,
  "duration_ms": 8123, "truncated": false,
  "output": "Hit:1 http://archive.ubuntu.com/ubuntu noble InRelease\n…",
  "output_omitted": false }
```

> 归档终端的命令对它**不再可见**（V3）：会返回 `terminal_archived`，即使 Agent
> 之前记下了 `command_id`。

### 3.7 `run_as_root`

> **状态：已实现（D47）。** 权限边界见 [`docs/decisions.md`](decisions.md) D47 与
> `AGENTS.md` §4.4：提权密码由应用在**独立通道**上投递，**对 Agent 完全不可见**。

**入参**

| 字段 | 类型 | 必填 | 说明 |
| ---- | ---- | ---- | ---- |
| `terminal_id` | string | ✅ | 目标终端 |
| `command` | string | ✅ | 要以特权身份执行的**一条**命令 |
| `cwd` | string \| null | ✗ | 绝对工作目录；省略则继承该终端数据面的当前目录 |

**输出**（`PrivilegedOutcome`）

| 字段 | 类型 | 说明 |
| ---- | ---- | ---- |
| `command_id` | string | 命令历史 ID（人类审计用；**无需轮询**——调用返回时命令已结束，见下） |
| `exit_code` | number \| null | 退出码；没拿到（如提权通道在结束标记到达前断开）时为 `null` |
| `duration_ms` | number | 耗时（毫秒；提权命令没有"仍在跑"的返回态，因此恒有值） |
| `output` | string | 命令输出（**不含**应用内部的身份核实行） |
| `truncated` | boolean | 输出是否因上限被截断 |
| `actual_uid` | number \| null | 远端核实到的**实际** uid；正常情况下为 `0` |
| `actual_user` | string \| null | 实际用户名（如 `root`）；取不到为 `null` |

```json
{ "command_id": "cmd_3c7e...", "exit_code": 0, "duration_ms": 118,
  "output": "0\n", "truncated": false, "actual_uid": 0, "actual_user": "root" }
```

**与 `run_command` 的三处刻意差异**

1. **没有异步模式，也不需要轮询**：一次调用 = 建立特权通道 → 切目录 → 执行 → 收通道，
   **调用返回时命令已结束**。提权命令与普通命令写的是**同一张命令表**，因此拿 `command_id`
   去 `get_command_status` 也能查到（例外只有两处：终端归档后按 §3.6 的 V3 不再可见；
   超时放弃等待那一次见下表）——正常用法里不需要这一步。
2. **没有"root 模式"**：不存在"进入 root 再退出"的会话态，因此不存在"忘记退出、
   后续命令继续以 root 执行"的风险；每条命令各自提权一次。
3. **环境按 sudo 语义重置**（`env_reset`）：不继承数据面的环境变量，
   `HOME` / `USER` 为提权后的身份。 **`cd` 与 `export` 都不跨命令保留**，
   需要特定目录请用 `cwd` 参数，不要依赖上一条命令的 `cd`。

**凭据声明为「特权身份」时（D60）**：该终端登录的用户本身就是特权用户，
**没有"提权"这一步**——`run_as_root` 仍然可用、返回结构不变，但底层**不执行 `sudo`**：
不需要提权密码、不会弹确认、上面第 3 点的 `env_reset` 也不适用（环境就是该身份的登录环境），
下表里"无提权密码 / 密码被拒 / `requiretty`"三类失败无从发生。
实际 uid **照旧**由远端核实并如实回报；`deny` 策略**仍然拒绝**（策略优先，D60）。

**串行语义**：提权命令与普通命令**共用该终端的串行队列**——同一终端任一时刻
只有一条命令在执行。要并行请使用不同终端。

**审计**：该命令在人类侧的命令历史里以 `[特权用户(uid=0)]` 前缀显示（文本前缀，
不改数据库结构）。若远端实际 uid 不是 0（例如 sudoers 把目标用户配成了别人），
输出开头会带一条明确告警，且 `actual_uid` 如实返回——**不会**在审计里谎称 root。

**可能失败**

| 情况 | 错误码 | Agent 应做什么 |
| ---- | ---- | ---- |
| 该主机被人类设为"禁止提权" | `sudo_elevation_failed` | **报告人类**，不要重试；请人类在主机设置中改为"自动注入" |
| 主机配置为 `auto` 但没有提权密码 | `sudo_elevation_failed` | 报告人类补齐密码配置 |
| 主机配置为 `ask` 且人类拒绝 / 超时 | `sudo_elevation_failed` | 改用 `run_command` 以普通身份完成，或稍后再试 |
| 密码被 sudo 拒绝 | `sudo_elevation_failed` | 报告人类核对提权密码 |
| 该主机 `sudoers` 要求 TTY（`requiretty`） | `sudo_elevation_failed` | **不可恢复**：报告人类（D47 明确不做 PTY 回退） |
| 无法确定数据面当前目录 | `sudo_elevation_failed` | 显式传 `cwd` 后重试 |
| 提权命令超过 30 秒未结束 | `sudo_elevation_failed` | 命令可能仍在远端执行；先查历史再决定是否重试 |
| 终端已归档 | `terminal_archived` | 另建终端 |
| 该终端在途命令数已达上限 | `command_queue_full` | 等现有命令结束，或改用别的终端——提权命令与普通命令**共用**该终端的串行队列，不能用它绕开上限 |

> **`requiretty` 主机不支持提权**，这是 D47 的明确取舍（不做 PTY 回退，理由见决策记录）。
> 报错文案会引导人类走免密路径（`NOPASSWD` 白名单 / `pam_ssh_agent_auth`）。

> **上表不是全集**：同一路径还可能返回 `terminal_not_found`（`terminal_id` 不存在）、
> `terminal_broken`（会话无法建立，通常是主机不可达）、`host_not_found` /
> `credential_not_found`（主机或凭据已被删除）、`credential_undecryptable`（密钥/主密码问题）
> 等，语义与 §3.4 / §4 的错误码表一致。

### 3.8 `archive_terminal`

**入参**：`terminal_id`（string，必填）

**输出**：`{terminal_id, status: "archived", message}`

```json
{ "terminal_id": "term_2b04...", "status": "archived",
  "message": "Terminal archived. Its command history remains available to the human user." }
```

**语义**：关闭 SSH 会话但**保留全部命令历史供人类审计**；归档后 Agent 不可见。
这是 Agent 释放配额槽位的**唯一**手段——Agent **不能删除**终端（D21）。

---

## 4. 错误码表

| code | 含义 | Agent 可采取的恢复动作 |
| ---- | ---- | ---- |
| `host_not_found` | 主机 ID 不存在 | 重新 `list_hosts` |
| `credential_not_found` | 主机未绑定凭据 | 报告人类；**不要重试** |
| `terminal_not_found` | 终端 ID 不存在 | 重新 `list_terminals`，或 `create_terminal` |
| `command_not_found` | 命令 ID 不存在 | 重新执行命令 |
| `terminal_archived` | 终端已归档（对 Agent 不可见） | 另建终端 |
| `terminal_broken` | 终端会话不可用 | 见错误文案；主机可达时下次命令会自动重连 |
| `terminal_quota_exceeded` | 终端配额已满 | 先 `archive_terminal` 释放槽位（`scope` 区分 per_host / global） |
| `command_queue_full` | 该终端在途命令过多 | 等现有命令结束，或换终端 |
| `sudo_elevation_failed` | 提权失败（被策略禁止、无密码、密码被拒、需 TTY、取不到目录、超时） | **不要盲目重试**：按错误文案判断——多数情况需报告人类（D47） |
| `ssh_auth_failed` | 认证失败 | 报告人类（凭据问题） |
| `ssh_connect_failed` | 连接失败（含重连失败）；**远端无 bash 也归这里**（30 秒就绪超时，文案会提示确认已安装 bash） | 确认主机可达、且已安装 bash 后重试 |
| `host_key_mismatch` | 主机密钥与首次记录不一致（或服务端出示证书形式密钥） | **停止**并报告人类（可能是中间人，D10）；错误文案会给出**已记录**与**本次出示**两个指纹，供人类独立核对 |
| `credential_undecryptable` | 凭据无法解密 | 报告人类（密钥/主密码问题） |
| `invalid_argument` | 参数非法 | 修正参数后重试 |
| `internal_error` | 其他内部错误 | 报告人类，附上原文 |

---

## 5. 契约问题的处理结果

### 5.1 已在首发前修复（D43，2026-09-12）

| # | 问题 | 处理 |
| ---- | ---- | ---- |
| ① | `run_command_async` 的 schema 里带 `wait_seconds`，但实现忽略它（字段描述里还写着"长任务请用 run_command_async"，自相矛盾） | 为异步工具新增独立入参 `RunCommandAsyncParams`，**去掉该字段**；单测 `async_tool_does_not_expose_wait_seconds` 守住 |
| ② | `run_command.wait_seconds` 只在描述里写"max 50"，schema 无 `maximum`（客户端会放行 999，实际被静默夹到 50） | 手写该字段的 schema：`anyOf` 内声明 `minimum: 0` / `maximum: 50`；单测 `wait_seconds_schema_declares_maximum` 守住（并与实现的常量比对） |
| ③ | `error.rs` 的 `ErrorPayload{code, message}` 是死代码，线上实际返回 `{error, code}` | 删除未使用的结构，保留线上形状；在原处留注释说明取舍，避免后来者照抄错的那份 |

> 说明：② 之所以手写 schema 而不用 derive 属性——schemars 1.x 的 `range`
> 在当前依赖组合下报 `unknown schemars attribute`（实测）。

### 5.2 有意保留（经客户确认）

| # | 现状 | 保留理由 |
| ---- | ---- | ---- |
| ④ | 输出统一是 `content[0].text` 里的 JSON **文本**，未用 `structuredContent` | 兼容性优先：只认文本的客户端最多，改动收益小、风险大 |
| ⑤ | `list_hosts` 的 schema 只有 `{"type":"object"}`（无 `properties`） | 无参数工具的标准写法，实测客户端均正常；补空 `properties` 收益极小 |

---

## 6. 如何重新导出（避免文档漂移）

用官方 Inspector 连到真实端点导出 `tools/list`（`--strict` 无输出即 schema 无问题）：

```bash
cd src-tauri
cargo run --features mcp --example mcp_schema_probe   # 打印 URL 与 Token 后保持运行
npx -y @modelcontextprotocol/inspector --cli \
  --transport http --server-url http://127.0.0.1:50001/mcp \
  --header "Authorization: Bearer <token>" \
  --method tools/list --strict
```

> 本文 §3 的**入参 schema 即由该方式导出**（不是手抄代码）；改完工具定义后请重新导出比对。
> 输出结构（§3 的"输出"表）来自 Rust 类型定义：`mcp/tools.rs` 的返回结构、
> `terminal::RunOutcome`、`terminal::PrivilegedOutcome`（§3.7）、
> `domain::command::CommandStatusView`、`error::AppError::code`。
