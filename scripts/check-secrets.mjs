#!/usr/bin/env node
/**
 * 提交前安全自检（AGENTS.md §2.6 的要求，可重复执行）。
 *
 * 用法：
 *   pnpm check:secrets            # 推荐
 *   node scripts/check-secrets.mjs
 *
 * 退出码非 0 表示发现可疑内容，**不要提交**。
 *
 * ## 为什么是纯 node 脚本，而不是原来的 bash 脚本
 *
 * 原实现是 `scripts/check-secrets.sh`，由 `package.json` 以
 * `bash scripts/check-secrets.sh` 调用。在 Windows 上 `bash` 会解析到
 * **WSL 的 bash**（`C:\Windows\system32\bash.exe`），而 WSL 里没有 node，
 * 于是第 1 步（用 `node -e` 做的密钥扫描）**永远失败**；更糟的是原脚本把
 * 标准错误重定向进了 `/dev/null`，连"命令不存在"这个真实原因都被吞掉，
 * 只留下一句"扫描未能执行（node 不可用？）"。
 *
 * 一条**长期失败的安全红线门禁，等于没有门禁**——它会被当成噪声忽略。
 * 改为不依赖任何 shell 的 node 脚本后，任何平台、任何终端下结果一致。
 * 排错记录见 `docs/development-troubleshooting.md`。
 *
 * ## 检测精度
 *
 * 刻意做了精度控制：真实私钥的正文是长串 base64，而单元测试里的假占位
 * （如 "abc"）不具备该特征，避免误报导致红线失效。
 */

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

/** 仓库根目录（本脚本位于 `<root>/scripts/`）。 */
const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");

/** 真实私钥：`-----BEGIN ... PRIVATE KEY-----` 之后跟着足够长的 base64 正文。 */
const KEY_HEADER = /-----BEGIN [A-Z ]*PRIVATE KEY-----/;

/** 常见令牌形态。 */
const TOKEN_PATTERNS = [
  { name: "OpenAI 风格", re: /sk-[a-zA-Z0-9]{20,}/ },
  { name: "GitHub PAT", re: /ghp_[a-zA-Z0-9]{30,}/ },
  { name: "GitHub fine-grained PAT", re: /github_pat_[a-zA-Z0-9_]{30,}/ },
  { name: "AWS Access Key ID", re: /AKIA[0-9A-Z]{16}/ },
  { name: "Slack", re: /xox[baprs]-[a-zA-Z0-9-]{20,}/ },
];

/** 凭据类文件：被跟踪即视为问题。 */
const CREDENTIAL_FILE = /\.env$|\.env\.|\.pem$|\.key$|\.p12$|\.pfx$|id_rsa|id_ed25519|\.sqlite$|\.sqlite3$|\.db$|credentials\.json$/i;

/** 临时文件：被跟踪即视为问题。 */
const TEMP_FILE = /\.tmp$|^\.tmp\/|\.bak$|~$/;

/** 执行 git 命令，返回标准输出（失败时抛错）。 */
function git(args) {
  return execFileSync("git", args, {
    cwd: root,
    encoding: "utf8",
    // git 在管道下按 UTF-8 输出文件列表；显式关掉颜色与分页。
    env: { ...process.env, GIT_PAGER: "cat", NO_COLOR: "1" },
  });
}

/** 被 git 跟踪的文件列表（仓库相对路径，分隔符为 `/`）。 */
function trackedFiles() {
  return git(["ls-files"]).split("\n").filter(Boolean);
}

/**
 * 第 1 步：扫描真实密钥与令牌形态。
 *
 * 返回命中的描述行；空数组表示未发现。
 */
function scanSecrets(files) {
  const hits = [];

  for (const file of files) {
    let text;
    try {
      text = readFileSync(resolve(root, file), "utf8");
    } catch {
      continue; // 读不到（例如子模块）就跳过，不因此失败
    }
    if (text.includes("\0")) continue; // 二进制文件跳过

    const lines = text.split("\n");
    lines.forEach((line, i) => {
      for (const { name, re } of TOKEN_PATTERNS) {
        if (re.test(line)) {
          hits.push(`${file}:${i + 1}  [${name}] ${line.trim().slice(0, 100)}`);
        }
      }
      if (KEY_HEADER.test(line)) {
        // 判定为真实密钥的条件：同处起 40 行内存在足够长的 base64 正文。
        const window = lines.slice(i, i + 40).join("");
        const body = window
          .replace(/-----[A-Z ]+PRIVATE KEY-----/g, "")
          .replace(/\s/g, "");
        if (/^[A-Za-z0-9+/=]{40,}/.test(body)) {
          hits.push(`${file}:${i + 1}  疑似真实私钥（正文长度 ${body.length}）`);
        }
      }
    });
  }

  return hits;
}

/** 主流程：四步检查，返回进程退出码。 */
function main() {
  let fail = false;

  console.log("== 1/4 扫描真实密钥形态 ==");
  let files;
  try {
    files = trackedFiles();
  } catch (e) {
    // 明确失败，绝不静默通过：拿不到文件列表就无法证明"没有密钥"。
    console.error(`  无法获取 git 文件列表（是否在仓库内运行？）：${e.message}`);
    return 2;
  }
  const hits = scanSecrets(files);
  if (hits.length === 0) {
    console.log("  OK：未发现真实密钥或令牌形态");
  } else {
    console.log("发现疑似真实密钥/令牌：");
    for (const h of hits) console.log(`  ${h}`);
    fail = true;
  }

  console.log("== 2/4 检查凭据类文件是否被跟踪 ==");
  const credentialFiles = files.filter((f) => CREDENTIAL_FILE.test(f));
  if (credentialFiles.length === 0) {
    console.log("  OK：无凭据类文件被跟踪");
  } else {
    console.log("以下凭据类文件被跟踪，必须移除：");
    for (const f of credentialFiles) console.log(`  ${f}`);
    fail = true;
  }

  console.log("== 3/4 检查本地测试目录是否被忽略 ==");
  // git check-ignore 的退出码就是答案：0 = 已被忽略。
  let tmpTestIgnored = false;
  try {
    execFileSync("git", ["check-ignore", "-q", ".tmp-test"], {
      cwd: root,
      stdio: "ignore",
    });
    tmpTestIgnored = true;
  } catch {
    tmpTestIgnored = false;
  }
  if (tmpTestIgnored) {
    console.log("  OK：.tmp-test/ 已被 .gitignore 排除");
  } else {
    console.log("  警告：.tmp-test/ 未被忽略，可能包含测试私钥");
    fail = true;
  }

  console.log("== 4/4 检查临时文件是否被跟踪 ==");
  const tempFiles = files.filter((f) => TEMP_FILE.test(f));
  if (tempFiles.length === 0) {
    console.log("  OK：无临时文件被跟踪");
  } else {
    console.log("以下临时文件被跟踪：");
    for (const f of tempFiles) console.log(`  ${f}`);
    fail = true;
  }

  if (fail) {
    console.log();
    console.log("自检未通过：请处理上述问题后再提交（AGENTS.md §2.6 安全红线）。");
    return 1;
  }

  console.log();
  console.log("自检通过。");
  return 0;
}

process.exit(main());
