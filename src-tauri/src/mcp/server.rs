//! MCP HTTP 服务与生命周期管理（D1 / D2）。
//!
//! 使用 `rmcp` 的 `StreamableHttpService` 挂载在 `/mcp` 路径，
//! 并在外层套一层 Bearer Token 鉴权中间件（Q2 采纳"自动生成 Token"方案）。

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::any_service;
use axum::Router;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::tower::{
    StreamableHttpServerConfig, StreamableHttpService,
};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::error::{AppError, Result};
use crate::mcp::endpoint::{self, RunningEndpoint};
use crate::mcp::tools::McpService;
use crate::state::AppState;
use crate::store::db;

/// 对外暴露的 MCP 状态（供 UI 展示）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct McpStatus {
    pub running: bool,
    pub port: Option<u16>,
    pub token: Option<String>,
    pub endpoint: Option<String>,
    pub allow_remote: bool,
    pub auto_start: bool,
}

/// MCP 服务生命周期管理器。
pub struct McpManager {
    state: Arc<AppState>,
    running: Mutex<Option<Arc<RunningEndpoint>>>,
    task: Mutex<Option<JoinHandle<()>>>,
    /// 串行化 start / stop / 重启类操作（V10）。
    ///
    /// 若不加这把锁，`start()` 的"检查是否已运行"与"写入 running"之间跨越多次
    /// `await`（读库、绑端口），两个并发 `start()` 会同时通过检查、各自绑定端口
    /// 并启动一个 axum 服务；后写入的会覆盖前者，被覆盖的实例仍在运行却已失去
    /// 引用，`stop()` 再也停不掉它，界面还会显示"已停止"。
    lifecycle: Mutex<()>,
}

/// MCP 会话的空闲超时（D38）。
///
/// **不能沿用 rmcp 的默认值**：`SessionConfig::default().keep_alive` 是
/// **5 分钟**，会话 worker 在闲置 5 分钟后自行退出、会话被从表中移除，
/// 之后客户端再带着原 `mcp-session-id` 请求就会收到
/// `404 Not Found: Session not found`。
///
/// 对一个"人 + Agent 交互使用"的桌面应用来说这个默认值太短：
/// 用户去开个会、Agent 停下来思考，回来连接就"断了"，而且
/// MCP Inspector 这类客户端不会自动重新 initialize，界面上就是一条
/// 报错（实测复现见 `mcp_e2e` 的相关用例）。
///
/// 取值 24 小时的考虑：
/// - 远大于任何人机交互的空闲间隔（含整夜挂着）；
/// - 仍保留一个**有界**的兜底，避免客户端异常断开（未发 DELETE）时
///   会话无限累积——这是 rmcp 文档提醒不要直接设为 `None` 的原因。
const SESSION_IDLE_TIMEOUT: Option<std::time::Duration> =
    Some(std::time::Duration::from_secs(24 * 60 * 60));

/// 构造会话管理器（统一在此处设定空闲超时，见 [`SESSION_IDLE_TIMEOUT`]）。
fn session_manager() -> LocalSessionManager {
    // `LocalSessionManager` / `SessionConfig` 都是 `#[non_exhaustive]`，
    // 无法用结构体字面量构造，故取默认值后改写单个字段。
    let mut manager = LocalSessionManager::default();
    manager.session_config.keep_alive = SESSION_IDLE_TIMEOUT;
    manager
}

impl McpManager {
    pub fn new(state: Arc<AppState>) -> Self {
        Self {
            state,
            running: Mutex::new(None),
            task: Mutex::new(None),
            lifecycle: Mutex::new(()),
        }
    }

    /// 当前主密钥（未解锁时为 `None`，用于 Token 落盘的加密决策）。
    async fn master_key(&self) -> Option<[u8; crate::store::crypto::KEY_LEN]> {
        let guard = self.state.master_key.lock().await;
        guard.get().ok().copied()
    }

    /// 查询当前状态（含持久化配置，便于界面展示）。
    pub async fn status(&self) -> Result<McpStatus> {
        let conn = self.state.db.lock().await;
        let allow_remote = endpoint::allow_remote(&conn)?;
        let auto = endpoint::auto_start(&conn)?;
        let key = self.master_key().await;
        let token = endpoint::existing_token(&conn, key.as_ref())?;

        let running = self.running.lock().await;
        Ok(match running.as_ref() {
            Some(ep) => McpStatus {
                running: true,
                port: Some(ep.addr.port()),
                token: Some(ep.token.clone()),
                endpoint: Some(endpoint::endpoint_url(ep.addr.port(), allow_remote)),
                allow_remote,
                auto_start: auto,
            },
            None => McpStatus {
                running: false,
                port: db::get_setting(&conn, endpoint::SETTING_PORT)?
                    .and_then(|p| p.parse().ok()),
                token,
                endpoint: None,
                allow_remote,
                auto_start: auto,
            },
        })
    }

