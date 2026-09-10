//! 终端仓储。
//!
//! 配额语义（Q11）：**归档终端不占配额**，因此配额校验只统计未归档终端。
//! 生命周期（D20 / D21）：active ↔ archived（可恢复）；broken 为连接断开；
//! 物理删除为终态，命令历史经外键级联一并删除。

use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::domain::terminal::{EnvSnapshot, Terminal, TerminalInfo, TerminalStatus};
use crate::domain::{now_rfc3339, DEFAULT_TERMINAL_LIMIT_GLOBAL, DEFAULT_TERMINAL_LIMIT_PER_HOST};
use crate::error::{AppError, Result};

const COLS: &str =
    "id, host_id, name, status, env_snapshot, created_at, updated_at, archived_at";

fn row_to_terminal(row: &Row<'_>) -> rusqlite::Result<Terminal> {
    let status_raw: String = row.get("status")?;
    let snapshot_raw: Option<String> = row.get("env_snapshot")?;

    Ok(Terminal {
        id: row.get("id")?,
        host_id: row.get("host_id")?,
        name: row.get("name")?,
        status: TerminalStatus::parse(&status_raw).unwrap_or(TerminalStatus::Active),
        env_snapshot: snapshot_raw.and_then(|s| serde_json::from_str(&s).ok()),
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        archived_at: row.get("archived_at")?,
    })
}

pub fn insert(conn: &Connection, t: &Terminal) -> Result<()> {
    let snapshot = t
        .env_snapshot
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;

    conn.execute(
        "INSERT INTO terminals
            (id, host_id, name, status, env_snapshot, created_at, updated_at, archived_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            t.id,
            t.host_id,
            t.name,
            t.status.as_str(),
            snapshot,
            t.created_at,
            t.updated_at,
            t.archived_at,
        ],
    )?;
    Ok(())
}

pub fn update(conn: &Connection, t: &Terminal) -> Result<()> {
    let snapshot = t
        .env_snapshot
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;

    let affected = conn.execute(
        "UPDATE terminals SET
            name = ?2, status = ?3, env_snapshot = ?4, updated_at = ?5, archived_at = ?6
         WHERE id = ?1",
        params![
            t.id,
            t.name,
            t.status.as_str(),
            snapshot,
            now_rfc3339(),
            t.archived_at,
        ],
    )?;

    if affected == 0 {
        return Err(AppError::TerminalNotFound(t.id.clone()));
    }
    Ok(())
}

pub fn get(conn: &Connection, id: &str) -> Result<Terminal> {
    conn.query_row(
        &format!("SELECT {COLS} FROM terminals WHERE id = ?1"),
        [id],
        |r| row_to_terminal(r),
    )
    .optional()?
    .ok_or_else(|| AppError::TerminalNotFound(id.to_string()))
}

/// 按状态列出终端（`None` 表示全部）。
pub fn list_by_status(conn: &Connection, status: Option<TerminalStatus>) -> Result<Vec<Terminal>> {
    let mut out = Vec::new();
    match status {
        Some(s) => {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLS} FROM terminals WHERE status = ?1 ORDER BY created_at DESC"
            ))?;
            let rows = stmt.query_map([s.as_str()], |r| row_to_terminal(r))?;
            for r in rows {
                out.push(r?);
            }
        }
        None => {
            let mut stmt =
                conn.prepare(&format!("SELECT {COLS} FROM terminals ORDER BY created_at DESC"))?;
            let rows = stmt.query_map([], |r| row_to_terminal(r))?;
            for r in rows {
                out.push(r?);
            }
        }
    }
    Ok(out)
}

/// 列出某主机下的终端。
pub fn list_by_host(conn: &Connection, host_id: &str) -> Result<Vec<Terminal>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM terminals WHERE host_id = ?1 ORDER BY created_at DESC"
    ))?;
    let rows = stmt.query_map([host_id], |r| row_to_terminal(r))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// 列出对 AI Agent 可见的终端——**归档终端不可见**（AGENTS.md 0.1）。
