//! MCP Server 端点管理（D1 / D2）。
//!
//! 传输方式**仅 Streamable HTTP**（D1）：不做 stdio，因为客户接受用
//! 第三方转换工具桥接不支持 HTTP 的客户端，这样凭据与连接池天然留在应用进程内。
//!
//! 监听与端口策略（Q2）：
//! - 默认仅监听 `127.0.0.1`；开启"允许远程连接"后监听 `0.0.0.0`
//! - 首次启动从 `50001` 起递增寻找可用端口，成功后**持久化**
//! - 下次启动优先用持久化端口；若被占用则**重新从 50001 递增**寻找
//! - Bearer Token 鉴权，Token 持久化在 `settings` 中

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::error::{AppError, Result};
use crate::store::crypto::{self, KEY_LEN};
use crate::store::db;

/// 端口起始值与上限（Q2：从 50001 开始迭代）。
pub const PORT_RANGE_START: u16 = 50001;
/// 扫描上限；到顶说明端口极度紧张，明确报错比无限扫描更合适。
pub const PORT_RANGE_END: u16 = 50100;

/// `settings` 中持久化端口的键（Q2：首个成功端口持久化）。
pub const SETTING_PORT: &str = "mcp_port";
/// `settings` 中持久化 Token 的键。
pub const SETTING_TOKEN: &str = "mcp_token";
/// `settings` 中"允许远程连接"的开关。
pub const SETTING_ALLOW_REMOTE: &str = "mcp_allow_remote";
/// `settings` 中"是否在启动时自动运行"的开关。
pub const SETTING_AUTO_START: &str = "mcp_auto_start";

/// 正在运行的 MCP 端点。
pub struct RunningEndpoint {
    pub addr: SocketAddr,
    pub token: String,
    cancellation: CancellationToken,
}

impl RunningEndpoint {
    /// 停止端点并等待其释放端口。
    pub async fn stop(&self) {
        self.cancellation.cancel();
        // 给监听循环一点时间真正退出，避免立即重启时端口仍被占用。
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    }
}

/// 生成或读取访问令牌。
///
/// Token 一旦生成即持久化，避免每次启动都要求用户更新客户端配置（Q2）。
/// 读取访问令牌；不存在时生成并持久化。
///
/// 令牌**不透明**：可能以 `enc:`（已加密）或 `plain:`（无主密钥时降级）或
/// 历史明文形态存储，统一由 [`crypto::decrypt_internal`] 还原（V16）。
///
/// `key` 为当前主密钥：有则加密落库，没有则明文落库（K2 未解锁时 MCP 仍需可用）。
pub fn ensure_token(
    conn: &rusqlite::Connection,
    key: Option<&[u8; KEY_LEN]>,
) -> Result<String> {
    if let Some(stored) = db::get_setting(conn, SETTING_TOKEN)? {
        if !stored.is_empty() {
            return crypto::decrypt_internal(key, &stored);
        }
    }

    use rand::Rng;
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    let token = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();

    let stored = crypto::encrypt_internal(key, &token)?;
    db::set_setting(conn, SETTING_TOKEN, &stored)?;
    Ok(token)
}

/// 读取已存在的令牌（不解密生成新值）；用于状态展示。
///
/// 返回 `None` 表示尚未生成过。
pub fn existing_token(
    conn: &rusqlite::Connection,
    key: Option<&[u8; KEY_LEN]>,
) -> Result<Option<String>> {
    match db::get_setting(conn, SETTING_TOKEN)? {
        Some(s) if !s.is_empty() => Ok(Some(crypto::decrypt_internal(key, &s)?)),
        _ => Ok(None),
    }
}

/// 重新生成令牌（旧 Token 即刻失效）。
pub fn regenerate_token(
    conn: &rusqlite::Connection,
    key: Option<&[u8; KEY_LEN]>,
) -> Result<String> {
    db::delete_setting(conn, SETTING_TOKEN)?;
    ensure_token(conn, key)
}

/// 是否允许远程连接。
pub fn allow_remote(conn: &rusqlite::Connection) -> Result<bool> {
    Ok(db::get_setting(conn, SETTING_ALLOW_REMOTE)?
        .map(|v| v == "true")
        .unwrap_or(false))
}

pub fn set_allow_remote(conn: &rusqlite::Connection, allow: bool) -> Result<()> {
    db::set_setting(conn, SETTING_ALLOW_REMOTE, if allow { "true" } else { "false" })
}

/// 是否随应用启动自动运行 MCP Server。
pub fn auto_start(conn: &rusqlite::Connection) -> Result<bool> {
    Ok(db::get_setting(conn, SETTING_AUTO_START)?
        .map(|v| v != "false")
        .unwrap_or(true))
}

