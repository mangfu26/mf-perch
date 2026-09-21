/**
 * `src/lib/toast.ts` 的测试（AGENTS.md §5.9：只测 src/lib 纯逻辑）。
 *
 * 守的是 Q38 的两个用户可见缺陷，都是"提示看不见"：
 * 1. **单槽互相遮盖 / 后到的顶掉先到的** → 现在是一条堆叠队列，
 *    断言"顺序 = 到达顺序""超上限只挤掉最旧的一条"；
 * 2. **长提示读不完就消失** → 悬停暂停、移开续 remaining，
 *    断言"暂停不丢失剩余时长""移开不重头等一个完整 ttl"。
 */
import { describe, expect, it } from "vitest";

import {
  MAX_VISIBLE,
  deadlineFrom,
  pushToast,
  remainingMs,
} from "./toast";
import type { Toast } from "./toast";

function toast(id: number, message = `m${id}`): Toast {
  return { id, kind: "notice", message, ttl: 3000 };
}

function ids(list: Toast[]): number[] {
  return list.map((t) => t.id);
}

describe("pushToast", () => {
  it("未达上限时按到达顺序追加，新提示排在末尾（贴住右下角）", () => {
    const first = pushToast([], toast(1));
    const second = pushToast(first.list, toast(2));

    expect(ids(second.list), "最新一条应在末尾").toEqual([1, 2]);
    expect(second.dropped, "未达上限不该丢提示").toEqual([]);
  });

  it("达到上限只挤掉最旧的一条，其余保持到达顺序", () => {
    const seeded = [toast(1), toast(2), toast(3), toast(4)];
    expect(seeded.length, "用例前提：已占满上限").toBe(MAX_VISIBLE);

    const { list, dropped } = pushToast(seeded, toast(5));

    expect(ids(list), "可见列应滑掉最旧、留下最新四条").toEqual([2, 3, 4, 5]);
    expect(ids(dropped), "被挤掉的必须是且仅是最旧那条").toEqual([1]);
  });

  it("连续灌入不会让可见条数越过上限", () => {
    let list: Toast[] = [];
    for (let i = 1; i <= 30; i++) {
      const r = pushToast(list, toast(i));
      // 每一步都必须正好挤掉一条（第一步除外），否则丢提示是静默发生的。
      expect(r.dropped.length, `第 ${i} 条的丢弃数不符`).toBe(
        i <= MAX_VISIBLE ? 0 : 1,
      );
      list = r.list;
      expect(list.length, `第 ${i} 条之后可见数越过上限`).toBeLessThanOrEqual(
        MAX_VISIBLE,
      );
    }
    expect(ids(list), "最终可见的应是最后四条").toEqual([27, 28, 29, 30]);
  });
});

describe("悬停暂停与续时", () => {
  it("暂停记下剩余时长，移开后只补剩下的，不重头等一个完整 ttl", () => {
    const ttl = 6000;
    const deadline = deadlineFrom(ttl, 1000); // 1000ms 时排到 7000ms 到期

    // 用户在 4000ms 悬停：还剩 3000ms。
    const remaining = remainingMs(deadline, 4000);
    expect(remaining, "剩余时长算错").toBe(3000);

    // 移开鼠标（6000ms）：到期时刻应推后恰好"剩下的 3000ms"，而不是重头 6000ms。
    expect(deadlineFrom(remaining, 6000), "移开后重新等满整个 ttl 了").toBe(9000);
  });

  it("多次悬停累计不丢失剩余时长", () => {
    const deadline = deadlineFrom(6000, 0);
    const first = remainingMs(deadline, 2000); // 剩 4000
    const afterFirst = deadlineFrom(first, 2000); // 回到 6000
    const second = remainingMs(afterFirst, 3500); // 又悬停一次，剩 2500

    expect(second, "两次暂停之间时长被凭空续上了").toBe(2500);
    expect(deadlineFrom(second, 5000)).toBe(7500);
  });

  it("已到期不返回负数剩余", () => {
    // 定时器回调尚未跑完时 now 可能已越过 deadline：钳成 0 表示"立刻该消失"。
    expect(remainingMs(5000, 8000), "剩余为负会让提示永不消失").toBe(0);
    expect(remainingMs(5000, 5000)).toBe(0);
  });
});
