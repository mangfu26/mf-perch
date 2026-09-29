# 测试规范与运行手册

> 状态：现行
> 本手册是测试规范（§5.1–§5.11）的权威落点；测试债台账在 [`test-debt.md`](test-debt.md)（§4.6）。
> **保留 AGENTS.md 原有编号（§5.1–§5.11）**：仓储内多处软引用按编号指向本节内容，
> 搬迁只搬正文、不改编号（理由与 D52 保留 `docs/decisions.md` 路径一致）。
> 与 [`docs/design/principles.md`](../design/principles.md) **P3** 是一对：
> **P3 管"测试可不可信"（能不能区分对错实现），本手册管"测试该不该存在"（有没有用、冗不冗余）。**

## 5.1 测试的存在理由

**测试必须保护真实行为、契约、领域不变量、数据兼容性或回归路径。**

- 只提升覆盖率数字、只证明"代码碰巧能跑通"、只锁死实现细节（而无用户可见或跨模块契约）的测试**不要写**；
- 自检问句：**"这条测试失败时，指向的是不是用户真的会在意的问题？"**——答不出"会"，就不要写；
- 每条测试都应能用一句话说清它守的是哪条契约 / 不变式。本项目值得守护的典型不变量：
  - 协议层：NUL 分帧、nonce 标记不可伪造、输出边界不误判（D3）；
  - 权限边界：认证信息对 Agent 不可见、Agent 不能删除终端、人类不能输入命令；
  - 凭据：密文落盘、错误密钥解不开、摘要不泄露秘密；
  - 资源：终端配额（每主机 / 全局）、命令历史保留策略、输出截断上限；
  - 契约：MCP 工具入参 schema（见 [`docs/mcp-tools.md`](../mcp-tools.md)）、更新清单格式（见
    [`docs/update-manifest.md`](../update-manifest.md)）、Tauri IPC 命令返回结构。

## 5.2 不要为一个改动到处撒测试

**本条针对的是"为一个小改动把测试撒到各层"的膨胀，不是"删除任何看起来重复的测试"的许可。**
判定两条测试是否真属重复、该收敛，必须**三条全为「是」**：

1. **是同一条不变式吗？** 换个更硬的问法：**同一个生产缺陷会让这两条测试都失败吗？**
   如果它们守的是**不同的**缺陷，那就不是重复——即使断言写法看着很像。
   例：`tests/version_check_e2e.rs`（走真实 JSON，覆盖"清单字段名 → 状态分支"的线上映射）与
   `src/update.rs` 的单测（直接构造 `UpdateManifest`，绕过 serde）：字段名写错只让前者失败，
   两者守的不是同一个缺陷，**都要留**。
2. **"已在别处覆盖"的那个"别处"，在同一道门禁里跑吗？**
   带 `#[ignore]`、需要真实 SSH / WSL / 外部服务的端到端测试**不能**作为进程内测试的替代覆盖——
   它平时不跑，删掉进程内断言等于把防线搬出默认门禁（P3 的教训形态：报告是绿的，属性却没被验证）。
3. **删掉后维护成本真的降了吗？** fixture 与断言**逐字重复**才算降；只是"看着像"不算。

三条全为「是」→ 收敛（合并 fixture 或删重复用例）。
有任何一条为「否」→ 保留，并把判断理由写进 §4.6（[`test-debt.md`](test-debt.md)）的
"已复核、判定不算债"小节，避免反复重审。

其余要求：

- 一个聚焦的功能 / 修复，**优先扩展现有合适的测试文件**；确实需要新建文件时**最多新增一个**，
  并把关键回归用例集中在那里；
- **禁止**因为调用链跨了 `store/`、`ssh/`、`mcp/`、`ipc/`、`terminal/` 等层，就在每一层各建一个测试文件；
- 触碰的生产文件数量多，**不是**增加测试文件的理由；
- 用例要紧凑，只针对可观察行为。

## 5.3 禁止"假测试"

