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

use std::sync::Arc;

/// 数据库句柄。
///
/// 用 `Arc<tokio::sync::Mutex<_>>` 而非裸 `Mutex`：
/// - `rusqlite::Connection` 是 `Send` 但**不是** `Sync`，不能把 `&Connection`
///   借用跨越 `.await`（那样 future 不再是 `Send`）
/// - 用 `Arc` 是为了让后台任务（如异步命令的终态回写）也能持有句柄
///
/// 访问约定：**分阶段加锁**——读信息 → 释放 → 做网络操作 → 再短暂加锁写入，
/// 避免在 SSH 连接（最长十余秒）期间把数据库锁住而拖住界面。
pub type Db = Arc<tokio::sync::Mutex<rusqlite::Connection>>;
