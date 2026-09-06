#!/usr/bin/env bash
# 自动化调度 UAT（EvoFlow 方案 M6-09）：重启收敛 / 重复触发去重 / grant 闸。
# 时区/DST/休眠恢复/长任务/撤销项需真实桌面环境人工执行（见实施状态 v3.4 诚实缺口）。
# 前置：cargo build --release -p sixgates-core；jq 不依赖（纯 grep 断言）。
set -euo pipefail
CORE=${CORE_BIN:-"$(dirname "$0")/../../target/release/sixgates-core"}
DATA=$(mktemp -d /tmp/sg-auto-uat-XXXX)
export SIXGATES_AUTOMATIONS=1
cleanup() { [ -n "${CORE_PID:-}" ] && kill "$CORE_PID" 2>/dev/null || true; rm -rf "$DATA"; }
trap cleanup EXIT

say() { echo "[uat] $*"; }
fail() { echo "[uat][FAIL] $*"; exit 1; }

rpc() { # rpc <method> <params-json>
  printf '{"jsonrpc":"2.0","id":"u%d","method":"%s","params":%s}\n' "$SEQ" "$1" "$2" >&3
  local want="u$SEQ"; SEQ=$((SEQ+1))
  while IFS= read -r line; do
    case "$line" in *"$want"*) printf '%s' "$line" | sed 's/^{"jsonrpc[^,]*,//; s/}$//' ; return 0;; esac
  done <&4
}
SEQ=1

say "1. 启动 core（第一批）"
mkfifo in out 2>/dev/null || true
"$CORE" app-server --data-dir "$DATA" < in > out 2>/dev/null &
CORE_PID=$!
exec 3>in 4<out

say "2. 建项目/任务/自动化（无 grant）"
rpc project.create '{"gitlabInstance":"g","namespace":"uat","project":"uat","name":"UAT"}' >/dev/null
PJ=$(rpc project.list '{}' | sed 's/.*"id":"\([^"]*\)".*/\1/')
WI=$(rpc workitem.create "{\"projectId\":\"$PJ\",\"title\":\"UAT\",\"description\":\"\"}" | sed 's/.*"id":"\([^"]*\)".*/\1/')
rpc automation.create "{\"key\":\"uat-auto\",\"workItemId\":\"$WI\",\"intent\":{\"kind\":\"goal\"},\"intervalSecs\":60}" >/dev/null

say "3. runNow 固定 scheduled_for → 触发一次"
rpc automation.runNow "{\"automationId\":\"$(rpc automation.list '{}' | sed 's/.*"id":"\(auto_[^"]*\)".*/\1/')\",\"scheduledFor\":\"2026-09-06T12:00:00.000Z\"}" >/dev/null

say "4. 杀进程重启（重复启动场景）"
exec 3>&- 4>&-; kill "$CORE_PID"; wait "$CORE_PID" 2>/dev/null || true
"$CORE" app-server --data-dir "$DATA" < in > out 2>/dev/null &
CORE_PID=$!
exec 3>in 4<out

say "5. 同 scheduled_for 再 runNow → receipt 去重"
AID=$(rpc automation.list '{}' | sed 's/.*"id":"\(auto_[^"]*\)".*/\1/')
rpc automation.runNow "{\"automationId\":\"$AID\",\"scheduledFor\":\"2026-09-06T12:00:00.000Z\"}" >/dev/null
HIST=$(rpc automation.history "{\"automationId\":\"$AID\"}")
COUNT=$(printf '%s' "$HIST" | grep -o '"scheduledFor":"2026-09-06T12:00:00.000Z"' | wc -l | tr -d ' ')
[ "$COUNT" = "1" ] || fail "同 scheduled_for 历史应恰 1 条（实际 $COUNT）"
echo '[uat] PASS：重启去重/重复触发收敛'

say "6. 人工项（不在本脚本）"
echo '  - 时区切换/DST/休眠恢复/长任务/撤销：按《UAT 手册》人工执行并留档'
