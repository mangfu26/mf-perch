# 视觉与主题规范

> 状态：已确认（Q22 / Q35，2026-09-10）
> 客户决定：支持**暗色 / 亮色**双主题切换。暗色 = **Deep Space（深空紫）**，亮色 = **Minimal Light（极简亮）**。

## 1. 视觉定位

客户对风格的要求：**简约、科技风、现代化**。不要求模仿 Termius 的终端交互，只借鉴"让 AI Agent 像人类一样访问主机"的产品隐喻。

- 暗色：深黑底 + 紫/青微光晕，毛玻璃层次，强调科技感与专业感。
- 亮色：近白底 + 黑白灰 + 靛蓝点缀，强调简约与克制。
- **同一套组件结构**，仅切换设计令牌（Design Tokens），不做两套独立设计。

## 2. 主题实现方式

- 通过 `<html data-theme="dark|light">` 切换，CSS 变量（Custom Properties）驱动全部颜色。
- 令牌定义见 [`docs/design/mockups/theme.css`](mockups/theme.css)。
- 前端实现：Pinia 管理主题状态 → 写入 `document.documentElement.dataset.theme` → 持久化到本地配置。
- 主题选择支持三态：**跟随系统 / 强制暗色 / 强制亮色**，默认"跟随系统"。

## 3. 令牌清单

| 类别 | 令牌 | 暗色（Deep Space） | 亮色（Minimal Light） |
| ---- | ---- | ---- | ---- |
| 背景 | `--bg` | `#08080c` | `#fafaf9` |
| 背景（抬升） | `--bg-elevated` | `rgba(0,0,0,.35)` | `#ffffff` |
| 表面 | `--surface` | `rgba(255,255,255,.035)` | `#ffffff` |
| 表面（悬停） | `--surface-hover` | `rgba(255,255,255,.06)` | `#f5f5f4` |
| 代码底 | `--surface-code` | `rgba(0,0,0,.30)` | `#fafaf9` |
| 描边 | `--border` | `rgba(255,255,255,.08)` | `#e7e5e4` |
| 正文 | `--text` | `#e8e8f0` | `#1c1917` |
| 次要文字 | `--text-muted` | `#8a8a9e` | `#78716c` |
| 品牌色 | `--accent` | `#7c5cff` | `#4f46e5` |
| 成功 | `--success` | `#34d399` | `#16a34a` |
| 警告 | `--warning` | `#fbbf24` | `#d97706` |
| 危险 | `--danger` | `#f87171` | `#dc2626` |
| 信息/执行中 | `--info` | `#22d3ee` | `#0891b2` |

每种语义色配套 `*-soft` 令牌（低透明度背景），用于状态标签。

## 4. 组件与状态约定

| 组件 | 约定 |
| ---- | ---- |
| 侧边导航 | 选中项用 `--accent-soft` 背景 + 高亮文字 |
| 主机卡片 | 选中项描边 `--accent`；运行中显示绿点（暗色带辉光） |
| 状态标签 | `exit 0` 绿 / 执行中 青 / `sudo 注入` 紫 / 异步 灰 |
| 命令时间线 | 每条命令一张卡片，命令行为等宽字体，输出区用 `--surface-code` |
| 执行中指示 | 卡片顶部 2px 渐变进度条（暗色下循环扫动） |
| 主按钮 | 暗色为紫渐变 + 辉光；亮色为纯靛蓝 + 浅阴影 |
| MCP 状态卡 | 左下角常驻，显示监听地址与运行指示点 |

## 5. 设计原则

1. **信息优先**：这是审计工具，命令与输出是主角，装饰元素克制。
2. **状态可辨**：运行中 / 成功 / 失败 / 归档 必须在视觉上立刻区分（颜色 + 文字双通道，不只靠颜色）。
3. **等宽字体承载数据**：命令、路径、输出、IP 一律等宽字体；界面文字用无衬线。
4. **深浅一致**：两套主题共用同一间距、圆角、层级规范，切换时布局不跳动。
5. **无障碍**：正文与背景对比度不低于 WCAG AA（4.5:1）；状态不只靠颜色区分。

## 6. 预览稿

| 文件 | 说明 |
| ---- | ---- |
| [`theme-preview.html`](mockups/theme-preview.html) | 可交互预览，右上角切换暗/亮 |
| [`theme-dark.png`](mockups/theme-dark.png) | 暗色渲染图 |
| [`theme-light.png`](mockups/theme-light.png) | 亮色渲染图 |
| [`style-a-deep-space.png`](mockups/style-a-deep-space.png) | 暗色原始方案稿 |
| [`style-c-minimal-light.png`](mockups/style-c-minimal-light.png) | 亮色原始方案稿 |

> 备选方案 B（Nord）与 D（Terminal Native）的稿件保留在 `mockups/` 目录，未采用，可作后续参考。
