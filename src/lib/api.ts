/**
 * 与 Rust 侧领域模型对应的前端类型。
 *
 * 命名与 `src-tauri/src/domain` 保持一致，便于对照维护。
 * 敏感字段（密码、私钥正文）**不在任何返回类型中**——
 * 后端也不返回它们（D6 / Q10）。
 */

export type SudoPolicy = "deny" | "ask" | "auto" | "not_needed";
export type SudoPasswordSource = "own" | "reuse_login";
export type ShellEnvMode = "login" | "clean";
export type CredentialKind = "password" | "key";
export type TerminalStatus = "active" | "broken" | "archived";
export type CommandStatus = "queued" | "running" | "completed" | "failed";
export type KeyProvider = "keyring" | "master_password" | "local_file";

export interface HostSummary {
  id: string;
  name: string | null;
  address: string;
  port: number;
  has_credential: boolean;
  sudo_policy: SudoPolicy;
  active_terminals: number;
  archived_terminals: number;
  // 供「编辑主机」表单原样回填（B5）：表单提交会整体覆盖主机配置，
  // 若摘要里没有这些字段，只改个名字就会把它们清空。
  credential_id: string | null;
  proxy_jump_host_id: string | null;
  sudo_password_source: SudoPasswordSource;
  shell_env_mode: ShellEnvMode;
  init_script: string | null;
}

export interface HostInput {
  id?: string | null;
  name?: string | null;
  address: string;
  port: number;
  credential_id?: string | null;
  proxy_jump_host_id?: string | null;
  sudo_policy: SudoPolicy;
  sudo_password_source: SudoPasswordSource;
  sudo_password?: string | null;
  shell_env_mode: ShellEnvMode;
  init_script?: string | null;
}

export interface CredentialSummary {
  id: string;
  name: string | null;
  username: string;
  kind: CredentialKind;
  fingerprint: string | null;
  has_passphrase: boolean;
  used_by_hosts: string[];
  created_at: string;
  updated_at: string;
}

export interface CredentialInput {
  id?: string | null;
  name?: string | null;
  username: string;
  kind: CredentialKind;
  /** 留空表示保持不变（更新场景）。 */
  secret?: string | null;
  passphrase?: string | null;
}

export interface EnvSnapshot {
  path: string | null;
  pwd: string | null;
  bash_version: string | null;
  shell_env_mode: string;
  captured_at: string;
}

export interface Terminal {
  id: string;
  host_id: string;
  name: string | null;
  status: TerminalStatus;
  env_snapshot: EnvSnapshot | null;
  created_at: string;
  updated_at: string;
  archived_at: string | null;
}

export interface TerminalView extends Terminal {
  host_name: string | null;
  command_count: number;
}

export interface CommandRecord {
  id: string;
  terminal_id: string;
  seq: number;
  command: string;
  status: CommandStatus;
  exit_code: number | null;
  duration_ms: number | null;
  truncated: boolean;
  output_bytes: number | null;
  created_at: string;
  started_at: string | null;
  finished_at: string | null;
}

export interface HistoryItem extends CommandRecord {
  output: string | null;
}

export interface HistoryStats {
  command_count: number;
  output_bytes: number;
  archived_command_count: number;
}

export interface KeyStatus {
  initialized: boolean;
  unlocked: boolean;
  provider: KeyProvider | null;
  keyring_available: boolean;
  unavailable_reason: string | null;
}

export interface McpStatus {
  running: boolean;
  port: number | null;
  token: string | null;
  endpoint: string | null;
  allow_remote: boolean;
  auto_start: boolean;
}
