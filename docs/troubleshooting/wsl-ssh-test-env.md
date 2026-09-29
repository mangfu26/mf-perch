# WSL SSH 联调环境：私钥权限、端口被占、Connection refused

> 状态：现行
> 真实环境用例在 Windows 侧连不上 WSL 里的 sshd 时，按"端口上有没有人 / 认证过没过"分类处置。

---

## Windows 侧手工验证 SSH 时：`Load key "...": bad permissions`，或 `ssh` 挂住不返回

### 现象

在 Windows 侧用手头的 `ssh.exe` 连联调环境（`docs/design/test-environment.md`）时：

```
Load key ".tmp-test/id_test": bad permissions
mfperch@127.0.0.1: Permission denied (publickey,password).
```

或者更坑的一种：**命令什么都不打印，一直挂着**，看起来像网络或 sshd 出了问题。

### 原因

1. **Windows 版 OpenSSH 客户端会校验私钥文件的 ACL**：`ssh-keygen`（Git for Windows）
   新建的私钥在 Windows 眼里权限过宽（继承了这个目录上原有的宽 ACL），客户端直接拒读。
   Git 自带的那版 `ssh.exe` 看的是 POSIX 位（`0644` 也算"过宽"），System32 那版看的是 ACL——
   **两个都得修**。
2. 公钥没被接受时客户端**回落到口令提示**，而提示写在 **`/dev/tty`**（不是 stdout/stderr）。
   把输出重定向或接进管道时看不见它 → 表现为"无声挂住"，直到服务端
   `LoginGraceTime` 超时。服务端日志里对应的是一句
   `Timeout before authentication for connection from 127.0.0.1`——**没有** `Accepted publickey`。

### 解决办法

```bash
chmod 600 .tmp-test/id_test                                          # Git 版客户端
icacls .tmp-test/id_test //inheritance:r //grant:r "$USERNAME:(R,W)" # System32 版客户端
```

排查时一律加 `-o BatchMode=yes`：禁止回落口令提示，认证失败会**立刻**返回并打印真实原因。

### 预防

- **这条只影响手工验证，与测试无关**：`src-tauri/tests/*.rs` 用 russh 自己读 PEM，
  **不校验文件权限**。不要因为 `ssh` 连不上就去怀疑测试环境或 `authorized_keys`。
- 判据顺序：先 `BatchMode=yes -vv` 看客户端有没有"Load key"报错，再看 WSL 侧
  `/var/log/auth.log` 有没有 `Accepted publickey`。两边都正常才是网络/端口问题。
- 换开发机时，`AGENTS.local.md`（gitignored）里的重建步骤应包含这一步——
  私钥是在 Windows 侧生成的，权限校验也就只在 Windows 侧触发。

---

## 测试端口被别的进程占了：一切换机/装软件就出现"公钥被拒"，但 `authorized_keys` 没问题

### 现象

真实环境用例（`src-tauri/tests/ssh_integration.rs` / `sudo_e2e.rs` / `mcp_e2e.rs`）成批失败，
报的都是认证层的错：

```
Permission denied (publickey,...)
```

而 WSL 侧什么都没改：`authorized_keys` 里公钥在、`sshd_config.d` 的 drop-in 在、
`/etc/passwd` 里用户也在。上一轮还是全绿的三组用例，这一轮**一条都连不上**。

### 原因

**端口上没有那个 sshd**。`127.0.0.1` 是回环地址，任何本机进程都能绑上去；一旦别的进程
先占了测试端口，客户端就连到了"另一个服务"——它同样会说 SSH 版本横幅甚至接受
`publickey` 协商，但对仓库里这把测试私钥必然返回拒绝。于是症状长得极像"公钥配错了"，
把人往 `authorized_keys`、文件权限、WSL 重置这些方向上带。

本机实例（2026-09-23）：客户机器上的**远程协助工具**监听了 `127.0.0.1:2222`，
测试 sshd 因此没能在该端口上服务。判据是这两条：

