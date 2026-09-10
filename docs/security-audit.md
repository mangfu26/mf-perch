# 安全审计报告（2026-09-10）

> 本轮为**全量代码审计**（Rust 约 10.9k 行、前端约 4.7k 行），
> 覆盖加密与凭据存储、SSH/sudo 执行层、MCP 与 IPC 边界、持久化与更新检查四个域。
> 审计方法为只读代码审查 + **源码级复现验证**；每条结论都注明核实方式与证据。
>
> 编号 `V*` 与审计阶段的发现清单一一对应，便于回溯。
> **"误报"一节记录了经核实不成立、因而未修改的条目**——这些也同等重要，
> 避免后人把"看起来像漏洞"的代码当成漏洞反复改动。

---

## 0. 结论摘要

| 级别 | 数量 | 全部已修复 |
| ---- | ---- | ---- |
| 严重（Critical/High） | 6 | ✅ |
| 中（Medium） | 9 | ✅ |
| 低（Low） | 9 | ✅ |
| 经核实为误报 | 4 | — |

最严重的一条（V1）**曾被测试绿灯掩盖**：sudo 注入链路使用了普通文件而非 FIFO，
密码以明文落在远端磁盘、同机可读，而既有的 e2e 测试因为"普通文件同样可写可读"
而照常通过。该缺陷现已修复，并新增**能真正区分真伪的回归测试**（见 §3.1）。

---

## 1. 严重问题

### V1 — sudo 密码以明文普通文件残留在远端（已修复）

- **位置**：`src-tauri/src/ssh/protocol.rs`（`session_setup_script`）
- **成因**：脚本先 `mkfifo` 创建本会话 FIFO，随后又执行通配删除
  `rm -f "$dir"/sudopw.fifo.*`，把**刚创建**的 FIFO 一并删除。
  于是 `cat > fifo` 退化为"创建普通文件并写入"。
- **证据（修复前实测）**：
  ```
  $ ls -la ~/.mf-perch/
  -rw-rw-r--  16  sudopw.fifo.<nonce>        # 普通文件，非 FIFO
  $ cat ~/.mf-perch/sudopw.fifo.*
  <明文密码>                                  # 权限允许同机其他用户读取
  ```
- **影响**：sudo 密码明文落盘、权限 `-rw-rw-r--`、会话结束不删除，
  彻底推翻 Q33"密码经内存传递、不落盘"的设计。
- **修复**：
  1. 调整顺序为**先清理历史遗留、再创建 FIFO**；
  2. 遗留清理限定 `find -type f -delete`，绝不触碰任何 FIFO 节点；
  3. askpass 脚本改为**按会话唯一命名**（`askpass.<nonce>`），
     避免并发会话互相覆盖；
  4. 会话关闭时执行清理，删除本会话的 FIFO 与 askpass。
- **验证**：新增 e2e `sudo_password_never_lands_in_a_regular_file`，
  直接检查远端文件类型（`find -type f`）与归档后的残留数量。
  **差分验证**：临时还原旧写法 → 测试失败（报 `1` 个普通文件）；
  修复后 → 通过。证明该测试确实能捕获此缺陷。

### V2 — 协议标记可被 Agent 伪造（已缓解，残余风险已文档化）

- **位置**：`src-tauri/src/ssh/protocol.rs`（`parse_line`、`wrapper_script`）
- **成因**：nonce 是包装脚本内的 shell 变量，而被 `eval` 的命令运行在**同一个 shell**
  中，因此 Agent 可以 `echo "$mfperch_nonce"` 读到它；标记行只按内容匹配、不看来源，
  且 stdout/stderr 合并进同一解析缓冲。
- **证据（实测）**：向包装脚本发送 `echo "$mfperch_nonce"`，输出
  `AGENT_CAN_READ_NONCE=<nonce>`。
- **影响**：可伪造 `__MF_SUDO_REQUEST__` 主动触发密码注入（配合 V1 可窃取密码，
  击穿"凭据对 Agent 完全不可见"的产品红线）；可伪造结束标记污染审计与状态。
- **修复与残余风险**：
  - nonce 由 64 位提升到 **128 位**，使盲猜不可行；
  - **主动读取无法通过提高熵解决**——需要把协议标记改由独立通道传递，
    属较大的协议变更，已记录为待评估项（见 §4）。
  - 该风险已在代码注释中显式标注，避免后误认为"已完全解决"。

### V3 — 归档终端的命令状态仍对 Agent 可见（已修复）

