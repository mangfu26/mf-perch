//! 主密钥管理：分层探测 + 显式降级（D6 / P1 / P2）。
//!
//! **核心约束：主密钥绝不与数据库同处。** 若密钥随库文件一起走，
//! 拿到文件即可解密，字段级加密就形同虚设。
//!
//! 分层策略（顺序探测，绝不静默降级）：
//!
//! ```text
//! 1. K1 系统钥匙串（Windows 凭据管理器 / macOS 钥匙串 / Linux Secret Service）
//!    ├─ 可用 → 使用 K1
//!    └─ 不可用 ↓
//! 2. 显式告知用户，由其选择：
//!    ├─ K2 主密码：Argon2id 派生，安全强度不降低（推荐）
//!    └─ K3 本地密钥文件：权限受限，但安全性低于钥匙串（必须标注风险）
//! ```
//!
//! 所选方式记录在数据库 `settings.key_provider`，应用据此选择解密路径。

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use zeroize::Zeroize;

use crate::error::{AppError, Result};
use crate::store::crypto::{self, KEY_LEN};
use crate::store::db;

/// 密钥提供方式，持久化在 `settings.key_provider`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyProvider {
    /// K1：系统钥匙串。
    Keyring,
    /// K2：用户主密码（Argon2id 派生）。
    MasterPassword,
    /// K3：本地密钥文件（权限保护，安全性低于钥匙串）。
    LocalFile,
}

impl KeyProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Keyring => "keyring",
            Self::MasterPassword => "master_password",
            Self::LocalFile => "local_file",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "keyring" => Some(Self::Keyring),
            "master_password" => Some(Self::MasterPassword),
            "local_file" => Some(Self::LocalFile),
            _ => None,
        }
    }

    /// 该方式的保护强度说明，用于界面展示（P2：安全降级必须显式告知）。
    pub fn security_note(self) -> &'static str {
        match self {
            Self::Keyring => "已由系统钥匙串保护",
            Self::MasterPassword => "由主密码保护（Argon2id 派生）",
            Self::LocalFile => "由本地密钥文件保护——安全性低于系统钥匙串",
        }
    }
}

/// `settings` 中记录所选密钥提供方式的键。
pub const SETTING_KEY_PROVIDER: &str = "key_provider";
/// K2 主密码派生所需的 salt。
pub const SETTING_KDF_SALT: &str = "kdf_salt";
/// 用于校验主密码是否正确（用派生密钥加密的一段已知明文）。
pub const SETTING_KDF_VERIFIER: &str = "kdf_verifier";
/// 钥匙串中的服务名与账户名。
const KEYRING_SERVICE: &str = "com.mfperch.app";
const KEYRING_ACCOUNT: &str = "master-key";

/// 用于校验主密码的固定明文（内容不重要，只要能验证派生密钥一致）。
const KDF_VERIFIER_PLAINTEXT: &str = "mf-perch-master-key-verifier-v1";

/// 探测结果：告知调用方应如何处理引导流程。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySetupState {
    /// 尚未初始化任何密钥方式，需要走引导流程。
    NotInitialized,
    /// 已初始化，可直接按记录的方式加载。
    Ready(KeyProvider),
    /// 已记录某方式，但该方式当前不可用（如钥匙串被卸载）——
    /// 必须提示用户"凭据无法解密"并提供恢复路径，**绝不清空数据**。
    ProviderUnavailable { provider: KeyProvider, reason: String },
}

/// 系统钥匙串是否可用（探测，不写入）。
///
/// Linux 上若未安装 Secret Service（gnome-keyring / KWallet 等），
/// 该探测会失败，此时应引导用户选择 K2 或 K3（P1：不假设环境存在依赖）。
pub fn is_keyring_available() -> bool {
    match keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT) {
        Ok(entry) => {
            // 尝试读取：条目不存在也算"服务可用"（可以写入）。
            match entry.get_password() {
                Ok(_) => true,
                Err(keyring::Error::NoEntry) => true,
                Err(_) => false,
            }
        }
        Err(_) => false,
    }
}

