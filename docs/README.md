# 项目文档索引（docs/）

> 本目录是 mf-perch 的**项目记忆库**。
> 阅读入口：`AGENTS.md` → 本文件 → 按需进入各专题文档。
>
> 如果你（人或 AI Agent）刚接手本项目，**先读 [`AGENTS.md`](../AGENTS.md)，再读本文件**，
> 然后按需深入下面列出的专题文档。不要凭直觉重新推导或推翻已确认的结论。

---

## 一、这个目录的定位

`docs/` 承担三件事，缺一不可：

1. **让接手的 AI Agent 不失忆**——知道项目是什么、为什么这样设计、哪些结论已定不可推翻。
2. **让人类（客户 / 未来的维护者）能核对**——决策的理由、风险的取舍、能力边界。
3. **让踩过的坑不再踩第二次**——真实发生过的环境问题、被证伪的方案、修复过的缺陷。

它**不是**：

- 不是临时的草稿区：写进来的内容按"长期资产"对待；
- 不是对话记录的存档：只沉淀**结论与理由**，不抄录对话过程；
- 不是代码的替代品：代码本身能说明的（函数做什么）不在这里重复，这里只记**为什么**。

### 时效性只有两种，必须明确

| 标记 | 含义 | 维护要求 |
| ---- | ---- | ---- |
| **现行** | 当前有效的结论，与代码一致 | 代码变更后**必须同步更新** |
| **快照** | 某个时间点的报告，之后可能已变化 | 只读，不追改；如需更新则新写一份 |

> 约定：**没有标记"快照"的文档一律视为现行**。写快照时必须在开头显式标注，避免后人把过期结论当现行方案。

---

## 二、文档清单

### A. 决策记录 —— 为什么这么做

| 文档 | 内容 | 时效性 |
| ---- | ---- | ---- |
| [`decisions.md`](decisions.md) | **技术决策记录（ADR）**：D1 起逐条记录决策、背景、影响。**新增决策追加在末尾，已定决策不擅自修改** | 现行 |

> `decisions.md` 是**技术决策的唯一事实来源**。其它文档若与它冲突，以它为准。

### B. 设计说明 —— 机制如何运作

| 文档 | 内容 | 时效性 |
| ---- | ---- | ---- |
| [`design/terminal-session.md`](design/terminal-session.md) | 终端会话模型（方案 C：常驻会话 + NUL 分帧 + 自打印标记） | 现行 |
| [`design/command-execution.md`](design/command-execution.md) | 命令执行的同步 / 异步双模式与输出归属 | 现行 |
| [`design/credential-storage.md`](design/credential-storage.md) | 凭据存储与字段级加密 | 现行 |
| [`design/sudo.md`](design/sudo.md) | sudo 三模式（禁止注入 / 每次询问 / 自动注入） | 现行 |
| [`design/history-retention.md`](design/history-retention.md) | 命令历史保留策略（按时间、归档永久保留） | 现行 |
| [`design/update-check.md`](design/update-check.md) | 版本检查与提示更新流程 | 现行 |
| [`design/frontend-stack.md`](design/frontend-stack.md) | 前端技术栈选型 | 现行 |
| [`design/theme-spec.md`](design/theme-spec.md) | 视觉与主题规范（暗色 Deep Space / 亮色 Minimal Light） | 现行 |
| [`design/test-environment.md`](design/test-environment.md) | 联调与集成测试环境（WSL Ubuntu）搭建说明 | 现行 |

### C. 规范与约定 —— 必须遵守

| 文档 | 内容 | 时效性 |
| ---- | ---- | ---- |
| [`design/principles.md`](design/principles.md) | 工程原则（来自真实教训，指导后续所有设计） | 现行 |
| [`update-manifest.md`](update-manifest.md) | 更新清单（Gist JSON）的格式规范 | 现行 |
| [`update-manifest.example.json`](update-manifest.example.json) | 上述格式的示例文件 | 现行 |

> Git 分支与提交规范、安全红线在 [`AGENTS.md`](../AGENTS.md)，不在此重复。

### D. 上下文 —— 项目是什么

| 文档 | 内容 | 时效性 |
| ---- | ---- | ---- |
| [`glossary.md`](glossary.md) | 项目术语表（主机 / 凭据 / 终端 / 命令 等概念定义） | 现行 |

