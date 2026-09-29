# 测试债台账（§4.6）

> 状态：现行
> 本手册是测试债的唯一台账（原 AGENTS.md §4.6）。**保留编号 §4.6**，既有引用继续可解析。
> 测试规范本身见 [`testing.md`](testing.md) §5。

## 4.6 当前测试债

> §5 测试规范落地后仍未完成的项，**登记在此，不要靠"没人记得"来掩盖**。
> 每条写清"为什么留着"；标 **待定** 的需要客户拍板，不要自行决定。

| 项 | 位置 | 说明 | 优先级 |
| ---- | ---- | ---- | ---- |
| 生成脚本文本断言 | `src/ssh/protocol.rs` | 对生成的 shell 脚本**源码**做子串/顺序断言（`set +e` 与结束标记的先后、sudo 垫片的拒绝语义）。按 §5.2 判定：替代覆盖在 `#[ignore]` 后面、默认不跑，**不构成"已在别处覆盖"**，故不属于该删的重复；真正遗留的弱点是**脆**（改脚本文案可能失效）。**已决策：保留**（客户确认） | 已决 |

**当前没有待决策的测试债。**

**已复核、判定不算债（不要重复上报）**：

| 疑似重复 | 复核结论 |
| ---- | ---- |
| `tests/version_check_e2e.rs` ↔ `src/update.rs` | **不是重复**。`src/update.rs` 的单测直接构造 `UpdateManifest` 结构体（`manifest()` / `manifest_with_asset()`），**绕过 serde 解析**；只有 e2e 走真实 JSON 文本，覆盖"清单字段名 → 状态分支"的线上映射。删掉 e2e 会丢掉"字段名写错、解析静默失配"这类缺陷的防线 |
| `tests/ssh_integration.rs::session_is_not_confused_by_marker_like_output` ↔ `src/ssh/session.rs` 的 nonce 单测 | **不是重复**。单测喂的是合成输入，集成测试走真实 SSH 回显与真实输出交错 |
| `src/ipc/tests.rs`、`src/mcp/tools.rs` 的 fixture ↔ `tests/common/mod.rs` | **无法合并**。`src/` 内的单元测试在生产 crate 内部，拿不到 `tests/` 的模块，只能各自保留 |

**已清偿的测试债**（只留结论与护栏，删除过程在 `git log`）：

- 集成测试 fixture 已收敛到 `tests/common/mod.rs`（`test_state` / `need_env` / `need_env_port`）；
  `src/sudo_bridge.rs` 的用例统一经 `register_pending` 登记，不直接操作私有字段；
- **前端测试基础设施已落地**：vitest 覆盖 `src/lib/` 纯逻辑，另有
  `scripts/check-ipc-contract.mjs` 守跨语言命令名契约，范围与边界见 §5.9；
- **不要再去找密码投递旧机制（askpass / FIFO）时代的用例**——它们已随 D49 整体删除。
  该面现行不变式由 `src/ssh/protocol.rs` 的 `wrapper_script_always_installs_reject_shim`、
  `reject_shim_refuses_and_guides_to_the_tool` 与 `tests/sudo_e2e.rs` 守住；
- **不要为第三方库的内部常量写断言**（曾有一条断言 rmcp 的默认空闲超时，已删）：
  它失败不指向用户问题（§5.1 自检为「否」），且会逼人在升级依赖时改数字。
  该知识保存在 **D38** 与 `src/mcp/server.rs` 的生产注释里，真不变式另有用例覆盖。
