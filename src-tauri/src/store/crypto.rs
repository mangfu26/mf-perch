//! 字段级加密（D6）。
//!
//! 设计要点：
//! - 算法：**AES-256-GCM**（认证加密，防篡改）
//! - 每个字段独立随机 12 字节 nonce
//! - 存储格式：`v1:<nonce_b64>:<ciphertext_b64>`，带版本号便于将来更换算法
//! - 主密钥**不存于此模块、也绝不写入数据库**，由 [`crate::store::keyring`] 提供
//!
//! 明文仅在内存中短暂存在，调用方应在使用后清理（见 `zeroize` 的使用）。

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use rand::Rng;

use crate::error::{AppError, Result};

/// 主密钥长度（AES-256）。
pub const KEY_LEN: usize = 32;
/// GCM nonce 长度。
pub const NONCE_LEN: usize = 12;
/// 当前密文格式版本。
pub const FORMAT_V1: &str = "v1";

/// 生成一个随机主密钥。仅在首次初始化密钥时调用。
pub fn generate_master_key() -> [u8; KEY_LEN] {
    let mut key = [0u8; KEY_LEN];
    rand::rng().fill_bytes(&mut key);
    key
}

/// 加密一个字段，返回可直接写入数据库的字符串。
pub fn encrypt(key: &[u8; KEY_LEN], plaintext: &str) -> Result<String> {
    let mut nonce_bytes = [0u8; NONCE_LEN];
    rand::rng().fill_bytes(&mut nonce_bytes);

    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|e| AppError::Crypto(format!("初始化加密器失败：{e}")))?;

    let nonce = Nonce::from(nonce_bytes);
    let ciphertext = cipher
        .encrypt(&nonce, plaintext.as_bytes())
        .map_err(|_| AppError::Crypto("加密失败".into()))?;

    Ok(format!(
        "{FORMAT_V1}:{}:{}",
        B64.encode(nonce_bytes),
        B64.encode(ciphertext)
    ))
}

/// 解密一个字段。
///
/// 密钥不正确、数据被篡改或格式非法时返回错误，**不返回部分明文**——
/// 认证加密保证要么完整成功、要么失败。
pub fn decrypt(key: &[u8; KEY_LEN], stored: &str) -> Result<String> {
    let (version, rest) = stored
        .split_once(':')
        .ok_or_else(|| AppError::Crypto("密文格式非法：缺少版本前缀".into()))?;

    if version != FORMAT_V1 {
        return Err(AppError::Crypto(format!("不支持的密文格式版本：{version}")));
    }

    let (nonce_b64, ct_b64) = rest
        .split_once(':')
        .ok_or_else(|| AppError::Crypto("密文格式非法：缺少 nonce 或密文".into()))?;

    let nonce_bytes: [u8; NONCE_LEN] = B64
        .decode(nonce_b64)
        .map_err(|e| AppError::Crypto(format!("nonce 解码失败：{e}")))?
        .try_into()
        .map_err(|_| AppError::Crypto("nonce 长度非法".into()))?;

    let ciphertext = B64
        .decode(ct_b64)
        .map_err(|e| AppError::Crypto(format!("密文解码失败：{e}")))?;

    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|e| AppError::Crypto(format!("初始化加密器失败：{e}")))?;

    let nonce = Nonce::from(nonce_bytes);
    let plaintext = cipher
        .decrypt(&nonce, ciphertext.as_ref())
        .map_err(|_| {
            AppError::CredentialUndecryptable("解密失败：密钥不匹配或数据已损坏".into())
        })?;

    String::from_utf8(plaintext)
        .map_err(|e| AppError::Crypto(format!("解密结果不是合法 UTF-8：{e}")))
}

/// 加密一个内部机密（如 MCP Bearer Token），**无主密钥时优雅降级**。
///
/// 与 [`encrypt`] 的区别：这里用于"缺少主密钥也不能让功能不可用"的场景。
/// 例如 K2（主密码）模式下，MCP 端点需要 Token 才能工作，
/// 而此时用户可能尚未输入主密码；若强制加密，MCP 将无法启动。
///
/// 降级行为（**必须显式记录，不得静默**）：
/// - `key` 为 `Some`：返回 `enc:v1:...` 密文；
/// - `key` 为 `None`：返回 `plain:v1:...` 明文，调用方应视需要提示用户。
///
/// 读取见 [`decrypt_internal`]；它同时兼容历史遗留的**无前缀明文**。
pub fn encrypt_internal(key: Option<&[u8; KEY_LEN]>, plaintext: &str) -> Result<String> {
    match key {
        Some(k) => Ok(format!("enc:{}", encrypt(k, plaintext)?)),
        None => Ok(format!("plain:{FORMAT_V1}:{plaintext}")),
    }
}

