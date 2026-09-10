import { defineStore } from "pinia";
import { ref } from "vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import { call } from "@/lib/ipc";

/**
 * sudo 提权请求的确认（Q33 ask 模式）。
 *
 * 后端在 Agent 执行 sudo 命令时把请求推到这里，由人类决定是否注入密码。
 * 超时（60 秒）由后端控制，超时即视为拒绝——因此不会出现
 * "无人确认却悄悄提权"。
 */
export interface SudoRequest {
  request_id: string;
  terminal_id: string;
  host_label: string;
}

/** 后端发出的事件名（与 Rust 侧 `EVENT_SUDO_REQUEST` 一致）。 */
const EVENT_SUDO_REQUEST = "sudo://request";

export const useSudoStore = defineStore("sudo", () => {
  /** 当前待处理的请求队列（可能同时有多个终端请求）。 */
  const queue = ref<SudoRequest[]>([]);
  /** 提交中标志，避免重复点击。 */
  const responding = ref(false);
  let unlisten: UnlistenFn | null = null;

  const current = () => queue.value[0] ?? null;

  /** 开始监听后端事件。应用启动时调用一次。 */
  async function startListening() {
    if (unlisten) return;
    unlisten = await listen<SudoRequest>(EVENT_SUDO_REQUEST, (event) => {
      // 同一请求只入队一次。
      if (!queue.value.some((r) => r.request_id === event.payload.request_id)) {
        queue.value.push(event.payload);
      }
    });
  }

  /** 停止监听（组件卸载或应用退出时）。 */
  function stopListening() {
    unlisten?.();
    unlisten = null;
  }

  /**
   * 提交决定。
   *
   * 无论后端是否接受（可能已超时），都从队列移除该请求——
   * 否则界面会卡在一个已失效的确认框上。
   */
  async function respond(request: SudoRequest, allow: boolean) {
    responding.value = true;
    try {
      await call<boolean>("sudo_respond", {
        requestId: request.request_id,
        allow,
      });
    } finally {
      queue.value = queue.value.filter((r) => r.request_id !== request.request_id);
      responding.value = false;
    }
  }

  return { queue, responding, current, startListening, stopListening, respond };
});
