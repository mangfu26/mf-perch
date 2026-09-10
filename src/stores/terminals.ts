import { defineStore } from "pinia";
import { ref, computed } from "vue";

import * as api from "@/lib/commands";
import { errorMessage } from "@/lib/ipc";
import type { HistoryItem, TerminalView } from "@/lib/api";
import { useAppStore } from "./app";

/**
 * 终端与审计状态（D26：终端页与审计页合并）。
 *
 * 权限边界（AGENTS.md 0.1）：人类只读、可归档/恢复/删除，
 * **不能向终端输入命令**——终端由 AI Agent 通过 MCP 创建与驱动。
 */
export const useTerminalsStore = defineStore("terminals", () => {
  const terminals = ref<TerminalView[]>([]);
  const history = ref<HistoryItem[]>([]);
  const loading = ref(false);
  const historyLoading = ref(false);

  const app = useAppStore();

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
    try {
      history.value = await api.searchHistory(query);
    } catch (e) {
      app.fail(e);
    } finally {
      historyLoading.value = false;
    }
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
    archive,
    restore,
    remove,
    findById,
  };
});

export { errorMessage };
