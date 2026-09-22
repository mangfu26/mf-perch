<p align="center">
  <a href="https://github.com/mangfu26/mf-perch">
    <img src="docs/assets/logo.png" width="160" height="160"
         alt="mf-perch 图标：深空渐变底色上，一只紫色小鸟停在终端提示符旁">
  </a>
</p>

# mf-perch

**让 AI Agent 像人类使用终端软件一样，安全地通过 SSH 操作和管理主机。**

mf-perch 是一款 Windows 桌面应用（Tauri）：人类在图形界面里管理 SSH 主机与凭据、
审计 AI 执行的每一条命令；AI Agent 则通过应用内嵌的 MCP Server，
在已授权的主机上创建终端、执行命令、查询状态——全程**看不到任何密码或私钥**。

## 核心特性

- **SSH 主机管理**：图形界面维护主机与认证信息（密码 / 私钥），字段级加密存储于本机。
- **常驻终端会话**：基于持久 SSH 会话执行命令，`cd`、`export` 等状态跨命令保留，
  支持同步返回与长耗时命令的异步查询。
- **AI 提权受控**：`sudo` 提权支持 `deny` / `ask` / `auto` 三种策略，另设
  `not_needed`（该主机以特权身份登录，直接执行即可）；
  提权密码走独立通道，不进入 AI Agent 可见的用户域。
- **内嵌 MCP Server**：向任意 MCP 客户端（如 Claude Desktop、各类 CLI Agent）
  暴露受控的主机操作工具，Bearer Token 鉴权。
- **完整审计**：每条 AI 命令都有历史记录与输出留存，人类可查看、可删除；
  AI 只能归档终端，不能删除。
- **版本更新检查**：应用内提示新版本并跳转下载，不自动安装。

## 权限模型

| 能力 | 人类 | AI Agent |
| ---- | ---- | ---- |
| 查看 / 编辑主机与凭据 | ✅ | ❌（凭据内容完全不可见） |
| 在终端中输入命令 | ❌（只读审计） | ✅（经 MCP 工具） |
| 删除终端 | ✅ | ❌（仅可归档） |
| 提权（sudo） | — | 受 `deny` / `ask` / `auto` / `not_needed` 策略约束 |

## 安装

1. 到 [Releases](https://github.com/mangfu26/mf-perch/releases) 下载最新版本的
   `.msi` 或 `.exe` 安装包并安装。
2. 安装包未做代码签名，首次运行如遇 Windows SmartScreen 提示，选择"仍要运行"。

## 快速上手

1. **添加主机**：在主机页填写地址、端口、用户名，并绑定密码或私钥凭据。
2. **启用 MCP Server**：在设置页开启服务端，应用会显示连接地址与访问 Token
   （默认监听 `127.0.0.1`，端口从 `50001` 起自动选择）。
3. **接入 AI 客户端**：在你的 MCP 客户端中配置上述地址，并添加请求头
   `Authorization: Bearer <token>`。例如：

   ```json
   {
     "mcpServers": {
       "mf-perch": {
         "url": "http://127.0.0.1:50001/mcp",
         "headers": { "Authorization": "Bearer <在设置页复制的 Token>" }
       }
     }
   }
   ```

4. AI 即可列出主机、创建终端并执行命令；你在界面中实时审计一切。

## 已知边界

- 仅支持 Windows x64；
- 不支持跳板机（ProxyJump）；
- 开启"允许远程连接"后，Token 与命令内容在网络中明文传输，请仅在可信局域网使用；
- 配置了 `Defaults requiretty` 的 sudo 主机不支持提权。

## 对于开发者

- 开发环境：`pnpm install` 后 `pnpm tauri dev`（需 Rust 与 Node 24+）；
- 构建安装包：`pnpm tauri build`；
- 测试：`cargo test`（`src-tauri/` 下）与 `pnpm test`；
- 设计与决策文档见 [`docs/`](docs/)，协作规范见 [`AGENTS.md`](AGENTS.md)。

## License

[Apache License 2.0](LICENSE)
