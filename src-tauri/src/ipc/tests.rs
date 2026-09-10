//! IPC 层逻辑测试。
//!
//! 命令函数签名依赖 Tauri 的 `State`，不便直接测试，
//! 因此测试其内部实现（`*_inner`）——真正的业务判断都在那里。

#![cfg(test)]

use std::sync::Arc;

use crate::domain::credential::CredentialKind;
use crate::domain::host::SudoPolicy;
use crate::ipc::{save_credential_inner, save_host_inner, CredentialInput, HostInput};
use crate::state::AppState;
use crate::store::{credentials, hosts};

fn test_state() -> (Arc<AppState>, [u8; 32]) {
    let conn = crate::store::db::open_in_memory().unwrap();
    let key = crate::store::crypto::generate_master_key();
    (Arc::new(AppState::new_for_test(conn, key)), key)
}

fn credential_input(username: &str, secret: Option<&str>) -> CredentialInput {
    CredentialInput {
        id: None,
        name: Some("test".into()),
        username: username.into(),
        kind: "password".into(),
        secret: secret.map(|s| s.to_string()),
        passphrase: None,
    }
}

fn host_input(address: &str, credential_id: Option<String>) -> HostInput {
    HostInput {
        id: None,
        name: Some("host".into()),
        address: address.into(),
        port: 22,
        credential_id,
        proxy_jump_host_id: None,
        sudo_policy: "deny".into(),
        sudo_password_source: "reuse_login".into(),
        sudo_password: None,
        shell_env_mode: "login".into(),
        init_script: None,
    }
}

#[tokio::test]
async fn save_host_rejects_unknown_credential() {
    // 悬空引用会让主机永远连不上，且原因难以排查，故必须在写入前拦截。
    let (state, _key) = test_state();
    let err = save_host_inner(&state, host_input("10.0.0.1", Some("cred_nope".into())))
        .await
        .unwrap_err();
    assert!(
        matches!(err, crate::error::AppError::CredentialNotFound(_)),
        "应拒绝不存在的凭据 id，实际：{err:?}"
    );
}

#[tokio::test]
async fn save_host_accepts_existing_credential() {
    let (state, _key) = test_state();
    let cred_id = save_credential_inner(&state, credential_input("root", Some("pw")))
        .await
        .unwrap();

    let id = save_host_inner(&state, host_input("10.0.0.1", Some(cred_id.clone())))
        .await
        .unwrap();

    let conn = state.db.lock().await;
    let host = hosts::get(&conn, &id).unwrap();
    assert_eq!(host.credential_id.as_deref(), Some(cred_id.as_str()));
    // 默认策略必须是最安全的 deny（Q33）。
    assert_eq!(host.sudo_policy, SudoPolicy::Deny);
    assert_eq!(host.address, "10.0.0.1");
}

#[tokio::test]
async fn save_host_rejects_invalid_policy_value() {
    let (state, _key) = test_state();
    let mut input = host_input("10.0.0.1", None);
    input.sudo_policy = "yolo".into();

    let err = save_host_inner(&state, input).await.unwrap_err();
    assert!(matches!(err, crate::error::AppError::InvalidArgument(_)));
}

#[tokio::test]
async fn save_host_update_preserves_sudo_password_when_blank() {
    // 关键行为：界面不回显 sudo 密码，因此"留空"必须表示保持不变，
    // 否则用户改个主机名就会把密码清掉。
    let (state, key) = test_state();

    let mut create = host_input("10.0.0.2", None);
    create.sudo_policy = "auto".into();
    create.sudo_password = Some("s3cret".into());
    let id = save_host_inner(&state, create).await.unwrap();

    // 再次保存但 password 留空。
    let mut update = host_input("10.0.0.2", None);
    update.id = Some(id.clone());
    update.name = Some("renamed".into());
    update.sudo_policy = "auto".into();
    update.sudo_password = None;
    save_host_inner(&state, update).await.unwrap();

    let conn = state.db.lock().await;
    let host = hosts::get(&conn, &id).unwrap();
    assert_eq!(host.name.as_deref(), Some("renamed"));

    let pw = hosts::get_sudo_password(&conn, &id, &key).unwrap();
    assert_eq!(
        pw.as_deref(),
        Some("s3cret"),
        "留空时不得清除既有 sudo 密码"
    );
}