/// 查询当前密钥配置状态。
pub fn inspect(conn: &Connection) -> Result<KeySetupState> {
    let Some(raw) = db::get_setting(conn, SETTING_KEY_PROVIDER)? else {
        return Ok(KeySetupState::NotInitialized);
    };

    let Some(provider) = KeyProvider::parse(&raw) else {
        return Err(AppError::Config(format!(
            "配置中的密钥提供方式无法识别：{raw}"
        )));
    };

    match provider {
        KeyProvider::Keyring => {
            if is_keyring_available() {
                Ok(KeySetupState::Ready(provider))
            } else {
                Ok(KeySetupState::ProviderUnavailable {
                    provider,
                    reason: "系统钥匙串当前不可用（可能未安装密钥服务）".into(),
                })
            }
        }
        KeyProvider::MasterPassword => {
            // 是否有 salt 与校验值——没有则说明初始化未完成。
            let has_salt = db::get_setting(conn, SETTING_KDF_SALT)?.is_some();
            let has_verifier = db::get_setting(conn, SETTING_KDF_VERIFIER)?.is_some();
            if has_salt && has_verifier {
                Ok(KeySetupState::Ready(provider))
            } else {
                Ok(KeySetupState::ProviderUnavailable {
                    provider,
                    reason: "主密码尚未初始化完成（缺少 salt 或校验值）".into(),
                })
            }
        }
        KeyProvider::LocalFile => {
            let path = local_key_path()?;
            if path.exists() {
                Ok(KeySetupState::Ready(provider))
            } else {
                Ok(KeySetupState::ProviderUnavailable {
                    provider,
                    reason: format!("本地密钥文件不存在：{}", path.display()),
                })
            }
        }
    }
}

/// 以 K1（系统钥匙串）初始化：生成随机主密钥并存入钥匙串。
///
/// 调用前应先用 [`is_keyring_available`] 确认可用性；
/// 不可用时**不要**静默降级，应让用户显式选择 K2 或 K3（P2）。
pub fn init_keyring(conn: &Connection) -> Result<[u8; KEY_LEN]> {
    let key = crypto::generate_master_key();

    let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT)
        .map_err(|e| AppError::KeyServiceUnavailable(format!("访问系统钥匙串失败：{e}")))?;

    entry
        .set_password(&crypto::encode_key(&key))
        .map_err(|e| AppError::KeyServiceUnavailable(format!("写入系统钥匙串失败：{e}")))?;

    db::set_setting(conn, SETTING_KEY_PROVIDER, KeyProvider::Keyring.as_str())?;
    Ok(key)
}

/// 以 K2（主密码）初始化：由主密码派生密钥，并写入 salt 与校验值。
///
/// 注意：K2 下**不生成随机主密钥**，密钥完全由主密码决定，
/// 因此换机器输入同一主密码即可解密（Q31 / Q32 的迁移诉求）。
pub fn init_master_password(conn: &Connection, password: &str) -> Result<[u8; KEY_LEN]> {
    if password.is_empty() {
        return Err(AppError::InvalidArgument("主密码不能为空".into()));
    }

    let salt = {
        use rand::Rng;
        let mut s = [0u8; 16];
        rand::rng().fill_bytes(&mut s);
        s
    };

    let key = derive_key(password, &salt)?;
    let verifier = crypto::encrypt(&key, KDF_VERIFIER_PLAINTEXT)?;

    // 三项设置放在同一事务里写入（V21 附带修复）：
    // 分开写时若中途失败/进程崩溃，会留下"有 salt 无 provider"之类的
    // 半初始化状态，下次引导会生成新 salt，导致既有凭据无法解密。
    let salt_b64 = B64.encode(salt);

    conn.execute_batch("BEGIN")?;
    let write = (|| -> Result<()> {
        db::set_setting(conn, SETTING_KDF_SALT, &salt_b64)?;
        db::set_setting(conn, SETTING_KDF_VERIFIER, &verifier)?;
        db::set_setting(
            conn,
            SETTING_KEY_PROVIDER,
            KeyProvider::MasterPassword.as_str(),
        )?;
        Ok(())
    })();

    match write {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(e);
        }
    }

    // 注意：`[u8; KEY_LEN]` 是 Copy，对局部变量 zeroize 只是清理副本，
    // 返回值仍持有明文；调用方负责在存入 MasterKey 后由 ZeroizeOnDrop 清理（V19）。
    Ok(key)
}

