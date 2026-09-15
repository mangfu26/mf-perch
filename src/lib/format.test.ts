/**
 * `src/lib/format.ts` 的测试（AGENTS.md §5.9）。
 *
 * 纯函数、显式输入 + 精确期望输出（§5.4 的表驱动写法）。
 * `formatRelativeTime` 依赖当前时间，因此用假定时器把"现在"钉死，
 * 否则断言会随真实时间漂移。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { formatBytes, formatDateTime, formatDuration, formatRelativeTime } from "./format";

describe("formatDuration", () => {
  it("空值显示占位符", () => {
    expect(formatDuration(null)).toBe("—");
    expect(formatDuration(undefined)).toBe("—");
  });

  it("按量级选单位，边界取值精确", () => {
    const cases: [number, string][] = [
      [0, "0 ms"],
      [1, "1 ms"],
      [999, "999 ms"],
      [1000, "1.00 s"],
      [1500, "1.50 s"],
      [59_000, "59.00 s"],
      [60_000, "1m 0s"],
      [61_000, "1m 1s"],
      [125_000, "2m 5s"],
    ];
    for (const [ms, expected] of cases) {
      expect(formatDuration(ms), `${ms} ms 的格式化结果`).toBe(expected);
    }
  });
});

describe("formatBytes", () => {
  it("空值显示占位符", () => {
    expect(formatBytes(null)).toBe("—");
    expect(formatBytes(undefined)).toBe("—");
  });

  it("按量级选单位，边界取值精确", () => {
    const cases: [number, string][] = [
      [0, "0 B"],
      [1023, "1023 B"],
      [1024, "1.0 KB"],
      [1536, "1.5 KB"],
      [1024 * 1024, "1.0 MB"],
      [5 * 1024 * 1024, "5.0 MB"],
      [1024 * 1024 * 1024, "1.00 GB"],
      [2.5 * 1024 * 1024 * 1024, "2.50 GB"],
    ];
    for (const [bytes, expected] of cases) {
      expect(formatBytes(bytes), `${bytes} B 的格式化结果`).toBe(expected);
    }
  });
});

describe("formatRelativeTime", () => {
  const now = new Date("2026-09-14T12:00:00Z");

  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(now);
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  /** 相对"现在"生成 ISO 时间串。 */
  const ago = (ms: number) => new Date(now.getTime() - ms).toISOString();
  const SECOND = 1000;
  const MINUTE = 60 * SECOND;
  const HOUR = 60 * MINUTE;
  const DAY = 24 * HOUR;

  it("空值与无法解析的时间显示占位符（而不是 NaN 之类的脏数据）", () => {
    expect(formatRelativeTime(null)).toBe("—");
    expect(formatRelativeTime(undefined)).toBe("—");
    expect(formatRelativeTime("")).toBe("—");
    expect(formatRelativeTime("not-a-date")).toBe("—");
  });

  it("未来时间归为「刚刚」（时钟偏差不应显示成负数秒）", () => {
    expect(formatRelativeTime(new Date(now.getTime() + 5 * MINUTE).toISOString())).toBe("刚刚");
  });

  it("按跨度选文案，边界取值精确", () => {
    const cases: [number, string][] = [
      [0, "0 秒前"],
      [30 * SECOND, "30 秒前"],
      [59 * SECOND, "59 秒前"],
      [MINUTE, "1 分钟前"],
      [59 * MINUTE, "59 分钟前"],
      [HOUR, "1 小时前"],
      [23 * HOUR, "23 小时前"],
      [DAY, "1 天前"],
      [29 * DAY, "29 天前"],
    ];
    for (const [ms, expected] of cases) {
      expect(formatRelativeTime(ago(ms)), `${ms} ms 前`).toBe(expected);
    }
  });

  it("超过 30 天显示绝对日期，不再用「…前」的措辞", () => {
    const out = formatRelativeTime(ago(40 * DAY));
    expect(out).not.toBe("—");
    expect(out).not.toContain("前");
  });
});

describe("formatDateTime", () => {
  it("空值与无法解析的时间显示占位符", () => {
    expect(formatDateTime(null)).toBe("—");
    expect(formatDateTime("not-a-date")).toBe("—");
  });

  it("有效时间给出可读的绝对时刻（审计场景要精确到秒）", () => {
    // 不比对具体文案：toLocaleString 的输出随运行环境（ICU / 时区）变化，
    // 锁死它只会制造与环境相关的假失败。这里只守"不是占位符、包含年份"。
    const out = formatDateTime("2026-09-14T12:00:00Z");
    expect(out).not.toBe("—");
    expect(out).toContain("2026");
  });
});
