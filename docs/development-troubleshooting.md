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

---

## `pnpm add` 报 `ERR_PNPM_UNEXPECTED_STORE`：装新依赖会重链全部依赖

### 现象

```
[ERR_PNPM_UNEXPECTED_STORE] Unexpected store location
The dependencies at "D:\projects\mf-perch\node_modules" are currently linked from
the store at "D:\.pnpm-store\v11".
pnpm now wants to use the store at "D:\projects\mf-perch\.pnpm-store\v11".
```

若改用 `--store-dir` 指向原 store，还可能继续报：

```
[ERR_SQLITE_ERROR] unable to open database file
```

### 原因

pnpm 的默认 store 在**项目所在盘**（`<项目>/.pnpm-store`），而本机的 `node_modules`
当初是从**盘根的另一个 store**（`D:\.pnpm-store\v11`）链出来的。两者不一致时 pnpm 会
拒绝继续——它不能把新包装进一个与现有链接来源不同的 store。

`ERR_SQLITE_ERROR` 则是**受限执行环境**的次生现象：盘根 store 在项目目录之外，
若执行环境只允许写项目目录，pnpm 打不开 store 的索引库。

### 解决办法

**保留原 store，不要重新 install**：

```bash
pnpm add -D <包名> --store-dir "D:\.pnpm-store\v11"
```

### 为什么不能直接 `pnpm install`

那会把**全部依赖**重新链接到新 store：耗时长，而且期间 `node_modules` 处于变动状态——
此时若有别的进程在用（另一个 Agent、`pnpm dev`、`pnpm tauri dev`），会直接失败。

### 预防

- 装包前先用 `pnpm config get store-dir` 与现有链接来源对一下；不一致就带上 `--store-dir`；
- 需要写项目目录之外的 store 时，确认当前执行环境允许——受限沙箱下会被拒。

---

## `pnpm test` / `pnpm check:ipc` 报 `spawn EPERM`

### 现象

```
Error: Build failed with 1 error:
[plugin externalize-deps]
Error: spawn EPERM
    at ChildProcess.spawn (node:internal/child_process:441:11)
    ...
    at optimizeSafeRealPathSync (.../vite/dist/node/chunks/node.js:2497:2)
    at windowsSafeRealPathSync (.../vite/dist/node/chunks/node.js:2483:2)
```

报错点在 `vite.config.ts` / `vitest.config.ts` **加载阶段**，看起来与测试内容无关。

### 原因

Vite 在 Windows 上解析真实路径时要先执行一次 `net use`（探测网络驱动器映射）。
该调用以 **piped stdio** 启动子进程；在**受限执行环境**（严格沙箱、受限服务账户）下，
创建管道会被拒绝，于是抛 `EPERM`。

这是环境限制，**不是配置或代码问题**——同一份代码在普通终端里正常运行。

### 解决办法

在允许创建子进程管道的环境里运行（普通终端，或放宽沙箱的写/执行范围）。

### 预防

- 看到 `spawn EPERM` + 调用栈里出现 `windowsSafeRealPathSync`，直接判定为环境限制，
  不要去改 `vite.config.ts` / `vitest.config.ts`；
- 与之无关：这条与上面的 `ERR_SQLITE_ERROR` 经常成对出现，但**根因不同**——
  前者是 store 位置，后者是子进程权限。

---

## `pnpm check:secrets` 第 1 步总是失败（脚本被 WSL 的 bash 执行）

### 现象

提交前自检的第 1 步（真实密钥形态扫描）总是报"扫描未能执行（node 不可用？）"，
整条命令以退出码 1 结束；而第 2–4 步却是 OK：

```
== 1/4 扫描真实密钥形态 ==
  警告：扫描未能执行（node 不可用？），请人工确认无凭据入库
== 2/4 检查凭据类文件是否被跟踪 ==
  OK：无凭据类文件被跟踪
...
自检未通过：请处理上述问题后再提交（AGENTS.md 2.6 安全红线）。
```

### 原因

三个环节叠加（均实测确认）：

1. `package.json` 里写的是 `bash scripts/check-secrets.sh`，而 Windows 上 `bash`
   解析到 **WSL 的 bash**（`C:\Windows\system32\bash.exe`），**不是 Git Bash**：

   ```powershell
   PS> (Get-Command bash).Source
   C:\Windows\system32\bash.exe
   PS> bash -c 'uname -s; command -v node; command -v git'
   Linux
   NO_NODE
   /usr/bin/git
   ```

2. WSL Ubuntu 里**有 git 但没有 node**，因此第 2–4 步（纯 git 命令）正常，
   唯独第 1 步的 `node -e` 失败（退出码 127）；
3. 原脚本把标准错误重定向进了 `/dev/null`（`2>/dev/null`），把"命令不存在"
   这个真实原因也一并吞掉，只留下一句猜谜式的"node 不可用？"。

**脚本逻辑本身没问题**：改用 Git Bash（`C:\Program Files\Git\bin\bash.exe`）
跑同一份 `.sh`，四步全过、退出码 0。

### 解决办法

**已改为纯 node 脚本**，不再依赖任何 shell：

| 项 | 变化 |
| ---- | ---- |
| `scripts/check-secrets.mjs` | 新增，承载全部四步检查 |
| `package.json` | `"check:secrets": "node scripts/check-secrets.mjs"` |
| `scripts/check-secrets.sh` | **已删除**（单一入口，避免两份逻辑并存） |

```bash
pnpm check:secrets
```

### 预防（重要）

- **不要把这条门禁改回 `.sh`**：只要它由 `bash` 执行，Windows 上就会再次落到
  WSL 的 bash，问题复发。要加检查就往 `.mjs` 里加。
- **不要用重定向吞掉子进程错误**：正是 `2>/dev/null` 把"node 不存在"变成了一句
  猜谜提示。宁可输出啰嗦的真实报错，也不要为了"输出干净"牺牲可诊断性。
- **一条长期失败的安全门禁等于没有门禁**——它会被当成噪声忽略，真出问题时没人拦。
  门禁必须在本机**默认可通过**（`pnpm check:secrets` → 退出码 0），
  红灯只应指向**真实问题**。
- 改动这类门禁后要做**差分验证**（P3）：故意放入假私钥 / 假令牌 / 凭据类文件，
  确认它**确实会红灯**；只验证"能通过"说明不了任何事。

