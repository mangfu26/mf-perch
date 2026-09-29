/**
 * 文档预算与索引登记门禁 —— `docs/agent/docs.md` §4.3 规则 9 的机器执行者。
 *
 * 为什么需要它：代码有编译器与测试兜底，文档的"膨胀"没人拦。没有门禁的预算等于没有预算，
 * 所以把三条口径写成可执行检查（与 `check-secrets.mjs` / `check-ipc-contract.mjs` 同一风格：
 * 纯 node、跨平台、不依赖 bash、可重复执行）：
 *
 *   1. `AGENTS.md` 是**每个请求都会被加载**的常驻文件 → 长度上限 CORE_MAX；
 *   2. `docs/**` 单个 Markdown → 上限 DOC_MAX，超限必须进白名单并写明理由；
 *   3. `docs/` 下的 Markdown（`docs/decisions/**` 除外，它由 `docs/decisions.md` 自己索引）
 *      必须能被**文档索引**点中：既接受逐个文件登记，也接受登记所在目录（如 `docs/troubleshooting/`）。
 *
 * 判定口径是"字符数"（UTF-16 长度），与文档里写的数字同口径；行尾已由 `.gitattributes` 固定为 LF，
 * 所以这里不再处理 CRLF。
 */
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join, normalize, relative, sep } from "node:path";
import process from "node:process";

const ROOT = process.cwd();
const CORE_FILE = "AGENTS.md";
const CORE_MAX = 8000;
const DOC_MAX = 12000;

/** ADR 正文由 `docs/decisions.md` 索引，不重复登记（依据见 `docs/agent/docs.md` §4.2 A 类）。 */
const ADR_PREFIX = "docs/decisions/";

/** 登记来源：常驻核心 + 唯一文档索引（`docs/agent/docs.md` §4.2）。 */
const INDEX_FILES = ["AGENTS.md", "docs/agent/docs.md"];

/** 超限白名单：文件 → 为什么允许超限（`docs/agent/docs.md` §4.5 同样登记，两处必须一致）。 */
const WHITELIST = new Map([
  ["docs/mcp-tools.md", "MCP 契约：入参 schema 由代码导出，表格必须完整"],
  ["docs/design/sudo.md", "含必须保留的历史护栏与对外说辞边界"],
]);

const toPosix = (p) => p.split(sep).join("/");

function walkMarkdown(dir) {
  const out = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) out.push(...walkMarkdown(full));
    else if (entry.isFile() && entry.name.endsWith(".md")) out.push(toPosix(relative(ROOT, full)));
  }
  return out.sort();
}

