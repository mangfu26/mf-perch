//! SSH 认证与主机密钥校验（D10）。
//!
//! 主机密钥策略为 **TOFU**：首次连接记录密钥；后续连接比对，
//! **不一致则阻止连接**并等待人类确认，绝不静默接受。

use russh::keys::PublicKey;
use russh::client;

use crate::error::{AppError, Result};

/// 主机密钥校验结果，交由调用方决定如何持久化与提示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKeyCheck {
    /// 首次连接：需要记录该密钥（TOFU）。
    Trusted {
        key: String,
        fingerprint: String,
    },
    /// 与已记录的一致，可正常连接。
    Match,
    /// 与已记录的不一致：**必须阻止连接**（D10）。
    Mismatch {
        expected_fingerprint: String,
        presented_fingerprint: String,
    },
}

/// 计算主机密钥的 SHA256 指纹（与 OpenSSH 显示格式一致）。
pub fn fingerprint(key: &PublicKey) -> String {
    use base64::engine::general_purpose::STANDARD_NO_PAD as B64_NP;
    use base64::Engine as _;
    use sha2::{Digest, Sha256};

    // `PublicKey::to_bytes()` 返回 Result，需先解包。
    let bytes: Vec<u8> = match key.to_bytes() {
        Ok(b) => b,
        Err(_) => return "SHA256:（无法序列化密钥）".to_string(),
    };
    let digest = Sha256::digest(bytes.as_slice());
    format!("SHA256:{}", B64_NP.encode(digest))
}

/// 把主机密钥序列化为可持久化的字符串。
///
/// 使用 OpenSSH 单行公钥格式（`ssh-ed25519 AAAA...`），
/// 便于人类用 `ssh-keygen -lf` 独立核对。
pub fn encode_host_key(key: &PublicKey) -> String {
    key.to_openssh().map(|k| k.to_string()).unwrap_or_default()
}

/// 依据已记录的密钥判断当前出示的密钥是否可信。
pub fn check_host_key(recorded: Option<&str>, presented: &PublicKey) -> HostKeyCheck {
    let presented_key = encode_host_key(presented);
    let presented_fp = fingerprint(presented);

    match recorded {
        None | Some("") => HostKeyCheck::Trusted {
            key: presented_key,
            fingerprint: presented_fp,
        },
        Some(known) => {
            if known == presented_key {
                HostKeyCheck::Match
            } else {
                // 无法可靠还原旧密钥的指纹时，用占位说明，
                // 但必须让人类知道"不一致"这一关键事实。
                let expected_fp = PublicKey::from_openssh(known)
                    .map(|k| fingerprint(&k))
                    .unwrap_or_else(|_| "（无法解析已记录密钥）".to_string());
                HostKeyCheck::Mismatch {
                    expected_fingerprint: expected_fp,
                    presented_fingerprint: presented_fp,
                }
            }
        }
    }
}

/// 认证方式（由已解密的凭据构造）。
///
/// 内含明文密码/私钥/口令，因此实现 [`Drop`] 在析构时清零（V20）——
/// 与 sudo 密码一样，这些副本不应在内存中长期残留。
pub enum AuthMethod {
    /// 用户名 + 密码。
    Password { username: String, password: String },
    /// 用户名 + 私钥（私钥正文与可选口令）。
    Key {
        username: String,
        private_key_pem: String,
        passphrase: Option<String>,
    },
}

impl Drop for AuthMethod {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        match self {
            Self::Password { password, .. } => password.zeroize(),
            Self::Key {
                private_key_pem,
                passphrase,
                ..
            } => {
                private_key_pem.zeroize();
                if let Some(p) = passphrase.as_mut() {
                    p.zeroize();
                }
            }
        }
    }
}

impl AuthMethod {
    pub fn username(&self) -> &str {
        match self {
            Self::Password { username, .. } | Self::Key { username, .. } => username,
        }
    }

    /// 由已解密的领域凭据构造认证方式。
    ///
    /// 判定依据是凭据自身声明的 `kind`，而不是猜测内容——
    /// 猜测私钥正文（如搜 "PRIVATE KEY"）会在 PEM 变体或含口令时出错。
    pub fn from_credential(c: &crate::domain::credential::Credential) -> Self {
        use crate::domain::credential::CredentialKind;
        match c.kind {
            CredentialKind::Password => Self::Password {
                username: c.username.clone(),
                password: c.secret.clone(),
            },
            CredentialKind::Key => Self::Key {
                username: c.username.clone(),
                private_key_pem: c.secret.clone(),
                passphrase: c.passphrase.clone(),
            },
        }
    }
}

