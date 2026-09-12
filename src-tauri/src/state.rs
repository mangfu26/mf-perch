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
        // 先清理旧值再覆盖（V19）：直接赋值会让上一份密钥残留在内存中，
        // 重复解锁（换方式/重试）时旧副本无人清理。
        self.clear();
        self.0 = Some(key);
    }

    pub fn clear(&mut self) {
        if let Some(mut k) = self.0.take() {
            use zeroize::Zeroize;
            k.zeroize();
        }
    }
}

impl Drop for MasterKey {
    /// 进程内持有期间不清理；对象析构时抹掉密钥（V19）。
    fn drop(&mut self) {
        self.clear();
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

        // 同时把上一轮遗留的 queued/running 命令收尾为 failed（V9）：
        // 它们的会话已不存在，永远不会收到结束标记；
        // 否则会永久占用队列计数，并在审计里永远显示"执行中"。
        let pending =
            crate::store::commands::fail_all_pending_on_startup(&conn, "应用已重启，会话不存在")?;
        if pending > 0 {
            tracing::info!("启动时收尾 {pending} 条未完成命令");
        }

        Ok(Arc::new(Self {
            db: std::sync::Arc::new(Mutex::new(conn)),
            master_key: Mutex::new(master_key),
            terminals,
        }))
    }

    /// 确保某终端有一个**可用**的会话；已断开则按需重建（D39）。
    ///
    /// 返回 `Ok(Some(说明))` 表示本次发生了重建，调用方应把说明透出给
    /// Agent（例如 `RunOutcome.session_reconnected = true`），
    /// 因为重建后 shell 状态已重置，Agent 需要重新 `cd` / `export`。
    ///
    /// 设计取舍（D39）：
    /// - 断开多由**人类重启应用**或网络引起，不是 Agent 的意图，
    ///   因此由服务端自动处置，而不是要求 Agent 先探测再调"重连工具"；
    /// - 只有 `broken`（或状态未同步但会话已丢）才重建；归档终端一律拒绝（D20）；
    /// - 凭据未解锁时**明确报错**，不静默降级（P2）。
    pub async fn ensure_terminal_session(&self, terminal_id: &str) -> Result<Option<String>> {
        use crate::terminal::{decide_session_action, SessionAction};

        let has_live = self.terminals.has_live_session(terminal_id).await;

        let status = {
            let conn = self.db.lock().await;
            crate::store::terminals::get(&conn, terminal_id)?.status
        };

        match decide_session_action(has_live, status) {
            SessionAction::UseExisting => Ok(None),
            SessionAction::RejectArchived => {
                Err(AppError::TerminalArchived(terminal_id.to_string()))
            }
            SessionAction::Reconnect => {
                let key = {
                    let guard = self.master_key.lock().await;
                    // 未解锁时 `get()` 返回错误，这里归一为"无密钥"由下面统一报错（P1）。
                    guard.get().ok().copied().map(zeroize::Zeroizing::new)
                }
                .ok_or_else(|| {
                    AppError::KeyServiceUnavailable(
                        "凭据尚未解锁，无法重建终端会话；请先在应用中解锁".into(),
                    )
                })?;

                let (_, note) = self
                    .terminals
                    .reconnect_terminal(&self.db, &key, terminal_id)
                    .await?;
                Ok(Some(note))
            }
        }
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
    ///
    /// **拒绝重复初始化（V21）**：各分支都会生成新的密钥或 salt 并覆盖旧值，
    /// 重复调用会让既有凭据**永久无法解密**。这里显式拒绝已初始化的库，
    /// 把"破坏数据"变成"明确报错"。
    pub async fn initialize_key_provider(&self, provider: KeyProvider, password: Option<&str>) -> Result<()> {
        let conn = self.db.lock().await;

        // 已初始化的库不允许再次初始化（换密钥需要专门的迁移流程，不支持直接覆盖）。
        if !matches!(keyring::inspect(&conn)?, KeySetupState::NotInitialized) {
            return Err(AppError::InvalidArgument(
                "密钥保护方式已初始化，不能重复设置；如需更换请使用备份与迁移流程".into(),
            ));
        }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::keyring::{self, KeyProvider, KeySetupState};

    fn test_state() -> Arc<AppState> {
        let conn = crate::store::db::open_in_memory().unwrap();
        let key = crate::store::crypto::generate_master_key();
        Arc::new(AppState::new_for_test(conn, key))
    }

    #[tokio::test]
    async fn init_key_provider_rejects_second_initialization() {
        // V21：重复初始化会覆盖主密钥/salt，令既有凭据永久无法解密。
        // 必须显式拒绝，而不是悄悄破坏数据。
        let state = test_state();

        state
            .initialize_key_provider(KeyProvider::MasterPassword, Some("pw-one"))
            .await
            .expect("首次初始化应成功");

        let err = state
            .initialize_key_provider(KeyProvider::MasterPassword, Some("pw-two"))
            .await
            .expect_err("重复初始化必须被拒绝");

        let msg = err.to_string();
        assert!(
            msg.contains("已初始化"),
            "应给出明确原因，实际：{msg}"
        );

        // 原始密钥仍然有效（未被覆盖）。
        {
            let conn = state.db.lock().await;
            assert_eq!(
                keyring::inspect(&conn).unwrap(),
                KeySetupState::Ready(KeyProvider::MasterPassword)
            );
            let k = keyring::load_with_master_password(&conn, "pw-one").unwrap();
            let stored = crate::store::crypto::encrypt(&k, "secret").unwrap();
            assert_eq!(crate::store::crypto::decrypt(&k, &stored).unwrap(), "secret");
        }
    }

    #[tokio::test]
    async fn init_key_provider_works_on_fresh_db() {
        let state = test_state();
        state
            .initialize_key_provider(KeyProvider::MasterPassword, Some("pw"))
            .await
            .expect("全新库应能初始化");
    }
}