pub fn set_auto_start(conn: &rusqlite::Connection, enabled: bool) -> Result<()> {
    db::set_setting(
        conn,
        SETTING_AUTO_START,
        if enabled { "true" } else { "false" },
    )
}

/// 按 Q2 的三步走策略选择端口。
///
/// 1. 优先尝试已持久化的端口；
/// 2. 被占用则**重新从 50001 起**递增寻找；
/// 3. 找到后由调用方持久化（本函数不接触数据库，避免持锁跨越 await）。
///
/// 返回 `(监听器, 是否为持久化端口)`：调用方据此决定是否需要写回配置。
pub async fn select_port(allow_remote: bool) -> Result<(TcpListener, u16)> {
    let ip = if allow_remote {
        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    } else {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    };

    for port in PORT_RANGE_START..=PORT_RANGE_END {
        if let Ok(listener) = TcpListener::bind(SocketAddr::new(ip, port)).await {
            tracing::info!("MCP 端点监听端口 {port}");
            return Ok((listener, port));
        }
    }

    Err(AppError::Mcp(format!(
        "在 {PORT_RANGE_START}–{PORT_RANGE_END} 范围内找不到可用端口，请释放端口后重试"
    )))
}

/// 按 Q2 策略选端口，并优先复用已持久化的端口。
///
/// 该函数不持数据库锁跨越 await：先在锁内读取偏好端口，再在锁外绑定。
pub async fn select_port_with_preference(
    preferred: Option<u16>,
    allow_remote: bool,
) -> Result<(TcpListener, u16)> {
    let ip = if allow_remote {
        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    } else {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    };

    // 第一步：尝试持久化的端口。
    if let Some(port) = preferred {
        if let Ok(listener) = TcpListener::bind(SocketAddr::new(ip, port)).await {
            tracing::info!("MCP 端点复用已持久化的端口 {port}");
            return Ok((listener, port));
        }
        tracing::info!("持久化端口 {port} 已被占用，重新从 {PORT_RANGE_START} 开始查找");
    }

    select_port(allow_remote).await
}

/// 创建一个可取消的端点句柄。
pub fn endpoint(addr: SocketAddr, token: String) -> Arc<RunningEndpoint> {
    Arc::new(RunningEndpoint {
        addr,
        token,
        cancellation: CancellationToken::new(),
    })
}

/// 供运行循环获取取消令牌。
pub fn cancellation_of(ep: &RunningEndpoint) -> CancellationToken {
    ep.cancellation.clone()
}

