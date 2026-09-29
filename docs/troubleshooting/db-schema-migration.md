# 数据库迁移：运行期 no such column 与 user_version 台阶

> 状态：现行
> 门禁与迁移用例全绿、应用打开列表却报 `no such column` 时，怎么判断"标记与磁盘形状脱节"并修复。

---

## 运行期报 `no such column`，而迁移用例全绿：`user_version` 已到位、表里却少列

### 现象

应用启动后打开某个列表（本仓库实测在"认证信息"）时报：

```
数据库错误：no such column: is_privileged in SELECT id, name, username, kind,
is_privileged, secret_enc, ... FROM credentials ORDER BY created_at ASC at offset 33
```

同时**默认门禁与升级迁移用例都是绿的**——那些用例建的是内存库，
而门禁里没有任何一台机器拿着"眼前这个形状"的库。

### 原因

台阶式迁移（`store/db.rs` 的 `migrate()`）只看 `PRAGMA user_version`：标记一到
`SCHEMA_VERSION` 就直接返回，**把标记当成了事实**。于是"版本号被复用"会留下一个
台阶永远修不回来的库：

1. 开发分支上先实现了一版 V2（动的是 A 表），本地 `pnpm dev` 跑过一次 → 本机库被盖章为 2；
2. 该方案被推翻，**同一个编号 2** 换成别的形状（动 B 表）——编号未发布，复用是允许的
   （不可复用的是发布过的编号，见 **D60** 的登记册）；
3. 新代码一看标记已是 2，认为无事可做，直到某条 SQL 撞上不存在的列。

`user_version` 是"某个构建写上去的断言"，不是磁盘形状的证明。已发布构建不会给出假断言
（登记册钉着原文），但**开发分支上的中间构建完全可能**——它只污染跑过它的机器，
所以进程内用例测不到。

### 如何确认（只读两步，先分类再动手）

```bash
python - <<'PY'
import sqlite3, os
db = os.path.expandvars(r"%APPDATA%/mf-perch/mf-perch.db")
c = sqlite3.connect("file:" + db + "?mode=ro", uri=True)
print(c.execute("PRAGMA user_version").fetchone()[0])               # 标记
print([r[1] for r in c.execute("PRAGMA table_info(credentials)")])  # 形状
print(c.execute("SELECT sql FROM sqlite_master WHERE name='hosts'").fetchone()[0])
PY
```

判据：**标记 ≥ `SCHEMA_VERSION` 且目标列/约束缺失** ⇒ 就是本条；
标记低于 `SCHEMA_VERSION` 才是"迁移步骤没写"，另案处理。
实测本机三项一起指向同一个旧构建：标记 2、`credentials` 无 `is_privileged`、
`hosts` 的 CHECK 里还留着已撤销的 `not_needed`。

### 解决办法

`migrate()` 里**增列一类步骤只按形状判定**（`if !has_column(…)`，不写版本号），
盖章改成"只往上写"。这样同时覆盖两种"标记与形状脱节"：

| 脱节方向 | 按版本号的后果 |
| ---- | ---- |
| 标记已到 N、列却不在（本条现象） | 台阶不回头，运行期 `no such column` |
| 列已加上、标记没写上（`ALTER` 与盖章之间被杀） | 重跑 `ADD COLUMN`，第二次启动 `duplicate column name`，**应用永久起不来** |

第二行是**存量用户升级**真实可能撞上的窗口，比第一行更值得防。
`ADD COLUMN` 天然幂等，问一句"列在不在"就都免疫，代价只是每次启动多一条 `PRAGMA` 查询。
此路径在同一天于真实数据目录上生效过：dev 监视器重建并重启 app，`open()` 当场补齐该列，
凭据行与密文原样保留。

**不要**为此手写一次性修库 SQL：重建表那条路（`CREATE …_new` + `INSERT SELECT` + `RENAME`）
① 会让 `sqlite_master` 存成 `CREATE TABLE "hosts" (…)`（表名带上引号），
**永远做不到与新建库逐字节一致**；② 随 `DROP TABLE` 一起消失的索引要手工复原，
实测就会漏掉 `idx_hosts_credential`。

### 预防

- **改变一个已被本机构用过的 `SCHEMA_VERSION` 编号的含义**之前，先假设已有库被旧含义盖了章；
  改完除了内存库用例，再跑一次"拿真实数据目录启动"。
- 追加迁移步骤按 `db.rs` 里 `RELEASED_SCHEMA_STEPS` 注释的四步走；
  **增列一步只写 `if !has_column(…)`，不要写成 `current < N || …`**，这条已写进那个流程。
