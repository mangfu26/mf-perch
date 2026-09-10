import { defineStore } from "pinia";
import { ref, computed } from "vue";

import * as api from "@/lib/commands";
import { errorMessage } from "@/lib/ipc";
import type { KeyProvider, KeyStatus } from "@/lib/api";

/**
 * 应用级状态：密钥锁状态与全局错误提示。
 *
 * 密钥状态决定界面能否显示凭据相关内容（D6）：
 * K2 主密码方式下，未解锁时凭据不可读，界面需引导解锁。
 */
export const useAppStore = defineStore("app", () => {
  const keyStatus = ref<KeyStatus | null>(null);
  const loading = ref(false);
  /** 全局错误提示（如后端不可达）；由布局层展示。 */
  const lastError = ref<string | null>(null);
  /** 全局成功提示。 */
  const lastNotice = ref<string | null>(null);

  const isUnlocked = computed(
    () => keyStatus.value?.unlocked ?? false,
  );
  const needsSetup = computed(
    () => keyStatus.value !== null && !keyStatus.value.initialized,
  );
  const needsUnlock = computed(
    () =>
      keyStatus.value !== null &&
      keyStatus.value.initialized &&
      !keyStatus.value.unlocked,
  );

  function notify(message: string) {
    lastNotice.value = message;
    setTimeout(() => {
      if (lastNotice.value === message) lastNotice.value = null;
    }, 3000);
  }

  function fail(e: unknown) {
    lastError.value = errorMessage(e);
    setTimeout(() => {
      lastError.value = null;
    }, 6000);
  }

  function clearError() {
    lastError.value = null;
  }

  async function refreshKeyStatus() {
    try {
      keyStatus.value = await api.keyStatus();
    } catch (e) {
      fail(e);
    }
  }

  async function initProvider(provider: KeyProvider, password?: string) {
    loading.value = true;
    try {
      keyStatus.value = await api.initKeyProvider(provider, password);
      notify("密钥保护方式已设置");
    } catch (e) {
      fail(e);
      throw e;
    } finally {
      loading.value = false;
    }
  }

  async function unlock(password: string) {
    loading.value = true;
    try {
      await api.unlockWithPassword(password);
      keyStatus.value = await api.keyStatus();
      notify("已解锁");
    } catch (e) {
      fail(e);
      throw e;
    } finally {
      loading.value = false;
    }
  }

  return {
    keyStatus,
    loading,
    lastError,
    lastNotice,
    isUnlocked,
    needsSetup,
    needsUnlock,
    notify,
    fail,
    clearError,
    refreshKeyStatus,
    initProvider,
    unlock,
  };
});