/// 解析私钥（支持 OpenSSH 与 PEM；PPK 明确不支持，Q10）。
///
/// 返回解码后的私钥；口令错误或格式不支持时给出可读原因，
/// 而不是笼统的"认证失败"。
pub fn decode_private_key(pem: &str, passphrase: Option<&str>) -> Result<russh::keys::PrivateKey> {
    russh::keys::decode_secret_key(pem, passphrase).map_err(|e| {
        let msg = e.to_string();
        if msg.contains("PuTTY") || msg.to_lowercase().contains("ppk") {
            AppError::InvalidArgument(
                "不支持 PuTTY 的 .ppk 私钥格式，请先用 PuTTYgen 转换为 OpenSSH 格式".into(),
            )
        } else if passphrase.is_some() {
            AppError::SshAuth(format!("私钥解析失败，可能是口令不正确：{e}"))
        } else {
            AppError::SshAuth(format!(
                "私钥解析失败（支持 OpenSSH 与 PEM 格式）：{e}"
            ))
        }
    })
}

/// 把 russh 的连接错误翻译为可读原因。
///
/// 直接抛出底层错误（如 `UnknownKey`）对用户毫无帮助，
/// 因此这里区分常见成因，给出可操作的提示（P1：明确报错）。
pub fn describe_connect_error(e: &russh::Error) -> String {
    match e {
        russh::Error::UnknownKey | russh::Error::KeyChanged { .. } => {
            "服务器出示的主机密钥与已记录的不一致，连接已被阻止。\
             若确认该主机密钥确实变更，请在主机详情中核对新指纹后再连接"
                .to_string()
        }
        russh::Error::NoCommonAlgo { kind, .. } => {
            format!("加密算法协商失败（{kind:?}），可能是服务端版本过旧或配置受限")
        }
        russh::Error::ConnectionTimeout => "连接超时，请检查网络与主机地址".to_string(),
        russh::Error::IO(io) if io.kind() == std::io::ErrorKind::ConnectionRefused => {
            "目标主机的 SSH 端口拒绝连接，请确认 SSH 服务已启动且端口正确".to_string()
        }
        russh::Error::IO(io) if io.kind() == std::io::ErrorKind::TimedOut => {
            "网络连接超时，请检查主机地址与防火墙".to_string()
        }
        other => format!("SSH 连接失败：{other}"),
    }
}

/// TOFU 策略下的客户端处理器。
///
/// `recorded_host_key` 为数据库中已记录的密钥；
/// `captured` 用于把首次连接的密钥回传给调用方以便持久化。
#[derive(Clone)]
pub struct TofuHandler {
    pub recorded_host_key: Option<String>,
    /// 首次连接时捕获到的密钥（供调用方写入数据库）。
    pub captured: std::sync::Arc<std::sync::Mutex<Option<(String, String)>>>,
}

