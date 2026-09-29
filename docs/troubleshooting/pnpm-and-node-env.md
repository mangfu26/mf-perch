# pnpm 与 Node 环境：store 位置、spawn EPERM、check:secrets 落到 WSL bash

> 状态：现行
> 装依赖、跑前端门禁与安全门禁时被"不是代码问题"的环境差异挡住，怎么定位与处置。

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

pnpm 的默认 store 在**项目所在盘**（`<项目>/.pnpm-store`），而本仓库现有的 `node_modules`
当初是从**盘根的另一个 store**（`D:\.pnpm-store\v11`）链出来的。两者不一致时 pnpm 会
拒绝继续——它不能把新包装进一个与现有链接来源不同的 store。

`ERR_SQLITE_ERROR` 则是**受限执行环境**的次生现象：盘根 store 在项目目录之外，
若执行环境只允许写项目目录，pnpm 打不开 store 的索引库。

### 解决办法

**保留原 store，不要重新 install**：

```bash
# <原 store> 取自报错信息里的 "linked from the store at ..."，或 `pnpm config get store-dir`
pnpm add -D <包名> --store-dir "<原 store>"       # 本项目当前值：D:\.pnpm-store\v11
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
  门禁必须在**任何一台开发机上默认都能通过**（`pnpm check:secrets` → 退出码 0），
  红灯只应指向**真实问题**。
- 改动这类门禁后要做**差分验证**（P3）：故意放入假私钥 / 假令牌 / 凭据类文件，
  确认它**确实会红灯**；只验证"能通过"说明不了任何事。