```bash
# ① 客户端确实提供了正确的公钥（指纹与 .tmp-test/id_test.pub 一致）
ssh -i .tmp-test/id_test -p 2222 -o BatchMode=yes -vv mfperch@127.0.0.1 'true' 2>&1 | grep -i 'Offering\|Server accepts'
# ② 该端口的 LISTEN 归属不是 sshd
ss -ltnp | grep 2222
```

①里能看到 `Offering publickey ... SHA256:<与 id_test.pub 相同的指纹>` 而后被拒——
**密钥没问题，是接电话的人不对**。

### 解决办法

换端口，不要去跟别人的进程抢 `2222`：

1. 改 `.tmp-test/setup-wsl-test-env.sh` 里的 `Port`，重跑该脚本（幂等，会覆写 drop-in 并重启 sshd）；
2. 改 `.tmp-test/mfperch_test_env.sh` 里的 `MFPERCH_TEST_PORT`；
3. 同步 `docs/design/test-environment.md`（选型与端口事实在那里，本文不复述）。

### 预防

- **测试端口按 [`design/test-environment.md`](design/test-environment.md) §3 的护栏选**：
  避开天然会被别的软件占用的常见替代端口。
- 三组真实环境用例**同时**变红、且报的都是 `Permission denied`，第一反应应该是
  "**还有没有人连着这台机器**"（远程协助、端口转发、IDE 的端口占用），而不是"公钥丢了"——
  后者很少一次性影响所有用户与所有用例。
- `ss -ltnp` 一条命令就能定性，跑在改配置之前。

---

## Windows 侧连不上 WSL 里的 sshd：`Connection refused`，而 WSL 内一切正常

### 现象

上一节换完端口之后，症状**换了**一种：

- WSL 内：`systemctl is-active ssh` = `active`，`ss -ltn` 有 `127.0.0.1:2223`；
- Windows 侧：`ssh -p 2223` 报 `connect to host 127.0.0.1 port 2223: Connection refused`，
  且 `netstat -ano | findstr 2223` **一行都没有**，`netsh interface portproxy show all` 为空。

也就是说：**WSL 里有人在监听，Windows 的回环上却根本没有这个端口**。

### 原因（观测结论，不是 WSL 内部实现结论）

NAT 模式下 Windows 侧那个监听**不是 sshd 自己的**，而是 WSL 的 localhost 自动转发代持的，
它**跟着 WSL 实例的生命周期走**：实例被回收（空闲停止）后 Windows 上就不再有这个端口，
表现为 `Connection refused` 而不是超时或拒绝认证。

本机实测：`uptime -s` 显示 VM 在每次探测前刚重启过；让开发者在 Windows 上开一个
`wsl -d Ubuntu` 终端**保持不退出**之后，`2223` 立刻出现在 `netstat` 里，随后三组
`--ignored` 用例全绿。

### 判读三分法（先分类，再去改配置）

| Windows `netstat` 有该端口？ | WSL `ss -ltn` 有？ | 结论 |
| ---- | ---- | ---- |
| 无 | 无 | WSL 实例没在跑 |
| 无 | 有 | 本节：转发随实例消失 |
| 有，但归属不是 sshd | — | 撞端口，见上一节 |

### 解决办法与预防

- **跑真实环境用例前，先确认 Windows 侧有该端口的监听**（`netstat -ano | findstr <端口>`），
  一条命令就能定性；隔夜没碰 WSL 之后尤其要确认。
- WSL 停止时**请开发者挂一个交互终端**（`wsl -d Ubuntu` 不退出）再跑。
  **不要**为此改成"从 Windows 连 WSL 的 NAT 内网 IP"：NAT 模式下那个地址每次开机会变，
  `MFPERCH_TEST_HOST` 就得跟着现取，等于把一条稳定判据换成一条易变项。
- 报的错是 `Connection refused` 还是 `Permission denied`，**分别指向"端口上没人"与
  "认证没通过"**，处置路径完全不同——前者不要去查 `authorized_keys`。
