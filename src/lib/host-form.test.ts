/**
 * `src/lib/host-form.ts` 的测试（AGENTS.md §5.9：只测 src/lib 纯逻辑）。
 *
 * 守的是"添加/编辑主机"这条用户路径上的契约：数字输入框回传的是 DOM 字符串，
 * 表单必须把它归一成 number 才能提交——否则合法端口会被前端校验拦下
 * （v0.1.0 的"无法添加 SSH 主机"），即使放过也过不了后端 `port: u16` 的反序列化。
 */
import { describe, expect, it } from "vitest";

import { parsePort } from "./host-form";

describe("parsePort", () => {
  it("DOM 字符串形式的合法端口归一为数字（v0.1.0 回归）", () => {
    // 断言用 toBe 而非 toEqual：返回字符串 "2222" 时它同样失败，
    // 因此这里同时锁住了"值正确"与"是 number"两件事。
    const cases: [unknown, number][] = [
      ["2222", 2222],
      ["22", 22],
      ["1", 1],
      ["65535", 65535],
      [22, 22],
      [2222, 2222],
    ];
    for (const [raw, expected] of cases) {
      expect(parsePort(raw), `${JSON.stringify(raw)} 应归一为 ${expected}`).toBe(expected);
    }
  });

  it("越界、非整数与空值一律判非法", () => {
    const cases: unknown[] = [
      "0",
      "-1",
      "65536",
      "70000",
      "22.5",
      "",
      "   ",
      "abc",
      null,
      undefined,
      Number.NaN,
    ];
    for (const raw of cases) {
      expect(parsePort(raw), `${JSON.stringify(raw)} 应判为非法端口`).toBeNull();
    }
  });
});