    /// 启动 MCP 端点。
    ///
    /// 整个"检查—启动—登记"过程由 `lifecycle` 串行化，避免并发启动
    /// 产生无法停止的第二个监听（V10）。
    pub async fn start(&self) -> Result<McpStatus> {
        let _lifecycle = self.lifecycle.lock().await;
        self.start_inner().await
    }

    /// 启动逻辑；调用方必须已持有 `lifecycle` 锁。
    async fn start_inner(&self) -> Result<McpStatus> {
        {
            let running = self.running.lock().await;
            if running.is_some() {
                drop(running);
                return self.status().await;
            }
        }

        // 主密钥在取库锁之前拿好，避免锁顺序交叉（master_key 与 db 是两把锁）。
        let key = self.master_key().await;

        // 第 1 步：在锁内只做同步的读取/写入，不跨越 await。
        let (allow_remote, token, preferred_port) = {
            let conn = self.state.db.lock().await;
            let allow_remote = endpoint::allow_remote(&conn)?;
            let token = endpoint::ensure_token(&conn, key.as_ref())?;
            let preferred = db::get_setting(&conn, endpoint::SETTING_PORT)?
                .and_then(|p| p.parse::<u16>().ok());
            (allow_remote, token, preferred)
        };

        // 第 2 步：绑定端口（异步，且**不持锁**）。
        // 优先复用持久化端口；被占用则重新从起始端口递增（Q2）。
        let (listener, port) =
            endpoint::select_port_with_preference(preferred_port, allow_remote).await?;

        // 第 3 步：持久化实际使用的端口（短暂加锁）。
        {
            let conn = self.state.db.lock().await;
            db::set_setting(&conn, endpoint::SETTING_PORT, &port.to_string())?;
        }

        let addr = listener
            .local_addr()
            .map_err(|e| AppError::Mcp(format!("读取监听地址失败：{e}")))?;

        // rmcp 的 Streamable HTTP 服务。
        let service: StreamableHttpService<McpService, LocalSessionManager> =
            StreamableHttpService::new(
                {
                    let state = self.state.clone();
                    move || Ok(McpService::new(state.clone()))
                },
                Arc::new(session_manager()),
                {
                    // 默认仅接受回环 Host，防止 DNS rebinding；
                    // 允许远程时放开，由 Token 与网络环境共同保护。
                    // 该结构体标记了 non_exhaustive，故用 builder 而非结构体字面量。
                    let config = StreamableHttpServerConfig::default();
                    if allow_remote {
                        config.with_allowed_hosts([
                            "localhost",
                            "127.0.0.1",
                            "0.0.0.0",
                        ])
                    } else {
                        config
                    }
                },
            );

        // Bearer Token 鉴权（Q2）。
        let auth_state = AuthState {
            token: token.clone(),
        };

        let app = Router::new()
            .route("/mcp", any_service(service))
            .layer(middleware::from_fn_with_state(auth_state, require_bearer))
            .with_state(());

        let ep = endpoint::endpoint(addr, token.clone());
        let cancel = endpoint::cancellation_of(&ep);

        let handle = tokio::spawn(async move {
            let server = axum::serve(listener, app).with_graceful_shutdown(async move {
                cancel.cancelled().await;
            });

            if let Err(e) = server.await {
                tracing::error!("MCP 端点异常退出：{e}");
            }
        });

        *self.running.lock().await = Some(ep);
        *self.task.lock().await = Some(handle);

        tracing::info!("MCP Server 已启动：{}", endpoint::endpoint_url(port, allow_remote));

        self.status().await
    }

    /// 停止 MCP 端点。
    pub async fn stop(&self) -> Result<McpStatus> {
        let _lifecycle = self.lifecycle.lock().await;
        self.stop_inner().await
    }

    /// 停止逻辑；调用方必须已持有 `lifecycle` 锁。
    async fn stop_inner(&self) -> Result<McpStatus> {
        let ep = self.running.lock().await.take();
        if let Some(ep) = ep {
            ep.stop().await;
        }
        if let Some(handle) = self.task.lock().await.take() {
            // 等待监听任务退出，确保端口已释放，便于立即重启。
            let _ = tokio::time::timeout(std::time::Duration::from_secs(3), handle).await;
        }
        tracing::info!("MCP Server 已停止");
        self.status().await
    }

