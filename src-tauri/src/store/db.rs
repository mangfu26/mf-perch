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
        conn.execute_batch(V2_CREDENTIAL_IS_PRIVILEGED)
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