/// 解密由 [`encrypt_internal`] 写出的内部机密。
///
/// 兼容三种形态：
/// - `enc:v1:<nonce>:<ct>` → 用主密钥解密；
/// - `plain:v1:<text>`     → 明文（写入时无主密钥）；
/// - 其它（历史遗留的值）   → 视为明文原样返回。
pub fn decrypt_internal(key: Option<&[u8; KEY_LEN]>, stored: &str) -> Result<String> {
    if let Some(rest) = stored.strip_prefix("plain:") {
        // 去掉明文前缀里的版本段。
        return Ok(rest
            .split_once(':')
            .map(|(_, t)| t)
            .unwrap_or(rest)
            .to_string());
    }

    if let Some(rest) = stored.strip_prefix("enc:") {
        let k = key.ok_or_else(|| {
            AppError::KeyServiceUnavailable("该数据已加密，需要先解锁凭据".into())
        })?;
        return decrypt(k, rest);
    }

    // 历史遗留：早期版本直接存明文，无前缀。
    Ok(stored.to_string())
}

/// 把主密钥编码为可导出/导入的字符串（供二期"导出加密备份"使用）。
pub fn encode_key(key: &[u8; KEY_LEN]) -> String {
    B64.encode(key)
}

/// 从字符串解析主密钥。
pub fn decode_key(encoded: &str) -> Result<[u8; KEY_LEN]> {
    let bytes = B64
        .decode(encoded.trim())
        .map_err(|e| AppError::Crypto(format!("主密钥解码失败：{e}")))?;
    if bytes.len() != KEY_LEN {
        return Err(AppError::Crypto(format!(
            "主密钥长度非法：期望 {KEY_LEN} 字节，实际 {} 字节",
            bytes.len()
        )));
    }
    let mut key = [0u8; KEY_LEN];
    key.copy_from_slice(&bytes);
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let key = generate_master_key();
        let plaintext = "correct horse battery staple";
        let stored = encrypt(&key, plaintext).unwrap();
        assert!(stored.starts_with("v1:"));
        // 密文中不应出现明文片段。
        assert!(!stored.contains("horse"));
        assert_eq!(decrypt(&key, &stored).unwrap(), plaintext);
    }

    #[test]
    fn same_plaintext_produces_different_ciphertext() {
        // 每次加密使用独立随机 nonce，因此相同明文密文不同。
        let key = generate_master_key();
        let a = encrypt(&key, "secret").unwrap();
        let b = encrypt(&key, "secret").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn wrong_key_fails_without_partial_plaintext() {
        let key1 = generate_master_key();
        let key2 = generate_master_key();
        let stored = encrypt(&key1, "top secret").unwrap();
        let err = decrypt(&key2, &stored).unwrap_err();
        match err {
            AppError::CredentialUndecryptable(_) => {}
            other => panic!("期望 CredentialUndecryptable，实际 {other:?}"),
        }
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let key = generate_master_key();
        let stored = encrypt(&key, "integrity matters").unwrap();
        // 改动密文末尾若干字符，认证标签应校验失败。
        let mut tampered = stored.clone();
        let last = tampered.pop().unwrap();
        tampered.push(if last == 'A' { 'B' } else { 'A' });
        assert!(decrypt(&key, &tampered).is_err());
    }

    #[test]
    fn malformed_input_is_rejected() {
        let key = generate_master_key();
        assert!(decrypt(&key, "not-a-ciphertext").is_err());
        assert!(decrypt(&key, "v9:AAAA:BBBB").is_err());
        assert!(decrypt(&key, "v1:AAAA").is_err());
    }

    #[test]
    fn unicode_roundtrip() {
        let key = generate_master_key();
        let plaintext = "中文口令与 emoji 🔐 都应正确往返";
        let stored = encrypt(&key, plaintext).unwrap();
        assert_eq!(decrypt(&key, &stored).unwrap(), plaintext);
    }

    #[test]
    fn key_encode_decode_roundtrip() {
        let key = generate_master_key();
        let encoded = encode_key(&key);
        assert_eq!(decode_key(&encoded).unwrap(), key);
        assert!(decode_key("tooshort").is_err());
    }
}