以下一律不写：

- 用随机输入、大循环次数、固定 `sleep`、计时比较拼出来的伪 fuzz / 压测 / 冒烟 / 性能测试；
- **只打日志**（`println!` / `eprintln!`）而不断言的测试；
- 换个名字重复同一分支、没有新增不变量的重复测试；
- 断言私有常量、字段清单、辅助函数内部结构、文件布局，而可观察行为在别处已覆盖的测试；
- 为了让测试变绿而把错误语义写进生产代码的测试——**测试服从实现语义，实现不得迁就测试**
  （尤其涉及 sudo 三模式、凭据可见性、配额与配额边界时）。

> **关于 `sleep`**：测试中**不得**用固定 `sleep` 等待"事情大概已经发生"。等待状态请轮询并设上限
> （deadline），或直接 await 事件 / 信号。
> **唯一例外**：测试要验证的**就是"时间流逝"本身**（如会话空闲超时）。此时必须
> ①在注释写明理由，②时长做成可通过环境变量覆盖的参数。

## 5.4 写法约定

- **优先确定性表驱动测试**：显式输入 + 精确期望输出，一张表覆盖多个边界；
- 断言统一使用标准宏：`assert!` / `assert_eq!` / `assert_ne!` / `matches!`；
  失败信息用中文讲清"期望什么、实际什么"，并带上实际值（如 `"退出码应为 0，实际 {code:?}"`）；
- **不手写断言辅助函数**，除非它编码的是一条可复用的项目专有不变式；
  写了就必须在注释里说明它守护的不变式是什么；
- 测试需要数据库、应用状态、设置、缓存时，**在 fixture 内显式初始化**
  （内存库 `open_in_memory`、`tempfile` 临时目录、自建 `AppState`），**不得**依赖机器上的既有数据或残留状态；
- 测试**不得依赖外网**；需要 HTTP 时起本地服务（范例见 `src-tauri/tests/version_check_e2e.rs`）。

## 5.5 测试放置位置

| 测试类型 | 位置 | 说明 |
| ---- | ---- | ---- |
| 单元 / 模块测试 | 生产文件内的 `#[cfg(test)] mod tests` | 贴近被测代码；本项目绝大多数测试属于此类 |
| 跨模块端到端（本地） | `src-tauri/tests/*.rs` | 不依赖真实 SSH / 外网，默认随 `cargo test` 运行 |
| 真实环境端到端 | `src-tauri/tests/*.rs` + `#[ignore]` | 需真实 SSH 服务器，见 §5.6 |

**`tests/*.rs` 的文件名不要含 `update` / `install` / `setup` / `patch` 关键字**——
Windows 会按文件名把测试 exe 误判为需要管理员权限的安装程序，cargo 根本拉不起来
（成因与处置见 [`docs/development-troubleshooting.md`](../development-troubleshooting.md)）。
文件名也不要求与被测模块同名，可在文件头注释里注明归属。

## 5.6 真实环境测试的门禁（本项目专有）

需要真实 SSH 服务器 / WSL 的端到端测试必须同时满足：

1. 打标注：`#[ignore = "需要真实 SSH 服务器；设置 MFPERCH_TEST_* 环境变量后以 --ignored 运行"]`；
   默认配置（不带 `--ignored`）**不要求**跑通，因为这些测试依赖客户机器上的环境；
2. 目标信息一律从 `MFPERCH_TEST_*` 环境变量读取（见 [`docs/design/test-environment.md`](../design/test-environment.md)）；
   各变量的**本机取值**记录在仓库根部的 `AGENTS.local.md`（gitignore 排除、不入库）——
   跑真实环境用例前先读它加载；若该文件不存在或取值已失效，向客户询问，**不要猜**；
