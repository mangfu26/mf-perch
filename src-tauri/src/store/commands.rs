//! 命令记录仓储：命令、输出、全文搜索与保留期清理（Q4 / Q12 / Q17）。
//!
//! 输出与命令分表存储：列表查询不加载大字段，避免拖垮内存；
//! 搜索走 FTS5 虚拟表（由触发器同步，见 `store::db`）。

use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::domain::command::{
    CommandRecord, CommandStatus, CommandStatusView, CommandWithOutput,
};
use crate::domain::{now_rfc3339, SortOrder};
use crate::error::{AppError, Result};

const COLS: &str = "id, terminal_id, seq, command, status, exit_code, duration_ms, \
                    truncated, output_bytes, created_at, started_at, finished_at";

fn row_to_record(row: &Row<'_>) -> rusqlite::Result<CommandRecord> {
    let status_raw: String = row.get("status")?;
    Ok(CommandRecord {
        id: row.get("id")?,
        terminal_id: row.get("terminal_id")?,
        seq: row.get::<_, i64>("seq")? as u64,
        command: row.get("command")?,
        status: CommandStatus::parse(&status_raw).unwrap_or(CommandStatus::Failed),
        exit_code: row.get("exit_code")?,
        duration_ms: row.get::<_, Option<i64>>("duration_ms")?.map(|v| v as u64),
        truncated: row.get::<_, i64>("truncated")? != 0,
        output_bytes: row.get::<_, Option<i64>>("output_bytes")?.map(|v| v as u64),
        created_at: row.get("created_at")?,
        started_at: row.get("started_at")?,
        finished_at: row.get("finished_at")?,
    })
}

/// 插入命令记录（入队时调用）。
pub fn insert(conn: &Connection, r: &CommandRecord) -> Result<()> {
    conn.execute(
        "INSERT INTO commands
            (id, terminal_id, seq, command, status, exit_code, duration_ms,
             truncated, output_bytes, created_at, started_at, finished_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        params![
            r.id,
            r.terminal_id,
            r.seq as i64,
            r.command,
            r.status.as_str(),
            r.exit_code,
            r.duration_ms.map(|v| v as i64),
            r.truncated as i64,
            r.output_bytes.map(|v| v as i64),
            r.created_at,
            r.started_at,
            r.finished_at,
        ],
    )?;
    Ok(())
}

/// 更新命令状态（开始执行 / 完成）。
pub fn update_status(conn: &Connection, r: &CommandRecord) -> Result<()> {
    let affected = conn.execute(
        "UPDATE commands SET
            status = ?2, exit_code = ?3, duration_ms = ?4, truncated = ?5,
            output_bytes = ?6, started_at = ?7, finished_at = ?8
         WHERE id = ?1",
        params![
            r.id,
            r.status.as_str(),
            r.exit_code,
            r.duration_ms.map(|v| v as i64),
            r.truncated as i64,
            r.output_bytes.map(|v| v as i64),
            r.started_at,
            r.finished_at,
        ],
    )?;
    if affected == 0 {
        return Err(AppError::CommandNotFound(r.id.clone()));
    }
    Ok(())
}

pub fn get(conn: &Connection, id: &str) -> Result<CommandRecord> {
    conn.query_row(
        &format!("SELECT {COLS} FROM commands WHERE id = ?1"),
        [id],
        |r| row_to_record(r),
    )
    .optional()?
    .ok_or_else(|| AppError::CommandNotFound(id.to_string()))
}

/// 保存（或覆盖）命令输出。输出在命令执行过程中会多次追加，
/// 因此使用 upsert 语义；FTS 索引由触发器自动同步。
pub fn save_output(conn: &Connection, command_id: &str, output: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO command_outputs (command_id, output) VALUES (?1, ?2)
         ON CONFLICT(command_id) DO UPDATE SET output = excluded.output",
        params![command_id, output],
    )?;
    Ok(())
}

pub fn get_output(conn: &Connection, command_id: &str) -> Result<Option<String>> {
    let v = conn
        .query_row(
            "SELECT output FROM command_outputs WHERE command_id = ?1",
            [command_id],
            |r| r.get::<_, String>(0),
        )
        .optional()?;
    Ok(v)
}

/// 读取命令及其输出。
pub fn get_with_output(conn: &Connection, id: &str) -> Result<CommandWithOutput> {
    let record = get(conn, id)?;
    let output = get_output(conn, id)?.unwrap_or_default();
    Ok(CommandWithOutput { record, output })
}

