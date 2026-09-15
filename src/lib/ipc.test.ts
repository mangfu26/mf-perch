/**
 * `src/lib/ipc.ts` 的测试（AGENTS.md §5.9）。
 *
 * 守的是**跨语言契约**：后端所有命令都返回 `{ ok, data }` / `{ ok, code, message }`
 * 这个信封，而两侧的类型都是**手写**的。`vue-tsc` 只能查类型，
 * 查不出"后端某天返回了裸值"——一旦失配，**所有**命令都会报 internal_error。
 * 所以这里针对每种信封形态给出精确断言。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

// ipc.ts 在模块顶层 import 了 @tauri-apps/api/core，真实实现依赖 WebView 环境。
// vi.hoisted 保证 mock 工厂能引用到它（vi.mock 会被提升到 import 之前）。
const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { IpcError, call, errorCode, errorMessage } from "./ipc";

/** 捕获拒绝原因；成功时抛错，避免"以为在测失败路径、其实没失败"。 */
async function rejection(p: Promise<unknown>): Promise<IpcError> {
  try {
    await p;
  } catch (e) {
    return e as IpcError;
  }
  throw new Error("期望该调用失败，但它成功了");
}

beforeEach(() => {
  invoke.mockReset();
});

describe("call：解包后端信封", () => {
  it("成功信封返回 data", async () => {
    invoke.mockResolvedValue({ ok: true, data: { id: "h1" } });
    await expect(call("list_hosts")).resolves.toEqual({ id: "h1" });
    expect(invoke).toHaveBeenCalledWith("list_hosts", undefined);
  });

  it("失败信封抛出带 code 的 IpcError（界面据此做差异化提示）", async () => {
    invoke.mockResolvedValue({
      ok: false,
      code: "terminal_quota_exceeded",
      message: "终端数量已达上限",
    });
    const err = await rejection(call("create_terminal"));
    expect(err).toBeInstanceOf(IpcError);
    expect(err.code).toBe("terminal_quota_exceeded");
    expect(err.message).toBe("终端数量已达上限");
  });

  it("返回裸值（不是信封）时按内部错误处理，绝不静默当数据用", async () => {
    // 这正是类型检查抓不到的运行期失配场景。
    invoke.mockResolvedValue("bare-value");
    const err = await rejection(call("key_status"));
    expect(err).toBeInstanceOf(IpcError);
    expect(err.code).toBe("internal_error");
  });

  it("ok 为真但缺 data 时返回 undefined，而不是抛错", async () => {
    invoke.mockResolvedValue({ ok: true });
    await expect(call("delete_host")).resolves.toBeUndefined();
  });

  it("invoke 抛普通 Error：包装为 internal_error 并保留原文案", async () => {
    invoke.mockRejectedValue(new Error("command not found"));
    const err = await rejection(call("nope"));
    expect(err).toBeInstanceOf(IpcError);
    expect(err.code).toBe("internal_error");
    expect(err.message).toBe("command not found");
  });

  it("invoke 抛非 Error：也要包装，不能把原始值漏给界面", async () => {
    invoke.mockRejectedValue("plain string");
    const err = await rejection(call("nope"));
    expect(err).toBeInstanceOf(IpcError);
    expect(err.message).toBe("plain string");
  });

  it("已是 IpcError 时原样透传，不被重复包装（错误码不能丢）", async () => {
    const original = new IpcError("terminal_archived", "终端已归档");
    invoke.mockRejectedValue(original);
    await expect(call("run_command")).rejects.toBe(original);
  });

  it("参数按原样透传给 invoke", async () => {
    invoke.mockResolvedValue({ ok: true, data: null });
    await call("save_host", { input: { id: "h1" } });
    expect(invoke).toHaveBeenCalledWith("save_host", { input: { id: "h1" } });
  });
});

describe("errorMessage / errorCode", () => {
  it("IpcError：分别取 message 与 code", () => {
    const e = new IpcError("ssh_auth", "认证失败");
    expect(errorMessage(e)).toBe("认证失败");
    expect(errorCode(e)).toBe("ssh_auth");
  });

  it("普通 Error：message 取原文，code 归为 internal_error", () => {
    const e = new Error("boom");
    expect(errorMessage(e)).toBe("boom");
    expect(errorCode(e)).toBe("internal_error");
  });

  it("非 Error 值：字符串化后可展示，code 归为 internal_error", () => {
    expect(errorMessage(42)).toBe("42");
    expect(errorCode(42)).toBe("internal_error");
  });
});
