use rusqlite::{Connection, OptionalExtension};
use std::path::{Path, PathBuf};

use crate::error::{AppError, Result};

/// 数据库 schema 版本。每次结构变更递增，并在 `migrate` 中补迁移步骤。
pub const SCHEMA_VERSION: i64 = 2;

/// 打开（或创建）数据库并完成迁移。
///
/// 采用单文件 SQLite（D6）：主机、认证信息、终端、命令历史同库，
/// 便于整库备份与迁移；敏感字段由 `crate::store::crypto` 加密后写入。
pub fn open(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let conn = Connection::open(path)?;

    // WAL 提升并发读写表现；foreign_keys 保证引用完整性；busy_timeout 避免瞬时锁冲突。
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "busy_timeout", 5_000)?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;

    migrate(&conn)?;
    Ok(conn)
}

/// 打开内存数据库。
///
/// 供单元测试与集成测试使用：完全隔离，不触碰用户真实数据目录。
pub fn open_in_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    migrate(&conn)?;
    Ok(conn)
}

/// 依据 `user_version` 执行增量迁移。
///
/// 建表按版本号走，但**增列一类只按形状走**：`user_version` 只是某个构建写上去的断言，
/// 标了不等于做到了，反之标小了也不必重跑（判据见 `has_column`）。
pub fn migrate(conn: &Connection) -> Result<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;

    if current < 1 {
        conn.execute_batch(V1_SCHEMA)
            .map_err(|e| AppError::Database(e))?;
    }

    // 增列一步**只按形状判定**，不看版本号，两个方向都覆盖：
    // ①标记已到 2 却缺列 = 被"声称是 2、其实不是 2"的构建盖过章（本项目唯一一次：
    //   开发分支上被撤销的第四档实现），只认版本号就永远修不回来，症状是运行期
    //   `no such column`；②列已加但标记没写上（`ALTER` 与盖章之间被杀）——按版本号
    //   会重跑 `ADD COLUMN` 并报 duplicate column。`ADD COLUMN` 幂等，问一句"列在不在"
    //   就同时免疫这两种脱节。
    if !has_column(conn, "credentials", "is_privileged")? {
        conn.execute_batch(V2_CREDENTIAL_IS_PRIVILEGED)
            .map_err(|e| AppError::Database(e))?;
    }

    // 版本号只往上写：退回旧构建时（current 比本构建大）把标记改小，
    // 会让下一次升级跳过中间那一级台阶。
    if current < SCHEMA_VERSION {
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    Ok(())
}

/// `table` 里是否已存在 `column`。
fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let found: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM pragma_table_info(?) WHERE name = ? LIMIT 1",
            [table, column],
            |r| r.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

/// 应用数据目录（D6 / Q12）：`%APPDATA%/mf-perch`（Windows）、
/// macOS 为 `~/Library/Application Support/mf-perch`，Linux 为 `~/.local/share/mf-perch`。
pub fn data_dir() -> Result<PathBuf> {
    let base = dirs::data_dir()
        .ok_or_else(|| AppError::Config("无法确定系统应用数据目录".into()))?;
    Ok(base.join("mf-perch"))
}

/// 默认数据库文件路径。
pub fn default_db_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("mf-perch.db"))
}

/// 读取 `settings` 表中的字符串配置。
pub fn get_setting(conn: &Connection, key: &str) -> Result<Option<String>> {
    let v = conn
        .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
            r.get::<_, String>(0)
        })
        .optional()?;
    Ok(v)
}

/// 写入或覆盖 `settings` 表中的字符串配置。
pub fn set_setting(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![key, value],
    )?;
    Ok(())
}

/// 删除配置项。
pub fn delete_setting(conn: &Connection, key: &str) -> Result<()> {
    conn.execute("DELETE FROM settings WHERE key = ?1", [key])?;
    Ok(())
}

