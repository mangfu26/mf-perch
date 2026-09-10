//! 应用共享状态。
//!
//! 由 Tauri 前端与 MCP Server 共同持有：
//! - 前端需要读主机/凭据/终端/历史（人类侧管理界面）
//! - MCP Server 需要操作终端（AI Agent 侧）
//!
//! 两者共享同一份数据库连接与终端运行时，因此界面能实时反映
//! Agent 的操作，Agent 也能看到人类新建的主机。

use std::sync::Arc;

use tokio::sync::Mutex;

use crate::error::{AppError, Result};
use crate::store::crypto::KEY_LEN;
use crate::store::keyring::{self, KeyProvider, KeySetupState};
use crate::terminal::TerminalRuntime;

/// 运行期主密钥的持有方式。
///
/// K2（主密码）下应用启动时无法自行取得密钥，必须等用户输入，
/// 因此用 `Option` 表示"尚未解锁"。
pub struct MasterKey(Option<[u8; KEY_LEN]>);

impl MasterKey {
    /// 取主密钥；未解锁时返回明确错误，提示需要输入主密码（P1：明确报错）。
    pub fn get(&self) -> Result<&[u8; KEY_LEN]> {
        self.0.as_ref().ok_or_else(|| {
            AppError::KeyServiceUnavailable(
                "凭据尚未解锁，请先在应用中输入主密码".into(),
            )
        })
    }

    pub fn is_unlocked(&self) -> bool {
        self.0.is_some()
    }

    pub fn set(&mut self, key: [u8; KEY_LEN]) {
        self.0 = Some(key);
    }

    pub fn clear(&mut self) {
        if let Some(mut k) = self.0.take() {
            use zeroize::Zeroize;
            k.zeroize();
        }
    }
}

/// 应用共享状态。
pub struct AppState {
    /// 数据库句柄（`Arc` 以便后台任务也能持有，见 [`crate::store::Db`]）。
    pub db: crate::store::Db,
    /// 运行期主密钥（用于解密封存的凭据）。
    pub master_key: Mutex<MasterKey>,
    /// 终端运行时：会话池、串行命令队列、输出泵。
    pub terminals: Arc<TerminalRuntime>,
}

impl AppState {
    /// 打开数据库并完成密钥引导。
    ///
    /// 密钥策略（D6）：系统钥匙串可用则自动解锁（K1/K3 无感）；
    /// K2 主密码方式需用户输入，此处仅登记状态，等待解锁。
    pub fn initialize() -> Result<Arc<Self>> {
        let db_path = crate::store::default_db_path()?;
        let conn = crate::store::db::open(&db_path)?;

        let mut master_key = MasterKey(None);
        match keyring::inspect(&conn)? {
            KeySetupState::Ready(KeyProvider::Keyring) => {
                let key = keyring::load_from_keyring(&conn)?;
                master_key.set(key);
                tracing::info!("已通过系统钥匙串解锁凭据");
            }
            KeySetupState::Ready(KeyProvider::LocalFile) => {
                let key = keyring::load_from_local_file(&conn)?;
                master_key.set(key);
                tracing::info!("已通过本地密钥文件解锁凭据");
            }
            KeySetupState::Ready(KeyProvider::MasterPassword) => {
                tracing::info!("凭据由主密码保护，等待用户输入主密码解锁");
            }
            KeySetupState::NotInitialized => {
                tracing::info!("尚未初始化密钥，等待用户选择保护方式");
            }
            KeySetupState::ProviderUnavailable { provider, reason } => {
                // 关键行为（D6）：不清空数据，只报告无法解密。
                tracing::warn!(
                    provider = provider.as_str(),
                    "密钥提供方式不可用：{reason}"
                );
            }
        }

        let terminals = Arc::new(TerminalRuntime::new());
        let _ = &conn;

        // 应用启动时，上一轮运行留下的活跃终端其会话已不存在，
        // 一律标记为 broken，由 Agent 重建（D3）。
        let broken = crate::store::terminals::mark_all_broken_on_startup(&conn)?;
        if broken > 0 {
            tracing::info!("启动时标记 {broken} 个终端为连接已断开");
        }

        Ok(Arc::new(Self {
            db: std::sync::Arc::new(Mutex::new(conn)),
            master_key: Mutex::new(master_key),
            terminals,
        }))
    }

    /// 以 K2 主密码解锁。
    pub async fn unlock_with_master_password(&self, password: &str) -> Result<()> {
        let conn = self.db.lock().await;
        let key = keyring::load_with_master_password(&conn, password)?;
        self.master_key.lock().await.set(key);
        Ok(())
    }

    /// 构造一个隔离的测试状态（内存数据库 + 直接注入主密钥）。
    ///
    /// 供集成测试使用：不触碰用户真实数据目录，也不读写系统钥匙串。
    pub fn new_for_test(conn: rusqlite::Connection, key: [u8; KEY_LEN]) -> Self {
        let mut master_key = MasterKey(None);
        master_key.set(key);
        Self {
            db: std::sync::Arc::new(Mutex::new(conn)),
            master_key: Mutex::new(master_key),
            terminals: Arc::new(TerminalRuntime::new()),
        }
    }

    /// 初始化密钥保护方式（首次引导）。
    ///
    /// 钥匙串不可用时**不静默降级**，而是返回错误由前端让用户显式选择（P2）。
    pub async fn initialize_key_provider(&self, provider: KeyProvider, password: Option<&str>) -> Result<()> {
        let conn = self.db.lock().await;
        let key = match provider {
            KeyProvider::Keyring => {
                if !keyring::is_keyring_available() {
                    return Err(AppError::KeyServiceUnavailable(
                        "系统钥匙串不可用，请改选主密码或本地密钥文件".into(),
                    ));
                }
                keyring::init_keyring(&conn)?
            }
            KeyProvider::MasterPassword => {
                let pw = password.ok_or_else(|| {
                    AppError::InvalidArgument("使用主密码保护时必须提供主密码".into())
                })?;
                keyring::init_master_password(&conn, pw)?
            }
            KeyProvider::LocalFile => keyring::init_local_file(&conn)?,
        };

        self.master_key.lock().await.set(key);
        Ok(())
    }
}
