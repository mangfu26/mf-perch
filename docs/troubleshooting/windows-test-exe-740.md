# Windows 按文件名拦测试 exe（os error 740）

> 状态：现行
> 某个测试 target 一条用例都没跑、还弹出"程序兼容性助手"时，先查文件名，不要重编或怀疑环境。

---

## Windows 上 `cargo test` 拉不起某个测试 exe：`请求的操作需要提升。(os error 740)`

### 现象

全量门禁跑到某个集成测试 target 时，cargo 报类似：

```
error: failed to run custom build command / could not execute process
  `target\debug\deps\update_e2e-<hash>.exe`, exit code: 请求的操作需要提升。 (os error 740)
```

或者直接**一条用例都没跑**（报告里显示 `never executed`），同时屏幕上弹出
**"程序兼容性助手"**：*"以下程序可能安装不正确……已停止运行此程序"*，列出的正是那个测试 exe。
其余 target 正常，看起来像"只有这一个测试文件坏了"。

### 原因

**Windows 的 Installer Detection（UAC 启发式）按文件名拦人**：

1. 文件名里含 `update` / `install` / `setup` / `patch` 这类关键字（`update_e2e` 命中 `update`）；
2. 该 exe **没有声明执行级别的 manifest**——Rust 编译出的二进制默认不带，
   所以整个 `target/debug/deps/` 里的 exe 都没有 `asInvoker`，**没 manifest 不是差异项**；
3. 两条同时成立 → 系统判定"这是一个需要管理员权限的安装程序"，普通令牌拉起即失败（740）。

因此**与被测代码、测试内容、产物损坏都无关**，纯粹是文件名撞上了启发式。

### 如何确认（差分，两步）

判据是"**同一份字节换个名字就好**"，不要靠猜：

```bash
# ① 复制成不含关键字的名字，直接跑它
cp src-tauri/target/debug/deps/update_e2e-<hash>.exe .tmp-test/ue_probe.exe
.tmp-test/ue_probe.exe --list        # 能列出用例 = 二进制本身是好的
.tmp-test/ue_probe.exe               # 全绿 = 拦人的确实是文件名

# ② 排除"PCA 记住了坏名声"：注册表两处应为空
reg query "HKCU\Software\Microsoft\Windows NT\CurrentVersion\AppCompatFlags\Layers"
reg query "HKCU\Software\Microsoft\Windows NT\CurrentVersion\Compatibility Assistant\Store"
```

若 ① 换了名字仍失败，那就不是本条，回去查 E0463 一类产物问题或杀毒软件拦截。
验证用的探针（`.tmp-test/ue_probe.exe` 之类）用后即删。

### 解决办法

**给测试 target 换个不含关键字的文件名**（不是"每台机器各自点一次弹窗"）。
现有的一例：`src-tauri/tests/update_e2e.rs` → **`version_check_e2e.rs`**，
被测模块仍是 `src/update.rs`，归属写在文件头注释里。

弹窗出现时点**"已正确安装此程序"**，**不要**点"使用兼容性设置重新安装"——
那会给这个 exe 叠一层兼容 shim，污染后续诊断。

### 预防

- **`tests/*.rs` 文件名不要含 `update` / `install` / `setup` / `patch`**，
  规范见 [`AGENTS.md`](../AGENTS.md) §5.5。新建端到端测试时先按这个约束挑名字。
- 同理，**不要**为此改注册表关掉 Installer Detection（`EnableLUA=0` 要重启、
  且削弱整机 UAC），也不要给测试二进制塞 manifest——为一条用例改构建链路不划算。
- 这条在**新 Windows 开发机上首次跑全量门禁时**必然复现（本仓库 2026-09-21 换机首跑遇到），
  所以看到 740 先查文件名，别去怀疑测试环境或重编。
