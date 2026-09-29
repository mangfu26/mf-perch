# 开发排错

> 状态：现行
> 开发过程中真实遇到的环境问题的定位与处置；本文是索引，正文按主题分片在 `docs/troubleshooting/`。

本文记录开发过程中实际遇到的环境问题与解决办法。每条都来自真实发生的状况，不是推测。

## 怎么用

1. 在下表按**报错原文片段或现象**定位到**一个**主题文件，只打开那一个，不要整目录通读；
2. 分片按"排错时怎么找"划分（开发进程与端口、构建内存与耗时、pnpm / Node 环境、WSL SSH 联调、测试判读、MCP schema、数据库迁移），**不按时间顺序**；
3. 正文自原单文件**逐字搬运**，小节标题保持原样；分片里原有"上一节 / 下一节 / 本文第一节"的互指措辞一律照旧，指向的是**同一分片内**的相邻小节——唯一的跨分片指针见文末「跨分片指针」；
4. 新增排错记录：写进对应分片，并在本表补一行关键词；若某条只是某时间点的快照（结论可能已变），该分片抬头改成 `> 状态：快照（YYYY-MM-DD）` 并保留原日期（状态口径见 [`docs/agent/docs.md`](agent/docs.md) §4.1）。

## 症状关键词 → 主题文件

| 报错原文片段 / 现象 | 主题文件 · 小节 |
| --- | --- |
| `Port 1420 is already in use`、`beforeDevCommand` 非零退出、残留 / 孤儿进程 | [`troubleshooting/dev-server-and-instances.md`](troubleshooting/dev-server-and-instances.md) · `pnpm tauri dev` 报「Port 1420 is already in use」 |
| MCP 端口不是 `50001`、端口不断向后漂移、日志 `持久化端口 50001 已被占用` | [`troubleshooting/dev-server-and-instances.md`](troubleshooting/dev-server-and-instances.md) · 端口漂移：MCP 端点不是每次都在 50001 |
| app 正在运行时 `cargo build` 失败、`os error 5` / `32`、另一个程序正在使用此文件、`CARGO_TARGET_DIR` / 备用 target 目录（D51 / P4） | [`troubleshooting/dev-server-and-instances.md`](troubleshooting/dev-server-and-instances.md) · app 正在运行时 `cargo build` 失败：禁止用备用 target 目录绕过（D51 / P4） |
| 首次 `pnpm tauri dev` 编译 2–3 分钟、改了 `Cargo.toml` 依赖后重编很久 | [`troubleshooting/cargo-build-memory.md`](troubleshooting/cargo-build-memory.md) · 首次 `pnpm tauri dev` 编译很慢（2–3 分钟） |
| `E0463` / `E0462` / `E0460` / `E0786`、`can't find crate`、`.rlib` 缺失或 0 字节、`页面文件太小` / `os error 1455` | [`troubleshooting/cargo-build-memory.md`](troubleshooting/cargo-build-memory.md) · `cargo build` / `cargo test` 报 `E0463` / `E0462` / `E0460`：`.rlib` 缺失、crate 无法加载 |
| 该不该把 `-j` 调大、`sccache`、`target` 在机械盘、磁盘活动长期 100% | [`troubleshooting/cargo-build-memory.md`](troubleshooting/cargo-build-memory.md) · 为什么不把 `cargo` 的并行度调大 |
| `test result: ok` 但命令退出码 1、`NativeCommandError`、`Select-Object -Last N` 截断 | [`troubleshooting/test-result-misreads.md`](troubleshooting/test-result-misreads.md) · `cargo test` 明明全绿，命令却以退出码 1 结束（警告走 stderr 被 PowerShell 当成错误） |
| 还原文件后测试仍红、输出里没有 `Compiling mf-perch`、`cargo clean -p mf-perch`、刷新 mtime | [`troubleshooting/test-result-misreads.md`](troubleshooting/test-result-misreads.md) · `Copy-Item` 还原文件后，测试仍在跑**旧实现**（mtime 被一起复制） |
| `ERR_PNPM_UNEXPECTED_STORE`、`ERR_SQLITE_ERROR`、`pnpm add` 要重链全部依赖、`--store-dir` | [`troubleshooting/pnpm-and-node-env.md`](troubleshooting/pnpm-and-node-env.md) · `pnpm add` 报 `ERR_PNPM_UNEXPECTED_STORE`：装新依赖会重链全部依赖 |
| `spawn EPERM`、调用栈里的 `windowsSafeRealPathSync`、`pnpm test` / `pnpm check:ipc` 起不来 | [`troubleshooting/pnpm-and-node-env.md`](troubleshooting/pnpm-and-node-env.md) · `pnpm test` / `pnpm check:ipc` 报 `spawn EPERM` |
| `pnpm check:secrets` 第 1 步失败、`扫描未能执行（node 不可用？）`、Windows 上 `bash` 落到 WSL | [`troubleshooting/pnpm-and-node-env.md`](troubleshooting/pnpm-and-node-env.md) · `pnpm check:secrets` 第 1 步总是失败（脚本被 WSL 的 bash 执行） |
| `Load key "...": bad permissions`、`ssh` 什么都不打印一直挂着、`Permission denied (publickey,password)` | [`troubleshooting/wsl-ssh-test-env.md`](troubleshooting/wsl-ssh-test-env.md) · Windows 侧手工验证 SSH 时：`Load key "...": bad permissions`，或 `ssh` 挂住不返回 |
| 三组真实环境用例同时 `Permission denied`、`authorized_keys` 却没问题、`ss -ltnp` 归属不是 sshd | [`troubleshooting/wsl-ssh-test-env.md`](troubleshooting/wsl-ssh-test-env.md) · 测试端口被别的进程占了：一切换机/装软件就出现"公钥被拒"，但 `authorized_keys` 没问题 |
| Windows 侧 `Connection refused`、WSL 内 `ss -ltn` 有而 `netstat` 没有、localhost 转发随实例消失 | [`troubleshooting/wsl-ssh-test-env.md`](troubleshooting/wsl-ssh-test-env.md) · Windows 侧连不上 WSL 里的 sshd：`Connection refused`，而 WSL 内一切正常 |
| `Schema portability: 0 error(s), 1 warning(s)`、`type` 是 `["integer","null"]`、`Nullable<T>`、`mcp_schema_probe` | [`troubleshooting/mcp-tool-schema.md`](troubleshooting/mcp-tool-schema.md) · MCP Inspector 报「工具 schema 可移植性」告警 |
| `请求的操作需要提升。(os error 740)`、弹出「程序兼容性助手」、报告里 `never executed`、测试文件名含 `update` / `install` / `setup` / `patch` | [`troubleshooting/windows-test-exe-740.md`](troubleshooting/windows-test-exe-740.md) · Windows 上 `cargo test` 拉不起某个测试 exe：`请求的操作需要提升。(os error 740)` |
| 运行期 `no such column`、门禁全绿、`user_version` 已到位、`RELEASED_SCHEMA_STEPS`、迁移只按形状判定 | [`troubleshooting/db-schema-migration.md`](troubleshooting/db-schema-migration.md) · 运行期报 `no such column`，而迁移用例全绿：`user_version` 已到位、表里却少列 |

## 跨分片指针

- `Windows 上 cargo test 拉不起某个测试 exe…` 一节里的"回去查 E0463 一类产物问题"，现落在 [`troubleshooting/cargo-build-memory.md`](troubleshooting/cargo-build-memory.md)。
- 其余"上一节 / 下一节 / 本文第一节"的指代都在同一分片内成立，**未做改写**。
