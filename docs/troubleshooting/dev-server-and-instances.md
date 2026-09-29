# 开发服务器与运行实例（端口 1420 / 端口漂移 / 构建被锁）

> 状态：现行
> `pnpm tauri dev` 起不来、MCP 端口不是 50001、`cargo build` 被正在运行的 app 挡住时，怎么定位与处置。

---

## `pnpm tauri dev` 报「Port 1420 is already in use」

### 现象

```
$ pnpm tauri dev
     Running BeforeDevCommand (`pnpm dev`)
$ vite
error when starting dev server:
Error: Port 1420 is already in use
    at httpServerStart (.../vite/dist/node/chunks/node.js:...)
[ELIFECYCLE] Command failed with exit code 1.
       Error The "beforeDevCommand" terminated with a non-zero status code.
```

### 原因

Tauri CLI 在 Windows 上被**中断**时（`Ctrl+C`、关闭终端、进程被杀），它拉起的子进程不一定随之退出，会变成**孤儿进程**继续存活：

| 残留进程 | 占用资源 | 后果 |
| ---- | ---- | ---- |
| Vite dev server | TCP `1420` | 下一次 `tauri dev` 直接失败 |
| `cargo run`（应用本体） | 编译锁 + 内存 | 编译变慢或失败；编译完成后可能自行弹出应用窗口 |
| `mf-perch.exe` | TCP `50001+` | 新实例改用 `50002`、`50003`…… 造成端口漂移 |

`1420` 是 Tauri 要求的**固定端口**（`vite.config.ts` 中 `strictPort: true`），
被占用时 Vite 不会自动换端口，而是直接报错退出。

### 解决办法

```bash
pnpm dev:clean
```

该脚本（`scripts/clean-dev.mjs`）会：

1. 查找占用 `1420` 与 `50001–50100` 的进程；
2. **仅终止命令行中包含本项目目录的进程**；
3. 对无法确认归属的进程**只提示、不终止**，避免误杀其他项目的开发服务器；
4. 清理残余的 `mf-perch.exe` 应用进程。

清理完成后即可重新运行 `pnpm tauri dev`。

### 为什么不能更激进地清理

曾考虑「凡是命令行含 `vite`/`pnpm` 字样的进程都可终止」，但这个判定
会误杀**其他项目**恰好占用同一端口的 Vite 服务。因此最终改为严格按
项目路径匹配，宁可让用户手动处理，也不擅自终止不确定归属的进程。

---

## 端口漂移：MCP 端点不是每次都在 50001

这是**设计行为**，不是故障（Q2 决策）：

- 首次启动从 `50001` 开始查找可用端口，成功后**持久化**；
- 下次启动优先复用该端口；
- 若该端口被占用（例如残留了上一个实例），**重新从 `50001` 递增**查找。

真实日志示例（同一台机器上有两个实例时）：

```
INFO mf_perch_lib::mcp::endpoint: 持久化端口 50001 已被占用，重新从 50001 开始查找
INFO mf_perch_lib::mcp::endpoint: MCP 端点监听端口 50002
INFO mf_perch_lib::mcp::server: MCP Server 已启动：http://127.0.0.1:50002/mcp
```

**当前端口的权威来源是应用设置页**（或日志），不要假设它一定是 `50001`。
若发现端口不断向后漂移，说明有残留实例，运行 `pnpm dev:clean` 清理。

---

## app 正在运行时 `cargo build` 失败：禁止用备用 target 目录绕过（D51 / P4）

### 现象

应用实例还在运行时执行构建 / 验证，cargo 覆写 `src-tauri/target/debug/mf-perch.exe` 失败
（Windows 会锁住正在执行的 exe，报"另一个程序正在使用此文件" / `os error 5` 或 `32`）。

### 曾经的处置（**错误，已废弃，不要再用**）

把 `CARGO_TARGET_DIR` 指到一个备用目录（本项目实际留下的是 `src-tauri/target-verify`），
让编译绕开被锁的文件继续进行。

**代价（2026-09-18 实测）**：备用目录不是"多出一个 exe"，而是**整棵依赖图重新编译一份**——

| 目录 | 体积 | 文件数 |
| ---- | ---- | ---- |
| `src-tauri/target/` | 11 GB | — |
| `src-tauri/target-verify/`（已删除） | **7.9 GB** | 5803 |

即主缓存的 72% 被重复了一份，而且 app 一关就**永久失去价值、无人回收**。
这条绕过只写在 `.gitignore` 的注释里（`src-tauri/target-*/`），`AGENTS.md` 和本文档都没有记录，
所以它既没被复用、也没被清理——典型的"做过但没沉淀"。

### 正确做法（"一条直线"，见 D51 / P4）

**停下来向客户求助，由客户退出正在运行的 app 实例**，然后在**同一个** `src-tauri/target` 上正常构建。

- 客户已明确承诺会配合退出实例来腾出构建条件——不要替客户决定"不打扰他、绕过去"；
- 若是**残留 / 孤儿进程**占着 exe（不是客户正在用的窗口），先跑 `pnpm dev:clean`
  （见本文第一节的 `scripts/clean-dev.mjs`），它会按项目路径严格匹配后清理；
- 仍清不掉就把**进程名与 PID 报给客户**，由客户处置。**不要**改用备用 target、备用端口或备用路径。

### 预防

- 原则见 [`design/principles.md`](design/principles.md) **P4**：遇到障碍求助，不得用绕过手段维持推进；
- 任何与客户/本地环境冲突的障碍（锁文件、占端口、只读文件、实例冲突）都适用同一条判断：
  **绕过去的那一步，往往比原来的问题更贵**；
- 不要因为"`.gitignore` 已经排除了 `target-*/`"就认为留这类目录无害——
  忽略规则只解决"不入库"，解决不了磁盘占用与认知负担。
