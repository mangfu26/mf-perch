//! 主机仓储：数据库读写与领域模型互转。
//!
//! 注意：主机的 sudo 密码同样是敏感字段，加密后存 `sudo_password_enc`（D6 / D11）。

use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::domain::credential::CredentialSummary;
use crate::domain::host::{
    Host, HostPublicInfo, HostSummary, ShellEnvMode, SudoPasswordSource, SudoPolicy,
};
use crate::domain::{new_id, now_rfc3339};
use crate::error::{AppError, Result};
use crate::store::crypto::{self, KEY_LEN};

const HOST_COLUMNS: &str = "id, name, address, port, credential_id, proxy_jump_host_id, \
     sudo_policy, sudo_password_source, sudo_password_enc, shell_env_mode, init_script, \
     host_key, host_key_fingerprint, created_at, updated_at";

/// 把库中的 `i64` 端口转为 `u16`，越界即报错（V23）。
///
/// 直接 `as u16` 会把 70000 静默截断成 4464，导致连到**错误的端口**却毫无提示。
/// 正常写入路径受 `u16` 类型约束不会越界，但库被手工改写或跨版本迁移时可能出现。
fn port_from_db(v: i64) -> rusqlite::Result<u16> {
    u16::try_from(v).map_err(|_| {
        rusqlite::Error::IntegralValueOutOfRange(
            0,
            // 复用 rusqlite 的错误形态，便于上层报出明确原因。
            v,
        )
    })
}

/// 把库中的计数转为 `u32`，超出取上限而非静默回绕（V23）。
fn count_from_db(v: i64) -> u32 {
    v.clamp(0, u32::MAX as i64) as u32
}