impl TofuHandler {
    pub fn new(recorded_host_key: Option<String>) -> Self {
        Self {
            recorded_host_key,
            captured: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// 取回本次连接捕获的新密钥（若有）。
    pub fn take_captured(&self) -> Option<(String, String)> {
        self.captured.lock().ok().and_then(|mut g| g.take())
    }
}

impl client::Handler for TofuHandler {
    type Error = russh::Error;

    /// 校验服务端主机密钥（D10）。
    ///
    /// russh 0.63 传入 `PublicKeyOrCertificate`：证书形式的密钥无法
    /// 与已知公钥直接比对，因此采取**保守拒绝**（fail-closed）——
    /// 只有纯公钥才进入 TOFU 流程。
    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> std::result::Result<bool, Self::Error> {
        let key = match server_public_key {
            russh::keys::PublicKeyOrCertificate::PublicKey { key, .. } => key,
            // 证书形式：本产品不管理 CA，拒绝以避免误信。
            russh::keys::PublicKeyOrCertificate::Certificate(_) => {
                tracing::warn!("服务端出示证书形式的密钥，当前不支持证书校验，已拒绝连接");
                return Ok(false);
            }
        };

        match check_host_key(self.recorded_host_key.as_deref(), key) {
            HostKeyCheck::Match => Ok(true),
            HostKeyCheck::Trusted { key, fingerprint } => {
                // 首次连接：记录以便调用方持久化（TOFU）。
                if let Ok(mut g) = self.captured.lock() {
                    *g = Some((key, fingerprint));
                }
                Ok(true)
            }
            HostKeyCheck::Mismatch { .. } => {
                // 关键安全行为：密钥变更时**拒绝连接**，由人类确认后再更新。
                Ok(false)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 两把**真实可解析**的 ed25519 测试公钥（非机密，仅用于比对分支）。
    ///
    /// 必须是真实格式：`check_host_key` 的比对与指纹计算都走
    /// `PublicKey::from_openssh`，构造出来的假 base64 无法解析，
    /// 会让测试退化成"字符串不相等"这种恒真断言。
    const ED25519_A: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIACMVRDrimjsRpnPnxWxVpK2RqCtL1k5Yb0qwgA1Na03 testA";
    const ED25519_B: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAINiIsvulZy36SSkIjC1O05QrsH5BESNWTPFIDzkZ6CZt testB";

    // 说明：无法用无效 base64 构造真实 PublicKey，故此处仅覆盖
    // 无需真实密钥的纯逻辑分支；带真实密钥的校验由集成测试覆盖。

    #[test]
    fn auth_method_username_is_accessible() {
        let pw = AuthMethod::Password {
            username: "root".into(),
            password: "secret".into(),
        };
        assert_eq!(pw.username(), "root");

        let key = AuthMethod::Key {
            username: "deploy".into(),
            private_key_pem: "pem".into(),
            passphrase: Some("pp".into()),
        };
        assert_eq!(key.username(), "deploy");
    }

    #[test]
    fn decode_private_key_reports_readable_error_for_garbage() {
        let err = decode_private_key("not a key", None).unwrap_err();
        match err {
            AppError::SshAuth(msg) => assert!(msg.contains("私钥解析失败")),
            other => panic!("期望 SshAuth，实际 {other:?}"),
        }
    }

    #[test]
    fn decode_private_key_mentions_passphrase_when_provided() {
        let err = decode_private_key("not a key", Some("wrong")).unwrap_err();
        match err {
            AppError::SshAuth(msg) => assert!(msg.contains("口令")),
            other => panic!("期望 SshAuth，实际 {other:?}"),
        }
    }

    #[test]
    fn decode_private_key_rejects_ppk_with_guidance() {
        // PuTTY PPK 头——应给出可操作的提示而非笼统失败（Q10 明确不支持 PPK）。
        let ppk = "PuTTY-User-Key-File-3: ssh-ed25519\nEncryption: none\n";
        let err = decode_private_key(ppk, None).unwrap_err();
        match err {
            AppError::InvalidArgument(msg) => {
                assert!(msg.contains("ppk") || msg.contains("PuTTY"));
            }
            // 某些版本可能归为解析失败；此时至少应是可读错误。
            AppError::SshAuth(msg) => assert!(!msg.is_empty()),
            other => panic!("期望可读错误，实际 {other:?}"),
        }
    }

    #[test]
    fn handler_starts_without_captured_key() {
        let h = TofuHandler::new(None);
        assert!(h.take_captured().is_none());
    }

    #[test]
    fn handler_take_captured_is_idempotent() {
        // 捕获一次后应被取走，避免重复持久化。
        let h = TofuHandler::new(Some(ED25519_A.to_string()));
        if let Ok(mut g) = h.captured.lock() {
            *g = Some(("key".into(), "fp".into()));
        }
        assert!(h.take_captured().is_some());
        assert!(h.take_captured().is_none());
    }

    /// TOFU 之后出示**不同的**密钥，必须判定为不一致（fail-closed，D10）。
    ///
    /// 这是主机密钥校验的核心安全属性：它直接调用 `check_host_key`，
    /// 而不是比较两个常量是否相等——后者无论实现怎么错都会通过。
    #[test]
    fn check_host_key_reports_mismatch_for_different_keys() {
        let presented = PublicKey::from_openssh(ED25519_B).expect("测试公钥 B 应可解析");
        match check_host_key(Some(ED25519_A), &presented) {
            HostKeyCheck::Mismatch {
                expected_fingerprint,
                presented_fingerprint,
            } => {
                assert!(
                    presented_fingerprint.starts_with("SHA256:"),
                    "出示密钥应给出可核对的指纹，实际：{presented_fingerprint}"
                );
                assert_ne!(
                    expected_fingerprint, presented_fingerprint,
                    "不一致的密钥必须给出不同的指纹，否则人类无法核对"
                );
            }
            other => panic!("密钥不一致时必须判定为 Mismatch（fail-closed），实际 {other:?}"),
        }
    }

    /// 与已记录密钥一致时不得误报不一致（否则正常连接会被拦下）。
    #[test]
    fn check_host_key_matches_identical_key() {
        let presented = PublicKey::from_openssh(ED25519_A).expect("测试公钥 A 应可解析");
        assert!(
            matches!(check_host_key(Some(ED25519_A), &presented), HostKeyCheck::Match),
            "同一把密钥应判定为 Match"
        );
    }

    /// 首次连接（无记录）按 TOFU 信任，并交回可持久化的密钥与指纹。
    #[test]
    fn check_host_key_trusts_on_first_use() {
        let presented = PublicKey::from_openssh(ED25519_A).expect("测试公钥 A 应可解析");
        match check_host_key(None, &presented) {
            HostKeyCheck::Trusted { key, fingerprint } => {
                assert_eq!(key, ED25519_A, "TOFU 应交回可原样持久化的 OpenSSH 公钥");
                assert!(fingerprint.starts_with("SHA256:"));
            }
            other => panic!("无记录时应按 TOFU 信任，实际 {other:?}"),
        }
    }
}
