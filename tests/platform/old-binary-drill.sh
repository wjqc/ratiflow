#!/usr/bin/env bash
# 旧二进制迁移演练（RDWS 审计 §9.2 migration-old-binary-drill：
# "0049→0055、旧包拒启、快照恢复"）：
#   场景一（前进迁移）：PRE_RDWS_COMMIT（0049-era）建库 → OLD_COMMIT 包打开并
#     把库前进迁移到自身上限（0049→0050+），启动正常、共享域可读。
#   场景二（拒启）：工作树新包（schema 上限最高）建库 → 0049-era 旧包打开——
#     必须显式拒绝启动（"schema version above supported maximum"），禁止旧包
#     原地打开未来 schema（§10.3）。
#   场景三（快照恢复）：旧包建库 → 快照拷贝 → 新包迁移前进 → 恢复快照 →
#     旧包可再次启动（回退走迁移前快照，不反向降库；重开即 no-op）。
# 用法：tests/platform/old-binary-drill.sh [OLD_COMMIT] [PRE_RDWS_COMMIT]
#   OLD_COMMIT      默认 HEAD（认知 0050+ 的"新包"，承担场景一的前进迁移）
#   PRE_RDWS_COMMIT 默认 8837371~1（最后一个 0049-era 提交；RDWS 0050+ 之前）
set -euo pipefail
cd "$(dirname "$0")/../.."

OLD_COMMIT=${1:-HEAD}
PRE_RDWS_COMMIT=${2:-8837371~1}
NEW_CORE="target/release/ratiflow-core"
if [[ ! -x "$NEW_CORE" ]]; then
  echo "先构建新 core：cargo build --release -p ratiflow-core" >&2
  exit 1
fi

WORK=$(mktemp -d /tmp/sg-drill-XXXXXX)
trap 'git worktree remove --force "$WORK/src" 2>/dev/null || true; git worktree remove --force "$WORK/prerdws" 2>/dev/null || true; rm -rf "$WORK"' EXIT

# 在指定 worktree 构建旧 core，输出二进制路径。
build_old() {
  local wt=$1 commit=$2
  git worktree add --detach "$wt" "$commit" >/dev/null
  (cd "$wt" && CARGO_TARGET_DIR="$WORK/target-$(basename "$wt")" cargo build --release -p ratiflow-core)
  echo "$WORK/target-$(basename "$wt")/release/ratiflow-core"
}

# 启动 core 并执行一段 RPC 脚本；脚本负责断言。输出进程 stderr 供失败诊断。
run_core() {
  local core=$1 data=$2 script=$3
  python3 - "$core" "$data" "$script" << 'PY'
import json, subprocess, sys
core, data, script = sys.argv[1], sys.argv[2], sys.argv[3]
p = subprocess.Popen([core, 'app-server', '--data-dir', data],
                     stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
nid = [0]
def call(method, params):
    nid[0] += 1
    p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": str(nid[0]), "method": method, "params": params}) + '\n')
    p.stdin.flush()
    while True:
        line = p.stdout.readline()
        if not line:
            raise RuntimeError(f'core exited before responding to {method}')
        m = json.loads(line)
        if m.get('id') == str(nid[0]):
            return m
exec(script)
p.kill()
PY
}

echo "[1/6] worktree @ ${PRE_RDWS_COMMIT}（0049-era 建库包）"
PRE_CORE=$(build_old "$WORK/prerdws" "$PRE_RDWS_COMMIT")

echo "[2/6] 0049-era 包建库（前进迁移起点）"
DATA=$(mktemp -d /tmp/sg-drill-data-XXXXXX)
run_core "$PRE_CORE" "$DATA" '
call("project.create", {"gitlabInstance": "x", "namespace": "n", "project": "drill", "name": "Drill"})
'

echo "[3/6] 场景一：OLD_COMMIT 包打开 0049 库 → 前进迁移 0049→0050+ 且共享域可读"
OLD_CORE=$(build_old "$WORK/src" "$OLD_COMMIT")
run_core "$OLD_CORE" "$DATA" '
hello = call("core.version", {})
schema = hello["result"]["schemaVersion"]
assert schema >= 50, "OLD_COMMIT 包应把库前进迁移到 0050+（实际 %s）" % schema
projects = call("project.list", {})
assert projects["result"]["items"], "迁移后共享域数据可读"
print("PASS：0049→%s 前进迁移成功，共享域可读" % schema)
'

echo "[4/6] 场景二准备：工作树新包建库（schema 上限最高——含未发布迁移）"
DATA2=$(mktemp -d /tmp/sg-drill-data2-XXXXXX)
run_core "$NEW_CORE" "$DATA2" '
call("project.create", {"gitlabInstance": "x", "namespace": "n", "project": "drill2", "name": "Drill2"})
'

echo "[5/6] 场景二：0049-era 旧包打开未来 schema 库必须拒启"
REFUSE_OUT=$(python3 - "$PRE_CORE" "$DATA2" << 'PY'
import json, subprocess, sys
core, data = sys.argv[1], sys.argv[2]
p = subprocess.Popen([core, 'app-server', '--data-dir', data],
                     stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
try:
    p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": "1", "method": "core.version", "params": {}}) + '\n')
    p.stdin.flush()
except BrokenPipeError:
    pass
try:
    out, err = p.communicate(timeout=15)
except subprocess.TimeoutExpired:
    p.kill()
    out, err = p.communicate()
print("EXIT:" + str(p.returncode))
print("STDOUT_HEAD:" + (out[:80] if out else "<empty>"))
print("STDERR:" + err.strip()[:400])
PY
)
echo "$REFUSE_OUT"
if ! echo "$REFUSE_OUT" | grep -q "^EXIT:1"; then
  echo "✕ 0049-era 旧包必须以非零码拒启" >&2
  exit 1
fi
if ! echo "$REFUSE_OUT" | grep -q "schema version above supported maximum"; then
  echo "✕ 拒启原因应含未来 schema 守卫文案" >&2
  exit 1
fi
echo "PASS：0049-era 旧包对 0050+ 库拒启（未来 schema 守卫）"

echo "[6/6] 场景三：快照恢复——旧包建库 → 新包迁移前进 → 恢复快照 → 旧包可启动"
DATA3=$(mktemp -d /tmp/sg-drill-data3-XXXXXX)
run_core "$PRE_CORE" "$DATA3" '
call("project.create", {"gitlabInstance": "x", "namespace": "n", "project": "snap", "name": "Snap"})
'
SNAP=$(mktemp -d /tmp/sg-drill-snap-XXXXXX)
cp -R "$DATA3"/. "$SNAP"/
run_core "$NEW_CORE" "$DATA3" '
v = call("core.version", {})
assert v["result"]["schemaVersion"] >= 50, "新包应已把库迁移到 0050+"
'
rm -rf "$DATA3"
cp -R "$SNAP"/ "$(dirname "$DATA3")/$(basename "$DATA3")"
run_core "$PRE_CORE" "$DATA3" '
projects = call("project.list", {})
assert projects["result"]["items"], "恢复快照后旧包可读建库时的数据"
'
echo "PASS：快照恢复后旧包正常启动（不反向降库）"

echo "旧二进制演练通过（兼容 + 拒启 + 快照恢复）"