/// v1 初始 schema。
///
/// **已发布过的表形状不要就地改**：改这里只覆盖新建库，存量装机拿不到同一列，
/// 与后续 `ALTER` 并存时还会撞 `duplicate column name`。结构变更一律作为
/// `migrate()` 的增量步骤追加（已发布装机的必要性见 **D50** 的条件句与下面的 V2）。
///
/// 设计取舍：
/// - 主键使用带前缀的文本 ID，便于日志与界面辨识。
/// - 敏感字段（`*_enc`）为加密后的字节，绝不存明文（D6）。
/// - 命令输出单独存放在 `command_outputs`，避免列表查询时把大字段读进内存。
/// - 命令与输出各建 FTS5 索引，支撑全文搜索（Q17）。
const V1_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- 认证信息：secret_enc / passphrase_enc 为 AES-256-GCM 密文（D6）
-- 密文格式为 `v1:<nonce_b64>:<ciphertext_b64>` 字符串，故用 TEXT 存储
CREATE TABLE IF NOT EXISTS credentials (
    id             TEXT PRIMARY KEY,
    name           TEXT,
    username       TEXT NOT NULL,
    kind           TEXT NOT NULL CHECK (kind IN ('password', 'key')),
    secret_enc     TEXT NOT NULL,
    passphrase_enc TEXT,
    fingerprint    TEXT,
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL
);

