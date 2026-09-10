//! 认证信息仓储（D6）。
//!
//! 敏感字段（密码、私钥正文、passphrase）加密后存储；
//! 读取时解密到内存，调用方用后应清理（`zeroize`）。

use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::domain::credential::{Credential, CredentialKind, CredentialSummary};
use crate::domain::now_rfc3339;
use crate::error::{AppError, Result};
use crate::store::crypto::{self, KEY_LEN};

const COLS: &str = "id, name, username, kind, secret_enc, passphrase_enc, fingerprint, \
                    created_at, updated_at";

/// 插入认证信息（自动加密敏感字段）。
pub fn insert(conn: &Connection, cred: &Credential, key: &[u8; KEY_LEN]) -> Result<()> {
    let secret_enc = crypto::encrypt(key, &cred.secret)?;
    let passphrase_enc = match &cred.passphrase {
        Some(p) if !p.is_empty() => Some(crypto::encrypt(key, p)?),
        _ => None,
    };

    conn.execute(
        "INSERT INTO credentials
            (id, name, username, kind, secret_enc, passphrase_enc, fingerprint, created_at, updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![
            cred.id,
            cred.name,
            cred.username,
            cred.kind.as_str(),
            secret_enc,
            passphrase_enc,
            cred.fingerprint,
            cred.created_at,
            cred.updated_at,
        ],
    )?;
    Ok(())
}

/// 更新认证信息。`replace_secret = false` 时保留原密钥正文（仅改名称/用户名）。
pub fn update(
    conn: &Connection,
    cred: &Credential,
    replace_secret: bool,
    key: &[u8; KEY_LEN],
) -> Result<()> {
    let affected = if replace_secret {
        let secret_enc = crypto::encrypt(key, &cred.secret)?;
        let passphrase_enc = match &cred.passphrase {
            Some(p) if !p.is_empty() => Some(crypto::encrypt(key, p)?),
            _ => None,
        };
        conn.execute(
            "UPDATE credentials SET
                name = ?2, username = ?3, kind = ?4, secret_enc = ?5,
                passphrase_enc = ?6, fingerprint = ?7, updated_at = ?8
             WHERE id = ?1",
            params![
                cred.id,
                cred.name,
                cred.username,
                cred.kind.as_str(),
                secret_enc,
                passphrase_enc,
                cred.fingerprint,
                now_rfc3339(),
            ],
        )?
    } else {
        conn.execute(
            "UPDATE credentials SET
                name = ?2, username = ?3, updated_at = ?4
             WHERE id = ?1",
            params![cred.id, cred.name, cred.username, now_rfc3339()],
        )?
    };

    if affected == 0 {
        return Err(AppError::CredentialNotFound(cred.id.clone()));
    }
    Ok(())
}

fn row_to_credential(row: &Row<'_>) -> rusqlite::Result<(Credential, String, Option<String>)> {
    let kind_raw: String = row.get("kind")?;
    let cred = Credential {
        id: row.get("id")?,
        name: row.get("name")?,
        username: row.get("username")?,
        kind: CredentialKind::parse(&kind_raw).unwrap_or(CredentialKind::Password),
        // 密文占位，下面用解密结果替换。
        secret: String::new(),
        passphrase: None,
        fingerprint: row.get("fingerprint")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    };
    Ok((cred, row.get("secret_enc")?, row.get("passphrase_enc")?))
}

/// 读取单条并解密。
pub fn get(conn: &Connection, id: &str, key: &[u8; KEY_LEN]) -> Result<Credential> {
    let (mut cred, secret_stored, passphrase_stored) = conn
        .query_row(
            &format!("SELECT {COLS} FROM credentials WHERE id = ?1"),
            [id],
            |r| row_to_credential(r),
        )
        .optional()?
        .ok_or_else(|| AppError::CredentialNotFound(id.to_string()))?;

    cred.secret = crypto::decrypt(key, &secret_stored)?;

    if let Some(stored) = passphrase_stored {
        cred.passphrase = Some(crypto::decrypt(key, &stored)?);
    }

    Ok(cred)
}

/// 列出摘要（**不解密敏感字段**，供界面列表使用）。
pub fn list_summaries(conn: &Connection) -> Result<Vec<CredentialSummary>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM credentials ORDER BY created_at ASC"
    ))?;

    let rows = stmt.query_map([], |r| {
        let (cred, _secret, passphrase_enc) = row_to_credential(r)?;
        let mut summary = CredentialSummary::from(&cred);
        summary.has_passphrase = passphrase_enc.is_some();
        Ok(summary)
    })?;

    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

pub fn delete(conn: &Connection, id: &str) -> Result<()> {
    let affected = conn.execute("DELETE FROM credentials WHERE id = ?1", [id])?;
    if affected == 0 {
        return Err(AppError::CredentialNotFound(id.to_string()));
    }
    Ok(())
}