/// 构造轮询视图。`tail_lines` 为 `Some(n)` 时只返回末尾 n 行，
/// 便于长输出场景下控制返回体积。
pub fn status_view(
    conn: &Connection,
    command_id: &str,
    include_output: bool,
    tail_lines: Option<usize>,
) -> Result<CommandStatusView> {
    let record = get(conn, command_id)?;

    let (output, omitted) = if include_output {
        let full = get_output(conn, command_id)?.unwrap_or_default();
        let sliced = match tail_lines {
            Some(n) => tail(&full, n),
            None => full,
        };
        (Some(sliced), false)
    } else {
        (None, true)
    };

    Ok(CommandStatusView {
        command_id: record.id,
        terminal_id: record.terminal_id,
        command: record.command,
        status: record.status,
        exit_code: record.exit_code,
        duration_ms: record.duration_ms,
        truncated: record.truncated,
        output,
        output_omitted: omitted,
    })
}

/// 取字符串末尾 `n` 行。
fn tail(s: &str, n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= n {
        return s.to_string();
    }
    lines[lines.len() - n..].join("\n")
}

/// 该终端的下一个命令序号（用于与远端结束标记配对，D3）。
pub fn next_seq(conn: &Connection, terminal_id: &str) -> Result<u64> {
    let max: Option<i64> = conn.query_row(
        "SELECT MAX(seq) FROM commands WHERE terminal_id = ?1",
        [terminal_id],
        |r| r.get(0),
    )?;
    Ok(max.map(|v| v as u64 + 1).unwrap_or(1))
}

/// 列出该终端尚未结束的命令（用于判断队列是否可用）。
pub fn pending_count(conn: &Connection, terminal_id: &str) -> Result<usize> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM commands WHERE terminal_id = ?1 AND status IN ('queued','running')",
        [terminal_id],
        |r| r.get(0),
    )?;
    Ok(n as usize)
}

/// 取该终端最近一条尚未结束的命令（串行执行时即为"当前命令"）。
pub fn current_pending(conn: &Connection, terminal_id: &str) -> Result<Option<CommandRecord>> {
    let v = conn
        .query_row(
            &format!(
                "SELECT {COLS} FROM commands
                 WHERE terminal_id = ?1 AND status IN ('queued','running')
                 ORDER BY seq ASC LIMIT 1"
            ),
            [terminal_id],
            |r| row_to_record(r),
        )
        .optional()?;
    Ok(v)
}

