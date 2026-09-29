# MCP 工具 schema：可移植性告警与复验

> 状态：现行
> MCP Inspector 对可空参数报 schema 可移植性告警时，它是什么意思、要不要修、怎么复验。

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
