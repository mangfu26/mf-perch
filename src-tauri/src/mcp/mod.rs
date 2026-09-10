//! MCP Server 层（D1 / D2）。
//!
//! - [`endpoint`]：端口策略、Token、启停
//! - [`tools`]：暴露给 AI Agent 的工具与 `ServerHandler` 实现
//!
//! 传输方式仅 **Streamable HTTP**：不做 stdio，客户端若不支持 HTTP
//! 可由用户用第三方工具桥接（D1）。这样凭据与连接池始终留在应用进程内。

pub mod endpoint;
pub mod server;
pub mod tools;

pub use endpoint::{endpoint_url, RunningEndpoint};
pub use server::{McpManager, McpStatus};
pub use tools::McpService;