3. **前置条件不满足时必须明确失败（`panic!` / `expect`），不得 `return` 静默跳过。**
   适用场景包括但不限于：`MFPERCH_TEST_*` 环境变量缺失、依赖的外部工具（如 `ssh-keygen`）不可用、
   测试私钥读不到、断言所需的样本拿不到。
   静默跳过会制造"绿灯假象"——测试报告显示通过，实际一行断言都没执行。
   **跳过只能由 `#[ignore]` 表达，不能由测试体自己决定**；
4. 测试用的私钥 / 口令只放在 `.tmp-test/`（已在 `.gitignore` 中排除），**绝不入库**（AGENTS.md §2.6）。

## 5.7 涉及安全属性的测试（P3，强制）

- 见 [`docs/design/principles.md`](../design/principles.md) **P3**：**功能测试通过 ≠ 安全属性成立**；
- "不落盘 / 不泄露 / 不可绕过 / 不可伪造 / 权限边界"一类的目标，必须有**能区分对错实现**的断言
  （直接检查载体本身：文件类型是 FIFO 还是普通文件、返回结构里有没有敏感字段、越权请求是否被拒绝），
  而不是只断言"操作返回成功"；
- 修复安全缺陷后做**差分验证**：临时还原错误实现，确认测试**确实会失败**；
- 无法根治的残余风险要诚实标注"已缓解未根治"（如 [`docs/security-audit.md`](../security-audit.md) 的 V2），
  不得用"已修复"掩盖。

## 5.8 清理测试时

- 清理测试**必须保留有意义的回归覆盖**；
- 若删除的测试间接守住了某条真实契约，要**补一个更小、直接断言该契约的测试**，而不是直接删掉；
- 尚未完成的测试债登记在 §4.6（[`test-debt.md`](test-debt.md)），不要靠"没人记得"来掩盖。

## 5.9 前端测试（范围与边界）

前端测试**范围刻意收窄**：只覆盖 `src/lib/` 的**纯逻辑**（IPC 信封解包与错误码映射、
数值格式化）。配置见 `vitest.config.ts`，运行方式见 §5.10。

**不覆盖**（有意为之，不是遗漏）：

- **组件测试**：`src/components/` 多为声明式展示，引入 `@vue/test-utils` + DOM 环境
  投入大、收益低。等出现"值得用测试锁住的交互逻辑"再加，避免"框架在就顺手写测试"（§5.1）。
- **Store 测试**：`src/stores/` 需要 Pinia 测试环境，同上，按需再加。

前端默认门禁如下，分工明确、互不替代：

| 门禁 | 守住什么 |
| ---- | ---- |
| `pnpm typecheck` | 类型层面（`vue-tsc`） |
| `pnpm check:ipc` | **跨语言契约**：命令名必须在「Rust `#[tauri::command]` 定义」「`generate_handler!` 注册」「前端 `call()` 字符串」**三处**一致。这是 `vue-tsc` 与 `cargo` **都查不出**的运行期失败——尤其"定义了但没注册 = 命令不可达"是 Tauri 最经典的静默坑 |
| `pnpm test` | 运行期逻辑：信封/错误码契约与格式化边界（vitest，node 环境） |

> `check:ipc` 只看**生产**调用点，会跳过 `*.test.ts`——测试里的 `call()` 是打桩，
> 命令名是假的，算进来只会制造误报。

## 5.10 运行方式（速查）

