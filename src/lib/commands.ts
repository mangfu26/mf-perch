/**
 * 后端命令的类型化封装。
 *
 * 组件不直接拼命令名与参数，统一从这里调用，便于重构与检索。
 */
import { call } from "./ipc";
import type {
  CredentialInput,
  CredentialSummary,
  HistoryItem,
  HistoryStats,
  HostInput,
  HostSummary,
  KeyProvider,
  KeyStatus,
  McpStatus,
  Terminal,
  TerminalView,
} from "./api";

// ==================== 密钥状态与引导 ====================

export const keyStatus = () => call<KeyStatus>("key_status");

export const initKeyProvider = (provider: KeyProvider, password?: string) =>
  call<KeyStatus>("init_key_provider", { provider, password: password ?? null });

export const unlockWithPassword = (password: string) =>
  call<boolean>("unlock_with_password", { password });

// ==================== 主机 ====================

export const listHosts = () => call<HostSummary[]>("list_hosts");

export const saveHost = (input: HostInput) => call<string>("save_host", { input });

export const deleteHost = (id: string) => call<boolean>("delete_host", { id });

// ==================== 认证信息 ====================

export const listCredentials = () =>
  call<CredentialSummary[]>("list_credentials");

export const saveCredential = (input: CredentialInput) =>
  call<string>("save_credential", { input });

export const deleteCredential = (id: string) =>
  call<boolean>("delete_credential", { id });

// ==================== 终端 ====================

export const listTerminals = (hostId?: string) =>
  call<TerminalView[]>("list_terminals", { hostId: hostId ?? null });

export const archiveTerminal = (id: string) =>
  call<Terminal>("archive_terminal", { id });

export const restoreTerminal = (id: string) =>
  call<Terminal>("restore_terminal", { id });

/** 重连已断开（broken）的终端：沿用原 ID 与历史，但 shell 状态会重置（D39）。 */
export const reconnectTerminal = (id: string) =>
  call<boolean>("reconnect_terminal", { id });

export const deleteTerminal = (id: string) =>
  call<boolean>("delete_terminal", { id });

// ==================== 命令历史（审计） ====================

export interface HistoryQuery {
  terminalId?: string;
  hostId?: string;
  query?: string;
  limit?: number;
  offset?: number;
}

export const searchHistory = (params: HistoryQuery = {}) =>
  call<HistoryItem[]>("search_history", {
    params: {
      terminal_id: params.terminalId ?? null,
      host_id: params.hostId ?? null,
      query: params.query ?? null,
      limit: params.limit ?? null,
      offset: params.offset ?? null,
    },
  });

export const historyStats = () => call<HistoryStats>("history_stats");

// ==================== 设置 ====================
//
// 刻意不提供通用的 get/set 设置读写：它会绕过语义校验并暴露 mcp_token（V18）。
// 具体设置请使用下列类型化接口（runtimeSettings / update* / mcp*）。

// ==================== MCP 管理 ====================

export const mcpStatus = () => call<McpStatus>("mcp_status");

export const mcpStart = () => call<McpStatus>("mcp_start");

export const mcpStop = () => call<McpStatus>("mcp_stop");

export const mcpRegenerateToken = () => call<string>("mcp_regenerate_token");

export const mcpSetAllowRemote = (allow: boolean) =>
  call<McpStatus>("mcp_set_allow_remote", { allow });

export const mcpSetAutoStart = (enabled: boolean) =>
  call<boolean>("mcp_set_auto_start", { enabled });

export const mcpClientConfig = () => call<string>("mcp_client_config");

// ==================== 更新检查（D23） ====================

export interface UpdateInfo {
  current_version: string;
  source_url: string;
  auto_check: boolean;
  ignored_version: string | null;
  last_result: unknown;
}

export const updateInfo = () => call<UpdateInfo>("update_info");

export const updateCheck = (force: boolean) =>
  call<unknown>("update_check", { force });

export const updateIgnoreVersion = (version: string) =>
  call<boolean>("update_ignore_version", { version });

export const updateSetSource = (url: string) =>
  call<boolean>("update_set_source", { url });

export const updateSetAutoCheck = (enabled: boolean) =>
  call<boolean>("update_set_auto_check", { enabled });

// ==================== 运行期设置（Q11 / Q12 / Q4） ====================

export interface RuntimeSettings {
  quota_per_host: number;
  quota_global: number;
  /** 历史保留小时数；0 表示永久保留。 */
  retention_hours: number;
  max_output_bytes: number;
  max_output_lines: number;
  queue_limit: number;
}

export const runtimeSettings = () => call<RuntimeSettings>("runtime_settings");

/** 写入设置，返回夹紧后的实际生效值。 */
export const setRuntimeSetting = (key: string, value: string) =>
  call<string>("set_runtime_setting", { key, value });
