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

---

## MCP Inspector 报「工具 schema 可移植性」告警

### 现象

用 `npx @modelcontextprotocol/inspector` 连接应用后，部分工具（`create_terminal`、
`run_command`、`run_command_async`、`get_command_status`、`list_terminals`）
被标记 `Schema portability: 0 error(s), 1 warning(s)`，警告指向各工具的可空参数：

> `type` is an array (`["integer","null"]`). The array form is legal JSON Schema,
> but several MCP clients read `type` as a single string and either reject the tool
> or drop the constraint.

### 原因

`schemars` 1.x 默认把 `Option<T>` 生成为 `"type": ["<T>", "null"]`。该写法**合法**
（JSON Schema 2020-12），我们的 server 与严格客户端都能正确处理，所以这是**兼容性提示，
不是功能故障**。但部分客户端只接受 `type` 为单个字符串，会丢弃约束甚至拒绝整个工具。

### 解决办法

已在 D32 中规避：可空参数改用等价的 `anyOf` 表达，并内联展开。相关代码在
`src-tauri/src/mcp/tools.rs` 的 `Nullable<T>`。

### 如何复验（无需启动桌面应用）

新增了示例程序，用内存数据库起一个真实 MCP 端点，不触碰用户数据目录：

```bash
# ① 启动探针，记下打印出的 MCP_URL 与 MCP_TOKEN
cargo run --features mcp --example mcp_schema_probe

# ② 另开终端，用 Inspector 的 --strict 检查（无输出 = 无问题）
npx -y @modelcontextprotocol/inspector --cli \
  --transport http --server-url http://127.0.0.1:50001/mcp \
  --header "Authorization: Bearer <上一步的 MCP_TOKEN>" \
  --method tools/list --strict
```

`--strict` 会把每个问题打印到 stderr；**没有任何输出即为通过**。

### 注意

- 该告警是 **warning 而非 error**，`--strict` 仍会以退出码 0 结束——判断依据是
  **stderr 是否有内容**，不要只看退出码。
- 后续新增带可选参数的工具时，凡 `Option<T>` 字段都要加
  `#[schemars(with = "Nullable<T>")]` 并保留 `#[serde(default)]`，否则告警会重新出现。

---

## `cargo build` / `cargo test` 报 `E0463` / `E0462` / `E0460`：`.rlib` 缺失、crate 无法加载

### 现象

构建在**没有任何代码错误**的情况下失败，报错彼此矛盾且都指向依赖：

```
error[E0463]: can't find crate for `mf_perch_lib`
error: crate `tokio` required to be available in rlib format, but was not found in this form
error[E0460]: found possibly newer version of crate `webview2_com_sys` which `mf_perch_lib` depends on
error[E0786]: found invalid metadata files for crate `webview2_com_sys`
  = note: failed to mmap file '...\libwebview2_com_sys-....rlib': 页面文件太小，无法完成操作。 (os error 1455)
```

特征：`target/debug/deps` 里 `.d` 文件齐全，但对应的 `.rlib` / `.rmeta` **缺失或只有 0 字节**；
cargo 的指纹仍认为"已是最新"，于是不去重建，直到链接阶段才暴露产物是坏的。

### 原因

**Windows 页面文件（虚拟内存）耗尽**。最后那行 `os error 1455`（"页面文件太小"）才是根因：
rustc 写 `.rlib` 失败，留下不完整的产物与"最新"的指纹记录。上面的 `E0460`
（"possibly newer version"）是**症状而不是病因**，不要去查依赖版本冲突。

本项目特别容易触发，因为：

1. Rust 侧依赖重（`tauri`、`russh`、`rusqlite` bundled SQLite、`rmcp`），默认并发会同时跑
   N 个 rustc（N = CPU 核数），内存峰值很高；
2. `src-tauri/target` 是**共享资源**——同时开两个构建（两个 Agent / 两个终端，
   或一个 `pnpm tauri dev` 加一个 `cargo test`）会成倍叠加内存压力。
   cargo 的文件锁只能让构建**串行**，防不住单个构建自身的内存峰值。

### 解决办法

```bash
cd src-tauri
cargo clean          # 必须先清：坏产物的指纹是"最新"，不清就不会重建
cargo test -j 2      # 限制并发，避免再次耗尽页面文件
```

`target` 约 40 GB，`cargo clean` 后首次全量编译需数分钟到十几分钟；之后增量编译恢复正常。

### 预防

- **并发跑构建时一律加 `-j 2`**（或设 `CARGO_BUILD_JOBS=2`）；
- 不要在 `pnpm tauri dev` 还在编译时另开 `cargo test` / `cargo build`；
- 看到 `E0460` / `E0463` 先往下翻有没有 `os error 1455`，有就直接走上面的恢复步骤，
  不要改 `Cargo.toml`。
