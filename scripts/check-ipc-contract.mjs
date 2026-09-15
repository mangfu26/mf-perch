#!/usr/bin/env node
/**
 * IPC 契约检查：前端命令名 ↔ Rust 命令定义 ↔ Tauri 注册清单。
 *
 * 同一个命令名在仓库里有**三处**，任何一处单独改动都会造成**运行期**失败，
 * 而 `vue-tsc` 与 `cargo` 都查不出来（AGENTS.md §5.4 的"契约"一类）：
 *
 *   1. Rust 的 `#[tauri::command]` 函数**定义**；
 *   2. `tauri::generate_handler![...]` 的**注册清单**——
 *      **定义了但没注册 = 命令不可达**，这是 Tauri 最经典的静默坑，
 *      编译器不会报错，只有真正调用时才失败；
 *   3. 前端 `src/**` 里的 `call<T>("命令名")` **字符串**。
 *
 * 本脚本把三处对齐。退出码非 0 表示存在漂移，**不要提交**。
 *
 * 用法：node scripts/check-ipc-contract.mjs
 */

import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));

/** 递归收集指定扩展名的文件（跳过依赖与构建产物）。 */
function walk(dir, exts, out = []) {
  for (const entry of readdirSync(dir)) {
    if (entry === "node_modules" || entry === "target" || entry === ".git") continue;
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) walk(full, exts, out);
    else if (exts.some((e) => entry.endsWith(e))) out.push(full);
  }
  return out;
}

const show = (p) => relative(root, p).replace(/\\/g, "/");
const lineAt = (text, idx) => text.slice(0, idx).split("\n").length;

// ───────────────────────── 1) Rust：命令定义 ─────────────────────────
const rustFiles = walk(join(root, "src-tauri", "src"), [".rs"]);
const rustDefs = new Map();
const defRe = /#\[tauri::command[^\]]*\]\s*(?:pub\s+)?(?:async\s+)?fn\s+([A-Za-z0-9_]+)/g;
for (const f of rustFiles) {
  const text = readFileSync(f, "utf8");
  for (const m of text.matchAll(defRe)) {
    rustDefs.set(m[1], `${show(f)}:${lineAt(text, m.index)}`);
  }
}

// ───────────────────────── 2) Rust：注册清单 ─────────────────────────
const registered = new Map();
let handlerWhere = null;
for (const f of rustFiles) {
  const text = readFileSync(f, "utf8");
  const at = text.indexOf("generate_handler!");
  if (at === -1) continue;
  handlerWhere = show(f);
  const open = text.indexOf("[", at);
  const close = text.indexOf("]", open);
  // 注意：仓库里 .rs 是 CRLF 行尾。`\/\/.*$` 在这种文本上**匹配不到**——
  // 不带 `m` 标志时 `$` 只匹配字符串末尾，而 `.` 匹配不了 `\r`。
  // 因此这里按 `\r?\n` 切分，并用不带 `$` 的注释正则。
  for (const raw of text.slice(open + 1, close).split(/\r?\n/)) {
    const code = raw.replace(/\/\/.*/, "").trim(); // 去掉整行/行尾注释
    if (!code) continue;
    const name = code.replace(/,$/, "").split("::").pop().trim();
    if (name) registered.set(name, handlerWhere);
  }
  break;
}

// ───────────────────────── 3) 前端：调用点 ─────────────────────────
// 只看**生产**调用点：测试文件会用 `call()` 打桩（命令名是假的、invoke 被 mock），
// 把它们算进来会让检查在每写一个测试后误报，进而诱使人去削弱检查本身。
const feFiles = walk(join(root, "src"), [".ts", ".vue"]).filter(
  (f) => !/\.(test|spec)\.[cm]?[jt]sx?$/.test(f) && !/[\\/]__tests__[\\/]/.test(f),
);
const feCalls = new Map(); // 命令名 -> Set("文件:行")
const unresolved = []; // 无法静态解析调用点（命令名来自变量等）
const callRe = /call\s*(?:<[^(]*?>)?\s*\(\s*(["'])([A-Za-z0-9_]+)\1/g;
const anyCallRe = /\bcall\s*(?:<[^(]*?>)?\s*\(/g;

for (const f of feFiles) {
  const text = readFileSync(f, "utf8");
  const matchedAt = new Set();
  for (const m of text.matchAll(callRe)) {
    matchedAt.add(m.index);
    if (!feCalls.has(m[2])) feCalls.set(m[2], new Set());
    feCalls.get(m[2]).add(`${show(f)}:${lineAt(text, m.index)}`);
  }
  for (const m of text.matchAll(anyCallRe)) {
    if (matchedAt.has(m.index)) continue;
    // 跳过 `export async function call<T>(...)` 这个定义本身。
    if (/function\s*$/.test(text.slice(Math.max(0, m.index - 12), m.index))) continue;
    unresolved.push(`${show(f)}:${lineAt(text, m.index)}`);
  }
}

// ───────────────────────── 4) 比对 ─────────────────────────
const errors = [];
const warnings = [];

if (!handlerWhere) {
  errors.push("找不到 `tauri::generate_handler!` 注册清单，无法完成检查");
}

for (const [name, where] of rustDefs) {
  if (!registered.has(name)) {
    errors.push(
      `命令 \`${name}\` 已定义（${where}）但**未在 generate_handler! 中注册** → 前端调用必然失败`,
    );
  }
}

for (const [name, where] of registered) {
  if (!rustDefs.has(name)) {
    errors.push(
      `generate_handler! 注册了 \`${name}\`（${where}），但找不到对应的 #[tauri::command] 定义`,
    );
  }
}

for (const [name, places] of feCalls) {
  if (!registered.has(name)) {
    errors.push(
      `前端调用了 \`${name}\`（${[...places].join("、")}），但它不在已注册命令清单里 → 运行期 invoke 失败`,
    );
  }
}

const unusedByFrontend = [...registered.keys()].filter((n) => !feCalls.has(n));

// ───────────────────────── 5) 输出 ─────────────────────────
console.log("== IPC 契约检查 ==");
console.log(`  Rust 命令定义        : ${rustDefs.size}`);
console.log(`  generate_handler 注册: ${registered.size}`);
console.log(`  前端引用的命令名      : ${feCalls.size}`);

if (unusedByFrontend.length) {
  warnings.push(
    `已注册但前端未引用（可能是有意留待界面接入）：${unusedByFrontend.join("、")}`,
  );
}
if (unresolved.length) {
  warnings.push(
    `无法静态解析的调用点（命令名不是字符串字面量，请人工确认）：${unresolved.join("、")}`,
  );
}

if (warnings.length) {
  console.log("\n== 提示（不阻断）==");
  for (const w of warnings) console.log(`  · ${w}`);
}

if (errors.length) {
  console.log("\n== 发现漂移 ==");
  for (const e of errors) console.log(`  ✗ ${e}`);
  console.log(
    "\n命令名必须在三处保持一致：Rust 定义、generate_handler! 注册、前端 call 字符串。",
  );
  process.exit(1);
}

console.log("\n契约一致。");
