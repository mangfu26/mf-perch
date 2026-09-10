import { defineStore } from "pinia";
import { ref, computed } from "vue";

import * as api from "@/lib/commands";
import type { McpStatus } from "@/lib/api";
import { useAppStore } from "./app";

/**
 * MCP Server 状态与操作（D1 / D2 / D16）。
 *
 * MCP 是本产品的核心：Agent 通过它访问终端。
 * 人类侧在此启停、查看端口与 Token、控制远程连接。
 */
export const useMcpStore = defineStore("mcp", () => {
  const status = ref<McpStatus | null>(null);
  const clientConfig = ref<string | null>(null);
  const loading = ref(false);

  const app = useAppStore();

  const running = computed(() => status.value?.running ?? false);
  const port = computed(() => status.value?.port ?? null);
  const token = computed(() => status.value?.token ?? null);
  const allowRemote = computed(() => status.value?.allow_remote ?? false);
  const autoStart = computed(() => status.value?.auto_start ?? true);
  const endpoint = computed(() => status.value?.endpoint ?? null);

  async function refresh() {
    try {
      status.value = await api.mcpStatus();
    } catch (e) {
      app.fail(e);
    }
  }

  async function start() {
    loading.value = true;
    try {
      status.value = await api.mcpStart();
      app.notify("MCP Server 已启动");
    } catch (e) {
      app.fail(e);
      throw e;
    } finally {
      loading.value = false;
    }
  }

  async function stop() {
    loading.value = true;
    try {
      status.value = await api.mcpStop();
      app.notify("MCP Server 已停止");
    } catch (e) {
      app.fail(e);
      throw e;
    } finally {
      loading.value = false;
    }
  }

  async function regenerateToken() {
    try {
      await api.mcpRegenerateToken();
      await refresh();
      await loadClientConfig();
      app.notify("访问令牌已重新生成，请更新客户端配置");
    } catch (e) {
      app.fail(e);
      throw e;
    }
  }

  async function setAllowRemote(allow: boolean) {
    try {
      status.value = await api.mcpSetAllowRemote(allow);
      app.notify(allow ? "已允许远程连接" : "已仅限本机访问");
    } catch (e) {
      app.fail(e);
      // 失败时重新拉取，避免界面停留在错误的开关状态。
      await refresh();
      throw e;
    }
  }

  async function setAutoStart(enabled: boolean) {
    try {
      await api.mcpSetAutoStart(enabled);
      await refresh();
    } catch (e) {
      app.fail(e);
      throw e;
    }
  }

  async function loadClientConfig() {
    try {
      clientConfig.value = await api.mcpClientConfig();
    } catch {
      // 未启动时无接入地址，属正常情况，不提示错误。
      clientConfig.value = null;
    }
  }

  return {
    status,
    clientConfig,
    loading,
    running,
    port,
    token,
    allowRemote,
    autoStart,
    endpoint,
    refresh,
    start,
    stop,
    regenerateToken,
    setAllowRemote,
    setAutoStart,
    loadClientConfig,
  };
});