/// 把终端的未完成命令标记为失败（终端断开时调用）。
pub fn fail_pending_for_terminal(conn: &Connection, terminal_id: &str, reason: &str) -> Result<usize> {
    let now = now_rfc3339();
    let affected = conn.execute(
        "UPDATE commands SET status = 'failed', finished_at = ?2
         WHERE terminal_id = ?1 AND status IN ('queued','running')",
        params![terminal_id, now],
    )?;
    if affected > 0 {
        // 把原因写入输出，便于人类审计时看到失败背景。
        let mut stmt = conn.prepare(
            "SELECT id FROM commands WHERE terminal_id = ?1 AND status = 'failed'
             AND finished_at = ?2",
        )?;
        let ids: Vec<String> = stmt
            .query_map(params![terminal_id, now], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for id in ids {
            let msg = format!("[mf-perch] 命令未能完成：{reason}\n");
            save_output(conn, &id, &msg)?;
        }
    }
    Ok(affected)
}

/// 列出来自某终端的命令记录。
pub fn list_by_terminal(
    conn: &Connection,
    terminal_id: &str,
    limit: usize,
    offset: usize,
) -> Result<Vec<CommandRecord>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM commands WHERE terminal_id = ?1
         ORDER BY seq DESC LIMIT ?2 OFFSET ?3"
    ))?;
    let rows = stmt.query_map(params![terminal_id, limit as i64, offset as i64], |r| {
        row_to_record(r)
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// 历史查询过滤条件（Q17：按终端 / 主机筛选）。
#[derive(Debug, Clone, Default)]
pub struct HistoryFilter {
    pub terminal_id: Option<String>,
    pub host_id: Option<String>,
    /// 全文搜索关键词（命令与输出）。
    pub query: Option<String>,
    pub status: Option<CommandStatus>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub order: SortOrder,
}

/// 按条件查询命令历史；`query` 走 FTS5，否则走普通索引。
pub fn search_history(conn: &Connection, filter: &HistoryFilter) -> Result<Vec<CommandRecord>> {
    let limit = filter.limit.unwrap_or(100).min(1000) as i64;
    let offset = filter.offset.unwrap_or(0) as i64;
    let order = match filter.order {
        SortOrder::Asc => "ASC",
        SortOrder::Desc => "DESC",
    };

    let mut sql = String::from("SELECT ");
    sql.push_str(
        "c.id, c.terminal_id, c.seq, c.command, c.status, c.exit_code, c.duration_ms, \
         c.truncated, c.output_bytes, c.created_at, c.started_at, c.finished_at FROM commands c",
    );

    // 全文搜索：命令或输出命中任一即算匹配。
    // 注意 FTS5 不允许对 MATCH 使用表别名（`fts o WHERE o MATCH ?`），
    // 必须写成 `fts_table MATCH ?`。
    if filter.query.is_some() {
        sql.push_str(
            " WHERE c.rowid IN (
                 SELECT rowid FROM commands_fts WHERE commands_fts MATCH ?1
                 UNION
                 SELECT rowid FROM command_outputs_fts
                 WHERE command_outputs_fts MATCH ?1
             )",
        );
    } else {
        sql.push_str(" WHERE 1=1");
    }

    let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(q) = &filter.query {
        params_vec.push(Box::new(q.clone()));
    }
    if let Some(t) = &filter.terminal_id {
        params_vec.push(Box::new(t.clone()));
        sql.push_str(&format!(" AND c.terminal_id = ?{}", params_vec.len()));
    }
    if let Some(h) = &filter.host_id {
        params_vec.push(Box::new(h.clone()));
        sql.push_str(&format!(
            " AND c.terminal_id IN (SELECT id FROM terminals WHERE host_id = ?{})",
            params_vec.len()
        ));
    }
    if let Some(s) = &filter.status {
        params_vec.push(Box::new(s.as_str().to_string()));
        sql.push_str(&format!(" AND c.status = ?{}", params_vec.len()));
    }

    sql.push_str(&format!(" ORDER BY c.created_at {order}, c.seq {order}"));
    params_vec.push(Box::new(limit));
    sql.push_str(&format!(" LIMIT ?{}", params_vec.len()));
    params_vec.push(Box::new(offset));
    sql.push_str(&format!(" OFFSET ?{}", params_vec.len()));

    let mut stmt = conn.prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|b| b.as_ref()).collect();
    let rows = stmt.query_map(refs.as_slice(), |r| row_to_record(r))?;

    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// 保留期清理结果（Q12）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct CleanupReport {
    /// 删除的命令条数。
    pub deleted: usize,
    /// 本次清理的截止时间（早于该时间的**活跃终端**历史被删除）。
    pub cutoff: String,
}

/// 按保留期清理历史（Q12 / D14）。
///
/// **只清理活跃终端的历史**——归档终端的历史永久保留，直到用户手动删除终端。
/// 清理行为需留痕：调用方应记录返回的 `CleanupReport`（D14）。
pub fn cleanup_expired(
    conn: &Connection,
    retention_hours: i64,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<CleanupReport> {
    if retention_hours <= 0 {
        // 保留期设为"永久"时不做清理。
        return Ok(CleanupReport {
            deleted: 0,
            cutoff: String::new(),
        });
    }

    let cutoff = now - chrono::Duration::hours(retention_hours);
    let cutoff_str = cutoff.to_rfc3339();

    let deleted = conn.execute(
        "DELETE FROM commands
         WHERE created_at < ?1
           AND terminal_id IN (SELECT id FROM terminals WHERE status != 'archived')",
        [&cutoff_str],
    )?;

    // 清理完成时间写入 settings，供设置页展示"上次清理"（D14）。
    crate::store::db::set_setting(conn, "history_last_cleanup_at", &now.to_rfc3339())?;
    crate::store::db::set_setting(conn, "history_last_cleanup_deleted", &deleted.to_string())?;

    Ok(CleanupReport {
        deleted,
        cutoff: cutoff_str,
    })
}

/// 统计历史占用（设置页展示用，D14）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct HistoryStats {
    pub command_count: i64,
    pub output_bytes: i64,
    pub archived_command_count: i64,
}