```bash
# 【日常主力】只跑受影响模块——秒级反馈
# 模块名就是测试路径前缀，例如 store::db / domain::command / update
cd src-tauri && cargo test -j 2 --lib <模块名>

# 或者让脚本按本次 git 改动自动挑模块（跨切改动自动退回全 lib）：
pnpm test:fast

# 【提交前】默认门禁（本地一条命令跑完）：与 CI 是同一组检查
pnpm gate
#   = check:secrets + check:ipc + check:docs + pnpm test + typecheck
#     + cargo test -j 2 --lib --test version_check_e2e --test mcp_e2e

# 为什么默认门禁不写裸 `cargo test`：
#   `src-tauri/tests/` 下的 ssh_integration 与 sudo_e2e 两个 target，用例**全部**带 #[ignore]，
#   默认一条都不执行，却同样要编译并链接一个二进制（各自链接整个应用库）。
#   把它们从提交前的循环里拿掉，**执行到的用例与裸 `cargo test` 默认执行的完全一样**
#   （--lib + version_check_e2e + mcp_e2e），省下的是两次链接；
#   这 2 个 target 的**编译验证**在下面的"全量"与 CI 里照常发生。
#   注意 mcp_e2e 必须留在默认门禁里：它的 401 鉴权分支覆盖是全仓唯一的一份。

# 【推送 / 并入 main 前】全量：所有 target 都要能编译 + 真机行为要过
cd src-tauri && cargo test -j 2

# 【推送 / 并入 main 前】全部真实环境用例（= 各 target 下所有 #[ignore] 用例）
# 注意 mcp_e2e 里有一条只等空闲超时的慢用例（默认 310 秒），整组一起跑可能超过单条命令的时限
# ——分组跑更稳。慢用例的时长都可用环境变量调小：
#   MFPERCH_TEST_IDLE_SECS、MFPERCH_TEST_SESSION_IDLE_SECS（见 docs/design/test-environment.md）
cd src-tauri && cargo test -j 2 -- --ignored --test-threads=1

# 只跑某一个 e2e target（--ignored 只跑该 target 下的 #[ignore] 用例）
cd src-tauri && cargo test -j 2 --test ssh_integration -- --ignored --test-threads=1
# 提权双通道的核心用例（含数据面拒绝 / 提权通道 / cwd 继承 / 身份核实）
# 需要额外的 MFPERCH_TEST_SUDO_PW；mcp 是默认特性，无需再写 --features mcp
cd src-tauri && cargo test -j 2 --test sudo_e2e -- --ignored --test-threads=1

# 少打字：`.cargo/config.toml` 里有两个别名（不改变任何构建语义）
cd src-tauri && cargo t               # = cargo test -j 2
cd src-tauri && cargo tl store::db    # = cargo test -j 2 --lib store::db

# 前端与文档门禁（都不需要真实环境；分工见 §5.9）
pnpm test           # src/lib 纯逻辑单测（vitest）
pnpm typecheck      # 类型（vue-tsc）
pnpm check:ipc      # 跨语言命令名契约：Rust 定义 / generate_handler! 注册 / 前端 call 字符串三处一致
pnpm check:docs     # 文档预算：AGENTS.md 长度上限、单文档长度上限、文档索引登记
pnpm check:secrets  # 提交前安全自检（AGENTS.md §2.6 红线；纯 node 实现，跨平台、不依赖 bash）
```

> **`-j 2` 的理由**：并发过高会耗尽 Windows 页面文件、把 `target` 产物写坏（E0463/E0462），
> 现象、根因与恢复步骤见 [`docs/development-troubleshooting.md`](../development-troubleshooting.md)。
> **不要**为了"跑快点"把 `-j` 调大：当可用内存有限、且 `target` 落在随机 I/O 慢的盘（机械盘）上时，
> 并行链接只是把瓶颈从 CPU 换到内存与磁盘，换不来速度。
> 反向也成立：CI runner 不属于该条件，所以 workflow 里用**默认并行度**，不写 `-j 2`。
>
> **判读以 cargo 自己的输出为准**（每个 target 的 `test result: ok` 才算绿；出现 `FAILED` /
> `error: test failed` 才是红）。PowerShell 会把 cargo 的 stderr 警告当成错误，
> 造成"全绿却退出码 1"；需要机器判读时写文件日志，别用 `Select-Object -Last N` 截断：
> `cargo test -j 2 *> ..\.tmp-test\full-test.log; "exit=$LASTEXITCODE"`

## 5.11 测试节奏（客户 2026-09-29 修订）

**目的：日常秒级反馈，每个提交有保证，完整性交给 CI 与"推送前全量"把关。** 四层：