///
/// `host_id` 为 `None` 时返回全部主机下的可见终端。
pub fn list_visible_to_agent(conn: &Connection, host_id: Option<&str>) -> Result<Vec<Terminal>> {
    let mut out = Vec::new();
    match host_id {
        Some(hid) => {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLS} FROM terminals
                 WHERE host_id = ?1 AND status != 'archived'
                 ORDER BY created_at DESC"
            ))?;
            let rows = stmt.query_map([hid], |r| row_to_terminal(r))?;
            for r in rows {
                out.push(r?);
            }
        }
        None => {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLS} FROM terminals WHERE status != 'archived' ORDER BY created_at DESC"
            ))?;
            let rows = stmt.query_map([], |r| row_to_terminal(r))?;
            for r in rows {
                out.push(r?);
            }
        }
    }
    Ok(out)
}

/// 物理删除终端；命令历史经外键级联删除（D21）。
pub fn delete(conn: &Connection, id: &str) -> Result<()> {
    let affected = conn.execute("DELETE FROM terminals WHERE id = ?1", [id])?;
    if affected == 0 {
        return Err(AppError::TerminalNotFound(id.to_string()));
    }
    Ok(())
}

/// 统计占用配额的终端数（未归档）。
pub fn count_active(conn: &Connection, host_id: Option<&str>) -> Result<u32> {
    let n: i64 = match host_id {
        Some(hid) => conn.query_row(
            "SELECT COUNT(*) FROM terminals WHERE host_id = ?1 AND status != 'archived'",
            [hid],
            |r| r.get(0),
        )?,
        None => conn.query_row(
            "SELECT COUNT(*) FROM terminals WHERE status != 'archived'",
            [],
            |r| r.get(0),
        )?,
    };
    Ok(n as u32)
}

/// 配额校验（Q11）。
///
/// 超出时返回 [`AppError::TerminalQuotaExceeded`]；调用方应把 `scope`
/// 原样传给用户，使其能区分"该主机满了"还是"全局满了"。
pub fn check_quota(conn: &Connection, host_id: &str, per_host: u32, global: u32) -> Result<()> {
    if count_active(conn, Some(host_id))? >= per_host {
        return Err(AppError::TerminalQuotaExceeded { scope: "per_host" });
    }
    if count_active(conn, None)? >= global {
        return Err(AppError::TerminalQuotaExceeded { scope: "global" });
    }
    Ok(())
}

/// 使用默认配额校验（便于调用方省略配置读取）。
pub fn check_quota_default(conn: &Connection, host_id: &str) -> Result<()> {
    check_quota(
        conn,
        host_id,
        DEFAULT_TERMINAL_LIMIT_PER_HOST,
        DEFAULT_TERMINAL_LIMIT_GLOBAL,
    )
}

/// 保存环境快照（D4）。
pub fn set_env_snapshot(conn: &Connection, id: &str, snapshot: &EnvSnapshot) -> Result<()> {
    let json = serde_json::to_string(snapshot)?;
    let affected = conn.execute(
        "UPDATE terminals SET env_snapshot = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, json, now_rfc3339()],
    )?;
    if affected == 0 {
        return Err(AppError::TerminalNotFound(id.to_string()));
    }
    Ok(())
}

/// 把某主机下所有未归档终端标记为 broken（应用重启或连接中断时，D3）。
pub fn mark_host_terminals_broken(conn: &Connection, host_id: &str) -> Result<usize> {
    let affected = conn.execute(
        "UPDATE terminals SET status = 'broken', updated_at = ?2
         WHERE host_id = ?1 AND status = 'active'",
        params![host_id, now_rfc3339()],
    )?;
    Ok(affected)
}