#[tokio::test]
async fn save_host_update_can_replace_sudo_password() {
    let (state, key) = test_state();

    let mut create = host_input("10.0.0.3", None);
    create.sudo_policy = "ask".into();
    create.sudo_password = Some("old".into());
    let id = save_host_inner(&state, create).await.unwrap();

    let mut update = host_input("10.0.0.3", None);
    update.id = Some(id.clone());
    update.sudo_policy = "ask".into();
    update.sudo_password = Some("new".into());
    save_host_inner(&state, update).await.unwrap();

    let conn = state.db.lock().await;
    let pw = hosts::get_sudo_password(&conn, &id, &key).unwrap();
    assert_eq!(pw.as_deref(), Some("new"));
}

#[tokio::test]
async fn save_credential_requires_secret_on_create() {
    let (state, _key) = test_state();
    let err = save_credential_inner(&state, credential_input("root", None))
        .await
        .unwrap_err();
    assert!(matches!(err, crate::error::AppError::InvalidArgument(_)));
}

#[tokio::test]
async fn save_credential_update_preserves_secret_when_blank() {
    // 与 sudo 密码同理：界面不回显私钥/密码正文，留空必须保留原值。
    let (state, key) = test_state();
    let id = save_credential_inner(&state, credential_input("root", Some("original-pw")))
        .await
        .unwrap();

    let mut update = credential_input("root", None);
    update.id = Some(id.clone());
    update.name = Some("renamed".into());
    save_credential_inner(&state, update).await.unwrap();

    let conn = state.db.lock().await;
    let cred = credentials::get(&conn, &id, &key).unwrap();
    assert_eq!(cred.name.as_deref(), Some("renamed"));
    assert_eq!(cred.secret, "original-pw", "留空时不得清空已存密码");
}

#[tokio::test]
async fn save_credential_update_replaces_secret_when_provided() {
    let (state, key) = test_state();
    let id = save_credential_inner(&state, credential_input("root", Some("old-pw")))
        .await
        .unwrap();

    let mut update = credential_input("root", Some("new-pw"));
    update.id = Some(id.clone());
    save_credential_inner(&state, update).await.unwrap();

    let conn = state.db.lock().await;
    assert_eq!(credentials::get(&conn, &id, &key).unwrap().secret, "new-pw");
}

#[tokio::test]
async fn save_credential_computes_key_fingerprint() {
    // 指纹是界面唯一的密钥标识（正文不回显），必须真实计算（Q10）。
    let (state, _key) = test_state();

    // 生成一把真实测试密钥。
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("k");
    let status = std::process::Command::new("ssh-keygen")
        .args(["-t", "ed25519", "-N", "", "-f"])
        .arg(&path)
        .arg("-q")
        .status();

    if status.map(|s| !s.success()).unwrap_or(true) {
        eprintln!("跳过：环境无 ssh-keygen");
        return;
    }

    let pem = std::fs::read_to_string(&path).unwrap();
    let mut input = credential_input("git", Some(&pem));
    input.kind = "key".into();

    let id = save_credential_inner(&state, input).await.unwrap();

    let conn = state.db.lock().await;
    let summary = credentials::list_summaries(&conn)
        .unwrap()
        .into_iter()
        .find(|s| s.id == id)
        .unwrap();

    assert_eq!(summary.kind, CredentialKind::Key);
    let fp = summary.fingerprint.expect("密钥类凭据应有指纹");
    assert!(fp.starts_with("SHA256:"), "指纹格式应与 OpenSSH 一致：{fp}");
}
