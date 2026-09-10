use serde::{Deserialize, Serialize};

use super::{new_id, now_rfc3339};

/// 终端状态。
///
/// 生命周期（D20 / D21）：
/// `active`（可用）↔ `archived`（人类可恢复，Agent 不可见）
/// `broken`（SSH 断开，需重建）
/// 人类删除为终态，连同历史一并移除。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalStatus {
    /// 活跃：Agent 可见、可执行命令。
    Active,
    /// 连接已断开，需重建；人类可见，Agent 收到 broken 错误。
    Broken,
    /// 已归档：Agent 不可见，人类仍可审计历史。
    Archived,
}

impl TerminalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Broken => "broken",
            Self::Archived => "archived",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "active" => Some(Self::Active),
            "broken" => Some(Self::Broken),
            "archived" => Some(Self::Archived),
            _ => None,
        }
    }

    /// 是否占用终端配额（Q11：归档终端不占配额）。
    pub fn occupies_quota(self) -> bool {
        !matches!(self, Self::Archived)
    }

    /// 对 AI Agent 是否可见（归档后不可见）。
    pub fn visible_to_agent(self) -> bool {
        !matches!(self, Self::Archived)
    }
}

/// SSH 终端：绑定到某主机的一条常驻会话（D3）。
///
/// 由 AI Agent 创建与使用；人类只读、可归档/恢复/删除，但不能输入命令。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Terminal {
    pub id: String,
    pub host_id: String,
    /// 终端名称，由 AI Agent 设置与修改，可选。
    pub name: Option<String>,
    pub status: TerminalStatus,
    /// 会话建立时捕获的环境快照（PATH / PWD / bash 版本），便于排查（D4）。
    pub env_snapshot: Option<EnvSnapshot>,
    pub created_at: String,
    pub updated_at: String,
    pub archived_at: Option<String>,
}

/// 终端建立时的环境快照（D4）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvSnapshot {
    pub path: Option<String>,
    pub pwd: Option<String>,
    pub bash_version: Option<String>,
    pub shell_env_mode: String,
    pub captured_at: String,
}

impl Terminal {
    pub fn new(host_id: impl Into<String>, name: Option<String>) -> Self {
        let now = now_rfc3339();
        Self {
            id: new_id("term"),
            host_id: host_id.into(),
            name,
            status: TerminalStatus::Active,
            env_snapshot: None,
            created_at: now.clone(),
            updated_at: now,
            archived_at: None,
        }
    }

    /// 归档：关闭会话但保留历史，归档后 Agent 不可见（D20）。
    pub fn archive(&mut self) {
        let now = now_rfc3339();
        self.status = TerminalStatus::Archived;
        self.archived_at = Some(now.clone());
        self.updated_at = now;
    }

    /// 恢复：沿用原终端 ID 与历史，底层会话由调用方重建（D20）。
    pub fn restore(&mut self) {
        let now = now_rfc3339();
        self.status = TerminalStatus::Active;
        self.archived_at = None;
        self.updated_at = now;
    }

    /// 标记连接断开（D3）。
    pub fn mark_broken(&mut self) {
        if self.status == TerminalStatus::Active {
            self.status = TerminalStatus::Broken;
            self.updated_at = now_rfc3339();
        }
    }
}

/// 供 AI Agent 查看的终端信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalInfo {
    pub id: String,
    pub host_id: String,
    pub name: Option<String>,
    pub status: TerminalStatus,
    pub created_at: String,
}

impl From<&Terminal> for TerminalInfo {
    fn from(t: &Terminal) -> Self {
        Self {
            id: t.id.clone(),
            host_id: t.host_id.clone(),
            name: t.name.clone(),
            status: t.status,
            created_at: t.created_at.clone(),
        }
    }
}