| 时机 | 跑什么 | 为什么 |
| ---- | ---- | ---- |
| **开发中（每改一处）** | **只跑受影响模块**：`pnpm test:fast`，或 `cargo test -j 2 --lib <模块>`；真实环境行为跑单条 e2e | 反馈是**秒级**。全量的价值在"没有遗漏"，不在"改这一处对不对" |
| **每次提交前** | **默认门禁**：`pnpm gate`（见 §5.10） | 覆盖默认执行的全部用例 + 前端 / 文档 / 安全门禁；只跳掉两个"全忽略" target 的链接 |
| **推送 / 并入 `main` 前** | **全量 + 全部真实环境 e2e** | 让 4 个 e2e target 都过编译、真实主机行为过验证——发布前的最终把关（客户原话："正式推送到稳定分支前再跑全量测试"） |
| **每次 push `develop/**` 或 PR → `main`** | CI 自动跑默认门禁（`.github/workflows/gate.yml`） | 在干净机器上复核一遍；本机的编译缓存、IDE 争用、内存问题不再能伪装成"通过" |

**为什么不再"每次提交都跑全量"**（取代客户 2026-09-16 的原节奏）：成本大头是**依赖编译**，
不是用例执行。参考实测（2026-09-29，一台 4 核 / 机械盘开发机）：
`cargo test -j 2 --lib` 冷构建 20 分钟只推进到 369 个依赖 crate、**还没进入用例执行阶段**；
同期 IDE 的 `cargo check --all-targets` 冷跑也要 7 分钟以上。
把这种代价压在每一次提交上，换来的是等待而不是保证——完整性由 CI 与"推送前全量"承担，日常提交由默认门禁承担。

配套纪律：

1. **不为同一件事重复跑全量**：需要二次确认时，跑**上一次失败的那一条**用例即可。
2. **"改了代码但结果没变"时，先怀疑产物陈旧**：执行
   `cargo clean -p mf-perch` 强制作废候选产物**再下结论**。
   **禁止**用"再跑一次全量"撞运气——它既慢又证明不了什么（本项目已被这个问题误导过两次，
   其中一次差点得出相反的设计结论）。
   **还原文件也会踩到它**：`Copy-Item` 会连**原始 mtime** 一起复制，cargo 因此认为无需重建
   （本项目实测：差分验证后还原实现，测试仍在跑被还原掉的旧实现）。判据是这次运行**有没有
   打印 `Compiling mf-perch`**；没有就说明跑的是旧二进制。
3. **长编译不要干等**：编译期间去做不依赖它的工作（改文档、写用例、查证据），
   或把它放进后台 job。
4. **构建被环境挡住时，向客户求助，不要绕过去**：app 正在运行会把 `mf-perch.exe`
   锁住（`cargo build` 报 `os error 5` / `32`）。**禁止**为此改用备用 target
   （`CARGO_TARGET_DIR=src-tauri/target-verify` 之类）旁路编译——本项目实测这样留下的
   目录是 **7.9 GB / 5803 个文件**（约主 `target` 的 72%），app 一关就永久失去价值且无人回收。
   正确做法是**一条直线**：报清现象，请客户退出正在运行的 app 实例；
   疑似残留进程先跑 `pnpm dev:clean`，清不掉就把进程名与 PID 交给客户。
   例外须**事前**征得同意、**用后即删**、同步登记文档。见 **D51** 与 **P4**。
5. **IDE 与测试互斥**：VS Code 的 rust-analyzer 会对**同一个** `src-tauri/target` 跑
   `cargo check`，与 `cargo test` 争构建锁（实测日志里出现
   `Blocking waiting for file lock on build directory`），而且 `check` 只产 rmeta、
   对 `cargo test` 需要的 rlib **毫无帮助**——等于白编译一遍。
   仓库已在 `.vscode/settings.json` 关闭 `check.allTargets`（不再为 test / bench target 做检查）；
   跑门禁前确认 IDE 的 check 已经结束。
