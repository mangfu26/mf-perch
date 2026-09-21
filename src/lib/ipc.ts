/**
 * IPC 客户端：封装 Tauri invoke 与统一的错误处理。
 *
 * 后端所有命令都返回 `{ ok: true, data }` 或 `{ ok: false, code, message }`，
 * 因此前端只需在一处解包，业务代码拿到的永远是数据或抛出的错误。
 */
import { invoke } from "@tauri-apps/api/core";

/** 后端返回的统一结构（与 Rust 侧 `IpcResult` 对应）。 */
type IpcEnvelope<T> =
  | { ok: true; data: T }
  | { ok: false; code: string; message: string };

/** 携带后端错误码的错误，供界面做差异化提示。 */
export class IpcError extends Error {
  constructor(
    public code: string,
    message: string,
  ) {
    super(message);
    this.name = "IpcError";
  }
}

/** 解包后端返回；失败时抛出携带 `code` 的 `IpcError`。 */
function unwrap<T>(raw: IpcEnvelope<T>): T {
  if (raw && typeof raw === "object" && "ok" in raw) {
    // 只认布尔字面量：`ok` 一旦被序列化成字符串标签（如 `"err"`），
    // 真值判断会把后端错误当成成功。宁可报"格式无法识别"，也不给假的成功（P1 / P5）。
    if (raw.ok === true) return raw.data;
    if (raw.ok === false) throw new IpcError(raw.code, raw.message);
  }
  throw new IpcError("internal_error", "后端返回了无法识别的数据格式");
}

/** 调用后端命令并将结果解包。 */
export async function call<T>(
  command: string,
  args?: Record<string, unknown>,
): Promise<T> {
  try {
    const raw = await invoke<IpcEnvelope<T>>(command, args);
    return unwrap(raw);
  } catch (e) {
    // 后端返回结构化错误时已是 IpcError；其余情况（如命令不存在）包装为内部错误。
    if (e instanceof IpcError) throw e;
    const message = e instanceof Error ? e.message : String(e);
    throw new IpcError("internal_error", message);
  }
}

/** 把任意错误转为可展示的文案。 */
export function errorMessage(e: unknown): string {
  if (e instanceof IpcError) return e.message;
  if (e instanceof Error) return e.message;
  return String(e);
}

/** 把任意错误转为错误码（用于 i18n 映射）。 */
export function errorCode(e: unknown): string {
  if (e instanceof IpcError) return e.code;
  return "internal_error";
}
