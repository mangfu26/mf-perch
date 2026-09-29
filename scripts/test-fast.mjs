/**
 * 按本次改动挑选受影响的 cargo 测试模块 —— `docs/agent/testing.md` §5.10 / §5.11 的"日常主力"。
 *
 * 用法：
 *   pnpm test:fast                    计算并执行
 *   pnpm test:fast -- --dry           只打印将要执行的命令（不编译、不跑测试）
 *   pnpm test:fast -- --base <ref>    指定比较基线（默认 main）
 *
 * 选择规则（**宁多勿漏**：拿不准就退回全 lib，因为漏跑比多跑贵得多）：
 *   - `src-tauri/src/<a>/<b>.rs` → 过滤 `<a>::<b>`（模块名就是测试路径前缀）
 *   - `src-tauri/src/<x>.rs`     → 过滤 `<x>`（`<x>/mod.rs` 同理收敛为 `<x>`）
 *   - 跨切文件（`lib.rs` / `state.rs` / `error.rs` / `Cargo.toml` / `build.rs` / `tests/**` 等）→ 全 lib
 *   - 只改前端 → 提示 `pnpm test` + `pnpm typecheck`（这里不代为执行：门禁清单见 §5.10）
 *   - 只改文档 → 提示 `pnpm check:docs`
 *   - 只改 workflow → 本地无法验证，提示推送后看 Actions
 */
import { execFileSync } from "node:child_process";
import process from "node:process";

const args = process.argv.slice(2);
const dryRun = args.includes("--dry");
const baseIndex = args.indexOf("--base");
const BASE = baseIndex >= 0 && args[baseIndex + 1] ? args[baseIndex + 1] : "main";

/** 单个文件变了就必须跑全 lib 的"跨切"清单。 */
const CROSS_CUTTING_FILES = new Set([
  "src-tauri/src/lib.rs",
  "src-tauri/src/state.rs",
  "src-tauri/src/error.rs",
  "src-tauri/src/settings.rs",
  "src-tauri/Cargo.toml",
  "src-tauri/build.rs",
  "src-tauri/tauri.conf.json",
  "Cargo.lock",
]);
const CROSS_CUTTING_PREFIXES = ["src-tauri/tests/", ".cargo/"];

const FRONTEND_FILES = new Set([
  "package.json",
  "pnpm-lock.yaml",
  "vitest.config.ts",
  "vite.config.ts",
  "tsconfig.json",
  "tsconfig.node.json",
  "index.html",
]);

/** 执行 git 命令并返回按行拆分的输出；失败时返回 null（由调用方决定如何降级）。 */
function gitLines(...gitArgs) {
  try {
    return execFileSync("git", gitArgs, { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] })
      .split("\n")
      .map((line) => line.trim())
      .filter(Boolean);
  } catch {
    return null;
  }
}

const changed = [...new Set([...(gitLines("diff", "--name-only", BASE) ?? []), ...(gitLines("ls-files", "--others", "--exclude-standard") ?? [])])]
  .map((p) => p.split("\\").join("/"));

if (changed.length === 0) {
  console.log(`没有检测到相对 ${BASE} 的改动（含未跟踪文件）。`);
  console.log("如果分支基线不是 main，可显式指定：pnpm test:fast -- --base <ref>");
  process.exit(0);
}

const cargoFilters = new Set();
const frontendChanges = [];
const docChanges = [];
const workflowChanges = [];
let crossCutting = null;

for (const file of changed) {
  if (CROSS_CUTTING_FILES.has(file) || CROSS_CUTTING_PREFIXES.some((p) => file.startsWith(p))) {
    crossCutting ??= file;
    continue;
  }
  if (file.startsWith("src-tauri/src/") && file.endsWith(".rs")) {
    const parts = file.slice("src-tauri/src/".length, -3).split("/");
    if (parts[parts.length - 1] === "mod") parts.pop();
    if (parts.length === 0) {
      crossCutting ??= file;
      continue;
    }
    cargoFilters.add(parts.join("::"));
    continue;
  }
  // Rust 侧的其它文件（例如新增的构建脚本、图标、tauri 配置）：保守起见跑全 lib。
  if (file.startsWith("src-tauri/")) {
    crossCutting ??= file;
    continue;
  }
  if (file.startsWith("src/") || FRONTEND_FILES.has(file)) {
    frontendChanges.push(file);
    continue;
  }
  if (file.startsWith("docs/") || file.endsWith(".md")) {
    docChanges.push(file);
    continue;
  }
  if (file.startsWith(".github/workflows/")) {
    workflowChanges.push(file);
    continue;
  }
  // 其余（`.gitattributes`、`LICENSE`、`public/**` 等）不触发测试。
}

const cargoArgs = [
  "test",
  "-j",
  "2",
  "--lib",
  "--manifest-path",
  "src-tauri/Cargo.toml",
  ...(crossCutting ? [] : [...cargoFilters].sort()),
];

console.log(`相对 ${BASE} 的改动共 ${changed.length} 个文件。`);
if (crossCutting) {
  console.log(`  跨切改动 ${crossCutting} → 跑**全 lib**（不为单个模块挑过滤词）`);
} else if (cargoFilters.size > 0) {
  console.log(`  受影响模块 → ${[...cargoFilters].sort().join("、")}`);
}
if (frontendChanges.length > 0) {
  console.log(`  前端改动 ${frontendChanges.length} 个 → 另需（本脚本不代为执行）：pnpm test && pnpm typecheck`);
  if (frontendChanges.some((f) => f.includes("/lib/ipc") || f === "package.json")) {
    console.log("  契约相关改动 → 另需：pnpm check:ipc");
  }
}
if (docChanges.length > 0) console.log(`  文档改动 ${docChanges.length} 个 → 另需：pnpm check:docs`);
if (workflowChanges.length > 0) {
  console.log(`  workflow 改动 ${workflowChanges.length} 个 → 本地无法验证，推送后看 GitHub Actions`);
}

const hasCargoWork = Boolean(crossCutting) || cargoFilters.size > 0;
if (!hasCargoWork) {
  console.log("本次改动不涉及 Rust 代码，无需跑 cargo test。");
  process.exit(0);
}

const printable = `cargo ${cargoArgs.join(" ")}`;
console.log(`\n将要执行：${printable}`);
if (dryRun) {
  console.log("（--dry：只打印，不执行）");
  process.exit(0);
}

try {
  execFileSync("cargo", cargoArgs, { stdio: "inherit" });
} catch (error) {
  // 子进程的退出码即判定结果；这里只报错、不吞掉。
  const code = typeof error.status === "number" ? error.status : 1;
  console.error(`\n失败：${printable}（退出码 ${code}）`);
  process.exit(code);
}