### E. 过程记录 —— 发生过什么

| 文档 | 内容 | 时效性 |
| ---- | ---- | ---- |
| [`development-troubleshooting.md`](development-troubleshooting.md) | 开发排错：真实遇到的环境问题与解决办法（持续追加） | 现行 |
| [`open-questions.md`](open-questions.md) | 需求澄清清单（Q1–Q35）：问题、状态、客户答复 | 现行（**逐步转为历史**，见下） |
| [`security-audit.md`](security-audit.md) | 全量安全审计报告（2026-09-10）：发现的漏洞、修复与验证 | **快照** |
| [`design/mockups/`](design/mockups/) | 主题选型阶段的设计稿（HTML + PNG）。其中 `theme.css` 是**现行**的主题令牌定义，`style-*.png` 为未采用的历史方案稿 | 混合（见文档内说明） |

---

## 三、维护规则

1. **入口唯一**：新增文档必须登记到本文件，否则等于不存在。
2. **单一事实来源**：
   - 技术决策 → `decisions.md`
   - 需求澄清的**结论** → 以 `decisions.md` 为准；`open-questions.md` 保留问题与状态，
     **结论只写摘要并指向对应决策**，避免同一事实两处维护、改一处漏一处。
   - 主题令牌 → `design/mockups/theme.css`
3. **代码与文档同步**：改动若影响已记录的结论，**同一次提交内**更新对应文档；
   若推翻了旧决策，**不要直接改旧条目**，而是新增一条决策并说明取代关系
   （与 `AGENTS.md` §2.3 的 `BREAKING CHANGE` 精神一致）。
4. **状态要真实**：文档开头的"状态"必须反映当前事实。
   **已确认的不要写成"待确认"**——那会误导接手的 Agent 重新提问甚至推翻结论。
5. **新增文档时的开头格式**：

   ```
   # 标题

   > 状态：已确认（Qxx）  或  > 状态：快照（YYYY-MM-DD）
   > 一句话说明本文解决什么问题
   ```

---

## 四、已知缺口（重要）

> 这些是**已承诺但尚未实现**或**已知有残余风险**的事项。对外说明前务必先看这里，
> 不要声称支持了实际没有的能力。

| 项 | 状态 | 说明 |
| ---- | ---- | ---- |
| **ProxyJump** | ❌ **未实现** | D11 / Q9 承诺"MVP 含 ProxyJump"，但只在数据库 / 领域模型 / IPC 层留了字段，**前端无控件、连接层完全未使用**。目前没有任何路径能让跳板机生效。详见 [`open-questions.md`](open-questions.md) 的 Q7 备注 |
| **sudo 凭据边界（V2）** | ⚠️ 已缓解，**未根治** | `ask` / `auto` 模式下，提权密码必须经 Agent 所在的用户域投递，Agent 可读到会话 nonce 并伪造协议标记。**"认证信息对 AI Agent 完全不可见"在提权链路下不成立**，对外表述需准确。详见 [`security-audit.md`](security-audit.md) §4 |
| **远程明文传输** | ⚠️ 已决策接受（D30） | 开启"允许远程连接"后，Token 与命令内容在网络中明文传输。局域网场景客户已接受，保留为未来工作 |
| 未完成任务 | — | Q29（提交身份邮箱）、Q32（备份与同步需求，二期） |

---

## 五、当前遗留的文档债

诚实记录，避免后人也踩：

| 项 | 说明 |
| ---- | ---- |
| 结论重复 | `open-questions.md` 的"已确认结论"与 `decisions.md` 内容重叠，尚未完全合并（规则见 §三.2） |
| 设计文档措辞 | 部分设计文档正文仍以"本文评估 / 团队建议"的口吻写成，但方案客户**已确认**；状态行已修正，正文措辞可在后续顺手统一 |

> **历史沿革**：`security-audit.md` 记录的那轮审计发现了一个曾被测试绿灯掩盖的严重缺陷
> （sudo 密码明文落盘）。教训已沉淀为 [`design/principles.md`](design/principles.md) 的 **P3**，
> 并在 [`AGENTS.md`](../AGENTS.md) §2.6 安全红线中引用：
> **功能测试通过 ≠ 安全属性成立**，涉及安全属性的修复必须构造能区分对错实现的断言。
