#!/usr/bin/env bash
# 提交前安全自检（AGENTS.md 2.6 的要求，可重复执行）
#
# 用法：bash scripts/check-secrets.sh
# 退出码非 0 表示发现可疑内容，**不要提交**。
#
# 检测逻辑刻意做了精度控制：真实私钥的正文是长串 base64，
# 而单元测试里的假占位（如 "abc"）不具备该特征，避免误报导致红线失效。

set -uo pipefail

fail=0
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

echo "== 1/4 扫描真实密钥形态 =="
# 用 node 做精确匹配：要求私钥正文具备足够长度的 base64 内容。
scan_result=$(node -e '
const { execSync } = require("child_process");
const fs = require("fs");

let files = [];
try {
  files = execSync("git ls-files", { encoding: "utf8" }).split("\n").filter(Boolean);
} catch { process.exit(2); }

// 真实私钥：头部之后跟着至少 40 个 base64 字符（跨行拼接后判断）。
const keyHeader = /-----BEGIN [A-Z ]*PRIVATE KEY-----/;
const tokenPatterns = [
  /sk-[a-zA-Z0-9]{20,}/,          // OpenAI 风格
  /ghp_[a-zA-Z0-9]{30,}/,         // GitHub PAT
  /github_pat_[a-zA-Z0-9_]{30,}/,
  /AKIA[0-9A-Z]{16}/,             // AWS Access Key ID
  /xox[baprs]-[a-zA-Z0-9-]{20,}/, // Slack
];

const hits = [];
for (const f of files) {
  let text;
  try { text = fs.readFileSync(f, "utf8"); } catch { continue; }
  if (text.includes("\0")) continue; // 二进制文件跳过

  text.split("\n").forEach((line, i) => {
    for (const p of tokenPatterns) {
      if (p.test(line)) hits.push(f + ":" + (i + 1) + "  " + line.trim().slice(0, 100));
    }
    if (keyHeader.test(line)) {
      // 判定为真实密钥的条件：同一行或后续内容中存在足够长的 base64 串。
      const window = text.split("\n").slice(i, i + 40).join("");
      const body = window.replace(/-----[A-Z ]+PRIVATE KEY-----/g, "").replace(/\s/g, "");
      if (/^[A-Za-z0-9+/=]{40,}/.test(body)) {
        hits.push(f + ":" + (i + 1) + "  疑似真实私钥（正文长度 " + body.length + "）");
      }
    }
  });
}

if (hits.length) { console.log(hits.join("\n")); process.exit(1); }
process.exit(0);
' 2>/dev/null)
scan_status=$?

if [ "$scan_status" -eq 0 ]; then
  echo "  OK：未发现真实密钥或令牌形态"
elif [ "$scan_status" -eq 1 ]; then
  echo "发现疑似真实密钥/令牌："
  echo "$scan_result"
  fail=1
else
  echo "  警告：扫描未能执行（node 不可用？），请人工确认无凭据入库"
  fail=1
fi

echo "== 2/4 检查凭据类文件是否被跟踪 =="
tracked=$(git ls-files | grep -iE '\.env$|\.env\.|\.pem$|\.key$|\.p12$|\.pfx$|id_rsa|id_ed25519|\.sqlite$|\.sqlite3$|\.db$|credentials\.json$' || true)
if [ -n "$tracked" ]; then
  echo "以下凭据类文件被跟踪，必须移除："
  echo "$tracked"
  fail=1
else
  echo "  OK：无凭据类文件被跟踪"
fi

echo "== 3/4 检查本地测试目录是否被忽略 =="
if [ -d .tmp-test ]; then
  if git check-ignore -q .tmp-test 2>/dev/null; then
    echo "  OK：.tmp-test/ 已被 .gitignore 排除"
  else
    echo "  警告：.tmp-test/ 存在但未被忽略，可能包含测试私钥"
    fail=1
  fi
else
  echo "  OK：无本地测试目录"
fi

echo "== 4/4 检查临时文件是否被跟踪 =="
tmp=$(git ls-files | grep -E '\.tmp$|^\.tmp/|\.bak$|~$' || true)
if [ -n "$tmp" ]; then
  echo "以下临时文件被跟踪："
  echo "$tmp"
  fail=1
else
  echo "  OK：无临时文件被跟踪"
fi

if [ "$fail" -ne 0 ]; then
  echo
  echo "自检未通过：请处理上述问题后再提交（AGENTS.md 2.6 安全红线）。"
  exit 1
fi

echo
echo "自检通过。"
