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
        conn.execute_batch(V2_COMMAND_STATUS)
            .map_err(|e| AppError::Database(e))?;
    }

    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

/// V2：`commands.status` 允许新状态 `connection_lost`（2026-09-16）。
///
/// 背景：断线时正在运行的命令原本被写成 `completed`（界面显示绿色"已完成"），
/// 而"连接断了、结局未知"与"命令跑过并失败"在审计上是两件事。客户要求单列状态。
///
/// **必须用迁移而不能只改建表语句**：已装用户的 `commands` 表早已建好，
/// `CREATE TABLE IF NOT EXISTS` 不会重建它，新状态会被旧 CHECK 约束拒绝写入
/// （SQLite 报 CHECK constraint failed），表现为"命令结束时落库失败"。
///
/// SQLite 不支持修改 CHECK，只能重建表。重建时必须同时补齐三件容易漏掉的事：
///
/// 1. **索引**：`DROP TABLE` 会连索引一起删，三个索引必须重建（否则列表/搜索变慢）；
/// 2. **FTS 与触发器**：`commands_fts` 是**外部内容表**（`content='commands'`），
///    三个同步触发器挂在 `commands` 上，删表会一并删掉——必须重建触发器，
///    并用 `VALUES('rebuild')` 让 FTS 按新表的 rowid 重建索引；
/// 3. **外键**：`command_outputs` 以 `ON DELETE CASCADE` 挂在 `commands` 上，
///    若开着外键，`DROP TABLE commands` 会把输出**级联删光**。
///    因此迁移期间临时关闭外键（迁移结束再打开）。
///
/// 整个过程在 `execute_batch` 里执行，SQLite 会隐式包一层事务：任一步失败则全部回滚，
/// 不会留下"表重建了一半"的中间状态。
const V2_COMMAND_STATUS: &str = r#"
PRAGMA foreign_keys = OFF;

CREATE TABLE commands_new (
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

INSERT INTO commands_new
    SELECT id, terminal_id, seq, command, status, exit_code, duration_ms,
           truncated, output_bytes, created_at, started_at, finished_at
      FROM commands;

DROP TABLE commands;
ALTER TABLE commands_new RENAME TO commands;

CREATE INDEX IF NOT EXISTS idx_commands_terminal ON commands(terminal_id, seq);
CREATE INDEX IF NOT EXISTS idx_commands_created  ON commands(created_at);
CREATE INDEX IF NOT EXISTS idx_commands_status   ON commands(status);

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

-- 重建两套 FTS 索引：commands 的 rowid 可能变化，command_outputs 的 rowid
-- 也可能因主键类型（TEXT）在表重建后重排。'rebuild' 幂等，重跑无副作用。
INSERT INTO commands_fts(commands_fts) VALUES('rebuild');
INSERT INTO command_outputs_fts(command_outputs_fts) VALUES('rebuild');

PRAGMA foreign_keys = ON;
"#;

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
                 CHECK (status IN ('queued', 'running', 'completed', 'failed')),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// **V2 迁移回归**：`connection_lost` 必须能被真正写进库（客户 2026-09-16 要求）。
    ///
    /// 判别性：`commands.status` 上有 CHECK 约束。若只改 `V1_SCHEMA` 而不加迁移，
    /// **已装用户的旧表**仍会拒绝这个新值（`CHECK constraint failed`），
    /// 表现为"命令结束时落库失败"——而全新库却一切正常，属于极难在开发机上发现的缺陷。
    /// 本用例先造一个"旧版库"（把 status 约束改回四态），再跑迁移，然后写入新状态。
    #[test]
    fn v2_migration_allows_connection_lost_status() {
        let conn = open_in_memory().expect("内存库（已迁移到当前版本）");

        // 把表退回"旧版"形态：CHECK 只认四个旧状态，并把 user_version 调回 1，
        // 这样下面的 migrate 会真正执行 V2 分支。
        conn.execute_batch(
            "PRAGMA foreign_keys = OFF;
             CREATE TABLE commands_old (
                 id           TEXT PRIMARY KEY,
                 terminal_id  TEXT NOT NULL REFERENCES terminals(id) ON DELETE CASCADE,
                 seq          INTEGER NOT NULL,
                 command      TEXT NOT NULL,
                 status       TEXT NOT NULL DEFAULT 'queued'
                              CHECK (status IN ('queued','running','completed','failed')),
                 exit_code    INTEGER,
                 duration_ms  INTEGER,
                 truncated    INTEGER NOT NULL DEFAULT 0,
                 output_bytes INTEGER,
                 created_at   TEXT NOT NULL,
                 started_at   TEXT,
                 finished_at  TEXT
             );
             DROP TABLE commands;
             ALTER TABLE commands_old RENAME TO commands;
             PRAGMA user_version = 1;
             PRAGMA foreign_keys = ON;",
        )
        .expect("造一个旧版库");

        // 旧约束下应当写不进去——先证明"这个用例确实抓得住旧形态"。
        conn.execute(
            "INSERT INTO hosts (id, address, port, created_at, updated_at)
             VALUES ('host_v2', '127.0.0.1', 22, 'now', 'now')",
            [],
        )
        .expect("写入主机");
        conn.execute(
            "INSERT INTO terminals (id, host_id, status, created_at, updated_at)
             VALUES ('term_v2', 'host_v2', 'active', 'now', 'now')",
            [],
        )
        .expect("写入终端");
        let before = conn.execute(
            "INSERT INTO commands (id, terminal_id, seq, command, status, created_at)
             VALUES ('cmd_old', 'term_v2', 1, 'ls', 'connection_lost', 'now')",
            [],
        );
        assert!(
            before.is_err(),
            "旧约束本应拒绝 connection_lost（否则本用例证明不了迁移的必要性）"
        );

        // 跑迁移，再写一次：这次必须成功。
        migrate(&conn).expect("迁移应成功");
        conn.execute(
            "INSERT INTO commands (id, terminal_id, seq, command, status, created_at)
             VALUES ('cmd_new', 'term_v2', 2, 'ls', 'connection_lost', 'now')",
            [],
        )
        .expect("迁移后 connection_lost 必须可写");

        // 迁移不能丢旧数据：先前那条 completed 记录要还在。
        conn.execute(
            "INSERT INTO commands (id, terminal_id, seq, command, status, created_at)
             VALUES ('cmd_keep', 'term_v2', 3, 'pwd', 'completed', 'now')",
            [],
        )
        .expect("写入保留记录");
        let kept: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM commands WHERE status = 'completed'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kept, 1, "迁移必须保留既有记录");

        // 输出与 FTS 也必须活着：迁移里重建了表，容易把这两样一起弄丢。
        conn.execute(
            "INSERT INTO command_outputs (command_id, output) VALUES ('cmd_keep', 'hello-fts')",
            [],
        )
        .expect("分离表输出应可写");
        let hit: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM commands_fts WHERE commands_fts MATCH 'pwd'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hit, 1, "迁移后 FTS 必须仍能搜到命令");
        let hit2: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM command_outputs_fts WHERE command_outputs_fts MATCH 'hello'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hit2, 1, "迁移后输出的 FTS 必须仍可用");
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