/// 构造供 MCP 客户端配置的接入地址。
pub fn endpoint_url(port: u16, allow_remote: bool) -> String {
    let host = if allow_remote { "0.0.0.0" } else { "127.0.0.1" };
    format!("http://{host}:{port}/mcp")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 端口扫描类测试必须串行执行。
    ///
    /// `select_port` 总是从 `PORT_RANGE_START` 开始扫描，若两个测试并行运行，
    /// 会互相占用对方期望的端口，导致结果不确定（并非产品缺陷）。
    static PORT_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn mem_conn() -> rusqlite::Connection {
        db::open_in_memory().unwrap()
    }

    #[test]
    fn token_is_generated_and_persisted() {
        let conn = mem_conn();
        let t1 = ensure_token(&conn, None).unwrap();
        assert_eq!(t1.len(), 64, "32 字节转十六进制应为 64 字符");
        assert!(t1.chars().all(|c| c.is_ascii_hexdigit()));

        // 再次读取应返回同一个 Token（避免客户端配置失效）。
        let t2 = ensure_token(&conn, None).unwrap();
        assert_eq!(t1, t2);
    }

    #[test]
    fn token_is_encrypted_at_rest_when_key_available() {
        // V16：有主密钥时，落库的必须是密文，库里不得出现 Token 明文。
        let conn = mem_conn();
        let key = crate::store::crypto::generate_master_key();
        let token = ensure_token(&conn, Some(&key)).unwrap();

        let stored = db::get_setting(&conn, SETTING_TOKEN).unwrap().unwrap();
        assert!(stored.starts_with("enc:"), "应加密存储：{stored}");
        assert!(
            !stored.contains(&token),
            "数据库中不得出现 Token 明文"
        );

        // 仍能正确读回并兼容解密。
        assert_eq!(ensure_token(&conn, Some(&key)).unwrap(), token);
    }

    #[test]
    fn token_falls_back_to_plaintext_without_key() {
        // K2 未解锁时 MCP 仍需可用：此时降级为显式明文前缀，而非静默。
        let conn = mem_conn();
        let token = ensure_token(&conn, None).unwrap();
        let stored = db::get_setting(&conn, SETTING_TOKEN).unwrap().unwrap();
        assert!(stored.starts_with("plain:"), "应带显式明文前缀：{stored}");

        // 后续解锁后仍能读回同一个 Token。
        let key = crate::store::crypto::generate_master_key();
        assert_eq!(ensure_token(&conn, Some(&key)).unwrap(), token);
    }

    #[test]
    fn legacy_plaintext_token_still_readable() {
        // 兼容历史数据：早期版本直接存明文、无前缀。
        let conn = mem_conn();
        db::set_setting(&conn, SETTING_TOKEN, "deadbeef").unwrap();
        assert_eq!(ensure_token(&conn, None).unwrap(), "deadbeef");
    }

    #[test]
    fn regenerate_token_replaces_old_one() {
        let conn = mem_conn();
        let old = ensure_token(&conn, None).unwrap();
        let new = regenerate_token(&conn, None).unwrap();
        assert_ne!(old, new, "重新生成应产生新 Token");
        assert_eq!(ensure_token(&conn, None).unwrap(), new, "新 Token 应被持久化");
    }

    #[test]
    fn allow_remote_defaults_to_false() {
        let conn = mem_conn();
        // 安全默认值：默认只监听回环（D2）。
        assert!(!allow_remote(&conn).unwrap());
        set_allow_remote(&conn, true).unwrap();
        assert!(allow_remote(&conn).unwrap());
    }

    #[test]
    fn auto_start_defaults_to_true() {
        let conn = mem_conn();
        assert!(auto_start(&conn).unwrap());
        set_auto_start(&conn, false).unwrap();
        assert!(!auto_start(&conn).unwrap());
    }

    /// 断言测试端口当前**可用**；被占用时给出可操作的失败提示。
    ///
    /// 最常见的占用者是**开发时正在运行的应用本体**——它监听的正是
    /// [`PORT_RANGE_START`]。
    ///
    /// 这里刻意**失败并说明该怎么办**，而不是静默跳过：
    /// 静默跳过会让"端口被占"这种环境问题伪装成绿色通过，
    /// 掩盖真实回归（团队约定：环境不具备时应提示人去处理，而不是让测试装作没事）。
    async fn require_port_free(port: u16) {
        match TcpListener::bind(("127.0.0.1", port)).await {
            Ok(listener) => drop(listener),
            Err(e) => panic!(
                "端口 {port} 被占用（{e}）。两种常见成因：\n\
                 ① 有**正在运行的 mf-perch 应用实例**（它监听的就是这个端口段）——请先退出该实例；\n\
                 ② 有**并发的测试**正在跑（e2e 也会启动 MCP 端点）——请等它结束后再跑本测试。\n\
                 若两者都不成立，请检查是否有残留的 mf-perch / 测试进程。"
            ),
        }
    }

    #[tokio::test]
    async fn select_port_prefers_persisted_port() {
        let _guard = PORT_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        require_port_free(PORT_RANGE_START).await;

        let (listener, port) = select_port_with_preference(Some(PORT_RANGE_START), false)
            .await
            .unwrap();
        assert_eq!(port, PORT_RANGE_START, "起始端口空闲时应被直接复用");

        drop(listener);
    }

    #[tokio::test]
    async fn select_port_rescans_when_persisted_is_taken() {
        let _guard = PORT_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        require_port_free(PORT_RANGE_START).await;

        // 占用起始端口，模拟"持久化端口被占用"。
        let blocker = TcpListener::bind(("127.0.0.1", PORT_RANGE_START))
            .await
            .expect("刚校验过端口可用，这里应能占用成功");

        let (listener, port) = select_port_with_preference(Some(PORT_RANGE_START), false)
            .await
            .unwrap();
        assert_ne!(port, PORT_RANGE_START, "应跳过被占用的持久化端口");
        assert!(port > PORT_RANGE_START, "应从起始端口向后递增");

        drop(listener);
        drop(blocker);
    }

    #[tokio::test]
    async fn select_port_without_preference_scans_from_start() {
        let _guard = PORT_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        require_port_free(PORT_RANGE_START).await;

        let (listener, port) = select_port_with_preference(None, false).await.unwrap();
        assert_eq!(port, PORT_RANGE_START, "无持久化端口时应从起始端口开始");
        drop(listener);
    }

    #[test]
    fn endpoint_url_reflects_listen_scope() {
        assert_eq!(endpoint_url(50001, false), "http://127.0.0.1:50001/mcp");
        assert_eq!(endpoint_url(50001, true), "http://0.0.0.0:50001/mcp");
    }
}
