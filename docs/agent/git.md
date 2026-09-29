# Git 分支、提交与仓库约定

> 状态：现行
> 本手册是分支模型、约定式提交细节与仓库信息的权威落点。
> **保留 AGENTS.md 原有编号（§1.1–§1.3、§2.1–§2.5、§3）**：仓储内多处软引用按编号指向本节内容，
> 搬迁只搬正文、不改编号（理由与 D52 保留 `docs/decisions.md` 路径一致）。
>
> **§1.4 / §1.5（合并与推送控制权、禁止事项）不在此展开**：它们是必须常驻的硬红线，
> 权威落点是 [`AGENTS.md`](../../AGENTS.md) §1，本手册不复制。

## 1. Git 分支规范

### 1.1 分支模型

| 类型       | 命名          | 用途                             | 规则                                                         |
| ---------- | ------------- | -------------------------------- | ------------------------------------------------------------ |
| 主分支     | `main`        | **开发主线 + 发布基线**          | **禁止直接修改**；所有改动都由 `develop/xxx` 分支合并进来；**发布通过在 `main` 上打 tag 完成**（tag 只在客户明确要求时创建） |
| 开发分支   | `develop/xxx` | 单轮功能 / 修复                  | **从 `main` 创建**，完成后**合并回 `main`**；`xxx` 自定义命名，但必须能看出这轮开发在做什么 |

命名示例：`develop/fix-terminal-timeout`、`develop/feat-proxyjump`。

> **历史沿革（2026-09-18，客户决定）**：首发阶段用过一条专职集成分支 `develop/init`。
> 首次发布基线建立后，客户明确"它只是首发特有的分支"，**该分支已删除**。
> 此后一律以 `main` 为主分支——`develop/xxx` 从 `main` 开、合并回 `main`。
> **不要再创建或引用 `develop/init`。**

### 1.2 工作流程

1. 需要新增功能、修复 bug、优化等，**从 `main` 创建 `develop/xxx` 分支**；
2. 在开发分支上完成开发与测试，提交信息遵循 §2 的约定式提交；
3. 开发完成并验证通过后，将 `develop/xxx` 合并回 **`main`**（合并由客户决定，见 AGENTS.md §1.4）；
4. **`main` 同时是开发主线与发布基线**：日常开发**不直接**落在它上面，改动一律经开发分支合并进来；
5. **合并进 `main` ≠ 发布**：发布是在 `main` 的稳定代码上**打 tag**；
   tag 推送后由**发布流水线**出安装包并创建 Release，流程见 **D54**。

### 1.3 示例命令

```bash
# ① 从主分支 main 创建开发分支
git checkout main
git checkout -b develop/fix-terminal-timeout

# ② 在开发分支上开发、提交、测试（提交信息遵循 Conventional Commits）

# ③ 开发测试完成后，等待客户指示，再合并回 main
#    （打 tag 属发布事项，需客户单独指示）
```

## 2. Git Commit 规范（约定式提交）

> 遵循 [Conventional Commits 1.0.0](https://www.conventionalcommits.org/zh-hans/v1.0.0/)。
> 本节为提炼摘要，与官方规范冲突时以官方规范为准。

### 2.1 提交信息结构

```
<type>[可选 scope]: <描述>

[可选正文]

[可选脚注]
```

- `type` 后接可选的 scope（圆括号包围）、可选的 `!`，然后是**英文半角冒号 + 一个空格**，再接描述；
- 描述与正文之间、正文与脚注之间各**空一行**；
- 脚注令牌用 `-` 连字符（如 `Reviewed-by`），唯一例外是 `BREAKING CHANGE`。

### 2.2 类型（type）

| 类型              | 含义                                               | SemVer 影响 |
| ----------------- | -------------------------------------------------- | ----------- |
| `feat`            | 新增功能（**必须**使用）                           | MINOR       |
| `fix`             | 修复 bug（**必须**使用）                           | PATCH       |
| `BREAKING CHANGE` | 破坏性变更（见 §2.3）                              | MAJOR       |
| `docs`            | 文档变更                                           | 无          |
| `refactor`        | 重构（既非新增功能也非修复 bug）                   | 无          |
| `perf`            | 性能优化                                           | 无          |
| `test`            | 测试的增删改                                       | 无          |
| `build`           | 构建系统或外部依赖变更                             | 无          |
| `ci`              | CI 配置变更                                        | 无          |
| `chore`           | 其他不修改 src 或测试的杂项                        | 无          |
| `style`           | 代码格式（不影响代码含义的空格、格式化等）         | 无          |
| `revert`          | 还原提交（脚注引用被还原的提交，如 `Refs: 676104e`） | 无        |

### 2.3 破坏性变更的声明方式

两种方式可同时使用：

1. **脚注方式**（令牌必须大写）：

   ```
   feat: allow provided config object to extend other configs

   BREAKING CHANGE: `extends` key in config file is now used for extending other config files
   ```

2. **`!` 方式**（描述中应说明破坏点）：

   ```
   feat(api)!: send an email to the customer when a product is shipped
   ```

### 2.4 完整示例

```
fix(terminal): 修复长耗时命令查询状态时偶发超时

为命令执行记录引入递增序号，查询时只返回最新状态；
移除已过时的固定超时兜底逻辑。

Reviewed-by: Z
Refs: #123
```

### 2.5 本项目约定

- 描述与正文使用**中文**；
- scope 建议与模块对应，推荐词表（按仓库历史提交的**实际用法**整理，2026-09-16 核对）：`host`（SSH 主机）、`ssh`（SSH 会话与协议）、`terminal`（SSH 终端）、`sudo`（提权）、`mcp`（MCP Server）、`ui`（桌面界面）、`settings`（设置）、`tray`（托盘）、`update`（版本检查）、`ci`（文档类改动用 `docs` 作 type，scope 仍写受影响模块，如 `docs(sudo)`）；
- 涉及前后端契约的改动（如 Tauri IPC 命令签名、MCP 工具入参/返回结构），正文应说明两侧是否已同步。

## 3. Git 仓库信息

- 托管平台：GitHub，仓库 **public**（2026-09-19 完成开源化，见 **D55**）
- 主分支：`main`（**开发主线 + 发布基线**；红线见 AGENTS.md §1.4 / §1.5）
  - **2026-09-18 建立**：经客户指示由 `develop/init` 建立（当时两者指向同一提交）；
    同日客户明确 `develop/init` 只是首发专用分支，**该分支已删除**（见 §1.1 的历史沿革）。
  - 本地与 `origin/main` 保持同步；**发布 = 在 `main` 的稳定提交上打 tag**
    （机制见 **D54 / D55**；推送与打 tag 仍需客户单独指示）。
  - **已发布版本不登记在任何文档里**：`git tag -l` 与仓库的 Releases 页就是事实来源
    （依据：不登记能从权威落点现取的内容，见 **D57 / D59**）。
- 远程仓库地址：以本地 `git remote -v` 为准（`git@github.com:mangfu26/mf-perch.git`）
- **行尾策略由仓库根的 `.gitattributes` 固定**（逐后缀显式声明 `text` / `-text`，文本入库一律 LF），
  **不要改成依赖各机器的 `core.autocrlf`**：后者只作用于单台机器，换机后要么把 CRLF 提交进仓库
  （此后每次改动都显示成整文件重写），要么制造"内容没变却显示已修改"的假象。新增文件类型时
  在 `.gitattributes` 里补一行；**Windows 批处理必须写 `*.bat text eol=crlf`**。
