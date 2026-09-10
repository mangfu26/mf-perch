//! SSH 终端引擎。
//!
//! - [`protocol`]：会话协议（NUL 分帧、结束标记、askpass 脚本生成）
//! - [`output`]：输出累积与截断
//! - [`auth`]：认证方式与主机密钥校验（TOFU，D10）
//! - [`session`]：基于 russh 的会话实现

pub mod auth;
pub mod output;
pub mod protocol;
pub mod session;

pub use auth::{AuthMethod, HostKeyCheck, TofuHandler};
pub use output::OutputAccumulator;
pub use protocol::SessionEvent;
pub use session::{Session, SessionOutput};