    /// 重新生成 Token（旧 Token 立即失效，需更新客户端配置）。
    pub async fn regenerate_token(&self) -> Result<String> {
        // 持有生命周期锁，保证"停止 → 换 Token → 启动"不被并发启动插入。
        let _lifecycle = self.lifecycle.lock().await;

        let was_running = self.running.lock().await.is_some();
        if was_running {
            // 先停服务，避免旧 Token 在重启前仍被接受。
            self.stop_inner().await?;
        }

        let key = self.master_key().await;
        let token = {
            let conn = self.state.db.lock().await;
            endpoint::regenerate_token(&conn, key.as_ref())?
        };

        if was_running {
            self.start_inner().await?;
        }
        Ok(token)
    }

    /// 设置"允许远程连接"。若服务正在运行则重启以应用新的监听范围。
    pub async fn set_allow_remote(&self, allow: bool) -> Result<McpStatus> {
        let _lifecycle = self.lifecycle.lock().await;

        let was_running = self.running.lock().await.is_some();
        if was_running {
            self.stop_inner().await?;
        }

        {
            let conn = self.state.db.lock().await;
            endpoint::set_allow_remote(&conn, allow)?;
        }

        if was_running {
            self.start_inner().await?;
        }
        self.status().await
    }

    /// 设置"随应用启动自动运行"。
    pub async fn set_auto_start(&self, enabled: bool) -> Result<()> {
        let conn = self.state.db.lock().await;
        endpoint::set_auto_start(&conn, enabled)
    }
}

/// 鉴权中间件所需的共享状态。
#[derive(Clone)]
struct AuthState {
    token: String,
}

/// Bearer Token 校验（Q2）。
///
/// 仅监听回环时，这一层用于挡住本机其他程序的误调用；
/// 开启远程连接后，它同时是唯一的访问闸门，因此必须恒定校验。
async fn require_bearer(
    State(auth): State<AuthState>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let provided = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    match provided {
        Some(t) if constant_time_eq(t, &auth.token) => next.run(request).await,
        _ => (
            StatusCode::UNAUTHORIZED,
            [(
                axum::http::header::WWW_AUTHENTICATE,
                "Bearer realm=\"mf-perch\"",
            )],
            "invalid or missing bearer token",
        )
            .into_response(),
    }
}

/// 常量时间比较，避免通过响应时间差逐字节猜测 Token。
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::transport::streamable_http_server::session::local::SessionConfig;

    #[test]
    fn rmcp_default_idle_timeout_is_the_five_minute_trap() {
        // 记录被我们绕开的坑：rmcp 默认 5 分钟不用就回收会话。
        // 若上游改了这个默认值，本用例会失败，提示重新评估 D38 的取值。
        assert_eq!(
            SessionConfig::default().keep_alive,
            Some(std::time::Duration::from_secs(300)),
            "rmcp 的默认会话空闲超时变了，需重新评估 D38"
        );
    }

    #[test]
    fn session_manager_does_not_inherit_rmcp_default_idle_timeout() {
        // 回归：曾经直接使用 `LocalSessionManager::default()`，
        // 于是 MCP 客户端空闲 5 分钟就被判"会话不存在"（用户实测反馈）。
        let manager = session_manager();
        assert_ne!(
            manager.session_config.keep_alive,
            Some(std::time::Duration::from_secs(300)),
            "不得沿用 rmcp 的 5 分钟默认空闲超时"
        );
    }

    #[test]
    fn session_idle_timeout_is_long_enough_for_human_pauses() {
        match SESSION_IDLE_TIMEOUT {
            // 允许显式关闭（None），但不允许短于 1 小时。
            None => {}
            Some(d) => assert!(
                d >= std::time::Duration::from_secs(60 * 60),
                "空闲超时必须远大于人机交互的停顿，当前：{d:?}"
            ),
        }
    }

    #[test]
    fn constant_time_eq_accepts_identical() {
        assert!(constant_time_eq("abc123", "abc123"));
    }

    #[test]
    fn constant_time_eq_rejects_different() {
        assert!(!constant_time_eq("abc123", "abc124"));
        assert!(!constant_time_eq("abc123", "abc12"));
        assert!(!constant_time_eq("", "x"));
        assert!(constant_time_eq("", ""));
    }

    #[test]
    fn constant_time_eq_handles_long_tokens() {
        // 真实 Token 为 64 个十六进制字符。
        let t = "a".repeat(64);
        assert!(constant_time_eq(&t, &t.clone()));
        let mut other = t.clone();
        other.pop();
        other.push('b');
        assert!(!constant_time_eq(&t, &other));
    }
}