pub fn stats(conn: &Connection) -> Result<HistoryStats> {
    let command_count: i64 = conn.query_row("SELECT COUNT(*) FROM commands", [], |r| r.get(0))?;

    let output_bytes: i64 = conn
        .query_row(
            "SELECT COALESCE(SUM(LENGTH(output)), 0) FROM command_outputs",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    let archived_command_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM commands
         WHERE terminal_id IN (SELECT id FROM terminals WHERE status = 'archived')",
        [],
        |r| r.get(0),
    )?;

    Ok(HistoryStats {
        command_count,
        output_bytes,
        archived_command_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::command::CommandRecord;
    use crate::domain::terminal::Terminal;

    fn setup() -> Connection {
        let conn = crate::store::db::open_in_memory().unwrap();
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
        conn
    }

    #[test]
    fn insert_get_roundtrip() {
        let conn = setup();
        let mut r = CommandRecord::new("term_1", 1, "ls -la");
        insert(&conn, &r).unwrap();
        assert_eq!(get(&conn, &r.id).unwrap().command, "ls -la");

        r.status = CommandStatus::Running;
        r.started_at = Some(now_rfc3339());
        update_status(&conn, &r).unwrap();
        assert_eq!(get(&conn, &r.id).unwrap().status, CommandStatus::Running);

        r.status = CommandStatus::Completed;
        r.exit_code = Some(0);
        r.duration_ms = Some(42);
        r.finished_at = Some(now_rfc3339());
        update_status(&conn, &r).unwrap();

        let done = get(&conn, &r.id).unwrap();
        assert!(done.is_success());
        assert_eq!(done.duration_ms, Some(42));
    }

    #[test]
    fn next_seq_increments() {
        let conn = setup();
        assert_eq!(next_seq(&conn, "term_1").unwrap(), 1);
        insert(&conn, &CommandRecord::new("term_1", 1, "a")).unwrap();
        assert_eq!(next_seq(&conn, "term_1").unwrap(), 2);
        insert(&conn, &CommandRecord::new("term_1", 2, "b")).unwrap();
        assert_eq!(next_seq(&conn, "term_1").unwrap(), 3);
    }

    #[test]
    fn output_save_and_upsert() {
        let conn = setup();
        let r = CommandRecord::new("term_1", 1, "cat big");
        insert(&conn, &r).unwrap();

        save_output(&conn, &r.id, "line1\n").unwrap();
        assert_eq!(get_output(&conn, &r.id).unwrap().unwrap(), "line1\n");

        // 追加式覆盖。
        save_output(&conn, &r.id, "line1\nline2\n").unwrap();
        assert_eq!(get_output(&conn, &r.id).unwrap().unwrap(), "line1\nline2\n");
    }

    #[test]
    fn status_view_respects_tail_lines() {
        let conn = setup();
        let r = CommandRecord::new("term_1", 1, "seq 1 100");
        insert(&conn, &r).unwrap();
        let out: String = (1..=100).map(|i| format!("{i}\n")).collect();
        save_output(&conn, &r.id, &out).unwrap();

        let view = status_view(&conn, &r.id, true, Some(3)).unwrap();
        let text = view.output.unwrap();
        assert_eq!(text, "98\n99\n100");

        // 不请求输出时明确标记省略。
        let view2 = status_view(&conn, &r.id, false, None).unwrap();
        assert!(view2.output_omitted);
        assert!(view2.output.is_none());
    }

    #[test]
    fn tail_shorter_than_limit_returns_all() {
        assert_eq!(tail("a\nb", 5), "a\nb");
        assert_eq!(tail("a\nb\nc", 0), "");
    }

    #[test]
    fn pending_count_and_current_pending() {
        let conn = setup();
        let mut running = CommandRecord::new("term_1", 1, "sleep 10");
        running.status = CommandStatus::Running;
        insert(&conn, &running).unwrap();
        insert(&conn, &CommandRecord::new("term_1", 2, "queued cmd")).unwrap();

        assert_eq!(pending_count(&conn, "term_1").unwrap(), 2);
        assert_eq!(
            current_pending(&conn, "term_1").unwrap().unwrap().id,
            running.id,
            "应返回最早未完成的命令"
        );
    }

    #[test]
    fn fail_pending_marks_and_records_reason() {
        let conn = setup();
        let mut running = CommandRecord::new("term_1", 1, "long job");
        running.status = CommandStatus::Running;
        insert(&conn, &running).unwrap();

        let n = fail_pending_for_terminal(&conn, "term_1", "SSH 连接已断开").unwrap();
        assert_eq!(n, 1);

        let rec = get(&conn, &running.id).unwrap();
        assert_eq!(rec.status, CommandStatus::Failed);
        assert!(rec.finished_at.is_some());
        let out = get_output(&conn, &running.id).unwrap().unwrap();
        assert!(out.contains("SSH 连接已断开"));
    }

    #[test]
    fn history_filter_by_terminal_and_status() {
        let conn = setup();
        conn.execute(
            "INSERT INTO terminals (id, host_id, status, created_at, updated_at)
             VALUES ('term_2', 'host_1', 'active', 'now', 'now')",
            [],
        )
        .unwrap();

        let mut a = CommandRecord::new("term_1", 1, "ls");
        a.status = CommandStatus::Completed;
        a.exit_code = Some(0);
        insert(&conn, &a).unwrap();

        let mut b = CommandRecord::new("term_2", 1, "pwd");
        b.status = CommandStatus::Completed;
        b.exit_code = Some(0);
        insert(&conn, &b).unwrap();

        let by_term = search_history(
            &conn,
            &HistoryFilter {
                terminal_id: Some("term_1".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(by_term.len(), 1);
        assert_eq!(by_term[0].command, "ls");

        let by_host = search_history(
            &conn,
            &HistoryFilter {
                host_id: Some("host_1".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(by_host.len(), 2);
    }

    #[test]
    fn full_text_search_matches_command_and_output() {
        let conn = setup();
        let mut a = CommandRecord::new("term_1", 1, "systemctl restart nginx");
        a.status = CommandStatus::Completed;
        insert(&conn, &a).unwrap();
        save_output(&conn, &a.id, "Job for nginx.service completed successfully").unwrap();

        let mut b = CommandRecord::new("term_1", 2, "uptime");
        b.status = CommandStatus::Completed;
        insert(&conn, &b).unwrap();
        save_output(&conn, &b.id, "load average: 0.12").unwrap();

        // 命中命令内容。
        let hits = search_history(
            &conn,
            &HistoryFilter {
                query: Some("nginx".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, a.id);

        // 命中输出内容（命令本身不含该词）。
        let hits2 = search_history(
            &conn,
            &HistoryFilter {
                query: Some("average".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(hits2.len(), 1);
        assert_eq!(hits2[0].id, b.id);
    }

    #[test]
    fn cleanup_only_touches_active_terminals() {
        let conn = setup();
        // 归档终端与其历史
        conn.execute(
            "INSERT INTO terminals (id, host_id, status, created_at, updated_at)
             VALUES ('term_arch', 'host_1', 'archived', 'now', 'now')",
            [],
        )
        .unwrap();

        let old = "2020-01-01T00:00:00+00:00";
        for (id, term) in [("c_active", "term_1"), ("c_arch", "term_arch")] {
            conn.execute(
                "INSERT INTO commands (id, terminal_id, seq, command, status, created_at)
                 VALUES (?1, ?2, 1, 'old cmd', 'completed', ?3)",
                params![id, term, old],
            )
            .unwrap();
        }

        let now = chrono::Utc::now();
        let report = cleanup_expired(&conn, 24 * 30, now).unwrap();
        assert_eq!(report.deleted, 1, "只应删除活跃终端的过期历史");

        // 归档终端的历史必须保留（D14）。
        let arch_left: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM commands WHERE terminal_id = 'term_arch'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(arch_left, 1);

        // 清理留痕。
        let last = crate::store::db::get_setting(&conn, "history_last_cleanup_deleted").unwrap();
        assert_eq!(last.as_deref(), Some("1"));
    }

    #[test]
    fn cleanup_with_permanent_retention_does_nothing() {
        let conn = setup();
        conn.execute(
            "INSERT INTO commands (id, terminal_id, seq, command, status, created_at)
             VALUES ('c1', 'term_1', 1, 'ancient', 'completed', '2020-01-01T00:00:00+00:00')",
            [],
        )
        .unwrap();

        let report = cleanup_expired(&conn, 0, chrono::Utc::now()).unwrap();
        assert_eq!(report.deleted, 0);
        let left: i64 = conn
            .query_row("SELECT COUNT(*) FROM commands", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 1, "永久保留时不应删除任何记录");
    }

    #[test]
    fn stats_reports_counts_and_bytes() {
        let conn = setup();
        let r = CommandRecord::new("term_1", 1, "echo hi");
        insert(&conn, &r).unwrap();
        save_output(&conn, &r.id, "hi\n").unwrap();

        let s = stats(&conn).unwrap();
        assert_eq!(s.command_count, 1);
        assert_eq!(s.output_bytes, 3);
        assert_eq!(s.archived_command_count, 0);
    }

    #[test]
    fn missing_command_reports_not_found() {
        let conn = setup();
        assert!(matches!(
            get(&conn, "cmd_nope"),
            Err(AppError::CommandNotFound(_))
        ));
        assert!(matches!(
            status_view(&conn, "cmd_nope", true, None),
            Err(AppError::CommandNotFound(_))
        ));
    }
}
