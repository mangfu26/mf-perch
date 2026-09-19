# 更新日志

本项目的所有重要变更都会记录在此文件中。

格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

## [v0.1.0] - 2026-09-19

首个公开版本。

mf-perch 是一款 Windows 桌面应用，让 AI Agent 通过内嵌的 MCP Server
安全地经 SSH 操作主机，人类保留完整的凭据控制权与命令审计权。

### 新增

- SSH 主机与凭据管理：密码 / 私钥两种认证，凭据字段级加密存储，对 AI Agent 不可见；
- 常驻终端会话：`cd` / `export` 等 shell 状态跨命令保留，支持同步执行与长任务异步查询；
- 内嵌 MCP Server：Bearer Token 鉴权，可接入任意 MCP 客户端；
- sudo 提权策略（`deny` / `ask` / `auto`），提权密码走独立通道，不进入 Agent 可见域；
- 命令历史与终端审计：AI 执行的每条命令可查，人类可删除、AI 仅可归档；
- 应用内版本更新检查（提示 + 跳转下载，不自动安装）；
- 系统托盘、暗色 / 亮色双主题。

[v0.1.0]: https://github.com/mangfu26/mf-perch/releases/tag/v0.1.0