fn row_to_host(row: &Row<'_>) -> rusqlite::Result<Host> {
    let sudo_policy_raw: String = row.get("sudo_policy")?;
    let sudo_source_raw: String = row.get("sudo_password_source")?;
    let shell_env_raw: String = row.get("shell_env_mode")?;

    Ok(Host {
        id: row.get("id")?,
        name: row.get("name")?,
        address: row.get("address")?,
        port: port_from_db(row.get::<_, i64>("port")?)?,
        credential_id: row.get("credential_id")?,
        proxy_jump_host_id: row.get("proxy_jump_host_id")?,
        sudo_policy: SudoPolicy::parse(&sudo_policy_raw).unwrap_or_default(),
        sudo_password_source: SudoPasswordSource::parse(&sudo_source_raw).unwrap_or_default(),
        shell_env_mode: ShellEnvMode::parse(&shell_env_raw).unwrap_or_default(),
        init_script: row.get("init_script")?,
        host_key: row.get("host_key")?,
        host_key_fingerprint: row.get("host_key_fingerprint")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

/// 插入主机。`sudo_password` 若提供则加密后写入。
pub fn insert(conn: &Connection, host: &Host, sudo_password: Option<&str>, key: &[u8; KEY_LEN]) -> Result<()> {
    let sudo_enc = match sudo_password {
        Some(pw) if !pw.is_empty() => Some(crypto::encrypt(key, pw)?),
        _ => None,
    };

    conn.execute(
        "INSERT INTO hosts (
            id, name, address, port, credential_id, proxy_jump_host_id,
            sudo_policy, sudo_password_source, sudo_password_enc,
            shell_env_mode, init_script, host_key, host_key_fingerprint,
            created_at, updated_at
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
        params![
            host.id,
            host.name,
            host.address,
            host.port as i64,
            host.credential_id,
            host.proxy_jump_host_id,
            host.sudo_policy.as_str(),
            host.sudo_password_source.as_str(),
            sudo_enc,
            host.shell_env_mode.as_str(),
            host.init_script,
            host.host_key,
            host.host_key_fingerprint,
            host.created_at,
            host.updated_at,
        ],
    )?;
    Ok(())
}

/// 更新主机的可变字段（不改密码；密码用 [`set_sudo_password`]）。
pub fn update(conn: &Connection, host: &Host) -> Result<()> {
    let affected = conn.execute(
        "UPDATE hosts SET
            name = ?2, address = ?3, port = ?4, credential_id = ?5,
            proxy_jump_host_id = ?6, sudo_policy = ?7, sudo_password_source = ?8,
            shell_env_mode = ?9, init_script = ?10, host_key = ?11,
            host_key_fingerprint = ?12, updated_at = ?13
         WHERE id = ?1",
        params![
            host.id,
            host.name,
            host.address,
            host.port as i64,
            host.credential_id,
            host.proxy_jump_host_id,
            host.sudo_policy.as_str(),
            host.sudo_password_source.as_str(),
            host.shell_env_mode.as_str(),
            host.init_script,
            host.host_key,
            host.host_key_fingerprint,
            now_rfc3339(),
        ],
    )?;

    if affected == 0 {
        return Err(AppError::HostNotFound(host.id.clone()));
    }
    Ok(())
}

/// 设置或清除某主机的 sudo 密码（加密存储）。
pub fn set_sudo_password(
    conn: &Connection,
    host_id: &str,
    password: Option<&str>,
    key: &[u8; KEY_LEN],
) -> Result<()> {
    let enc = match password {
        Some(pw) if !pw.is_empty() => Some(crypto::encrypt(key, pw)?),
        _ => None,
    };
    let affected = conn.execute(
        "UPDATE hosts SET sudo_password_enc = ?2, updated_at = ?3 WHERE id = ?1",
        params![host_id, enc, now_rfc3339()],
    )?;
    if affected == 0 {
        return Err(AppError::HostNotFound(host_id.to_string()));
    }
    Ok(())
}

/// 读取并解密某主机的 sudo 密码。
pub fn get_sudo_password(
    conn: &Connection,
    host_id: &str,
    key: &[u8; KEY_LEN],
) -> Result<Option<String>> {
    let enc: Option<String> = conn
        .query_row(
            "SELECT sudo_password_enc FROM hosts WHERE id = ?1",
            [host_id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();

    match enc {
        None => Ok(None),
        Some(stored) => Ok(Some(crypto::decrypt(key, &stored)?)),
    }
}

pub fn get(conn: &Connection, id: &str) -> Result<Host> {
    conn.query_row(
        &format!("SELECT {HOST_COLUMNS} FROM hosts WHERE id = ?1"),
        [id],
        |r| row_to_host(r),
    )
    .optional()?
    .ok_or_else(|| AppError::HostNotFound(id.to_string()))
}

pub fn list(conn: &Connection) -> Result<Vec<Host>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {HOST_COLUMNS} FROM hosts ORDER BY created_at ASC"
    ))?;
    let rows = stmt.query_map([], |r| row_to_host(r))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// 删除主机。终端的命令历史经外键级联一并删除（Q19）。
pub fn delete(conn: &Connection, id: &str) -> Result<()> {
    let affected = conn.execute("DELETE FROM hosts WHERE id = ?1", [id])?;
    if affected == 0 {
        return Err(AppError::HostNotFound(id.to_string()));
    }
    Ok(())
}

/// 更新 TOFU 信任的主机密钥（D10）。
pub fn set_host_key(
    conn: &Connection,
    id: &str,
    host_key: &str,
    fingerprint: &str,
) -> Result<()> {
    let affected = conn.execute(
        "UPDATE hosts SET host_key = ?2, host_key_fingerprint = ?3, updated_at = ?4 WHERE id = ?1",
        params![id, host_key, fingerprint, now_rfc3339()],
    )?;
    if affected == 0 {
        return Err(AppError::HostNotFound(id.to_string()));
    }
    Ok(())
}

/// 列出供人类界面展示的主机摘要（含终端计数）。
pub fn list_summaries(conn: &Connection) -> Result<Vec<HostSummary>> {
    let mut stmt = conn.prepare(
        "SELECT h.id, h.name, h.address, h.port, h.credential_id, h.sudo_policy,
                COALESCE(SUM(CASE WHEN t.status != 'archived' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN t.status = 'archived' THEN 1 ELSE 0 END), 0)
         FROM hosts h
         LEFT JOIN terminals t ON t.host_id = h.id
         GROUP BY h.id
         ORDER BY h.created_at ASC",
    )?;

    let rows = stmt.query_map([], |r| {
        let policy_raw: String = r.get(5)?;
        Ok(HostSummary {
            id: r.get(0)?,
            name: r.get(1)?,
            address: r.get(2)?,
            port: port_from_db(r.get::<_, i64>(3)?)?,
            has_credential: r.get::<_, Option<String>>(4)?.is_some(),
            sudo_policy: SudoPolicy::parse(&policy_raw).unwrap_or_default(),
            active_terminals: count_from_db(r.get::<_, i64>(6)?),
            archived_terminals: count_from_db(r.get::<_, i64>(7)?),
        })
    })?;

    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// 列出供 AI Agent 使用的主机信息（**不含凭据等敏感字段**）。
///
/// 只返回对 Agent 可见的主机：凭据未配置的主机标记为 `ready = false`，
/// 使 Agent 能明确知道"这台机器还不能用"，而不是拿到一个含糊的失败。
pub fn list_public(conn: &Connection) -> Result<Vec<HostPublicInfo>> {
    Ok(list(conn)?.iter().map(HostPublicInfo::from).collect())
}

/// 统计引用某认证信息的主机名（用于删除前的提示）。
pub fn hosts_using_credential(conn: &Connection, credential_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(name, address) FROM hosts WHERE credential_id = ?1 ORDER BY created_at",
    )?;
    let rows = stmt.query_map([credential_id], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// 供认证信息列表展示"被哪些主机引用"。
pub fn fill_credential_usage(
    conn: &Connection,
    summaries: &mut [CredentialSummary],
) -> Result<()> {
    for s in summaries.iter_mut() {
        s.used_by_hosts = hosts_using_credential(conn, &s.id)?;
    }
    Ok(())
}

/// 为测试与内部使用构造一个最小主机。
pub fn make_host(address: &str, port: u16, credential_id: Option<String>) -> Host {
    let now = now_rfc3339();
    Host {
        id: new_id("host"),
        name: None,
        address: address.to_string(),
        port,
        credential_id,
        proxy_jump_host_id: None,
        sudo_policy: SudoPolicy::Deny,
        sudo_password_source: SudoPasswordSource::ReuseLogin,
        shell_env_mode: ShellEnvMode::LoginThenTask,
        init_script: None,
        host_key: None,
        host_key_fingerprint: None,
        created_at: now.clone(),
        updated_at: now,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::credential::CredentialKind;

    fn setup() -> (Connection, [u8; KEY_LEN]) {
        let conn = crate::store::db::open_in_memory().unwrap();
        let key = crypto::generate_master_key();
        (conn, key)
    }

    fn sample_credential(conn: &Connection) -> String {
        let id = new_id("cred");
        conn.execute(
            "INSERT INTO credentials (id, name, username, kind, secret_enc, created_at, updated_at)
             VALUES (?1, 'key1', 'root', 'password', X'00', 'now', 'now')",
            [&id],
        )
        .unwrap();
        id
    }

    #[test]
    fn insert_get_list_delete_roundtrip() {
        let (conn, key) = setup();
        let cred = sample_credential(&conn);
        let mut host = make_host("10.0.0.5", 22, Some(cred));
        host.name = Some("prod".into());
        insert(&conn, &host, None, &key).unwrap();

        let got = get(&conn, &host.id).unwrap();
        assert_eq!(got.address, "10.0.0.5");
        assert_eq!(got.name.as_deref(), Some("prod"));
        assert_eq!(got.port, 22);
        assert_eq!(got.sudo_policy, SudoPolicy::Deny);

        assert_eq!(list(&conn).unwrap().len(), 1);

        delete(&conn, &host.id).unwrap();
        assert!(matches!(
            get(&conn, &host.id),
            Err(AppError::HostNotFound(_))
        ));
    }

    #[test]
    fn missing_host_reports_not_found() {
        let (conn, _key) = setup();
        assert!(matches!(
            get(&conn, "host_nope"),
            Err(AppError::HostNotFound(_))
        ));
    }

    #[test]
    fn sudo_password_is_encrypted_at_rest() {
        let (conn, key) = setup();
        let host = make_host("10.0.0.6", 22, None);
        insert(&conn, &host, Some("sup3r-s3cret"), &key).unwrap();

        // 数据库中不应出现明文。
        let raw: String = conn
            .query_row(
                "SELECT sudo_password_enc FROM hosts WHERE id = ?1",
                [&host.id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!raw.contains("sup3r-s3cret"));
        assert!(raw.starts_with("v1:"));

        // 解密后应还原。
        let plain = get_sudo_password(&conn, &host.id, &key).unwrap();
        assert_eq!(plain.as_deref(), Some("sup3r-s3cret"));
    }

    #[test]
    fn sudo_password_with_wrong_key_fails() {
        let (conn, key) = setup();
        let host = make_host("10.0.0.7", 22, None);
        insert(&conn, &host, Some("pw"), &key).unwrap();

        let other = crypto::generate_master_key();
        assert!(get_sudo_password(&conn, &host.id, &other).is_err());
    }

    #[test]
    fn missing_sudo_password_returns_none() {
        let (conn, key) = setup();
        let host = make_host("10.0.0.8", 22, None);
        insert(&conn, &host, None, &key).unwrap();
        assert!(get_sudo_password(&conn, &host.id, &key).unwrap().is_none());
    }

    #[test]
    fn public_info_hides_credential_details() {
        let (conn, key) = setup();
        let cred = sample_credential(&conn);
        let host = make_host("example.com", 2222, Some(cred.clone()));
        insert(&conn, &host, None, &key).unwrap();

        let public = list_public(&conn).unwrap();
        assert_eq!(public.len(), 1);
        assert!(public[0].ready);
        assert_eq!(public[0].address, "example.com");
        assert_eq!(public[0].port, 2222);

        // 未配置凭据的主机应标记为未就绪。
        let host2 = make_host("noconfig.local", 22, None);
        insert(&conn, &host2, None, &key).unwrap();
        let public2 = list_public(&conn).unwrap();
        let h2 = public2.iter().find(|h| h.address == "noconfig.local").unwrap();
        assert!(!h2.ready);
    }

    #[test]
    fn summaries_count_terminals_by_status() {
        let (conn, key) = setup();
        let host = make_host("10.0.0.9", 22, None);
        insert(&conn, &host, None, &key).unwrap();

        for (id, status) in [
            ("t1", "active"),
            ("t2", "active"),
            ("t3", "archived"),
            ("t4", "broken"),
        ] {
            conn.execute(
                "INSERT INTO terminals (id, host_id, status, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 'now', 'now')",
                params![id, host.id, status],
            )
            .unwrap();
        }

        let summaries = list_summaries(&conn).unwrap();
        assert_eq!(summaries.len(), 1);
        // 配额只看未归档终端：active(2) + broken(1) = 3
        assert_eq!(summaries[0].active_terminals, 3);
        assert_eq!(summaries[0].archived_terminals, 1);
    }

    #[test]
    fn host_key_update_persists() {
        let (conn, key) = setup();
        let host = make_host("10.0.0.10", 22, None);
        insert(&conn, &host, None, &key).unwrap();

        set_host_key(&conn, &host.id, "ssh-ed25519 AAAA...", "SHA256:abc123").unwrap();
        let got = get(&conn, &host.id).unwrap();
        assert_eq!(got.host_key.as_deref(), Some("ssh-ed25519 AAAA..."));
        assert_eq!(got.host_key_fingerprint.as_deref(), Some("SHA256:abc123"));
    }

    #[test]
    fn deleting_credential_nulls_host_reference() {
        let (conn, key) = setup();
        let cred = sample_credential(&conn);
        let host = make_host("10.0.0.11", 22, Some(cred.clone()));
        insert(&conn, &host, None, &key).unwrap();

        conn.execute("DELETE FROM credentials WHERE id = ?1", [&cred])
            .unwrap();

        // ON DELETE SET NULL：主机保留，但凭据引用被清空。
        let got = get(&conn, &host.id).unwrap();
        assert!(got.credential_id.is_none());
    }

    #[test]
    fn credential_usage_reports_host_names() {
        let (conn, key) = setup();
        let cred = sample_credential(&conn);
        let mut h1 = make_host("a.example", 22, Some(cred.clone()));
        h1.name = Some("alpha".into());
        insert(&conn, &h1, None, &key).unwrap();

        let mut h2 = make_host("b.example", 22, Some(cred.clone()));
        h2.name = Some("beta".into());
        insert(&conn, &h2, None, &key).unwrap();

        let used = hosts_using_credential(&conn, &cred).unwrap();
        assert_eq!(used, vec!["alpha".to_string(), "beta".to_string()]);

        let mut summaries = vec![CredentialSummary {
            id: cred.clone(),
            name: None,
            username: "root".into(),
            kind: CredentialKind::Password,
            fingerprint: None,
            has_passphrase: false,
            used_by_hosts: Vec::new(),
            created_at: "now".into(),
            updated_at: "now".into(),
        }];
        fill_credential_usage(&conn, &mut summaries).unwrap();
        assert_eq!(summaries[0].used_by_hosts.len(), 2);
    }
}
