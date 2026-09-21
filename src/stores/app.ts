import { defineStore } from "pinia";
import { ref, computed } from "vue";

import * as api from "@/lib/commands";
import { errorMessage } from "@/lib/ipc";
import { deadlineFrom, pushToast, remainingMs } from "@/lib/toast";
import type { Toast, ToastKind } from "@/lib/toast";
import type { KeyProvider, KeyStatus } from "@/lib/api";

const NOTICE_TTL_MS = 3000;
const ERROR_TTL_MS = 6000;

/**
 * 应用级状态：密钥锁状态与全局提示。
 *
 * 密钥状态决定界面能否显示凭据相关内容（D6）：
 * K2 主密码方式下，未解锁时凭据不可读，界面需引导解锁。
 *
 * ## 全局提示是一条队列，不是两个单槽（Q38）
 *
 * 早期实现用 `lastError` / `lastNotice` 两个 ref 各存一条，在同一锚点各自计时，
 * 于是错误与成功提示会互相压住、后到的会把先到的直接顶掉——用户漏看提示。
 * 现在统一进 `toasts`：按到达顺序堆叠，超过上限挤掉最旧的一条（规则见 `lib/toast`）。
 *
 * 倒计时可被**悬停暂停**：`hold` 冻结、`release` 只补剩下的时长。
 * 长错误（如后端把整段排错指引塞进 message）在 6 秒内读不完，
 * 而错误提示上的关闭按钮需要一个能真正点到的时间窗口。
 */
export const useAppStore = defineStore("app", () => {
  const keyStatus = ref<KeyStatus | null>(null);
  const loading = ref(false);
  /** 当前可见的全局提示，按到达顺序排列（末位最新、贴住右下角）。 */
  const toasts = ref<Toast[]>([]);

  let seq = 0;
  // 计时器、到期时刻与暂停剩余量都不参与渲染，用普通 Map 持有，避免进响应式状态。
  const timers = new Map<number, ReturnType<typeof setTimeout>>();
  const deadlines = new Map<number, number>();
  const held = new Map<number, number>();

  function arm(id: number, ttl: number) {
    const pending = timers.get(id);
    if (pending !== undefined) clearTimeout(pending);
    deadlines.set(id, deadlineFrom(ttl, Date.now()));
    timers.set(id, setTimeout(dismiss, ttl, id));
  }

  function cancel(id: number) {
    const pending = timers.get(id);
    if (pending !== undefined) clearTimeout(pending);
    timers.delete(id);
    deadlines.delete(id);
    held.delete(id);
  }

  function push(kind: ToastKind, message: string, ttl: number) {
    const item: Toast = { id: ++seq, kind, message, ttl };
    const { list, dropped } = pushToast(toasts.value, item);
    // 被上限挤掉的条目顺带取消计时，别让已不在屏幕上的提示留着定时器空转。
    for (const gone of dropped) cancel(gone.id);
    toasts.value = list;
    arm(item.id, ttl);
  }

  function dismiss(id: number) {
    cancel(id);
    toasts.value = toasts.value.filter((t) => t.id !== id);
  }

  /** 鼠标移入：冻结倒计时。 */
  function hold(id: number) {
    if (held.has(id)) return;
    const deadline = deadlines.get(id);
    if (deadline === undefined) return;
    held.set(id, remainingMs(deadline, Date.now()));
    const pending = timers.get(id);
    if (pending !== undefined) clearTimeout(pending);
    timers.delete(id);
    deadlines.delete(id);
  }

  /** 鼠标移出：接着剩下的时长走，**不**重头等一个完整 ttl。 */
  function release(id: number) {
    const remaining = held.get(id);
    if (remaining === undefined) return;
    held.delete(id);
    arm(id, remaining);
  }

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
    push("notice", message, NOTICE_TTL_MS);
  }

  function fail(e: unknown) {
    push("error", errorMessage(e), ERROR_TTL_MS);
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
    toasts,
    isUnlocked,
    needsSetup,
    needsUnlock,
    notify,
    fail,
    dismiss,
    hold,
    release,
    refreshKeyStatus,
    initProvider,
    unlock,
  };
});
