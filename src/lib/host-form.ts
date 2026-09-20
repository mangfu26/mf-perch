/**
 * 主机表单的纯逻辑（不依赖 DOM，可单测，范围见 AGENTS.md §5.9）。
 */

/**
 * 归一化表单中的端口取值，非法时返回 `null`。
 *
 * `<input type="number">` 的 `value` 始终是字符串（`"2222"`），直接拿去
 * `Number.isInteger` 判定必然为假，用户填了合法端口也会被拦下；即使放过，
 * 字符串也过不了后端 `HostInput.port: u16` 的反序列化。故端口必须先过这里。
 * 合法区间与后端 `u16` 一致：1–65535 的整数。
 */
export function parsePort(raw: unknown): number | null {
  const n = typeof raw === "number" ? raw : Number(raw);
  return Number.isInteger(n) && n >= 1 && n <= 65535 ? n : null;
}