-- SSH 主机：sudo_password_enc 同样为密文；host_key 为 TOFU 记录的信任密钥（D10）
CREATE TABLE IF NOT EXISTS hosts (
    id                   TEXT PRIMARY KEY,
    name                 TEXT,
    address              TEXT NOT NULL,
    port                 INTEGER NOT NULL DEFAULT 22,
    credential_id        TEXT REFERENCES credentials(id) ON DELETE SET NULL,
    proxy_jump_host_id   TEXT REFERENCES hosts(id) ON DELETE SET NULL,
    sudo_policy          TEXT NOT NULL DEFAULT 'deny'
                         CHECK (sudo_policy IN ('deny', 'ask', 'auto')),
    sudo_password_source TEXT NOT NULL DEFAULT 'reuse_login'
                         CHECK (sudo_password_source IN ('own', 'reuse_login')),
    sudo_password_enc    TEXT,
    shell_env_mode       TEXT NOT NULL DEFAULT 'login'
                         CHECK (shell_env_mode IN ('login', 'clean')),
    init_script          TEXT,
    host_key             TEXT,
    host_key_fingerprint TEXT,
    created_at           TEXT NOT NULL,
    updated_at           TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_hosts_credential ON hosts(credential_id);

-- SSH 终端：D20 中归档可恢复（archived），D21 中人类删除为终态（物理删除）
CREATE TABLE IF NOT EXISTS terminals (
    id              TEXT PRIMARY KEY,
    host_id         TEXT NOT NULL REFERENCES hosts(id) ON DELETE CASCADE,
    name            TEXT,
    status          TEXT NOT NULL DEFAULT 'active'
                    CHECK (status IN ('active', 'broken', 'archived')),
    env_snapshot    TEXT,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    archived_at     TEXT
);

CREATE INDEX IF NOT EXISTS idx_terminals_host   ON terminals(host_id);
CREATE INDEX IF NOT EXISTS idx_terminals_status ON terminals(status);

-- 命令记录：seq 为终端内递增序号，用于与远端结束标记配对（D3）
CREATE TABLE IF NOT EXISTS commands (
    id           TEXT PRIMARY KEY,
    terminal_id  TEXT NOT NULL REFERENCES terminals(id) ON DELETE CASCADE,
    seq          INTEGER NOT NULL,
    command      TEXT NOT NULL,
    status       TEXT NOT NULL DEFAULT 'queued'
                 CHECK (status IN ('queued', 'running', 'completed', 'failed', 'connection_lost')),
    exit_code    INTEGER,
    duration_ms  INTEGER,
    truncated    INTEGER NOT NULL DEFAULT 0,
    output_bytes INTEGER,
    created_at   TEXT NOT NULL,
    started_at   TEXT,
    finished_at  TEXT
);

CREATE INDEX IF NOT EXISTS idx_commands_terminal ON commands(terminal_id, seq);
CREATE INDEX IF NOT EXISTS idx_commands_created  ON commands(created_at);
CREATE INDEX IF NOT EXISTS idx_commands_status   ON commands(status);

-- 命令输出单独存放：体积可能很大，按需加载避免列表查询拖垮内存
CREATE TABLE IF NOT EXISTS command_outputs (
    command_id TEXT PRIMARY KEY REFERENCES commands(id) ON DELETE CASCADE,
    output     TEXT NOT NULL
);

-- 全文搜索（Q17）：命令与输出分别建索引，均随主表删除而清理
CREATE VIRTUAL TABLE IF NOT EXISTS commands_fts USING fts5(
    command,
    content='commands',
    content_rowid='rowid',
    tokenize='unicode61'
);

CREATE VIRTUAL TABLE IF NOT EXISTS command_outputs_fts USING fts5(
    output,
    content='command_outputs',
    content_rowid='rowid',
    tokenize='unicode61'
);

-- 触发器：保持 FTS 索引与主表同步
CREATE TRIGGER IF NOT EXISTS commands_ai AFTER INSERT ON commands BEGIN
    INSERT INTO commands_fts(rowid, command) VALUES (new.rowid, new.command);
END;

CREATE TRIGGER IF NOT EXISTS commands_ad AFTER DELETE ON commands BEGIN
    INSERT INTO commands_fts(commands_fts, rowid, command) VALUES ('delete', old.rowid, old.command);
END;

CREATE TRIGGER IF NOT EXISTS commands_au AFTER UPDATE ON commands BEGIN
    INSERT INTO commands_fts(commands_fts, rowid, command) VALUES ('delete', old.rowid, old.command);
    INSERT INTO commands_fts(rowid, command) VALUES (new.rowid, new.command);
END;

CREATE TRIGGER IF NOT EXISTS command_outputs_ai AFTER INSERT ON command_outputs BEGIN
    INSERT INTO command_outputs_fts(rowid, output) VALUES (new.rowid, new.output);
END;

CREATE TRIGGER IF NOT EXISTS command_outputs_ad AFTER DELETE ON command_outputs BEGIN
    INSERT INTO command_outputs_fts(command_outputs_fts, rowid, output) VALUES ('delete', old.rowid, old.output);
END;

CREATE TRIGGER IF NOT EXISTS command_outputs_au AFTER UPDATE ON command_outputs BEGIN
    INSERT INTO command_outputs_fts(command_outputs_fts, rowid, output) VALUES ('delete', old.rowid, old.output);
    INSERT INTO command_outputs_fts(rowid, output) VALUES (new.rowid, new.output);
END;
"#;

/// V2：`credentials` 增列 `is_privileged`（D60）。
///
/// **为什么这次必须有迁移**，而不像 **D50** 那样只改建表语句：已发布版本（v0.1.1 起）
/// 装机上的 `credentials` 表早已建好，`CREATE TABLE IF NOT EXISTS` 不会重建它，
/// 读写这一列会以 `no such column` 失败——表现为"勾选特权身份后保存凭据就报错"。
/// D50 当时刻意不迁移的前提是"产品未发布、只影响开发机本地数据"，并在同一条里写明
/// "**首发前若已存在用户库，则必须改回迁移方案**"；该前提现已不成立，本步就是它预留的那条路。
///
/// 形态刻意选 `ADD COLUMN` 而**不重建任何表**：它只改 `credentials` 的元数据，
/// 不搬行、不重建索引，`hosts` / `terminals` / `commands` 一律不碰，因此不存在
/// "迁移把审计数据连带删掉"那一类风险，也就不需要 `PRAGMA foreign_keys = OFF`
/// （顺带避开"`foreign_keys` 在事务内是 no-op"这个坑）。
///
/// `DEFAULT 0` 是常量默认值，SQLite 因此不必回填每一行；已有凭据一律按**非特权**处理，
/// 方向是 fail-closed：宁可让用户去界面勾一次，也不凭空给提权短路。
const V2_CREDENTIAL_IS_PRIVILEGED: &str = r#"
ALTER TABLE credentials ADD COLUMN is_privileged INTEGER NOT NULL DEFAULT 0;
"#;

/// **已发布过的 schema 形状登记册**：`(该步骤执行后达到的版本号, SQL 原文, 原文的 SHA-256)`。
///
/// 库的完整形状是一条**重放链**：版本 N = 表中前 N 项原文依次执行的结果。
/// 所以钉住每一项的原文，就等于钉住历史上每一种"发出去的装机形状"。
///
/// **为什么这里允许用摘要锁文本**（平时这样写会被 §5.3 判成锁死实现细节的假测试）：
/// 这些 SQL 不是实现细节，而是**已发布的二进制能在用户机器上建出来的磁盘形状**，
/// 属 §5.1 明列的"数据兼容性"。改一个字符就可能让某个老库升不上来，
/// 而那种坏法在功能测试里看不出来——只有摘要会红。
///
/// **加新步骤时只追加，不改写既有项**（就地改 V1/V2 等于制造第二个 V1，见 D60）：
/// ①新写一个 `const V3_…: &str = r#"…"#;`；②`SCHEMA_VERSION` 递增；
/// ③在 `migrate()` 补一段 `if current < 3`（**增列一类只写 `if !has_column(…)`**，
/// 不要写成 `current < 3 || …`，理由同 V2 那段注释）；④在这里追加一行，第三项先随便填——
/// `released_schema_texts_are_frozen` 的失败信息会打印实际摘要。
/// 漏掉 ④ 会撞到 `released_schema_steps_cover_every_version`（版本号与条目必须一一对应）。
const RELEASED_SCHEMA_STEPS: &[(i64, &str, &str)] = &[
    (
        1,
        V1_SCHEMA,
        "0c79667c504eed9a2876bf94613f906838e44463abd19b83e14de501c92b547b",
    ),
    (
        2,
        V2_CREDENTIAL_IS_PRIVILEGED,
        "6428c201d50b4a97acdcb49be6d3e09e5e3c7728b5fedb587e884ac8433be114",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// `connection_lost` 必须能被真正写进库。
    ///
    /// 判别性：`commands.status` 上有 CHECK 约束，只改枚举而忘了改建表语句时，
    /// 这条写入会以 `CHECK constraint failed` 失败——正是本用例要抓的缺陷。
    /// （刻意**不做 schema 迁移**：理由与旧 dev 库的处置见 **D50**。）
    #[test]
    fn connection_lost_is_accepted_by_the_schema() {
        let conn = open_in_memory().expect("内存库");
        conn.execute(
            "INSERT INTO hosts (id, address, port, created_at, updated_at)
             VALUES ('host_cl', '127.0.0.1', 22, 'now', 'now')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO terminals (id, host_id, status, created_at, updated_at)
             VALUES ('term_cl', 'host_cl', 'active', 'now', 'now')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO commands (id, terminal_id, seq, command, status, created_at)
             VALUES ('cmd_cl', 'term_cl', 1, 'sleep 60', 'connection_lost', 'now')",
            [],
        )
        .expect("建表语句必须认这个新状态");
    }

    #[test]
    fn migrate_is_idempotent() {
        let conn = open_in_memory().expect("open in-memory db");
        // 再次迁移不应报错，也不应重复建表。
        migrate(&conn).expect("second migrate");
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }

    /// 守的是**版本号与形状脱节**这一类库：`user_version` 已被某个构建标成 2，
    /// 但 `credentials` 里并没有 `is_privileged`。本项目真实踩过——开发分支上
    /// 第四档实现（提交 `731abc9`，其 V2 只重建 `hosts`、不加列）给 dev 库盖了章，
    /// 该实现随后被撤销、版本号 2 换成了别的形状，这台机器上的库就成了孤儿。
    ///
    /// 判别性：把 `migrate()` 改回"只看版本号"（`if current >= SCHEMA_VERSION { return }`），
    /// 本用例立刻红在"列应存在"上；而 `credentials` 那行数据丢了也会红。
    #[test]
    fn a_stale_version_stamp_is_repaired_by_shape() {
        let conn = Connection::open_in_memory().expect("内存库");
        conn.execute_batch(V1_SCHEMA).expect("按 V1 建表");
        conn.pragma_update(None, "user_version", 2).expect("盖章 2");
        conn.execute(
            "INSERT INTO credentials (id, username, kind, secret_enc, created_at, updated_at)
             VALUES ('cred_stale', 'root', 'password', 'v1:nonce:cipher', 'now', 'now')",
            [],
        )
        .unwrap();
        assert!(
            !has_column(&conn, "credentials", "is_privileged").unwrap(),
            "前置条件：这个库的形状与它的版本标记不符"
        );

        migrate(&conn).expect("迁移应把它补成真正的 2");

        assert!(
            has_column(&conn, "credentials", "is_privileged").unwrap(),
            "缺列必须按形状补回来"
        );
        let flag: i64 = conn
            .query_row(
                "SELECT is_privileged FROM credentials WHERE id = 'cred_stale'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(flag, 0, "补列后原有凭据仍按非特权处理（fail-closed）");
        let enc: String = conn
            .query_row(
                "SELECT secret_enc FROM credentials WHERE id = 'cred_stale'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(enc, "v1:nonce:cipher", "补列不得动已有密文");
    }

    /// 守的是**升级做了一半就断掉**这条路径：`ALTER` 已经落盘、版本标记还没写上
    /// （两步之间进程被杀 / 断电）。此时库里已有该列，若迁移仍按版本号重跑
    /// `ADD COLUMN`，第二次启动会以 `duplicate column name: is_privileged` **永久起不来**。
    ///
    /// 判别性：判据写成 `current < 2 || !has_column(…)`（版本号优先）时，
    /// 本用例红在 `migrate(…)` 那一句 `expect` 上，报 duplicate column。
    #[test]
    fn an_upgrade_that_lost_its_stamp_restarts_cleanly() {
        let conn = Connection::open_in_memory().expect("内存库");
        conn.execute_batch(V1_SCHEMA).expect("按 V1 建表");
        conn.pragma_update(None, "user_version", 1)
            .expect("盖章 1（已发布构建的终点状态）");
        conn.execute(
            "INSERT INTO credentials (id, username, kind, secret_enc, created_at, updated_at)
             VALUES ('cred_half', 'root', 'password', 'v1:nonce:cipher', 'now', 'now')",
            [],
        )
        .unwrap();
        conn.execute_batch(V2_CREDENTIAL_IS_PRIVILEGED)
            .expect("模拟列已加上");
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 1, "前置条件：标记停在 1，形状却已是 2");

        migrate(&conn).expect("第二次启动不该因为重跑 ADD COLUMN 而失败");

        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION, "补上丢掉的版本标记");
        let flag: i64 = conn
            .query_row(
                "SELECT is_privileged FROM credentials WHERE id = 'cred_half'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(flag, 0, "列不该被重加，原值仍为默认 0");
    }

    /// 守的是**退回旧构建**这条路径：库的版本比本构建高时只跳过迁移，
    /// 不能顺手把标记改小——那会让下次升级跳掉中间那一级台阶。
    #[test]
    fn a_newer_database_keeps_its_version_stamp() {
        let conn = open_in_memory().expect("内存库");
        conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .expect("模拟更新的库");

        migrate(&conn).expect("更新的库不迁移");

        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION + 1, "版本标记不得被改小，实际 {v}");
    }

    /// 守的是**已发布装机升级**这条路径：V1 库的 `credentials` 没有 `is_privileged`，
    /// 里面有凭据、主机、终端与命令历史；迁移之后四样都得还在，新列读得到且写得进。
    ///
    /// 判别性（每一项都能抓一个真实缺陷，缺一即红）：
    /// - 升级前就查得到这一列 → V1 形状被改过，本用例测不到升级（前置断言抓这个）；
    /// - 升级后读不到 → 忘了追加 V2 步骤，装机上表现为"勾选特权身份后保存就报错"；
    /// - 已有凭据取到 1 → 默认值写反，等于凭空给存量凭据提权短路（**fail-open**）；
    /// - `secret_enc` 变了 → 迁移搬运了不该搬的表，或列错位；
    /// - 终端/命令消失、`foreign_key_check` 非空 → 迁移形态被改成重建表并留下悬空引用；
    /// - `not_needed` 写得进 → 有人又把第四档塞回 `hosts` 的 CHECK（它已挪到凭据上）。
    #[test]
    fn upgrading_a_v1_database_adds_the_flag_without_losing_rows() {
        // 手工摆出 V1 装机的形状：按 V1 建表、版本停在 1（不走 open_in_memory，
        // 那会直接迁到当前版本，测不到升级）。
        let conn = Connection::open_in_memory().expect("内存库");
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        conn.execute_batch(V1_SCHEMA).expect("建 V1 库");
        conn.pragma_update(None, "user_version", 1).unwrap();

        conn.execute_batch(
            "INSERT INTO credentials (id, username, kind, secret_enc, created_at, updated_at)
             VALUES ('cred_v1', 'root', 'password', 'v1:nonce:cipher', 'now', 'now');
             INSERT INTO hosts (id, address, port, credential_id, sudo_policy,
                                sudo_password_enc, created_at, updated_at)
             VALUES ('host_v1', '127.0.0.1', 22, 'cred_v1', 'auto', 'v1:nonce:pw',
                     'now', 'now');
             INSERT INTO terminals (id, host_id, status, created_at, updated_at)
             VALUES ('term_v1', 'host_v1', 'active', 'now', 'now');
             INSERT INTO commands (id, terminal_id, seq, command, status, created_at)
             VALUES ('cmd_v1', 'term_v1', 1, 'whoami', 'completed', 'now');",
        )
        .expect("V1 形状的样本数据");

        // 前置：V1 真的没有这一列，否则下面的断言全是在测既有事实。
        assert!(
            conn.execute(
                "UPDATE credentials SET is_privileged = 1 WHERE id = 'cred_v1'",
                []
            )
            .is_err(),
            "V1_SCHEMA 里已带 is_privileged：这条用例测不到升级路径"
        );

        migrate(&conn).expect("V1 → V2 迁移");

        let (flag, cipher): (i64, String) = conn
            .query_row(
                "SELECT is_privileged, secret_enc FROM credentials WHERE id = 'cred_v1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("凭据行必须原样存活");
        assert_eq!(
            flag, 0,
            "存量凭据应为非特权：默认给 1 等于无人确认就短路提权（fail-open）"
        );
        assert_eq!(
            cipher, "v1:nonce:cipher",
            "密文不得被改写：它与表结构无关，重新加密只会解不开"
        );

        conn.execute(
            "UPDATE credentials SET is_privileged = 1 WHERE id = 'cred_v1'",
            [],
        )
        .expect("升级后新列必须写得进");
        let flag: i64 = conn
            .query_row(
                "SELECT is_privileged FROM credentials WHERE id = 'cred_v1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(flag, 1, "写入未生效（默认值覆盖了显式赋值？）");

        // `ADD COLUMN` 只该动 credentials 的元数据；下面三条守住"没动别的表"。
        let kept: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM terminals WHERE id='term_v1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kept, 1, "终端没了：迁移动了不该动的表");
        let kept: i64 = conn
            .query_row("SELECT COUNT(*) FROM commands WHERE id='cmd_v1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kept, 1, "命令历史（审计的载体）没了");
        let pw: String = conn
            .query_row(
                "SELECT sudo_password_enc FROM hosts WHERE id='host_v1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pw, "v1:nonce:pw", "主机行的提口令密文被改写了");

        // 提权策略仍只有三档：这一轮把"是否特权"挪到了凭据上，没有放宽 hosts。
        assert!(
            conn.execute(
                "UPDATE hosts SET sudo_policy='not_needed' WHERE id='host_v1'",
                []
            )
            .is_err(),
            "hosts 不该接受第四档（它已改由 credentials.is_privileged 表达）"
        );
        assert!(
            conn.execute("UPDATE hosts SET sudo_policy='yolo' WHERE id='host_v1'", [])
                .is_err(),
            "CHECK 丢了，非法取值将静默入库"
        );

        // 外键完整性：上面的断言只看"数据还在"，看不出"引用指向了不存在的表"。
        let mut stmt = conn.prepare("PRAGMA foreign_key_check").unwrap();
        let dangling: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert!(
            dangling.is_empty(),
            "迁移留下了悬空外键引用（受影响的表）：{dangling:?}"
        );
    }

    /// 新建库的终点形状必须与升级后的库一致：`credentials` 有 `is_privileged`（默认 0），
    /// `hosts.sudo_policy` 仍只认三档。
    ///
    /// 判别性：追加 V2 步骤时把列名拼错、或把 ALTER 写成只作用于某张别的表，本用例即红；
    /// 有人把第四档塞回 `V1_SCHEMA` 的 CHECK，本用例也红。存量装机那一半由上一条用例守。
    #[test]
    fn fresh_database_matches_the_upgraded_shape() {
        let conn = open_in_memory().expect("open in-memory db");

        let mut stmt = conn.prepare("PRAGMA table_info(credentials)").unwrap();
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>("name"))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert!(
            cols.iter().any(|c| c == "is_privileged"),
            "新库缺 is_privileged 列，实际列为 {cols:?}"
        );

        conn.execute(
            "INSERT INTO credentials (id, username, kind, secret_enc, created_at, updated_at)
             VALUES ('cred_f', 'root', 'password', 'v1:nonce:cipher', 'now', 'now')",
            [],
        )
        .unwrap();
        let flag: i64 = conn
            .query_row(
                "SELECT is_privileged FROM credentials WHERE id = 'cred_f'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(flag, 0, "新建库的默认值应为非特权（fail-closed）");

        assert!(
            conn.execute(
                "INSERT INTO hosts (id, address, port, sudo_policy, created_at, updated_at)
                 VALUES ('host_nn', '127.0.0.1', 22, 'not_needed', 'now', 'now')",
                [],
            )
            .is_err(),
            "新库不该接受第四档"
        );
    }

    /// 守住 `RELEASED_SCHEMA_STEPS` 里每一项的**原文不可变**。
    ///
    /// 判别性：有人就地改 `V1_SCHEMA`（或改任何一个已发布步骤）来"顺手加个字段"时红——
    /// 那种改法只作用于新建库，存量装机拿不到同一列，升级路径还会静默失配（D50 / D60）。
    /// 摘要不符时失败信息直接打印实际值，按上面的四步流程追加新步骤后照抄即可。
    ///
    /// 第一条断言（不含 `\r`）守的是**摘要的可移植性**：登记摘要的前提是"这段文本在任何
    /// 开发机上字节一致"。本机工作区的 `.rs` 是 CRLF（`core.autocrlf=true`），而 Rust 会把
    /// 字符串字面量里的 CRLF 规范化为 LF，故该断言在两类行尾的机器上都是绿的（2026-09-23
    /// 实测：CRLF 工作区上 `\r` 计数为 0）。留着它，是因为一旦哪天不再成立
    /// （改 `.gitattributes`、或换编译器行为），摘要会随机器而变、这条测试就会
    /// 一台绿一台红——那时先修行尾策略（`*.rs text eol=lf`），不要改摘要。
    #[test]
    fn released_schema_texts_are_frozen() {
        use sha2::{Digest, Sha256};

        for (version, sql, expected_sha) in RELEASED_SCHEMA_STEPS {
            assert!(
                !sql.contains('\r'),
                "版本 {version} 的 schema 文本里出现了 \\r：摘要将随开发机的行尾配置而变，\
                 换机器就会红。行尾策略见仓库根 .gitattributes"
            );
            let actual = Sha256::digest(sql.as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>();
            assert_eq!(
                actual, *expected_sha,
                "版本 {version} 的已发布 schema 原文被改动了（摘要不符）。实际摘要 = {actual}"
            );
        }
    }

    /// 守住**登记册与 `migrate()` 说的是同一件事**：版本号每 +1 就必须有一项登记，
    /// 且按登记册重放出来的形状必须与 `migrate()` 建出来的完全一致。
    ///
    /// 判别性：把 `SCHEMA_VERSION` 抬到 3 却忘了在登记册追加（或反之，登记了却没在
    /// `migrate()` 里执行）都会红。`sqlite_master` 逐行比对抓的是"登记册漏了一项 SQL"
    /// 或"某一步实际没跑"——比字段清单更硬，因为它比的是磁盘上真正长出来的东西。
    #[test]
    fn released_schema_steps_cover_every_version() {
        let versions: Vec<i64> = RELEASED_SCHEMA_STEPS.iter().map(|(v, _, _)| *v).collect();
        let expected: Vec<i64> = (1..=SCHEMA_VERSION).collect();
        assert_eq!(
            versions, expected,
            "登记册必须与版本号一一对应：加迁移 = 追加一项 + 递增 SCHEMA_VERSION"
        );

        let replayed = Connection::open_in_memory().expect("内存库");
        for (_, sql, _) in RELEASED_SCHEMA_STEPS {
            replayed.execute_batch(sql).expect("按登记册重放");
        }
        let migrated = open_in_memory().expect("open in-memory db");

        let dump = |conn: &Connection| -> Vec<(String, String, String)> {
            conn.prepare(
                "SELECT type, name, sql FROM sqlite_master \
                 WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
        };
        let (a, b) = (dump(&replayed), dump(&migrated));
        assert_eq!(a.len(), b.len(), "登记册重放出的对象数量与 migrate() 不一致");
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(x, y, "登记册与 migrate() 建出的形状不同：{x:?} vs {y:?}");
        }
    }

    #[test]
    fn settings_roundtrip() {
        let conn = open_in_memory().unwrap();
        assert_eq!(get_setting(&conn, "key_provider").unwrap(), None);
        set_setting(&conn, "key_provider", "keyring").unwrap();
        assert_eq!(
            get_setting(&conn, "key_provider").unwrap().as_deref(),
            Some("keyring")
        );
        set_setting(&conn, "key_provider", "local_file").unwrap();
        assert_eq!(
            get_setting(&conn, "key_provider").unwrap().as_deref(),
            Some("local_file")
        );
        delete_setting(&conn, "key_provider").unwrap();
        assert_eq!(get_setting(&conn, "key_provider").unwrap(), None);
    }

    #[test]
    fn cascade_delete_removes_terminals_and_commands() {
        let conn = open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO hosts (id, address, port, created_at, updated_at)
             VALUES ('host_1', '10.0.0.1', 22, 'now', 'now')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO terminals (id, host_id, status, created_at, updated_at)
             VALUES ('term_1', 'host_1', 'active', 'now', 'now')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO commands (id, terminal_id, seq, command, status, created_at)
             VALUES ('cmd_1', 'term_1', 1, 'ls', 'completed', 'now')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO command_outputs (command_id, output) VALUES ('cmd_1', 'file.txt')",
            [],
        )
        .unwrap();

        // 删除主机应级联清空终端与命令（D21：删除终端连同历史）。
        conn.execute("DELETE FROM hosts WHERE id = 'host_1'", [])
            .unwrap();

        let terminals: i64 = conn
            .query_row("SELECT COUNT(*) FROM terminals", [], |r| r.get(0))
            .unwrap();
        let commands: i64 = conn
            .query_row("SELECT COUNT(*) FROM commands", [], |r| r.get(0))
            .unwrap();
        let outputs: i64 = conn
            .query_row("SELECT COUNT(*) FROM command_outputs", [], |r| r.get(0))
            .unwrap();
        assert_eq!((terminals, commands, outputs), (0, 0, 0));
    }

    #[test]
    fn fts_indexes_command_and_output() {
        let conn = open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO hosts (id, address, port, created_at, updated_at)
             VALUES ('host_1', '10.0.0.1', 22, 'now', 'now')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO terminals (id, host_id, status, created_at, updated_at)
             VALUES ('term_1', 'host_1', 'active', 'now', 'now')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO commands (id, terminal_id, seq, command, status, created_at)
             VALUES ('cmd_1', 'term_1', 1, 'systemctl restart nginx', 'completed', 'now')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO command_outputs (command_id, output) VALUES ('cmd_1', 'Job for nginx.service completed')",
            [],
        )
        .unwrap();

        let by_command: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM commands_fts WHERE commands_fts MATCH 'nginx'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let by_output: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM command_outputs_fts WHERE command_outputs_fts MATCH 'completed'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(by_command, 1);
        assert_eq!(by_output, 1);
    }
}