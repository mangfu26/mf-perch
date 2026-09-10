import { defineStore } from "pinia";
import { ref } from "vue";

import * as api from "@/lib/commands";
import { errorMessage } from "@/lib/ipc";
import type { HostInput, HostSummary } from "@/lib/api";
import { useAppStore } from "./app";

/**
 * 主机管理状态（人类侧）。
 *
 * 权限边界（AGENTS.md 0.1）：主机由人类创建与管理，AI Agent 只读。
 */
export const useHostsStore = defineStore("hosts", () => {
  const hosts = ref<HostSummary[]>([]);
  const loading = ref(false);

  const app = useAppStore();

  async function refresh() {
    loading.value = true;
    try {
      hosts.value = await api.listHosts();
    } catch (e) {
      app.fail(e);
    } finally {
      loading.value = false;
    }
  }

  async function save(input: HostInput) {
    try {
      const id = await api.saveHost(input);
      await refresh();
      app.notify(input.id ? "主机已更新" : "主机已添加");
      return id;
    } catch (e) {
      app.fail(e);
      throw e;
    }
  }

  async function remove(id: string) {
    try {
      await api.deleteHost(id);
      await refresh();
      app.notify("主机已删除");
    } catch (e) {
      app.fail(e);
      throw e;
    }
  }

  /** 供表单展示：地址与端口的可读描述。 */
  function label(h: HostSummary) {
    return h.name || `${h.address}:${h.port}`;
  }

  /** 供下拉选择使用。 */
  function options() {
    return hosts.value.map((h) => ({
      value: h.id,
      label: label(h),
    }));
  }

  return { hosts, loading, refresh, save, remove, label, options };
});

export { errorMessage };
