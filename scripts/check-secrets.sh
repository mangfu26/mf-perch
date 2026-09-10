#!/usr/bin/env bash
# 提交前安全自检（AGENTS.md 2.6 的要求，可重复执行）
#
# 用法：bash scripts/check-secrets.sh
# 退出码非 0 表示发现可疑内容，**不要提交**。

set -uo pipefail

fail=0
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

echo "== 1/4 扫描真实密钥形态 =="
# 只扫描将被提交的文件；命中真实密钥形态即报警。
hits=$(git grep -inE \
  'sk-[a-zA-Z0-9]{20,}|ghp_[a-zA-Z0-9]{20,}|AKIA[0-9A-Z]{16}|-----BEGIN [A-Z ]*PRIVATE KEY-----' \
  -- . 2>/dev/null | grep -vE 'docs/|\.md:' || true)
if [ -n "$hits" ]; then
  echo "发现疑似真实密钥："
  echo "$hits"
  fail=1
else
  echo "  OK：未发现真实密钥形态"
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
tmp=$(git ls-files | grep -E '\.tmp$|\.tmp/|\.bak$|~$' || true)
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