/// 把所有未归档终端标记为 broken（应用启动时调用：
/// 上次运行结束后的会话已不存在，需由 Agent 重建）。
pub fn mark_all_broken_on_startup(conn: &Connection) -> Result<usize> {
    let affected = conn.execute(
        "UPDATE terminals SET status = 'broken', updated_at = ?1 WHERE status = 'active'",
        [now_rfc3339()],
    )?;
    Ok(affected)
}

/// 供 Agent 查看的终端信息。
pub fn list_agent_info(conn: &Connection, host_id: Option<&str>) -> Result<Vec<TerminalInfo>> {
    Ok(list_visible_to_agent(conn, host_id)?
        .iter()
        .map(TerminalInfo::from)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::terminal::Terminal;

    fn setup() -> Connection {
        let conn = crate::store::db::open_in_memory().unwrap();
        conn.execute(
            "INSERT INTO hosts (id, address, port, created_at, updated_at)
             VALUES ('host_1', '10.0.0.1', 22, 'now', 'now')",
            [],
        )
        .unwrap();
        conn
    }

    #[test]
    fn insert_get_update_roundtrip() {
        let conn = setup();
        let t = Terminal::new("host_1", Some("my terminal".into()));
        insert(&conn, &t).unwrap();

        let got = get(&conn, &t.id).unwrap();
        assert_eq!(got.host_id, "host_1");
        assert_eq!(got.name.as_deref(), Some("my terminal"));
        assert_eq!(got.status, TerminalStatus::Active);

        let mut edited = got.clone();
        edited.name = Some("renamed".into());
        update(&conn, &edited).unwrap();
        assert_eq!(get(&conn, &t.id).unwrap().name.as_deref(), Some("renamed"));
    }

    #[test]
    fn archive_then_restore_keeps_id_and_status() {
        let conn = setup();
        let mut t = Terminal::new("host_1", None);
        insert(&conn, &t).unwrap();

        t.archive();
        update(&conn, &t).unwrap();
        let archived = get(&conn, &t.id).unwrap();
        assert_eq!(archived.status, TerminalStatus::Archived);
        assert!(archived.archived_at.is_some());

        // D20：恢复沿用原 ID。
        t.restore();
        update(&conn, &t).unwrap();
        let restored = get(&conn, &t.id).unwrap();
        assert_eq!(restored.id, t.id);
        assert_eq!(restored.status, TerminalStatus::Active);
        assert!(restored.archived_at.is_none());
    }

    #[test]
    fn archived_terminals_are_invisible_to_agent() {
        let conn = setup();
        let mut t1 = Terminal::new("host_1", None);
        insert(&conn, &t1).unwrap();
        let mut t2 = Terminal::new("host_1", None);
        insert(&conn, &t2).unwrap();

        t2.archive();
        update(&conn, &t2).unwrap();

        let visible = list_visible_to_agent(&conn, Some("host_1")).unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, t1.id);

        // 但人类侧仍能看到全部（含归档）。
        assert_eq!(list_by_host(&conn, "host_1").unwrap().len(), 2);

        // 恢复后重新可见。
        t2.restore();
        update(&conn, &t2).unwrap();
        assert_eq!(
            list_visible_to_agent(&conn, Some("host_1")).unwrap().len(),
            2
        );
        let _ = &mut t1;
    }

    #[test]
    fn quota_ignores_archived_terminals() {
        let conn = setup();
        // 每主机配额设为 2。
        for _ in 0..2 {
            let t = Terminal::new("host_1", None);
            insert(&conn, &t).unwrap();
        }
        assert!(check_quota(&conn, "host_1", 2, 20).is_err(), "已满应拒绝");

        // 归档一个后释放配额，应可再建。
        let mut t = list_by_host(&conn, "host_1").unwrap().remove(0);
        t.archive();
        update(&conn, &t).unwrap();
        assert!(
            check_quota(&conn, "host_1", 2, 20).is_ok(),
            "归档不占配额，应可再建"
        );
    }

    #[test]
    fn global_quota_is_enforced_across_hosts() {
        let conn = setup();
        conn.execute(
            "INSERT INTO hosts (id, address, port, created_at, updated_at)
             VALUES ('host_2', '10.0.0.2', 22, 'now', 'now')",
            [],
        )
        .unwrap();

        insert(&conn, &Terminal::new("host_1", None)).unwrap();
        insert(&conn, &Terminal::new("host_2", None)).unwrap();

        // 全局配额 2：host_1 自己没满，但全局已满。
        let err = check_quota(&conn, "host_1", 5, 2).unwrap_err();
        match err {
            AppError::TerminalQuotaExceeded { scope } => assert_eq!(scope, "global"),
            other => panic!("期望 quota 错误，实际 {other:?}"),
        }
    }

    #[test]
    fn per_host_quota_scope_is_reported() {
        let conn = setup();
        insert(&conn, &Terminal::new("host_1", None)).unwrap();
        let err = check_quota(&conn, "host_1", 1, 20).unwrap_err();
        match err {
            AppError::TerminalQuotaExceeded { scope } => assert_eq!(scope, "per_host"),
            other => panic!("期望 quota 错误，实际 {other:?}"),
        }
    }

    #[test]
    fn env_snapshot_persists_as_json() {
        let conn = setup();
        let t = Terminal::new("host_1", None);
        insert(&conn, &t).unwrap();

        let snap = EnvSnapshot {
            path: Some("/usr/local/bin:/usr/bin".into()),
            pwd: Some("/home/deploy".into()),
            bash_version: Some("5.3.9".into()),
            shell_env_mode: "login".into(),
            captured_at: now_rfc3339(),
        };
        set_env_snapshot(&conn, &t.id, &snap).unwrap();

        let got = get(&conn, &t.id).unwrap();
        let stored = got.env_snapshot.expect("snapshot should persist");
        assert_eq!(stored.path.as_deref(), snap.path.as_deref());
        assert_eq!(stored.pwd.as_deref(), snap.pwd.as_deref());
    }

    #[test]
    fn delete_cascades_to_commands() {
        let conn = setup();
        let t = Terminal::new("host_1", None);
        insert(&conn, &t).unwrap();
        conn.execute(
            "INSERT INTO commands (id, terminal_id, seq, command, status, created_at)
             VALUES ('cmd_1', ?1, 1, 'ls', 'completed', 'now')",
            [&t.id],
        )
        .unwrap();

        delete(&conn, &t.id).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM commands", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "删除终端应连同命令历史（D21）");
    }

    #[test]
    fn mark_all_broken_on_startup_only_affects_active() {
        let conn = setup();
        let mut archived = Terminal::new("host_1", None);
        insert(&conn, &archived).unwrap();
        archived.archive();
        update(&conn, &archived).unwrap();

        let active = Terminal::new("host_1", None);
        insert(&conn, &active).unwrap();

        let n = mark_all_broken_on_startup(&conn).unwrap();
        assert_eq!(n, 1, "仅活跃终端应被标记");
        assert_eq!(get(&conn, &active.id).unwrap().status, TerminalStatus::Broken);
        assert_eq!(
            get(&conn, &archived.id).unwrap().status,
            TerminalStatus::Archived,
            "归档状态不应被启动逻辑改动"
        );
    }

    #[test]
    fn missing_terminal_reports_not_found() {
        let conn = setup();
        assert!(matches!(
            get(&conn, "term_nope"),
            Err(AppError::TerminalNotFound(_))
        ));
        assert!(matches!(
            delete(&conn, "term_nope"),
            Err(AppError::TerminalNotFound(_))
        ));
    }

    #[test]
    fn agent_info_reflects_status() {
        let conn = setup();
        let t = Terminal::new("host_1", Some("t".into()));
        insert(&conn, &t).unwrap();

        let info = list_agent_info(&conn, Some("host_1")).unwrap();
        assert_eq!(info.len(), 1);
        assert_eq!(info[0].status, TerminalStatus::Active);
        assert_eq!(info[0].name.as_deref(), Some("t"));
    }
}
