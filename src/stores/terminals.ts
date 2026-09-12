import { defineStore } from "pinia";
import { ref, computed } from "vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import * as api from "@/lib/commands";
import { errorMessage } from "@/lib/ipc";
import type { HistoryItem, TerminalView } from "@/lib/api";
import { useAppStore } from "./app";

/** 后端事件名（与 Rust 侧 `EVENT_TERMINAL` 一致，D22）。 */
const EVENT_TERMINAL = "terminal://event";

/** 终端运行时事件负载（与 Rust 侧 `TerminalEvent` 对应）。 */
interface TerminalEventPayload {
  kind:
    | "terminal_created"
    | "terminal_removed"
    | "command_started"
    | "command_finished"
    | "session_changed";
  terminal_id: string;
  /** `session_changed`：active / broken / archived */
  status?: string;
  /** `session_changed`：面向人类的简短说明 */
  reason?: string;
}

/** 合并突发事件的时间窗：一条命令可能伴随多行输出与多个事件。 */
const REFRESH_DEBOUNCE_MS = 250;

/**
 * 终端与审计状态（D26：终端页与审计页合并）。
 *
 * 权限边界（AGENTS.md 0.1）：人类只读、可归档/恢复/删除，
 * **不能向终端输入命令**——终端由 AI Agent 通过 MCP 创建与驱动。
 *
 * 实时性（D22）：订阅后端终端事件，让界面自动跟上 Agent 的动作
 * （命令开始/结束、会话断开与自动重连、终端新建/删除/归档），
 * 而不是要人工点刷新——否则会把"已自动重连"显示成"连接已断开"。
 */
export const useTerminalsStore = defineStore("terminals", () => {
  const terminals = ref<TerminalView[]>([]);
  const history = ref<HistoryItem[]>([]);
  const loading = ref(false);
  const historyLoading = ref(false);

  const app = useAppStore();

  /** 最近一次历史查询条件：事件到达时按同样条件重放（保持用户的筛选/搜索）。 */
  let lastHistoryQuery: api.HistoryQuery | null = null;
  let unlisten: UnlistenFn | null = null;
  let debounceTimer: ReturnType<typeof setTimeout> | null = null;
  /** 本轮回调里是否已请求刷新历史，避免同一批事件重复拉取。 */
  let historyDirty = false;

  /** 仅活跃/断开的终端（Agent 可见）。 */
  const activeTerminals = computed(() =>
    terminals.value.filter((t) => t.status !== "archived"),
  );

  /** 已归档终端（仅人类可见，用于审计，D20）。 */
  const archivedTerminals = computed(() =>
    terminals.value.filter((t) => t.status === "archived"),
  );

  /** 按主机分组，便于"按主机浏览"（D26）。 */
  const grouped = computed(() => {
    const map = new Map<string, TerminalView[]>();
    for (const t of terminals.value) {
      const key = t.host_name || t.host_id;
      const list = map.get(key) ?? [];
      list.push(t);
      map.set(key, list);
    }
    return Array.from(map.entries()).map(([host, items]) => ({ host, items }));
  });

  async function refresh(hostId?: string) {
    loading.value = true;
    try {
      terminals.value = await api.listTerminals(hostId);
    } catch (e) {
      app.fail(e);
    } finally {
      loading.value = false;
    }
  }

  async function search(query: api.HistoryQuery = {}) {
    historyLoading.value = true;
    lastHistoryQuery = query;
    try {
      history.value = await api.searchHistory(query);
    } catch (e) {
      app.fail(e);
    } finally {
      historyLoading.value = false;
    }
  }

  /** 重放最近一次历史查询（事件到达时用；没有查询过则忽略）。 */
  async function refreshHistory() {
    if (!lastHistoryQuery) return;
    await search(lastHistoryQuery);
  }

  /**
   * 订阅后端终端事件（D22）。
   *
   * 收到任何事件都**去抖**刷新一次：宁可多拉一次列表，
   * 也不要让界面停在陈旧状态（这正是客户反馈的问题①）。
   */
  async function startListening() {
    if (unlisten) return;
    unlisten = await listen<TerminalEventPayload>(EVENT_TERMINAL, (event) => {
      const payload = event.payload;

      // 只有与"当前正在看的历史"相关时才刷新历史，避免无谓的查询。
      if (
        payload.kind === "command_started" ||
        payload.kind === "command_finished"
      ) {
        const scope = lastHistoryQuery?.terminalId;
        if (!scope || scope === payload.terminal_id) historyDirty = true;
      }

      if (debounceTimer) clearTimeout(debounceTimer);
      debounceTimer = setTimeout(() => {
        debounceTimer = null;
        void refresh();
        if (historyDirty) {
          historyDirty = false;
          void refreshHistory();
        }
      }, REFRESH_DEBOUNCE_MS);
    });
  }

  function stopListening() {
    if (debounceTimer) {
      clearTimeout(debounceTimer);
      debounceTimer = null;
    }
    unlisten?.();
    unlisten = null;
  }

  async function archive(id: string) {
    try {
      await api.archiveTerminal(id);
      await refresh();
      app.notify("终端已归档，AI Agent 不再可见");
    } catch (e) {
      app.fail(e);
      throw e;
    }
  }

  async function restore(id: string) {
    try {
      await api.restoreTerminal(id);
      await refresh();
      app.notify("终端已恢复");
    } catch (e) {
      app.fail(e);
      throw e;
    }
  }

  /** 重连已断开的终端（D39）：沿用原 ID 与历史，但 shell 状态会重置。 */
  async function reconnect(id: string) {
    try {
      await api.reconnectTerminal(id);
      await refresh();
      app.notify("终端已重连（shell 状态已重置：工作目录、环境变量不再保留）");
    } catch (e) {
      app.fail(e);
      throw e;
    }
  }

  async function remove(id: string) {
    try {
      await api.deleteTerminal(id);
      await refresh();
      app.notify("终端及其命令历史已删除");
    } catch (e) {
      app.fail(e);
      throw e;
    }
  }

  function findById(id: string) {
    return terminals.value.find((t) => t.id === id) ?? null;
  }

  return {
    terminals,
    history,
    loading,
    historyLoading,
    activeTerminals,
    archivedTerminals,
    grouped,
    refresh,
    search,
    refreshHistory,
    startListening,
    stopListening,
    archive,
    restore,
    reconnect,
    remove,
    findById,
  };
});

export { errorMessage };
