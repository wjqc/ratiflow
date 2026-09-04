#!/usr/bin/env python3
"""项目记忆 7 天观察指标采集（M5 UAT 手册 §2；只读，不写库）。

用法：
  python3 tests/platform/memory-uat-collect.py                       # 默认数据目录
  python3 tests/platform/memory-uat-collect.py --data-dir <路径>     # 指定数据目录

输出 JSON：记忆状态分布、capture job 状态与候选裁决率、每个 Run 的记忆注入
条数/字节/token 估算、FTS 完整性与探针耗时。把输出贴进 UAT 记录表。
"""
import argparse
import json
import os
import sqlite3
import sys
import time

DEFAULT_DATA = os.path.expanduser("~/Library/Application Support/SixGates/data")
DB = "sixgates.db"


def q(conn, sql, params=()):
    cur = conn.execute(sql, params)
    return cur.fetchall()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--data-dir", default=DEFAULT_DATA)
    args = ap.parse_args()
    db_path = os.path.join(args.data_dir, DB)
    if not os.path.exists(db_path):
        print(json.dumps({"error": f"库不存在：{db_path}"}, ensure_ascii=False))
        sys.exit(1)

    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    out = {"dataDir": args.data_dir, "collectedAt": time.strftime("%Y-%m-%dT%H:%M:%S")}

    # 0. v23 是否就位（旧包/旧库先启动新构建完成迁移）。
    version = q(conn, "SELECT COALESCE(MAX(version),0) FROM schema_migrations")[0][0]
    out["schemaVersion"] = version
    has_v23 = q(
        conn,
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='memory_entries'",
    )[0][0]
    if not has_v23:
        out["hint"] = "当前库尚未迁移到 v23：先用新构建的 SixGates 启动一次，再运行本脚本。"
        print(json.dumps(out, ensure_ascii=False, indent=2))
        return

    # 1. 记忆状态分布。
    out["entries"] = {
        status: n
        for status, n in q(
            conn, "SELECT status, COUNT(*) FROM memory_entries GROUP BY status"
        )
    }

    # 2. capture job 状态与候选裁决。
    out["capture"] = {
        "jobs": {s: n for s, n in q(
            conn, "SELECT status, COUNT(*) FROM memory_capture_jobs GROUP BY status")},
        "unknownJobIds": [r[0] for r in q(
            conn, "SELECT id FROM memory_capture_jobs WHERE status='unknown' LIMIT 10")],
        "candidates": {s: n for s, n in q(
            conn, "SELECT status, COUNT(*) FROM memory_candidates GROUP BY status")},
    }
    acc = out["capture"]["candidates"].get("accepted", 0)
    rej = out["capture"]["candidates"].get("rejected", 0)
    decided = acc + rej
    out["capture"]["acceptRate"] = round(acc / decided, 3) if decided else None

    # 3. 每个 Run 的记忆注入证据（字节 / token 估算）。
    runs = q(
        conn,
        """SELECT r.id, COUNT(*), COALESCE(SUM(m.bytes),0), COALESCE(SUM(m.token_estimate),0)
           FROM agent_runs r
           JOIN context_manifest_memories m ON m.manifest_id = r.context_manifest_id AND m.included = 1
           GROUP BY r.id ORDER BY r.created_at DESC LIMIT 20""",
    )
    out["injection"] = [
        {"runId": rid, "memories": n, "bytes": b, "tokenEstimate": t}
        for rid, n, b, t in runs
    ]
    # 预算红线：单 Run 记忆 >12 KiB 或 >8 条即列出（§7.1 预算）。
    out["injectionOverBudgetRuns"] = [
        rid for rid, n, b, _t in runs if b > 12288 or n > 8
    ]

    # 4. FTS 完整性 + 探针耗时。
    expected = q(
        conn,
        """SELECT COUNT(*) FROM memory_entries e
           JOIN memory_revisions r ON r.id = e.current_revision_id
           WHERE e.status != 'purged'""",
    )[0][0]
    actual = q(conn, "SELECT COUNT(*) FROM memory_fts")[0][0]
    t0 = time.time()
    probe = q(
        conn,
        """SELECT COUNT(*) FROM memory_fts WHERE project_id IN
           (SELECT DISTINCT project_id FROM memory_fts LIMIT 1)
           AND (title LIKE '%的%' OR body LIKE '%的%') LIMIT 50""",
    )
    out["fts"] = {
        "expected": expected,
        "actual": actual,
        "consistent": expected == actual,
        "probeMs": round((time.time() - t0) * 1000, 1),
        "probeHits": probe[0][0],
    }

    conn.close()
    print(json.dumps(out, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
