import { defineStore } from "pinia";
import { ref, computed } from "vue";

/**
 * MCP Server 状态。
 *
 * 阶段一只暴露展示所需的最小状态；启动/停止逻辑在阶段二接入
 * Rust 侧的真实实现（D1 / D2）。
 */
export const useMcpStore = defineStore("mcp", () => {
  const running = ref(false);
  const port = ref<number | null>(null);
  const token = ref<string | null>(null);
  const allowRemote = ref(false);
  const loading = ref(false);

  /** 供 MCP 客户端配置的接入地址。 */
  const endpoint = computed(() => {
    if (port.value === null) return null;
    const host = allowRemote.value ? "0.0.0.0" : "127.0.0.1";
    return `http://${host}:${port.value}/mcp`;
  });

  /**
   * 供用户复制到 MCP 客户端的配置片段（Streamable HTTP，仅此一种传输，D1）。
   */
  const clientConfig = computed(() => {
    if (!endpoint.value || !token.value) return null;
    return JSON.stringify(
      {
        mcpServers: {
          "mf-perch": {
            type: "streamable-http",
            url: endpoint.value,
            headers: {
              Authorization: `Bearer ${token.value}`,
            },
          },
        },
      },
      null,
      2,
    );
  });

  return {
    running,
    port,
    token,
    allowRemote,
    loading,
    endpoint,
    clientConfig,
  };
});