/// 是否存在该认证信息（供主机绑定时校验）。
pub fn exists(conn: &Connection, id: &str) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM credentials WHERE id = ?1",
        [id],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::credential::Credential;

    fn setup() -> (Connection, [u8; KEY_LEN]) {
        let conn = crate::store::db::open_in_memory().unwrap();
        (conn, crypto::generate_master_key())
    }

    #[test]
    fn insert_get_roundtrip_for_password() {
        let (conn, key) = setup();
        let mut c = Credential::new("root", CredentialKind::Password, "p@ssw0rd");
        c.name = Some("prod root".into());
        insert(&conn, &c, &key).unwrap();

        let got = get(&conn, &c.id, &key).unwrap();
        assert_eq!(got.username, "root");
        assert_eq!(got.secret, "p@ssw0rd");
        assert_eq!(got.kind, CredentialKind::Password);
        assert!(got.passphrase.is_none());
    }

    #[test]
    fn secret_is_encrypted_at_rest() {
        let (conn, key) = setup();
        let c = Credential::new("deploy", CredentialKind::Password, "TOPSECRET123");
        insert(&conn, &c, &key).unwrap();

        let raw: String = conn
            .query_row(
                "SELECT secret_enc FROM credentials WHERE id = ?1",
                [&c.id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!raw.contains("TOPSECRET123"));
        assert!(raw.starts_with("v1:"));
    }

    #[test]
    fn key_with_passphrase_roundtrip() {
        let (conn, key) = setup();
        let mut c = Credential::new(
            "git",
            CredentialKind::Key,
            "-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n-----END OPENSSH PRIVATE KEY-----",
        );
        c.passphrase = Some("key-passphrase".into());
        c.fingerprint = Some("SHA256:xyz".into());
        insert(&conn, &c, &key).unwrap();

        let got = get(&conn, &c.id, &key).unwrap();
        assert_eq!(got.kind, CredentialKind::Key);
        assert_eq!(got.passphrase.as_deref(), Some("key-passphrase"));
        assert_eq!(got.fingerprint.as_deref(), Some("SHA256:xyz"));
    }

    #[test]
    fn passphrase_is_encrypted_at_rest() {
        let (conn, key) = setup();
        let mut c = Credential::new("git", CredentialKind::Key, "keybody");
        c.passphrase = Some("PASSPHRASE-XYZ".to_string());
        insert(&conn, &c, &key).unwrap();

        let raw: Option<String> = conn
            .query_row(
                "SELECT passphrase_enc FROM credentials WHERE id = ?1",
                [&c.id],
                |r| r.get(0),
            )
            .unwrap();
        let raw_str = raw.unwrap();
        assert!(!raw_str.contains("PASSPHRASE-XYZ"));
    }

    #[test]
    fn summaries_do_not_leak_secret() {
        let (conn, key) = setup();
        let c = Credential::new("u", CredentialKind::Password, "SECRET-VALUE");
        insert(&conn, &c, &key).unwrap();

        let summaries = list_summaries(&conn).unwrap();
        assert_eq!(summaries.len(), 1);
        let debug = format!("{:?}", summaries[0]);
        assert!(!debug.contains("SECRET-VALUE"));
    }

    #[test]
    fn debug_impl_redacts_sensitive_fields() {
        // Credential 的 Debug 必须遮蔽 secret 与 passphrase（D6 / P2）。
        let mut c = Credential::new("u", CredentialKind::Key, "PRIVATE-KEY-BODY");
        c.passphrase = Some("MY-PASSPHRASE".to_string());
        let debug = format!("{c:?}");
        assert!(!debug.contains("PRIVATE-KEY-BODY"));
        assert!(!debug.contains("MY-PASSPHRASE"));
        assert!(debug.contains("redacted"));
    }

    #[test]
    fn update_without_secret_change_keeps_original() {
        let (conn, key) = setup();
        let c = Credential::new("u", CredentialKind::Password, "original-pw");
        insert(&conn, &c, &key).unwrap();

        // 仅改名称，不替换密钥正文。
        let mut edited = c.clone();
        edited.name = Some("renamed".into());
        edited.secret = String::new(); // 调用方未提供正文
        update(&conn, &edited, false, &key).unwrap();

        let got = get(&conn, &c.id, &key).unwrap();
        assert_eq!(got.name.as_deref(), Some("renamed"));
        assert_eq!(got.secret, "original-pw", "未替换时应保留原密钥正文");
    }

    #[test]
    fn update_with_secret_replaces_value() {
        let (conn, key) = setup();
        let c = Credential::new("u", CredentialKind::Password, "old-pw");
        insert(&conn, &c, &key).unwrap();

        let mut edited = c.clone();
        edited.secret = "new-pw".into();
        update(&conn, &edited, true, &key).unwrap();

        assert_eq!(get(&conn, &c.id, &key).unwrap().secret, "new-pw");
    }

    #[test]
    fn wrong_key_cannot_decrypt() {
        let (conn, key) = setup();
        let c = Credential::new("u", CredentialKind::Password, "pw");
        insert(&conn, &c, &key).unwrap();

        let other = crypto::generate_master_key();
        assert!(matches!(
            get(&conn, &c.id, &other),
            Err(AppError::CredentialUndecryptable(_))
        ));
    }

    #[test]
    fn delete_and_exists() {
        let (conn, key) = setup();
        let c = Credential::new("u", CredentialKind::Password, "pw");
        insert(&conn, &c, &key).unwrap();
        assert!(exists(&conn, &c.id).unwrap());

        delete(&conn, &c.id).unwrap();
        assert!(!exists(&conn, &c.id).unwrap());
        assert!(matches!(
            get(&conn, &c.id, &key),
            Err(AppError::CredentialNotFound(_))
        ));
    }

    #[test]
    fn unicode_username_and_secret_roundtrip() {
        let (conn, key) = setup();
        let c = Credential::new("运维", CredentialKind::Password, "密码🔐");
        insert(&conn, &c, &key).unwrap();
        let got = get(&conn, &c.id, &key).unwrap();
        assert_eq!(got.username, "运维");
        assert_eq!(got.secret, "密码🔐");
    }
}
