//! 持久化层（D6）。
//!
//! 结构：
//! - [`db`]：连接、schema 与迁移、`settings` 读写
//! - [`crypto`]：字段级 AES-256-GCM 加解密
//! - [`keyring`]：主密钥的来源与分层降级（K1 / K2 / K3）
//! - [`hosts`] / [`credentials`] / [`terminals`] / [`commands`]：各实体仓储
//!
//! **安全边界**：认证信息与 sudo 密码在数据库中始终为密文；
//! 主密钥保存在数据库之外（系统钥匙串 / 主密码派生 / 本地密钥文件）。

pub mod commands;
pub mod credentials;
pub mod crypto;
pub mod db;
pub mod hosts;
pub mod keyring;
pub mod terminals;

pub use db::{data_dir, default_db_path};
