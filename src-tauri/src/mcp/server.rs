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
}

impl McpManager {
    pub fn new(state: Arc<AppState>) -> Self {
        Self {
            state,
            running: Mutex::new(None),
            task: Mutex::new(None),
        }
    }

    /// 查询当前状态（含持久化配置，便于界面展示）。
    pub async fn status(&self) -> Result<McpStatus> {
        let conn = self.state.db.lock().await;
        let allow_remote = endpoint::allow_remote(&conn)?;
        let auto = endpoint::auto_start(&conn)?;
        let token = db::get_setting(&conn, endpoint::SETTING_TOKEN)?;

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
    pub async fn start(&self) -> Result<McpStatus> {
        {
            let running = self.running.lock().await;
            if running.is_some() {
                drop(running);
                return self.status().await;
            }
        }

        // 第 1 步：在锁内只做同步的读取/写入，不跨越 await。
        let (allow_remote, token, preferred_port) = {
            let conn = self.state.db.lock().await;
            let allow_remote = endpoint::allow_remote(&conn)?;
            let token = endpoint::ensure_token(&conn)?;
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
                Arc::new(LocalSessionManager::default()),
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
        let was_running = self.running.lock().await.is_some();
        if was_running {
            // 先停服务，避免旧 Token 在重启前仍被接受。
            self.stop().await?;
        }

        let token = {
            let conn = self.state.db.lock().await;
            endpoint::regenerate_token(&conn)?
        };

        if was_running {
            self.start().await?;
        }
        Ok(token)
    }

    /// 设置"允许远程连接"。若服务正在运行则重启以应用新的监听范围。
    pub async fn set_allow_remote(&self, allow: bool) -> Result<McpStatus> {
        let was_running = self.running.lock().await.is_some();
        if was_running {
            self.stop().await?;
        }

        {
            let conn = self.state.db.lock().await;
            endpoint::set_allow_remote(&conn, allow)?;
        }

        if was_running {
            self.start().await?;
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
