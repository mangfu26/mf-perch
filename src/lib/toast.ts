/**
 * 全局提示（右下角 toast）的纯逻辑：可见条数与悬停暂停的时长换算。
 *
 * 抽成纯函数有两个原因：
 * 1. 这两处正是缺陷的成因所在——**单槽互相遮盖**（漏看提示）与
 *    **固定时长到点即消失**（长提示读不完），必须能被测试区分对错（§5.9 只测 `src/lib/`）；
 * 2. 时长换算依赖"当前时刻"，把 `now` 做成入参才是确定性的（§5.4 表驱动）。
 *
 * 计时器本体在 store 里（`src/stores/app.ts`），这里只定义规则。
 */

export type ToastKind = "notice" | "error";

export interface Toast {
  id: number;
  kind: ToastKind;
  message: string;
  /** 自动消失时长（ms）。 */
  ttl: number;
}

/**
 * 同时可见的提示条数上限。
 *
 * 不设上限时，批量操作（如逐个删除主机、事件推送的重连通知）会把提示叠到
 * 盖住整个内容区；挤掉最旧的一条，是因为用户正在读的一定是最新的。
 */
export const MAX_VISIBLE = 4;

export interface PushResult {
  list: Toast[];
  /** 因超出上限被挤掉的条目（调用方须取消其计时器，否则会"幽灵消失"）。 */
  dropped: Toast[];
}

/** 追加最新一条到末尾（视觉上贴住右下角），超出上限则从最旧的一端挤掉。 */
export function pushToast(list: Toast[], next: Toast, max = MAX_VISIBLE): PushResult {
  const merged = [...list, next];
  const overflow = Math.max(0, merged.length - max);
  return {
    list: merged.slice(overflow),
    dropped: merged.slice(0, overflow),
  };
}

/**
 * 悬停暂停：到期时刻 `deadline` 在 `now` 时刻还剩多久。
 *
 * 下限 0：定时器已排到点但回调尚未跑完时，剩余是负数，钳成 0 表示"该走了"。
 */
export function remainingMs(deadline: number, now: number): number {
  return Math.max(0, deadline - now);
}

/**
 * 悬停结束：以暂停时剩下的时长重排到期时刻。
 *
 * 这里**刻意不用 `toast.ttl`**——挪开鼠标又等一整个时长，等于让悬停无限续命，
 * 提示会永远赖在屏幕上。
 */
export function deadlineFrom(remaining: number, now: number): number {
  return now + remaining;
}