/** 从索引文件里取出候选路径：Markdown 链接，以及反引号 / 裸提及的 `docs/...md`。 */
function extractCandidates(text) {
  const out = [];
  for (const m of text.matchAll(/\]\(([^)\s]+)\)/g)) out.push({ target: m[1], kind: "link" });
  for (const m of text.matchAll(/`([^`\n]+)`/g)) out.push({ target: m[1], kind: "mention" });
  for (const m of text.matchAll(/(?:^|[\s(（])(docs\/[A-Za-z0-9._/-]+\.md)/gm))
    out.push({ target: m[1], kind: "mention" });
  return out;
}

/** 登记集合：文件路径 + 目录前缀（目录行以 `/` 结尾，覆盖其下所有文件）。 */
function collectRegistrations() {
  const files = new Set();
  const dirs = new Set();
  for (const indexFile of INDEX_FILES) {
    const text = readFileSync(join(ROOT, indexFile), "utf8");
    for (const { target: rawTarget, kind } of extractCandidates(text)) {
      let target = rawTarget.trim().replace(/^<|>$/g, "");
      const hash = target.indexOf("#");
      if (hash >= 0) target = target.slice(0, hash);
      if (!target) continue;
      if (/^[a-z][a-z0-9+.-]*:/i.test(target)) continue; // 外部链接 / mailto
      const isDir = target.endsWith("/");
      // `docs/` 开头的写法是仓库相对路径；其余（`process.md`、`../decisions.md`）相对索引文件解析。
      const repoRelative =
        target.startsWith("docs/") || target === CORE_FILE
          ? target
          : toPosix(normalize(join(dirname(indexFile), target)));
      const cleaned = repoRelative.replace(/^\.\//, "");
      if (isDir) {
        // 只有**链接**形式的目录登记才算数。正文里顺口提到的 `` `docs/` `` 会把整棵树都算成
        // "已登记"，门禁随即失效——本项目的差分验证正是这样漏掉了一个未登记的文件。
        const dir = cleaned.endsWith("/") ? cleaned : `${cleaned}/`;
        if (kind === "link" && dir !== "docs/") dirs.add(dir);
      } else {
        files.add(cleaned);
      }
    }
  }
  return { files, dirs };
}

const problems = [];
const coreLength = readFileSync(join(ROOT, CORE_FILE), "utf8").length;
if (coreLength > CORE_MAX) {
  problems.push(
    `${CORE_FILE} 有 ${coreLength} 字符，超过常驻上限 ${CORE_MAX}（这是每个请求都会加载的文件）：` +
      "把「每次都可能用到」以外的正文搬进 `docs/agent/` 的按需手册，并在本文件留一行指针",
  );
}

const { files: registeredFiles, dirs: registeredDirs } = collectRegistrations();
const isRegistered = (doc) =>
  registeredFiles.has(doc) || [...registeredDirs].some((dir) => doc.startsWith(dir));

const docs = walkMarkdown(join(ROOT, "docs"));
const unregistered = [];
const oversized = [];
for (const doc of docs) {
  const length = readFileSync(join(ROOT, doc), "utf8").length;
  if (length > DOC_MAX && !WHITELIST.has(doc)) {
    oversized.push({ doc, length });
  }
  if (!doc.startsWith(ADR_PREFIX) && !isRegistered(doc)) {
    unregistered.push(doc);
  }
}

if (oversized.length > 0) {
  const lines = oversized.map(({ doc, length }) => `  ${doc}：${length} 字符（上限 ${DOC_MAX}）`);
  problems.push(
    `以下文档超过单文档上限：\n${lines.join("\n")}\n` +
      "  → 按主题拆片（参照 `docs/decisions/` 与 `docs/troubleshooting/` 的做法），" +
      "或在脚本与 `docs/agent/docs.md` §4.5 同时登记白名单理由",
  );
}

if (unregistered.length > 0) {
  problems.push(
    `以下文档没有被任何索引点中（等于不存在）：\n${unregistered.map((d) => `  ${d}`).join("\n")}\n` +
      "  → 在 `docs/agent/docs.md` §4.2 的对应分类里加一行（登记文件路径，或登记它所在目录）",
  );
}

const sizes = docs
  .map((doc) => ({ doc, length: readFileSync(join(ROOT, doc), "utf8").length }))
  .sort((a, b) => b.length - a.length);

console.log("文档门禁（口径见 docs/agent/docs.md §4.3 规则 9）");
console.log(
  `  常驻核心 ${CORE_FILE}: ${coreLength} / ${CORE_MAX} 字符${coreLength > CORE_MAX ? " ✗" : " ✓"}`,
);
console.log(`  docs/ 单文档上限: ${DOC_MAX} 字符（白名单 ${WHITELIST.size} 个，理由已登记）`);
console.log(
  `  索引登记: ${docs.length} 个 Markdown，未登记 ${unregistered.length} 个${
    unregistered.length === 0 ? " ✓" : " ✗"
  }`,
);
console.log(`  最大的五个文档：${sizes.slice(0, 5).map((s) => `${s.doc} (${s.length})`).join("、")}`);

if (problems.length > 0) {
  console.error("\n自检未通过：文档预算或索引登记有问题，请处理后再提交。\n");
  for (const p of problems) console.error(`- ${p}\n`);
  process.exit(1);
}

console.log("\n通过：文档预算与索引登记均符合规则。");
