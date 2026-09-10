# 开发排错

本文记录开发过程中实际遇到的环境问题与解决办法。每条都来自真实发生的状况，不是推测。

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

## 首次 `pnpm tauri dev` 编译很慢（2–3 分钟）

正常现象：Rust 侧依赖（`russh`、`rusqlite`/SQLite、`rmcp` 等）首次需要全量编译。
之后增量编译通常只占几秒到几十秒。若修改了 `Cargo.toml` 的依赖，会重新触发较长编译。

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
