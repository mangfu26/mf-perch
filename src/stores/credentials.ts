import { defineStore } from "pinia";
import { ref } from "vue";

import * as api from "@/lib/commands";
import type { CredentialInput, CredentialSummary } from "@/lib/api";
import { useAppStore } from "./app";

/**
 * 认证信息状态（人类侧）。
 *
 * 安全约束（D6 / Q10）：后端**从不返回**密码或私钥正文，
 * 列表只含用户名、类型与指纹，因此前端也无从泄露。
 */
export const useCredentialsStore = defineStore("credentials", () => {
  const credentials = ref<CredentialSummary[]>([]);
  const loading = ref(false);

  const app = useAppStore();

  async function refresh() {
    loading.value = true;
    try {
      credentials.value = await api.listCredentials();
    } catch (e) {
      app.fail(e);
    } finally {
      loading.value = false;
    }
  }

  async function save(input: CredentialInput) {
    try {
      const id = await api.saveCredential(input);
      await refresh();
      app.notify(input.id ? "认证信息已更新" : "认证信息已添加");
      return id;
    } catch (e) {
      app.fail(e);
      throw e;
    }
  }

  async function remove(id: string) {
    try {
      await api.deleteCredential(id);
      await refresh();
      app.notify("认证信息已删除");
    } catch (e) {
      app.fail(e);
      throw e;
    }
  }

  function label(c: CredentialSummary) {
    return c.name || c.username;
  }

  function options() {
    return credentials.value.map((c) => ({
      value: c.id,
      label: label(c),
    }));
  }

  return { credentials, loading, refresh, save, remove, label, options };
});
