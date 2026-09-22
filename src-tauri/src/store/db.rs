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
pub fn migrate(conn: &Connection) -> Result<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;

    if current >= SCHEMA_VERSION {
        return Ok(());
    }

    if current < 1 {
        conn.execute_batch(V1_SCHEMA)
            .map_err(|e| AppError::Database(e))?;
    }

    if current < 2 {
        conn.execute_batch(V2_SUDO_NOT_NEEDED)
            .map_err(|e| AppError::Database(e))?;
    }

    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
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
--
-- **这里的 `sudo_policy` CHECK 是 V1 的历史形状，不要就地改宽**：改这里只会让
-- 新建库跳过迁移，存量 V1 库仍然写不进 `not_needed`。放宽由 V2 负责（见下）。
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

/// V2：`hosts.sudo_policy` 允许第四档 `not_needed`（D60）。
///
/// **为什么这次必须有迁移**，而不像 **D50** 那样只改建表语句：已发布版本（v0.1.1 起）
/// 装机上的 `hosts` 表早已按三值 CHECK 建好，`CREATE TABLE IF NOT EXISTS` 不会重建它，
/// 写入 `not_needed` 会以 `CHECK constraint failed` 失败——表现为"把提权策略改成
/// 「无需提权」之后保存主机就报错"。D50 当时刻意不迁移的前提是"产品未发布、
/// 只影响开发机本地数据"，并在同一条里写明"**首发前若已存在用户库，则必须改回迁移方案**"；
/// 该前提现已不成立，本步就是它预留的那条路（实现形态取自 `53b5621` 删除的 V2）。
///
/// SQLite 不支持修改 CHECK，只能重建表。这里有三个**各自独立**的坑：
///
/// 1. **外键方向**：`hosts` 是 `terminals.host_id` 的**父表**且 `ON DELETE CASCADE`。
///    若开着外键执行 `DROP TABLE hosts`，会把这些主机下的**全部终端与命令历史连带删光**
///    ——迁移把一个约束改宽，代价却是把审计数据清了。因此必须临时关闭外键。
/// 2. **`PRAGMA foreign_keys` 在事务内是 no-op**：把这两句写进 `BEGIN`/`COMMIT` 之间
///    等于没关，正好踩回坑 1。所以它们单独放在事务之外（autocommit 下才生效）。
/// 3. **索引**：`DROP TABLE` 连索引一起删，`idx_hosts_credential` 必须重建，
///    否则主机列表按凭据过滤退化成全表扫描。
///
/// 重建方向刻意选"建新表 → 逐列拷数据 → 删旧表 → 把新表**改名成 `hosts`**"：
/// 子表按**名字**引用 `hosts`，只要最终落到这个名字上，`terminals` 的外键仍然指对。
/// 反过来的做法（先 `ALTER TABLE hosts RENAME TO hosts_old`）在 `legacy_alter_table=OFF`
/// 下会连带改写子表的引用，把终端表挂到临时表上——那是这条路径上最容易踩的第四个坑。
///
/// `sudo_password_enc` 逐列原样搬运，**不重新加密**：密文由主密钥保护，与表结构无关。
const V2_SUDO_NOT_NEEDED: &str = r#"
PRAGMA foreign_keys = OFF;

BEGIN;

