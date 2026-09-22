import { defineStore } from "pinia";
import { ref, computed } from "vue";

import { call } from "@/lib/ipc";
import { useAppStore } from "./app";

/**
 * 更新检查状态（D23）。
 *
 * 设计要点：
 * - 不做自动下载/安装，只提示并提供下载地址（客户决定）
 * - 手动检查会把失败原因展示给用户；自动检查静默
 * - 支持「忽略此版本」，同版本不再提示
 */
export interface UpdateInfo {
  current_version: string;
  /** **生效的**更新源地址（内置默认或用户自定义）。 */
  source_url: string;
  /** 是否为用户自定义；false 表示正在使用内置默认地址（D42）。 */
  source_is_custom: boolean;
  auto_check: boolean;
  ignored_version: string | null;
  last_result: UpdateStatus | null;
}

export type UpdateStatus =
  | { status: "up_to_date"; current: string; latest: string }
  | {
      status: "available";
      current: string;
      latest: string;
      notes: string | null;
      published_at: string | null;
      /** 该版本的 Release 页面地址（D58）；null 表示清单既没给也推导不出。 */
      release_url: string | null;
      sha256: string | null;
      size: number | null;
    }
  | { status: "ignored"; current: string; latest: string }
  | { status: "failed"; reason: string };

export const useUpdateStore = defineStore("update", () => {
  const info = ref<UpdateInfo | null>(null);
  const result = ref<UpdateStatus | null>(null);
  const checking = ref(false);
  /** 手动检查失败时的提示（自动检查不显示）。 */
  const manualError = ref<string | null>(null);

  const app = useAppStore();

  const currentVersion = computed(() => info.value?.current_version ?? "—");
  const hasUpdate = computed(() => result.value?.status === "available");

  async function refresh() {
    try {
      info.value = await call<UpdateInfo>("update_info");
      // 已有缓存结果时直接展示，避免设置页空白。
      if (info.value?.last_result && !result.value) {
        result.value = info.value.last_result;
      }
    } catch (e) {
      app.fail(e);
    }
  }

  /** 检查更新。`manual` 为真时展示失败原因。 */
  async function check(manual = true) {
    checking.value = true;
    manualError.value = null;
    try {
      const r = await call<UpdateStatus>("update_check", { force: manual });
      // "已忽略"不作为当前提示展示，保持界面清爽。
      result.value = r;
      if (manual && r.status === "available") {
        app.notify(`发现新版本 ${r.latest}`);
      }
      if (manual && r.status === "up_to_date") {
        app.notify("已是最新版本");
      }
      if (manual && r.status === "failed") {
        manualError.value = r.reason;
      }
    } catch (e) {
      if (manual) manualError.value = e instanceof Error ? e.message : String(e);
    } finally {
      checking.value = false;
    }
  }

  async function ignoreVersion(version: string) {
    try {
      await call<boolean>("update_ignore_version", { version });
      await refresh();
      // 忽略后立即清除提示，避免仍然显示。
      result.value = { status: "ignored", current: currentVersion.value, latest: version };
      app.notify(`已忽略版本 ${version}`);
    } catch (e) {
      app.fail(e);
    }
  }

  async function setSource(url: string) {
    try {
      await call<boolean>("update_set_source", { url });
      await refresh();
      app.notify("更新源已保存");
    } catch (e) {
      app.fail(e);
      throw e;
    }
  }

  /** 恢复内置默认更新源（清空自定义值即为回退，见 D42）。 */
  async function resetSource() {
    try {
      await call<boolean>("update_set_source", { url: "" });
      await refresh();
      app.notify("已恢复内置默认更新源");
    } catch (e) {
      app.fail(e);
      throw e;
    }
  }

  async function setAutoCheck(enabled: boolean) {
    try {
      await call<boolean>("update_set_auto_check", { enabled });
      await refresh();
    } catch (e) {
      app.fail(e);
      throw e;
    }
  }

  return {
    info,
    result,
    checking,
    manualError,
    currentVersion,
    hasUpdate,
    refresh,
    check,
    ignoreVersion,
    setSource,
    resetSource,
    setAutoCheck,
  };
});