/// 以 K2（主密码）加载密钥，并校验密码正确性。
pub fn load_with_master_password(conn: &Connection, password: &str) -> Result<[u8; KEY_LEN]> {
    let salt_b64 = db::get_setting(conn, SETTING_KDF_SALT)?
        .ok_or_else(|| AppError::Config("缺少 KDF salt，无法派生密钥".into()))?;
    let verifier = db::get_setting(conn, SETTING_KDF_VERIFIER)?
        .ok_or_else(|| AppError::Config("缺少校验值，无法验证主密码".into()))?;

    let salt = B64
        .decode(&salt_b64)
        .map_err(|e| AppError::Crypto(format!("salt 解码失败：{e}")))?;

    let mut key = derive_key(password, &salt)?;

    // 用派生密钥解密校验值：成功说明密码正确。
    let ok = crypto::decrypt(&key, &verifier)
        .map(|v| v == KDF_VERIFIER_PLAINTEXT)
        .unwrap_or(false);

    if !ok {
        key.zeroize();
        return Err(AppError::CredentialUndecryptable(
            "主密码不正确".into(),
        ));
    }

    Ok(key)
}

/// 以 K3（本地密钥文件）初始化：生成随机密钥写入受限权限的文件。
///
/// 安全性低于钥匙串，界面必须明确标注风险（P2）。
pub fn init_local_file(conn: &Connection) -> Result<[u8; KEY_LEN]> {
    let key = crypto::generate_master_key();
    let path = local_key_path()?;
    write_local_key_file(&path, &key)?;

    db::set_setting(conn, SETTING_KEY_PROVIDER, KeyProvider::LocalFile.as_str())?;
    Ok(key)
}

/// 从 K3 本地密钥文件加载。
pub fn load_from_local_file(_conn: &Connection) -> Result<[u8; KEY_LEN]> {
    let path = local_key_path()?;
    let content = std::fs::read_to_string(&path).map_err(|e| {
        AppError::KeyServiceUnavailable(format!(
            "读取本地密钥文件失败（{}）：{e}",
            path.display()
        ))
    })?;
    crypto::decode_key(content.trim())
}

/// 从 K1 系统钥匙串加载。
pub fn load_from_keyring(_conn: &Connection) -> Result<[u8; KEY_LEN]> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT)
        .map_err(|e| AppError::KeyServiceUnavailable(format!("访问系统钥匙串失败：{e}")))?;

    let encoded = entry.get_password().map_err(|e| match e {
        keyring::Error::NoEntry => AppError::KeyServiceUnavailable(
            "系统钥匙串中不存在主密钥，可能是在其他机器上初始化的".into(),
        ),
        other => AppError::KeyServiceUnavailable(format!("读取系统钥匙串失败：{other}")),
    })?;

    crypto::decode_key(&encoded)
}

/// 按 `settings` 中记录的方式加载主密钥。适用于 K1 与 K3——
/// K2 需要用户输入主密码，请改用 [`load_with_master_password`]。
pub fn load(conn: &Connection) -> Result<[u8; KEY_LEN]> {
    match inspect(conn)? {
        KeySetupState::Ready(KeyProvider::Keyring) => load_from_keyring(conn),
        KeySetupState::Ready(KeyProvider::LocalFile) => load_from_local_file(conn),
        KeySetupState::Ready(KeyProvider::MasterPassword) => Err(AppError::Config(
            "主密码方式需要用户输入主密码后加载".into(),
        )),
        KeySetupState::NotInitialized => {
            Err(AppError::Config("尚未初始化密钥，无法加载".into()))
        }
        KeySetupState::ProviderUnavailable { provider, reason } => {
            Err(AppError::KeyServiceUnavailable(format!(
                "密钥提供方式 {} 当前不可用：{reason}",
                provider.as_str()
            )))
        }
    }
}

/// 本地密钥文件路径（K3）。
///
/// **刻意放在数据库所在目录之外**（V6）：两处同目录时，用户按文档"整库备份/迁移"
/// 或该目录被云盘同步、打包外带，会把密文与主密钥一起带走，
/// 字段级加密对"数据目录被拿走"这一威胁完全失效。
///
/// 这里放到一个独立的同级目录 `mf-perch-keys/`，并在写入时收紧权限。
/// 它不随数据库一起备份，需由用户单独迁移（K3 的已知使用代价，界面已提示风险）。
fn local_key_path() -> Result<PathBuf> {
    let base = db::data_dir()?;
    let parent = base
        .parent()
        .ok_or_else(|| AppError::Config("无法确定密钥文件所在目录".into()))?;
    Ok(parent.join("mf-perch-keys").join("master.key"))
}

