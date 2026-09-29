# 测试结果判读的两个假象：退出码 1、还原文件后仍跑旧二进制

> 状态：现行
> 测试报告"看着红（其实全绿）"或"看着仍红（其实跑的是旧二进制）"时，先查证据再改代码。

---

## `cargo test` 明明全绿，命令却以退出码 1 结束（警告走 stderr 被 PowerShell 当成错误）

### 现象

```powershell
cd src-tauri; cargo test -j 2 2>&1 | Select-Object -Last 60
```

输出里**每个 target 都是 `test result: ok`**，也没有任何 `error:` 行，但整条命令以**退出码 1** 结束。
配上 `Select-Object -Last 60` 只保留尾部，看上去就像"有一处失败，但被截断看不见了"——
实测在 2026-09-16 的一次全量提交前检查中出现过，白排查了一轮。

### 原因（两步，均已实测）

1. 链接 cdylib 时 MSVC 的 `link.exe` 会往 **stdout** 打一行进度
   （`正在创建库 ...\mf_perch_lib.dll.lib 和对象 ...\mf_perch_lib.dll.exp`），
   cargo 把它作为 `warning:` **转发到 stderr**；
2. PowerShell 会把原生命令写到 **stderr** 的内容包成 `NativeCommandError` 错误记录。
   当这种合并管道（`2>&1 | ...`）是脚本的**最后一条语句**时，pwsh 进程就以 **1** 退出
   ——**尽管 `$LASTEXITCODE` 是 0**。

最小复现（与 cargo 无关，一键可验）：

```powershell
cmd /c "echo boom 1>&2" 2>&1 | Select-Object -Last 2
# → NativeCommandError: cmd : boom ...
# → 整个命令以 [exit code: 1] 结束，而 cmd 自己的退出码是 0
```

同一类现象：`Get-Process 不存在的名字 -ErrorAction SilentlyContinue | Select-Object ...`
也会以 1 结束（不是"没找到进程"这种业务失败，而是 PowerShell 的退出码语义）。

### 解决办法（怎么判读才可信）

**判绿看 cargo 自己的输出，不看 shell 退出码**：

- 红：出现 `test result: FAILED` 或结尾的 `error: test failed, to rerun pass ...`；
- 绿：所有 target 的 `test result: ok`，且没有 `error:` 行；
- 需要机器判读时**显式取退出码**，并**把全量输出写进文件**：

  ```powershell
  cd src-tauri; cargo test -j 2 *> ..\.tmp-test\full-test.log; "exit=$LASTEXITCODE"
  # 之后用 Select-String -Pattern 'test result:|FAILED|error' 精确取结论
  ```

  `*>` 把所有流写进文件，既避免 stderr 被包成错误记录，也避免失败分节被缓冲区丢掉。

### 预防

- **全量测试一律写文件日志**，不要用 `Select-Object -Last N` 截断——一次失败的分节
  会被缓冲区丢掉，只剩一个"退出码 1"和看不到的红灯；
- 看到"**退出码 1，但输出全是 ok**"时，先怀疑这条，**不要去改代码**；
- 这条与"产物陈旧"（AGENTS.md §5.11 纪律 2）是两类不同的假象：
  一个是**绿灯看红**，一个是**红灯看绿**，都要先怀疑工具链/环境，再怀疑代码。

---

## `Copy-Item` 还原文件后，测试仍在跑**旧实现**（mtime 被一起复制）

### 现象

做安全属性的差分验证时：先备份文件 → 临时改成错误实现 → 跑测试（红灯，符合预期）→
`Copy-Item` 还原正确实现 → 再跑，**仍然是红灯**，而且失败信息与错误实现时**一字不差**。
看起来像"修复没生效"，很容易误判成"我的实现不对"。

### 原因

`Copy-Item` 默认**连同原始时间戳一起复制**，于是还原后的文件 mtime 等于备份时的时间
（早于构建产物）→ cargo 判定"源文件没比产物新"→ **根本不重新编译**，
跑的还是那份带有错误实现的旧二进制。

判据很硬：出问题的那次 `cargo test` 输出里**没有 `Compiling mf-perch`**。
实测（2026-09-16，主机密钥错误码的差分验证）：

```
# 还原后直接跑：没有 Compiling，测试仍按错误实现失败
test result: FAILED. 57 passed; 1 failed ... 实际错误：SshConnect(...)
# 刷新 mtime 后再跑：出现 Compiling，随即全绿
cargo :    Compiling mf-perch v0.1.0 (D:\projects\mf-perch\src-tauri)
test result: ok. 58 passed; 0 failed
```

### 解决办法

还原文件后**强制让 cargo 重新编译**，二选一：

```powershell
# ① 文档推荐的做法：作废候选产物
cargo clean -p mf-perch

# ② 或只刷新 mtime（省一次全量重编）
(Get-Item src-tauri\src\ssh\auth.rs).LastWriteTime = Get-Date
```

然后**确认这次运行打印了 `Compiling mf-perch`** 再下结论。

### 预防

- **备份 / 还原源码一律记着这条**：`git stash`、`git checkout -- <file>` 会正确更新 mtime，
  但 `Copy-Item` / 资源管理器复制**不会**；
- 差分验证的结论必须建立在"这次确实重编了"之上——否则"红灯"证明不了任何事；
- 这与"退出码 1"那条相反：那条是**看错信号**，这条是**跑错二进制**，都先查证据再改代码。