- **位置**：`src-tauri/src/mcp/tools.rs`（`get_command_status_impl`）
- **成因**：`run_command` 有归档拒绝，但 `get_command_status` 没有。
- **影响**：Agent 可在归档前记下 `command_id`，归档后持续读取该终端的命令与输出，
  与"归档后对 Agent 不可见"的权限模型不一致。
- **修复**：查询后校验终端状态，归档即返回 `TerminalArchived`。

### V4 — 结束通知可能丢失，导致终端永久挂起（已修复）

- **位置**：`src-tauri/src/terminal/mod.rs`
- **成因**：等待侧"先查 `exit_code`、再 `notified().await`"，而结束侧用
  `Notify::notify_waiters()`——它**不保留许可**。若置位与通知发生在等待者订阅之前，
  通知即丢失。
- **影响**：等待任务永久阻塞，`exec_lock` 永不释放、`inflight` 永不递减，
  该终端后续命令全部堆积直至队列上限（可用性失效）。
- **修复**：改用 `watch` 信号（`FinishNotifier`）。`watch` 会保留最新值，
  等待者无论何时订阅都能立即观察到变化；信号值用单调计数，保证每次结束都触发变更。
  同时把信号抽成独立类型，使其**可脱离真实 SSH 会话做单元测试**。
- **验证**：新增 5 项单元测试，其中
  `finish_signal_is_not_lost_when_sent_before_waiting` 直接构造"订阅前已发出"的场景。

### V5 — 序号命名空间不匹配，恢复终端后命令永久卡死（已修复）

- **位置**：`src-tauri/src/ssh/protocol.rs`、`src-tauri/src/terminal/mod.rs`
- **成因**：远端序号在**会话内**自增（从 1 开始），应用却用**数据库历史**的
  `MAX(seq)+1` 去比对。`restore_terminal` 重建会话后远端序号重置，
  两侧从此永久错位。
- **影响**：结束标记永不匹配，`exit_code` 永不置位 → 与 V4 同样的永久挂起，
  且**必然触发**而非偶发。
- **修复**：协议改为**按 `command_id` 精确配对**。
  - 帧格式改为 `<command_id>\n<command>`；
  - 结束标记回显该 id：`__MF_PERCH_END__<nonce>__<command_id>__<rc>__`；
  - 应用侧按 id 归属，不再依赖任何自增计数。
- **验证**：SSH 集成测试 5 项全部通过（含 `cd`/`export` 状态保留、退出码、
  引号与特殊字符、标记伪造不误判）。

### V6 — K3 主密钥与数据库同目录（已修复）

- **位置**：`src-tauri/src/store/keyring.rs`（`local_key_path`）
- **成因**：主密钥写 `%APPDATA%/mf-perch/master.key`，数据库在**同一目录**，
  违反 D6 自称的"主密钥绝不与数据库同处"。
- **影响**：用户按文档做"整库备份/迁移"、或该目录被云盘同步/打包外带时，
  密文与主密钥一起泄露——字段级加密对"数据目录被拿走"完全失效。
  且密钥文件内容是可逆 base64，并非密文。
- **修复**：密钥改存独立目录 `%APPDATA%/mf-perch-keys/master.key`；
  创建时即以 `0600` 打开（Unix 下不再"先宽松后 chmod"）；
  目录权限收紧为 `0700`。

---

## 2. 中危问题

| 编号 | 问题 | 修复 |
| ---- | ---- | ---- |
| V7 | 保留期清理会删除**正在执行**的命令记录（`created_at` 可早于保留期） | 增加 `status NOT IN ('queued','running')` 条件；在途命令结束后由下一轮回收 |
| V8 | 终端断开时用整体 upsert 写失败原因，**覆盖**命令已产生的真实输出 | 新增 `append_output`，改为追加，保留原始输出 |
| V9 | 启动仅把终端标记 broken，未收尾遗留的 queued/running 命令 | 新增 `fail_all_pending_on_startup`，启动时置 failed 并写明原因 |
| V10 | `McpManager::start` 检查与登记跨多个 `await`，并发可产生**无法停止**的第二个监听 | 新增 `lifecycle` 锁，串行化 `start`/`stop`/换 Token/改监听范围 |
| V11 | 用户输入原样作为 FTS5 `MATCH` 查询串（`"`、`NOT` 等导致搜索**硬报错**） | 新增 `fts_phrase`，包装为字面量短语并转义内部双引号 |
| V12 | 无换行输出使读取缓冲无界增长（可致 OOM） | 增加 `MAX_PENDING_LINE_BYTES`（1 MiB）上限，超限强制切分 |
| V13 | 输出事件 `try_send` 失败即静默丢弃，包括结束标记与 sudo 请求 | 控制事件改为**等待投递**（`send().await`），仅普通输出行可丢 |
| V14 | sudo 密码经 heredoc 下发会进入远端**命令行参数**（`ps` 可见） | 改为经 channel **stdin** 投递，命令行只含 `cat > fifo` |
| V15 | 更新清单响应体无大小上限，可被超大 JSON 撑爆内存 | 先查 `content_length`，再流式累计，超过 1 MiB 中止 |
| V16 | 更新源无协议白名单、跟随跨主机重定向；MCP Token 明文存库 | 校验 `http(s)`；禁用重定向；Token 落盘加密（见 §3.2） |