CREATE TABLE hosts_new (
    id                   TEXT PRIMARY KEY,
    name                 TEXT,
    address              TEXT NOT NULL,
    port                 INTEGER NOT NULL DEFAULT 22,
    credential_id        TEXT REFERENCES credentials(id) ON DELETE SET NULL,
    proxy_jump_host_id   TEXT REFERENCES hosts(id) ON DELETE SET NULL,
    sudo_policy          TEXT NOT NULL DEFAULT 'deny'
                         CHECK (sudo_policy IN ('deny', 'ask', 'auto', 'not_needed')),
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

-- 逐列点名而不是 SELECT *：新旧表列顺序一旦不同，`*` 会静默错位。
INSERT INTO hosts_new
    SELECT id, name, address, port, credential_id, proxy_jump_host_id,
           sudo_policy, sudo_password_source, sudo_password_enc,
           shell_env_mode, init_script, host_key, host_key_fingerprint,
           created_at, updated_at
      FROM hosts;

DROP TABLE hosts;
ALTER TABLE hosts_new RENAME TO hosts;

CREATE INDEX IF NOT EXISTS idx_hosts_credential ON hosts(credential_id);

COMMIT;

PRAGMA foreign_keys = ON;
"#;

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

    /// 守的是**已发布装机升级**这条路径：V1 库（三值 CHECK）里有主机、终端、
    /// 命令历史与提权口令密文，迁移之后四样都得还在，且新档位写得进去。
    ///
    /// 判别性（每一项都能抓一个真实缺陷，缺一即红）：
    /// - `not_needed` 写不进 → 忘了放宽 CHECK；
    /// - 终端/命令消失 → `DROP TABLE hosts` 开着外键，被 `ON DELETE CASCADE`
    ///   连带删光——**审计数据被迁移清掉**，是最贵的一种错；
    /// - `sudo_password_enc` 变了 → 拷数据时重新加密或列错位；
    /// - `'yolo'` 写得进 → 重建时把 CHECK 整个丢了。
    #[test]
    fn upgrading_a_v1_database_widens_the_check_without_losing_rows() {
        // 手工摆出 V1 装机的形状：按 V1 建表、版本停在 1（不走 open_in_memory，
        // 那会直接迁到当前版本，测不到升级）。
        let conn = Connection::open_in_memory().expect("内存库");
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        conn.execute_batch(V1_SCHEMA).expect("建 V1 库");
        conn.pragma_update(None, "user_version", 1).unwrap();

        conn.execute_batch(
            "INSERT INTO hosts (id, address, port, sudo_policy, sudo_password_enc,
                                created_at, updated_at)
             VALUES ('host_v1', '127.0.0.1', 22, 'auto', 'v1:nonce:cipher',
                     'now', 'now');
             -- 自引用一行（跳板机）：`REFERENCES hosts(id)` 指向**本表**，
             -- 重建 + 改名最容易把它指向一张不存在的表。
             INSERT INTO hosts (id, address, port, proxy_jump_host_id, created_at, updated_at)
             VALUES ('host_v1_jump', '10.0.0.2', 22, 'host_v1', 'now', 'now');
             INSERT INTO terminals (id, host_id, status, created_at, updated_at)
             VALUES ('term_v1', 'host_v1', 'active', 'now', 'now');
             INSERT INTO commands (id, terminal_id, seq, command, status, created_at)
             VALUES ('cmd_v1', 'term_v1', 1, 'whoami', 'completed', 'now');",
        )
        .expect("V1 形状的样本数据");

        migrate(&conn).expect("V1 → V2 迁移");

        let (cipher, policy): (String, String) = conn
            .query_row(
                "SELECT sudo_password_enc, sudo_policy FROM hosts WHERE id='host_v1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("主机行必须原样存活");
        assert_eq!(
            cipher, "v1:nonce:cipher",
            "提口令密文不得被改写：它与表结构无关，重新加密只会解不开"
        );
        assert_eq!(policy, "auto", "既有档位不得被重新解释");

        let kept: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM terminals WHERE id='term_v1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kept, 1, "终端被级联删掉了：迁移期间没关外键（见坑 1）");
        let kept: i64 = conn
            .query_row("SELECT COUNT(*) FROM commands WHERE id='cmd_v1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kept, 1, "命令历史（审计的载体）被迁移删掉了");

        // 新档位现在写得进去——这正是本条迁移存在的理由。
        conn.execute(
            "UPDATE hosts SET sudo_policy='not_needed' WHERE id='host_v1'",
            [],
        )
        .expect("升级后必须接受 not_needed");
        // 但 CHECK 仍在：放宽取值不等于取消约束。
        assert!(
            conn.execute("UPDATE hosts SET sudo_policy='yolo' WHERE id='host_v1'", [])
                .is_err(),
            "重建时丢了 CHECK，非法取值将静默入库"
        );
        // 索引也得跟着回来：DROP TABLE 会连索引一起删（见坑 3）。
        let idx: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index'
                   AND name='idx_hosts_credential'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(idx, 1, "idx_hosts_credential 没重建：凭据过滤退化为全表扫描");

        // 外键完整性：上面几条断言只看"数据还在"，看不出"引用指向了不存在的表"。
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
        let jump: Option<String> = conn
            .query_row(
                "SELECT proxy_jump_host_id FROM hosts WHERE id='host_v1_jump'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            jump.as_deref(),
            Some("host_v1"),
            "自引用（跳板机）的值在搬运中丢了"
        );
    }

    /// 新建库的终点形状必须与升级后的库**完全一致**（V1 建表 + V2 重建两步都走）。
    ///
    /// 判别性：若有人图省事只改 `V1_SCHEMA` 的 CHECK，本用例仍会绿——但它守住的是
    /// "两条路径收敛到同一形状"里另一条：忘了写 V2 时新库看似没问题，存量装机却
    /// 永远升不上去。真正抓那半边的是上一条用例。
    #[test]
    fn fresh_database_accepts_the_fourth_policy() {
        let conn = open_in_memory().expect("open in-memory db");
        conn.execute(
            "INSERT INTO hosts (id, address, port, sudo_policy, created_at, updated_at)
             VALUES ('host_nn', '127.0.0.1', 22, 'not_needed', 'now', 'now')",
            [],
        )
        .expect("新库应直接支持第四档");
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