/// 写入本地密钥文件，并收紧权限。
///
/// 先建目录并**立即**收紧目录权限、再写文件，缩小"文件已存在但权限尚未收紧"
/// 的窗口（V6 的附带加固）；Windows 上依赖用户目录继承的 ACL。
fn write_local_key_file(path: &Path, key: &[u8; KEY_LEN]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
    }

    // 以"仅创建者可读写"的方式创建文件，避免先以宽松权限落盘再 chmod。
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(crypto::encode_key(key).as_bytes())?;
        f.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, crypto::encode_key(key))?;
    }

    Ok(())
}

/// 用 Argon2id 从主密码派生 32 字节密钥。
fn derive_key(password: &str, salt: &[u8]) -> Result<[u8; KEY_LEN]> {
    use argon2::{Algorithm, Argon2, Params, Version};

    // 参数取向：显著提高内存成本以抵抗离线爆破，同时保持桌面端可接受的派生耗时。
    let params = Params::new(64 * 1024, 3, 1, Some(KEY_LEN))
        .map_err(|e| AppError::Crypto(format!("Argon2 参数非法：{e}")))?;

    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);

    let mut out = [0u8; KEY_LEN];
    argon
        .hash_password_into(password.as_bytes(), salt, &mut out)
        .map_err(|e| AppError::Crypto(format!("主密码派生失败：{e}")))?;

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_conn() -> Connection {
        crate::store::db::open_in_memory().expect("in-memory db")
    }

    #[test]
    fn inspect_reports_not_initialized_for_fresh_db() {
        let conn = mem_conn();
        assert_eq!(inspect(&conn).unwrap(), KeySetupState::NotInitialized);
    }

    #[test]
    fn master_password_roundtrip() {
        let conn = mem_conn();
        let key = init_master_password(&conn, "hunter2-correct-horse").unwrap();
        assert_eq!(
            inspect(&conn).unwrap(),
            KeySetupState::Ready(KeyProvider::MasterPassword)
        );

        // 正确密码可派生出同一密钥。
        let again = load_with_master_password(&conn, "hunter2-correct-horse").unwrap();
        assert_eq!(key, again);

        // 加密的数据能被重新加载的密钥解开。
        let stored = crypto::encrypt(&key, "ssh-password").unwrap();
        let reloaded = load_with_master_password(&conn, "hunter2-correct-horse").unwrap();
        assert_eq!(crypto::decrypt(&reloaded, &stored).unwrap(), "ssh-password");
    }

    #[test]
    fn wrong_master_password_is_rejected() {
        let conn = mem_conn();
        init_master_password(&conn, "right-password").unwrap();
        let err = load_with_master_password(&conn, "wrong-password").unwrap_err();
        match err {
            AppError::CredentialUndecryptable(_) => {}
            other => panic!("期望 CredentialUndecryptable，实际 {other:?}"),
        }
    }

    #[test]
    fn empty_master_password_is_rejected() {
        let conn = mem_conn();
        assert!(init_master_password(&conn, "").is_err());
    }

    #[test]
    fn master_password_derivation_is_salt_dependent() {
        // 同一密码配不同 salt 必须派生出不同密钥，否则 salt 形同虚设。
        let salt_a = [1u8; 16];
        let salt_b = [2u8; 16];
        let k1 = derive_key("same-password", &salt_a).unwrap();
        let k2 = derive_key("same-password", &salt_b).unwrap();
        assert_ne!(k1, k2);

        // 同密码同 salt 必须可复现。
        let k3 = derive_key("same-password", &salt_a).unwrap();
        assert_eq!(k1, k3);
    }

    #[test]
    fn provider_string_roundtrip() {
        for p in [
            KeyProvider::Keyring,
            KeyProvider::MasterPassword,
            KeyProvider::LocalFile,
        ] {
            assert_eq!(KeyProvider::parse(p.as_str()), Some(p));
        }
        assert_eq!(KeyProvider::parse("nonsense"), None);
    }
}