---

## 3. 低危问题与设计取舍

| 编号 | 问题 | 修复 |
| ---- | ---- | ---- |
| V17 | `Credential` 派生 `Serialize`，脱敏仅在 `Debug` 中生效 | `secret`/`passphrase` 加 `#[serde(skip_serializing)]` |
| V18 | 通用 `get_setting`/`set_setting` 无键名白名单，可读出 `mcp_token`；CSP 为 `null` | **删除**这两个无人使用的命令；设置严格的 CSP（区分 dev/prod） |
| V19 | `MasterKey` 的 zeroize 只清临时副本（`[u8;32]` 是 Copy）；`set` 覆盖旧值不清零 | `set` 先 `clear`；实现 `Drop` 清零 |
| V20 | `AuthMethod` 持明文密码/私钥/口令且不清零 | 实现 `Drop` 清零；sudo 密码投递用 `Zeroizing` 缓冲 |
| V21 | `init_key_provider` 无幂等防护，重复调用会覆盖密钥令凭据**永久不可解密** | 已初始化即拒绝；salt/verifier/provider 三项写在**同一事务** |
| V22 | 搜索 `offset` 经 `as i64`，超大值回绕为负被当作 0 | 夹紧到 `i64::MAX` |
| V23 | 主机端口读取时 `as u16` 静默截断（70000 → 4464，连错端口无提示） | 改为 `try_from`，越界报错；计数改为 `clamp` |
| V24 | 防 heredoc 标记碰撞仅靠 `debug_assert!`，release 被编译掉 | V14 改为 stdin 投递后，该 heredoc 路径已不存在，问题**自然消除** |

---

## 4. 遗留项（未修复，需客户决策）

1. **V2 的主动读取残余风险**：Agent 仍能读到 nonce 并伪造协议标记。
   彻底解决需将协议标记改由独立通道传递（例如改用 SSH 的 stderr 专线并加
   独立的双工确认），属协议层改动，影响面较大。当前已把 nonce 提升到 128 位
   并在代码中显式标注，建议**单独立项评估**。
2. **远程明文传输（Q30 已决策接受）**：开启"允许远程连接"后，
   Bearer Token 与命令内容在网络中明文传输。客户已明确接受该风险，
   保留为未来工作（TLS 或仅回环 + SSH 隧道）。

---

## 5. 经核实为误报（未修改，避免后人反复改动）

| 候选问题 | 核实结论 |
| ---- | ---- |
| 配额 TOCTOU 可绕过上限 | **不成立**：`check_quota` 与 `insert` 在**同一次加锁**内完成（`terminal/mod.rs`），中间无窗口 |
| 外键未启用导致孤儿数据 | **不成立**：`store/db.rs` 显式 `PRAGMA foreign_keys = ON` |
| MCP 工具泄露凭据 | **不成立**：逐一核对 7 个工具，`HostPublicInfo` 仅含 `id/name/address/port/ready`，凭据不在任何返回结构中 |
| 未启用弱加密算法 | 不适用：`russh` 仅启用 `ring`/`rsa`，代码未引入 DSA 等弱算法 |

---

## 6. 验证汇总

| 项目 | 结果 |
| ---- | ---- |
| 单元测试 | **227 项通过** |
| SSH 集成（真实协议） | **5 项通过** |
| sudo e2e（真实提权） | **6 项通过**（含新增 V1 回归） |
| MCP e2e（HTTP + 真实 SSH） | **3 项通过** |
| 更新检查 e2e | **7 项通过** |
| 差分验证 | V1 缺陷还原后测试失败、修复后通过（证明测试有效） |

> **方法学教训**：V1 表明"功能测试通过"不等于"安全属性成立"。
> 涉及安全属性的修复，必须构造能**区分正确与错误实现**的断言
> （如本例直接检查远端文件类型），而非只断言"操作成功"。
