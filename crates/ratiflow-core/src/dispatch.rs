//! RPC 方法分发：~45 个方法覆盖项目/知识库/附件/工作项/工件/Agent/门禁/审批/证据/部署/时间线。
use std::sync::Arc;

use serde_json::{json, Value};
use sg_protocol::{ErrorCode, RpcError};
use sg_store::{objects, outbox, Error, Store};

use crate::settings_dispatch::serr;
use crate::state::AppState;

pub(crate) type RpcResult = Result<Value, RpcError>;

fn err(kind: ErrorCode, msg: impl Into<String>) -> RpcError {
    RpcError::new(kind, msg)
}

pub(crate) fn store_err(e: Error) -> RpcError {
    let msg = e.to_string();
    for (needle, kind) in [
        ("etag_mismatch", ErrorCode::EtagMismatch),
        ("revision_frozen", ErrorCode::RevisionFrozen),
        // receipt 门控域的校验类错误（WP-0 错误分类补齐）：确定性失败，落 completed envelope，
        // 不得落入 InternalError=Transient 面（否则 receipt 误开重执行窗口）。
        ("workflow_template_invalid", ErrorCode::InvalidParams),
        ("plan_validation_failed", ErrorCode::InvalidParams),
        ("task_outcome_invalid", ErrorCode::InvalidParams),
        ("workflow_version_not_active", ErrorCode::InvalidParams),
        (
            "invalid_stage_transition",
            ErrorCode::InvalidStageTransition,
        ),
        ("trace_incomplete", ErrorCode::TraceIncomplete),
        ("metrics_invalid", ErrorCode::InvalidParams),
        ("trace_cycle", ErrorCode::Conflict),
        ("output_digest_changed", ErrorCode::Conflict),
        ("snapshot_failed", ErrorCode::SnapshotFailed),
        ("rollback_drift", ErrorCode::RollbackDrift),
        (
            "rollback_manual_action_required",
            ErrorCode::RollbackManualActionRequired,
        ),
        (
            "agent_profile_unavailable",
            ErrorCode::AgentProfileUnavailable,
        ),
        ("capability_mismatch", ErrorCode::AgentCapabilityMismatch),
        ("approval_expired", ErrorCode::ApprovalExpired),
        ("attempt_active_exists", ErrorCode::Conflict),
        ("deliverable_missing", ErrorCode::Conflict),
        ("digest_drift", ErrorCode::Conflict),
        ("deployment", ErrorCode::Conflict),
        ("object_contains_secrets", ErrorCode::ObjectSecrets),
        ("manifest_workitem_mismatch", ErrorCode::InvalidParams),
        ("not_found", ErrorCode::NotFound),
        ("path_outside_project", ErrorCode::PathOutsideProject),
        ("budget_exhausted", ErrorCode::BudgetExceeded),
        ("context_too_large", ErrorCode::ContextTooLarge),
        ("approval_invalid", ErrorCode::ApprovalInvalid),
        ("model_", ErrorCode::ModelUnavailable),
        ("preflight_failed", ErrorCode::Conflict),
        ("verification_failed", ErrorCode::Conflict),
        ("rollback", ErrorCode::Conflict),
        ("ssh_command_rejected", ErrorCode::ManifestRejected),
        ("approval_required", ErrorCode::ApprovalRequired),
        ("action_denied", ErrorCode::ActionDenied),
        ("required", ErrorCode::InvalidParams),
    ] {
        if msg.contains(needle) {
            return err(kind, msg);
        }
    }
    err(ErrorCode::InternalError, msg)
}

fn str_param(params: &Value, key: &str) -> Result<String, RpcError> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| err(ErrorCode::InvalidParams, format!("缺少参数 {key}")))
}

fn opt_str_param(params: &Value, key: &str) -> Option<String> {
    params.get(key).and_then(|v| v.as_str()).map(String::from)
}

fn bool_param(params: &Value, key: &str) -> Result<bool, RpcError> {
    params
        .get(key)
        .and_then(|v| v.as_bool())
        .ok_or_else(|| err(ErrorCode::InvalidParams, format!("缺少参数 {key}")))
}

/// 放行 digest 的 policy version 分量：当前权限快照的 canonical digest。
pub(crate) fn release_policy_version(store: &Store) -> String {
    let (snapshot, _) = assemble_policy_snapshot(store);
    sg_policy::action_digest(&serde_json::to_value(&snapshot).unwrap_or_default())
}

/// WP-8：某关最近一次已批准的 gate_skip 豁免审批 id（护照 outcome 归因用）。
fn gate_skip_waiver_approval_id(
    store: &Store,
    workitem_id: &str,
    gate: &str,
) -> Result<Option<String>, Error> {
    store.with_conn(|conn| {
        match conn.query_row(
            "SELECT a.id FROM approvals a
             WHERE a.subject_type='gate_skip' AND a.workitem_id=?1 AND a.status='approved'
               AND a.stage_attempt_id IN
                 (SELECT id FROM stage_attempts WHERE workitem_id=?1 AND gate=?2)
             ORDER BY a.decided_at DESC, a.rowid DESC LIMIT 1",
            rusqlite::params![workitem_id, gate],
            |r| r.get::<_, String>(0),
        ) {
            Ok(id) => Ok(Some(id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(other) => Err(other.into()),
        }
    })
}

/// WP-8：按 action_digest 查既有 gate_skip 审批（幂等重放）。
fn gate_skip_existing_approval(
    store: &Store,
    digest: &str,
) -> Result<Option<serde_json::Value>, Error> {
    store.with_conn(|conn| {
        match conn.query_row(
            "SELECT id, status, reason, created_at FROM approvals
             WHERE subject_type='gate_skip' AND action_digest=?1",
            [digest],
            |r| {
                Ok(serde_json::json!({
                    "approvalId": r.get::<_, String>(0)?,
                    "state": r.get::<_, String>(1)?,
                    "reason": r.get::<_, String>(2)?,
                    "createdAt": r.get::<_, String>(3)?,
                    "idempotentReplay": true,
                }))
            },
        ) {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(other) => Err(other.into()),
        }
    })
}

/// gate.decideRelease 的 RPC 体（评审 P1 修复抽出共享）：
/// approval.decide 对 gate_release 主体必须路由到这里——digest 漂移复检（AC-SW-03）
/// 只存在于 decide_release 治理链，通用 sg_policy::decide 会旁路它。
fn gate_decide_release_rpc(store: &Store, params: &Value) -> RpcResult {
    let mut result = sg_workitem::release::decide_release(
        store,
        &str_param(params, "approvalId")?,
        &str_param(params, "decision")?,
        &str_param(params, "decidedBy")?,
        &opt_str_param(params, "reason").unwrap_or_default(),
        &release_policy_version(store),
    )
    .map_err(store_err)?;
    sg_store::audit::append(
        store,
        &str_param(params, "decidedBy")?,
        &format!("gate.release.{}", str_param(params, "decision")?),
        "approval",
        &str_param(params, "approvalId")?,
        json!({}),
    )
    .map_err(store_err)?;
    // ADR-031 C2：放行成功 → 镜像 ReleaseDecided 事件（事件权威，DB 为投影）。
    // 事件写入失败不回滚已成的放行，但如实记审计并在响应标注（事件先行的
    // 严格顺序属投影化改造里程碑，见 ADR-031 实施状态）。
    match sg_workitem::release_events::mirror_release_decided(
        store,
        result["attempt"]["workitem_id"]
            .as_str()
            .unwrap_or_default(),
        result["attempt"]["gate"].as_str().unwrap_or_default(),
        result["attempt"]["id"].as_str().unwrap_or_default(),
        result["release_digest"].as_str().unwrap_or_default(),
        &str_param(params, "decision")?,
        &str_param(params, "decidedBy")?,
    ) {
        Ok(Some(event_id)) => {
            result["releaseEventId"] = json!(event_id);
        }
        Ok(None) => {
            result["releaseEventId"] = json!(null);
        }
        Err(e) => {
            let _ = sg_store::audit::append(
                store,
                "system",
                "gate.release.event_mirror_failed",
                "approval",
                &str_param(params, "approvalId")?,
                json!({ "error": e.to_string() }),
            );
            result["releaseEventMirrorError"] = json!(e.to_string());
        }
    }
    Ok(result)
}

/// M1 谱系新写开关（可回退点）：RATIFLOW_TRACE_WRITES=0 关闭全部谱系写入/回填，保留表结构。
pub(crate) fn trace_writes_enabled() -> bool {
    std::env::var("RATIFLOW_TRACE_WRITES")
        .map(|v| v != "0")
        .unwrap_or(true)
}

/// 显式幂等回执——lease 三态语义（RDWS 实施计划 v1.4 §1.3，0041 最终结构）：
/// 1) 门控 mutation 缺 idempotencyKey 一律拒绝（idempotency_key_required），不退化为直接执行；
/// 2) 指纹门：request_fingerprint = sha256(canonical params 剔除 idempotencyKey)，
///    同 (method,idem_key) 异指纹重放在执行前被拒（rpc_receipt_fingerprint_mismatch）；
/// 3) lease CAS：认领（新行 INSERT、retryable_failed→in_flight、in_flight 过期接管）必须
///    条件 UPDATE 命中 1 行才获得执行权；执行权由 (owner,lease_revision,lease_state) 三元组锁定；
/// 4) 终态：成功/deterministic 错误 → completed（revision+1，envelope 可重放）；
///    transient 错误（IO/内部/超时，构造处按码标注 ErrClass）→ retryable_failed 原子释放
///    执行语义，后续请求按 CAS 重新认领，不再收到 in_flight 假象；
/// 5) TTL 只解决崩溃租约（in_flight 超时接管自愈），不替代领域幂等——本层只防网络/UI 重放。
const RPC_RECEIPT_LEASE_TTL_SECS: i64 = 300;
const RPC_RECEIPT_ERROR_KEY: &str = "__sg_rpc_error";

/// 认领令牌：进程 id + 启动标识 + 进程内单调计数，每次认领唯一，CAS 可精确归因。
fn receipt_owner() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static STARTUP: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let base =
        STARTUP.get_or_init(|| format!("{}:{}", std::process::id(), sg_store::ids::new_id("boot")));
    format!("{base}:{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// 指纹参数 canonical 形态：递归键排序，剔除 idempotencyKey 本身（key 不同不构成参数漂移）。
fn receipt_fingerprint(params: &Value) -> String {
    fn sort_json(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut sorted = serde_json::Map::new();
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                for key in keys {
                    sorted.insert(key.clone(), sort_json(&map[key]));
                }
                Value::Object(sorted)
            }
            Value::Array(items) => Value::Array(items.iter().map(sort_json).collect()),
            other => other.clone(),
        }
    }
    let mut params = params.clone();
    if let Some(obj) = params.as_object_mut() {
        obj.remove("idempotencyKey");
    }
    let canonical = serde_json::to_string(&sort_json(&params)).unwrap_or_default();
    use sha2::Digest;
    sg_store::ids::hex(&sha2::Sha256::digest(canonical.as_bytes()))
}

/// 已持有的执行权（写终态的 CAS 凭据）。
struct ReceiptClaim {
    owner: String,
    lease_revision: i64,
}

/// 认领结果：获得执行权，或直接重放首次终态。
enum ReceiptGate {
    Claimed(ReceiptClaim),
    Replay(Result<Value, RpcError>),
}

fn receipt_in_flight_err() -> RpcError {
    RpcError::new(
        ErrorCode::Conflict,
        "rpc_receipt_in_flight: 同幂等键请求正在执行，请稍后重试",
    )
}

fn replay_body(body: &str) -> Result<Value, RpcError> {
    if let Ok(env) = serde_json::from_str::<Value>(body) {
        if let Some(err_env) = env.get(RPC_RECEIPT_ERROR_KEY) {
            let rpc_err: RpcError = serde_json::from_value(err_env.clone())
                .map_err(|_| RpcError::new(ErrorCode::InternalError, "rpc_receipt_corrupt"))?;
            return Err(rpc_err);
        }
    }
    serde_json::from_str(body)
        .map_err(|_| RpcError::new(ErrorCode::InternalError, "rpc_receipt_corrupt"))
}

/// 认领（或读出可重放终态）。行不存在 → INSERT 新行；已存在 → 校验指纹后按 lease_state 分派。
fn receipt_claim(
    store: &Store,
    method: &str,
    idem_key: &str,
    fingerprint: &str,
) -> Result<ReceiptGate, RpcError> {
    let owner = receipt_owner();
    let now = sg_store::timefmt::now();
    let lease_until = sg_store::timefmt::now_plus_minutes(RPC_RECEIPT_LEASE_TTL_SECS / 60);
    let inserted = store
        .with_conn(|conn| {
            conn.execute(
                "INSERT OR IGNORE INTO rpc_receipts
                   (method, idem_key, request_fingerprint, owner, lease_revision,
                    lease_expires_at, lease_state, response_json, created_at, updated_at)
                 VALUES (?1,?2,?3,?4,1,?5,'in_flight','',?6,?6)",
                rusqlite::params![method, idem_key, fingerprint, owner, lease_until, now],
            )
            .map_err(sg_store::Error::from)?;
            Ok(conn.changes() == 1)
        })
        .map_err(|e| RpcError::new(ErrorCode::InternalError, e.to_string().as_str()))?;
    if inserted {
        return Ok(ReceiptGate::Claimed(ReceiptClaim {
            owner,
            lease_revision: 1,
        }));
    }

    let row = store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT request_fingerprint, owner, lease_revision, lease_expires_at,
                        lease_state, response_json
                 FROM rpc_receipts WHERE method=?1 AND idem_key=?2",
                [method, idem_key],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                    ))
                },
            )
            .map_err(|_| sg_store::Error::Message("rpc_receipt_missing".into()))
        })
        .map_err(|e| RpcError::new(ErrorCode::InternalError, e.to_string().as_str()))?;
    let (row_fp, row_owner, row_revision, row_expires, row_state, row_body) = row;

    // 指纹门：legacy 空指纹跳过（保持旧重放行为），非空且不一致 → 执行前拒绝。
    if !row_fp.is_empty() && row_fp != fingerprint {
        return Err(RpcError::new(
            ErrorCode::ReceiptFingerprintMismatch,
            format!(
                "rpc_receipt_fingerprint_mismatch: 同幂等键 {method} 参数指纹不一致（期望 {}，实到 {}）",
                &row_fp[..row_fp.len().min(8)],
                &fingerprint[..fingerprint.len().min(8)]
            ),
        ));
    }

    // CAS 认领：命中 1 行才获得执行权；租约竞争 → in_flight 冲突。
    let cas_claim = |set_state: bool, expect_state: &str| -> Result<bool, RpcError> {
        let state_set = if set_state {
            ", lease_state='in_flight'"
        } else {
            ""
        };
        let sql = format!(
            "UPDATE rpc_receipts SET owner=?3, lease_revision=lease_revision+1, \
             lease_expires_at=?4, updated_at=?5{state_set} \
             WHERE method=?1 AND idem_key=?2 AND owner=?6 AND lease_revision=?7 AND lease_state=?8"
        );
        store
            .with_conn(|conn| {
                conn.execute(
                    sql.as_str(),
                    rusqlite::params![
                        method,
                        idem_key,
                        owner,
                        lease_until,
                        now,
                        row_owner,
                        row_revision,
                        expect_state
                    ],
                )
                .map_err(sg_store::Error::from)?;
                Ok(conn.changes() == 1)
            })
            .map_err(|e| RpcError::new(ErrorCode::InternalError, e.to_string().as_str()))
    };

    match row_state.as_str() {
        "completed" => Ok(ReceiptGate::Replay(replay_body(&row_body))),
        "retryable_failed" => match cas_claim(true, "retryable_failed")? {
            true => Ok(ReceiptGate::Claimed(ReceiptClaim {
                owner,
                lease_revision: row_revision + 1,
            })),
            false => Err(receipt_in_flight_err()),
        },
        // in_flight：租约未过期 → 拒绝；过期（或无租期/损坏时间戳）→ 接管自愈。
        _ => {
            let expired = row_expires
                .as_deref()
                .map(|t| sg_store::timefmt::age_secs(t) > 0)
                .unwrap_or(true);
            if !expired {
                return Err(receipt_in_flight_err());
            }
            match cas_claim(false, "in_flight")? {
                true => Ok(ReceiptGate::Claimed(ReceiptClaim {
                    owner,
                    lease_revision: row_revision + 1,
                })),
                false => Err(receipt_in_flight_err()),
            }
        }
    }
}

/// 终态写入（成功 / deterministic 错误 envelope）：completed + revision+1 + 清租期。
/// CAS 锁定 (owner,revision)：租约被接管后原执行者不再有写权（0 行 = 已易主，放弃写）。
fn receipt_complete(
    store: &Store,
    method: &str,
    idem_key: &str,
    claim: &ReceiptClaim,
    response_body: &str,
) -> Result<(), Error> {
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE rpc_receipts SET lease_state='completed', response_json=?4,
                    lease_revision=lease_revision+1, lease_expires_at=NULL, updated_at=?5
             WHERE method=?1 AND idem_key=?2 AND owner=?6 AND lease_revision=?7
               AND lease_state='in_flight'",
            rusqlite::params![
                method,
                idem_key,
                "",
                response_body,
                sg_store::timefmt::now(),
                claim.owner,
                claim.lease_revision
            ],
        )?;
        Ok(())
    })
}

/// 执行失败后的回执落库（领域结果已回滚/已失败，这里只写传输层事实）：
/// Transient → retryable_failed（释放执行语义）；Deterministic → completed + 可重放 envelope。
fn receipt_fail(
    store: &Store,
    method: &str,
    idem_key: &str,
    claim: &ReceiptClaim,
    e: &RpcError,
) -> Result<(), Error> {
    let (state, body) = if e.err_class() == sg_protocol::ErrClass::Transient {
        ("retryable_failed", String::new())
    } else {
        let env = json!({ RPC_RECEIPT_ERROR_KEY: serde_json::to_value(e).unwrap_or_default() });
        ("completed", env.to_string())
    };
    store.with_conn(|conn| {
        conn.execute(
            "UPDATE rpc_receipts SET lease_state=?4, response_json=?5,
                    lease_revision=lease_revision+1, lease_expires_at=NULL, updated_at=?6
             WHERE method=?1 AND idem_key=?2 AND owner=?7 AND lease_revision=?8
               AND lease_state='in_flight'",
            rusqlite::params![
                method,
                idem_key,
                "",
                state,
                body,
                sg_store::timefmt::now(),
                claim.owner,
                claim.lease_revision
            ],
        )?;
        Ok(())
    })
}

pub(crate) fn with_rpc_receipt<F>(
    store: &Store,
    idem_key: &str,
    method: &str,
    params: &Value,
    f: F,
) -> Result<Value, RpcError>
where
    F: FnOnce() -> Result<Value, RpcError>,
{
    if idem_key.is_empty() {
        return Err(RpcError::new(
            ErrorCode::IdempotencyKeyRequired,
            format!("{method}: receipt 门控 mutation 必须携带 idempotencyKey"),
        ));
    }
    let fingerprint = receipt_fingerprint(params);
    let claim = match receipt_claim(store, method, idem_key, &fingerprint)? {
        ReceiptGate::Claimed(claim) => claim,
        ReceiptGate::Replay(replay) => return replay,
    };
    match f() {
        Ok(out) => {
            receipt_complete(store, method, idem_key, &claim, &out.to_string()).map_err(|e| {
                RpcError::new(
                    ErrorCode::InternalError,
                    format!("rpc_receipt_write_failed: {e}").as_str(),
                )
            })?;
            Ok(out)
        }
        Err(e) => {
            let _ = receipt_fail(store, method, idem_key, &claim, &e);
            Err(e)
        }
    }
}

/// 同事务回执变体（RDWS 实施计划 v1.4 §1.3 with_rpc_receipt_tx）：
/// closure 接收领域事务连接——领域写与 receipt completed 终态同一 SQLite 事务，
/// 任一失败整体回滚（领域状态零残留）；deterministic 错误的 envelope 在回滚后
/// 独立小事务落库。closure 内禁止调用会 with_conn/with_tx 的领域函数（Mutex 不可重入），
/// 只允许消费传入连接的 tx 变体；回执终态 UPDATE 命中 0 行（租约被接管）时整个事务
/// 回滚——接管者重执行，本执行者不落任何领域写（不双执行）。
/// 独立小事务落库（失败不掩盖原始错误，租约到期自愈）。
/// 消费方为后续 WP 的 tx 变体改造（WP-4 导入 / WP-8 skip / WP-9 rework §1.3 清单），
/// WP-0 批次先落地基础设施与测试向量。
#[allow(dead_code)]
pub(crate) fn with_rpc_receipt_tx<F>(
    store: &Store,
    idem_key: &str,
    method: &str,
    params: &Value,
    f: F,
) -> Result<Value, RpcError>
where
    F: FnOnce(&rusqlite::Connection) -> Result<Value, RpcError>,
{
    if idem_key.is_empty() {
        return Err(RpcError::new(
            ErrorCode::IdempotencyKeyRequired,
            format!("{method}: receipt 门控 mutation 必须携带 idempotencyKey"),
        ));
    }
    let fingerprint = receipt_fingerprint(params);
    let claim = match receipt_claim(store, method, idem_key, &fingerprint)? {
        ReceiptGate::Claimed(claim) => claim,
        ReceiptGate::Replay(replay) => return replay,
    };
    let mut closure_err: Option<RpcError> = None;
    let executed = store.with_tx_immediate(|tx| {
        let out = match f(tx) {
            Ok(out) => out,
            Err(e) => {
                closure_err = Some(e);
                return Err(Error::Message("rpc_closure_failed".into()));
            }
        };
        // 同事务写 completed 终态；0 行 = 租约已被接管 → 回滚领域写。
        let n = tx
            .execute(
                "UPDATE rpc_receipts SET lease_state='completed', response_json=?4,
                        lease_revision=lease_revision+1, lease_expires_at=NULL, updated_at=?5
                 WHERE method=?1 AND idem_key=?2 AND owner=?6 AND lease_revision=?7
                   AND lease_state='in_flight'",
                rusqlite::params![
                    method,
                    idem_key,
                    "",
                    out.to_string(),
                    sg_store::timefmt::now(),
                    claim.owner,
                    claim.lease_revision
                ],
            )
            .map_err(Error::from)?;
        if n != 1 {
            return Err(Error::Message("rpc_receipt_lease_lost".into()));
        }
        Ok(out)
    });
    match executed {
        Ok(out) => Ok(out),
        Err(store_e) => {
            let e = closure_err.take().unwrap_or_else(|| {
                if store_e.to_string().contains("rpc_receipt_lease_lost") {
                    receipt_in_flight_err()
                } else {
                    // 存储层失败（IO/内部面）：按 transient 语义释放租约，客户端可重试。
                    RpcError::new(ErrorCode::InternalError, store_e.to_string().as_str())
                }
            });
            // 领域事务已回滚；回执事实独立小事务落库（失败不掩盖原始错误，租约到期自愈）。
            let _ = receipt_fail(store, method, idem_key, &claim, &e);
            Err(e)
        }
    }
}

/// WP-6（RDWS v1.4 A3）：tool_proposal 审批 decide 前重算双 digest。
/// - digest_schema_version 未知 → fail-closed（不猜测公式）；
/// - impact_digest 漂移 → expire + impact_digest_changed；
/// - scope_facts_digest（incomplete/unknown 审批的全图摘要）漂移 → expire。
///
/// legacy（impact_digest 空）跳过比对，行为与 WP-6 前一致。
fn verify_tool_proposal_binding(
    store: &Store,
    subject: &sg_policy::Approval,
) -> Result<(), RpcError> {
    if subject.digest_schema_version != sg_policy::DIGEST_SCHEMA_VERSION {
        return Err(err(
            ErrorCode::ApprovalInvalid,
            format!(
                "approval_digest_version_unknown: digest_schema_version={} 不受支持（fail-closed）",
                subject.digest_schema_version
            ),
        ));
    }
    let current =
        sg_provenance::impact::for_proposal(store, &subject.subject_id).map_err(store_err)?;
    if current.impact_digest != subject.impact_digest {
        sg_policy::expire_drifted(store, &subject.id, "impact_digest_changed")
            .map_err(store_err)?;
        return Err(err(
            ErrorCode::Conflict,
            "impact_digest_changed: 影响面在审批等待期间漂移，待审已失效（请重新发起）",
        ));
    }
    if !subject.scope_facts_digest.is_empty()
        && current.workitem_facts_digest != subject.scope_facts_digest
    {
        sg_policy::expire_drifted(store, &subject.id, "scope_facts_changed").map_err(store_err)?;
        return Err(err(
            ErrorCode::Conflict,
            "scope_facts_changed: workitem 全图 facts 在审批等待期间漂移，待审已失效",
        ));
    }
    Ok(())
}

/// 统一取消服务（agent.cancel 与 Slash /取消 共用，EvoFlow 评审 P0 修复）：
/// 活跃任务置位取消令牌——阻塞中的模型 HTTP/SSE 在 select 点即时中止
/// （放弃 future 即关闭 socket，目标 <1s），Run 循环以 cancelled 终态收尾并
/// 发恰好一条 run.cancelled；无活跃任务（遗留 running/paused 行，如进程崩溃
/// 后）才直接置库收尾。禁止绕过令牌直调领域库取消。
pub(crate) fn cancel_run_shared(
    state: &crate::state::AppState,
    store: &Store,
    run_id: &str,
) -> Result<Value, String> {
    let run = sg_agent::get_run(store, run_id).map_err(|e| e.to_string())?;
    let terminal = matches!(
        run.status.as_str(),
        "completed_execution" | "failed" | "cancelled"
    );
    if terminal {
        return Ok(json!({"runId": run_id, "status": run.status}));
    }
    if let Some(token) = state.runs.get(run_id) {
        token.cancel();
        Ok(json!({"runId": run_id, "status": "cancelling"}))
    } else {
        sg_agent::cancel(store, run_id).map_err(|e| e.to_string())?;
        Ok(json!({"runId": run_id, "status": "cancelled"}))
    }
}

fn workitem_gate(store: &Store, workitem_id: &str) -> String {
    store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT current_gate FROM workitems WHERE id=?1",
                [workitem_id],
                |r| r.get::<_, String>(0),
            )
            .map_err(sg_store::Error::from)
        })
        .unwrap_or_default()
}

fn str_list_param(params: &Value, key: &str) -> Vec<String> {
    params
        .get(key)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// 产出入口的谱系接线（蓝图 §6.1 最小父边纪律，fail-closed）：
/// 先解析需求 key → item id（缺修订/缺条目即 trace_incomplete），调用方确认无副作用后再建边。
fn resolve_requirement_items(
    store: &Store,
    workitem_id: &str,
    requirement_keys: &[String],
) -> Result<Vec<String>, Error> {
    if requirement_keys.is_empty() {
        return Ok(vec![]);
    }
    let Some(revision_id) = sg_workitem::requirements::latest_revision_id(store, workitem_id)?
    else {
        return Err(Error::Message(format!(
            "trace_incomplete: 工作项 {workitem_id} 尚无需求修订，无法建立谱系边"
        )));
    };
    let mut item_ids = Vec::with_capacity(requirement_keys.len());
    for key in requirement_keys {
        item_ids.push(
            sg_workitem::requirements::item_id_by_key(store, &revision_id, key)?.ok_or_else(
                || {
                    Error::Message(format!(
                        "trace_incomplete: 修订 {revision_id} 中不存在需求项 {key}"
                    ))
                },
            )?,
        );
    }
    Ok(item_ids)
}

fn trace_link_items(
    store: &Store,
    workitem_id: &str,
    from_node_type: &str,
    from_entity_id: &str,
    relation: &str,
    item_ids: &[String],
) -> Result<(), Error> {
    for item_id in item_ids {
        sg_provenance::add_edge(
            store,
            &sg_provenance::EdgeInput {
                workitem_id,
                from_node_type,
                from_entity_id,
                relation,
                to_node_type: sg_provenance::node_type::REQUIREMENT_ITEM,
                to_entity_id: item_id,
                stage_attempt_id: "",
                created_by_run_id: "",
            },
        )?;
    }
    Ok(())
}

/// WP-8a：shadow 域错误分类（确定性码，不落 InternalError=Transient 面）。
/// （automation_rpc 内另有同名闭包，语义一致。）
fn shadow_err(e: &sg_store::Error) -> RpcError {
    let msg = e.to_string();
    let code = if msg.contains("shadow_decision_conflict") {
        ErrorCode::Conflict
    } else if msg.contains("shadow_suggestion_missing") || msg.contains("shadow_decision_missing") {
        ErrorCode::NotFound
    } else if msg.contains("shadow_decision_invalid") || msg.contains("shadow_suggestion_invalid") {
        ErrorCode::InvalidParams
    } else {
        ErrorCode::InternalError
    };
    RpcError::new(code, msg.as_str())
}

/// 分发一个 RPC 请求（在 DB actor 线程上执行；store 由 actor 提供）。
pub fn dispatch(state: &AppState, store: &Store, method: &str, params: &Value) -> RpcResult {
    if let Some(result) = crate::settings_dispatch::dispatch(state, store, method, params) {
        return result;
    }
    // --- 数据化工作流模板（EvoFlow M1-07 / ADR-036）：方法字面量供契约对齐检查 ---
    if matches!(
        method,
        "workflowTemplate.list"
            | "workflowTemplate.get"
            | "workflowTemplate.create"
            | "workflowTemplate.updateDraft"
            | "workflowTemplate.activate"
            | "workflowTemplate.deprecate"
            | "workflow.getInstance"
            | "workflow.migrationPreview"
            | "workflow.migrate"
    ) {
        return crate::workflow_dispatch::dispatch(state, store, method, params);
    }
    // --- 自动化调度与 Goal（EvoFlow M6-04 / ADR-037/039）---
    if matches!(
        method,
        "automation.create"
            | "automation.list"
            | "automation.pause"
            | "automation.resume"
            | "automation.runNow"
            | "automation.history"
            | "automation.decideSuggestion"
            | "automation.reviewSuggestion"
            | "automation.observations"
            | "goal.autoReleaseCheck"
            | "autonomy.createGrant"
            | "autonomy.revokeGrant"
            | "notification.list"
    ) {
        return automation_rpc(store, method, params);
    }
    // --- Trace 与 Slash（EvoFlow M5-04/06 / ADR-039）---
    if matches!(
        method,
        "trace.graph"
            | "trace.usage"
            | "trace.taskReadModel"
            | "trace.restoreCheckpoint"
            | "command.preview"
            | "command.execute"
    ) {
        if method == "command.preview" || method == "command.execute" {
            let workitem_id = params
                .get("workItemId")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
            return match method {
                "command.preview" => crate::commands::preview(store, text, workitem_id)
                    .map_err(|e| RpcError::new(ErrorCode::InvalidParams, e.to_string().as_str())),
                _ => {
                    let token = params
                        .get("previewToken")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    crate::commands::execute(state, store, text, workitem_id, token).map_err(|e| {
                        RpcError::new(ErrorCode::InvalidParams, e.to_string().as_str())
                    })
                }
            };
        }
        return crate::trace_dispatch::dispatch(state, store, method, params);
    }
    // --- 结构化计划与任务工作区（EvoFlow M2-09 / ADR-036/037）---
    if matches!(
        method,
        "plan.createDraft"
            | "plan.updateDraft"
            | "plan.get"
            | "plan.list"
            | "plan.submit"
            | "plan.decide"
            | "plan.start"
            | "plan.cancel"
            | "taskWorkspace.prepare"
            | "taskWorkspace.get"
            | "taskWorkspace.finalize"
            | "plan.replanPreview"
            | "plan.replan"
            | "planTask.list"
            | "planTask.prepare"
            | "planTask.transition"
            | "planTask.reconcile"
            | "plan.startRunning"
            | "plan.dispatchReady"
    ) {
        return crate::plan_dispatch::dispatch(state, store, method, params);
    }
    match method {
        // --- 系统 ---
        "core.version" => Ok(
            json!({"version": state.core_version, "protocolVersion": sg_protocol::PROTOCOL_VERSION,
            "schemaVersion": store.schema_version().map_err(store_err)?}),
        ),
        "diagnostics.check" => diagnostics(state, store),

        // --- 项目 ---
        "project.gitStatus" => {
            let project_id = str_param(params, "projectId")?;
            let local_root: String = store
                .with_conn(|conn| {
                    Ok(conn
                        .query_row(
                            "SELECT COALESCE(local_root,'') FROM projects WHERE id=?1",
                            [&project_id],
                            |r| r.get(0),
                        )
                        .unwrap_or_default())
                })
                .map_err(store_err)?;
            if local_root.is_empty() {
                return Ok(json!({"available": false, "reason": "project_root_missing"}));
            }
            match sg_workitem::worktree::repo_git_status(std::path::Path::new(&local_root)) {
                Some((branch, dirty)) => {
                    Ok(json!({"available": true, "branch": branch, "dirty": dirty}))
                }
                None => Ok(json!({"available": false, "reason": "not_a_git_repo"})),
            }
        }
        "project.list" => {
            let include_archived = params
                .get("includeArchived")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let items = sg_project::list(store, include_archived).map_err(store_err)?;
            Ok(json!({"items": items}))
        }
        "project.get" => {
            let id = str_param(params, "projectId")?;
            sg_project::get(store, &id)
                .map(|p| serde_json::to_value(p).unwrap_or_default())
                .map_err(store_err)
        }
        "project.create" => {
            let p = sg_project::register(
                store,
                &str_param(params, "gitlabInstance")?,
                &str_param(params, "namespace")?,
                &str_param(params, "project")?,
                &opt_str_param(params, "defaultBranch").unwrap_or_default(),
                &opt_str_param(params, "name").unwrap_or_default(),
                &opt_str_param(params, "localRoot").unwrap_or_default(),
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(p).unwrap_or_default())
        }
        "project.update" => {
            let id = str_param(params, "projectId")?;
            let p = sg_project::update(
                store,
                &id,
                opt_str_param(params, "name").as_deref(),
                opt_str_param(params, "localRoot").as_deref(),
                opt_str_param(params, "defaultBranch").as_deref(),
                opt_str_param(params, "status").as_deref(),
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(p).unwrap_or_default())
        }
        "project.archive" => {
            let id = str_param(params, "projectId")?;
            let archived = params
                .get("archived")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            sg_project::archive(store, &id, archived).map_err(store_err)?;
            Ok(json!({"status": if archived { "archived" } else { "active" }}))
        }
        "project.summary" => {
            sg_project::summary(store, &str_param(params, "projectId")?).map_err(store_err)
        }

        // --- 知识库 ---
        "knowledge.list" => {
            let items = sg_knowledge::list_sources(store, &str_param(params, "projectId")?)
                .map_err(store_err)?;
            Ok(json!({"items": items}))
        }
        "knowledge.create" => {
            let src = sg_knowledge::create_source(
                store,
                &str_param(params, "projectId")?,
                &str_param(params, "kind")?,
                &str_param(params, "name")?,
                &str_param(params, "locator")?,
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(src).unwrap_or_default())
        }
        "knowledge.update" => {
            sg_knowledge::update_source(
                store,
                &str_param(params, "sourceId")?,
                params.get("enabled").and_then(|v| v.as_bool()),
                opt_str_param(params, "name").as_deref(),
            )
            .map_err(store_err)?;
            Ok(json!({"status": "updated"}))
        }
        "knowledge.remove" => {
            sg_knowledge::remove_source(store, &str_param(params, "sourceId")?)
                .map_err(store_err)?;
            Ok(json!({"status": "removed"}))
        }
        "knowledge.scan" => {
            let src = sg_knowledge::scan_source(
                store,
                &str_param(params, "sourceId")?,
                opt_str_param(params, "projectRoot")
                    .map(std::path::PathBuf::from)
                    .as_deref(),
                500,
                2 << 20,
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(src).unwrap_or_default())
        }
        "knowledge.syncFromRepo" => {
            let project_id = str_param(params, "projectId")?;
            let root =
                sg_knowledge::reconcile::project_root(store, &project_id).map_err(store_err)?;
            let reconcile = sg_knowledge::reconcile::sync_from_repo(store, &project_id, &root)
                .map_err(store_err)?;
            let (generation_id, generation_status) =
                sg_knowledge::reconcile::build_and_activate_generation(store, &project_id, &root)
                    .map_err(store_err)?;
            Ok(json!({
                "reconcile": reconcile,
                "generationId": generation_id,
                "generationStatus": generation_status,
            }))
        }
        "knowledge.manifestCreate" => {
            sg_knowledge::manifest::manifest_create(store, params).map_err(store_err)
        }
        "knowledge.manifestUpdate" => {
            sg_knowledge::manifest::manifest_update(store, params).map_err(store_err)
        }
        "knowledge.manifestRemove" => {
            sg_knowledge::manifest::manifest_remove(store, params).map_err(store_err)
        }
        "knowledge.search" => {
            let hits = sg_knowledge::search(
                store,
                &str_param(params, "projectId")?,
                &str_param(params, "query")?,
                params.get("limit").and_then(|v| v.as_i64()).unwrap_or(20),
            )
            .map_err(store_err)?;
            Ok(json!({"items": hits}))
        }
        "context.preview" => sg_knowledge::context_preview(
            store,
            &str_param(params, "projectId")?,
            &str_param(params, "query")?,
            params
                .get("maxBytes")
                .and_then(|v| v.as_i64())
                .unwrap_or(64 << 10),
        )
        .map_err(store_err),
        "context.instructions" => {
            // F07/M2：分层指令文件预览（全局→项目根→docs/，含装配字节占比）。
            let project_id = str_param(params, "projectId")?;
            let local_root: Option<String> = store
                .with_conn(|conn| {
                    Ok(conn
                        .query_row(
                            "SELECT COALESCE(local_root,'') FROM projects WHERE id=?1",
                            [&project_id],
                            |r| r.get::<_, String>(0),
                        )
                        .ok())
                })
                .map_err(store_err)?;
            let root = local_root
                .filter(|p| !p.is_empty())
                .map(std::path::PathBuf::from);
            let knowledge_settings = sg_settings::knowledge_defaults::get(store, Some(&project_id))
                .unwrap_or_else(|_| json!({}));
            let instr = sg_agent::instructions::settings_from_json(&knowledge_settings);
            let (text, layers, warnings) =
                sg_agent::instructions::aggregate(&store.data_dir, root.as_deref(), &instr);
            let knowledge = sg_agent::prompt::knowledge_text(&text, "");
            let env = sg_agent::prompt::PromptEnv {
                mode: Some(state.executor_mode),
                work_dir_label: root.as_ref().map(|p| p.to_string_lossy().to_string()),
                requires_approval_tools: vec!["run_command".into()],
            };
            let initial =
                sg_agent::prompt::assemble(&env, &["read_file".to_string()], &knowledge, "");
            Ok(json!({
                "layers": sg_agent::instructions::layers_json(&layers),
                "totalBytes": text.len(),
                "warnings": warnings,
                "promptBytes": sg_agent::prompt::segment_bytes(&initial),
            }))
        }
        "context.create" => {
            let selected: Vec<String> = params
                .get("selectedSources")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            sg_context::build_manifest(
                store,
                &sg_context::BuildInput {
                    project_id: &str_param(params, "projectId")?,
                    workitem_id: &str_param(params, "workItemId")?,
                    goal: &str_param(params, "query")?,
                    selected_sources: &selected,
                },
            )
            .map_err(store_err)
        }

        // --- 附件 ---
        "attachment.import" => {
            let content_b64 = str_param(params, "contentBase64")?;
            let content =
                base64_decode(&content_b64).map_err(|e| err(ErrorCode::InvalidParams, e))?;
            let att = sg_attachment::import(
                store,
                &str_param(params, "workItemId")?,
                &str_param(params, "filename")?,
                &content,
                objects::PutOptions::default(),
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(att).unwrap_or_default())
        }
        "attachment.list" => {
            let items =
                sg_attachment::list(store, &str_param(params, "workItemId")?).map_err(store_err)?;
            Ok(json!({"items": items}))
        }
        "attachment.parse" => {
            sg_attachment::set_parse_result(
                store,
                &str_param(params, "attachmentId")?,
                &str_param(params, "state")?,
                opt_str_param(params, "extractedText").as_deref(),
                &opt_str_param(params, "error").unwrap_or_default(),
            )
            .map_err(store_err)?;
            sg_attachment::get(store, &str_param(params, "attachmentId")?)
                .map(|a| serde_json::to_value(a).unwrap_or_default())
                .map_err(store_err)
        }
        "attachment.remove" => {
            sg_attachment::remove(store, &str_param(params, "attachmentId")?).map_err(store_err)?;
            Ok(json!({"status": "removed"}))
        }

        // --- 工作项 ---
        "workitem.list" => {
            let (items, next) = sg_workitem::list(
                store,
                &str_param(params, "projectId")?,
                &opt_str_param(params, "cursor").unwrap_or_default(),
                params.get("limit").and_then(|v| v.as_i64()).unwrap_or(20),
                params
                    .get("includeArchived")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            )
            .map_err(store_err)?;
            Ok(json!({"items": items, "nextCursor": next}))
        }
        "workitem.get" => {
            let id = str_param(params, "workItemId")?;
            let wi = sg_workitem::get(store, &id).map_err(store_err)?;
            let stages = sg_workitem::stages(store, &id).map_err(store_err)?;
            Ok(json!({"workItem": wi, "stages": stages}))
        }
        "workitem.create" => {
            let labels: Vec<String> = params
                .get("labels")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|l| l.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            // M1-03：templateId（模板 key）可选；提供时受 Flag 门控（默认模板不受限）。
            let template_id = opt_str_param(params, "templateId");
            if template_id.is_some()
                && template_id.as_deref() != Some(sg_workflow::template::DEFAULT_TEMPLATE_KEY)
                && !sg_workflow::template::template_v2_enabled()
            {
                return Err(err(
                    ErrorCode::InvalidRequest,
                    "feature_disabled: RATIFLOW_WORKFLOW_TEMPLATE_V2 未开启",
                ));
            }
            let wi = sg_workitem::create_with_template(
                store,
                &str_param(params, "projectId")?,
                &str_param(params, "title")?,
                &opt_str_param(params, "description").unwrap_or_default(),
                opt_str_param(params, "gitlabIssueIid").as_deref(),
                &labels,
                template_id.as_deref(),
            )
            .map_err(store_err)?;
            // 需求文档落盘（工作目录 data/docs/）+ 需求修订/条目/谱系节点（M1）。
            let doc = format!("# {}\n\n{}\n", wi.title, wi.description);
            let doc_path = sg_workitem::docs::save(store, &wi.id, "requirement.md", &doc)
                .map_err(store_err)?;
            if trace_writes_enabled() {
                sg_workitem::requirements::import_revision(
                    store,
                    &wi.id,
                    "requirement.md",
                    &doc,
                    "inline",
                    "local-user",
                    "verified",
                )
                .map_err(store_err)?;
            }
            let mut value = serde_json::to_value(&wi).unwrap_or_default();
            value["requirementDoc"] = json!(doc_path);
            Ok(value)
        }
        "workitem.progress" => {
            sg_workitem::progress::progress(store, &str_param(params, "workItemId")?)
                .map_err(store_err)
        }
        "workitem.archive" => {
            let id = str_param(params, "workItemId")?;
            let archived = params
                .get("archived")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            sg_workitem::archive(store, &id, archived).map_err(store_err)?;
            Ok(json!({"status": if archived { "archived" } else { "active" }}))
        }
        "workitem.documents" => {
            let names = sg_workitem::docs::list(store, &str_param(params, "workItemId")?)
                .map_err(store_err)?;
            Ok(json!({"items": names}))
        }
        "workitem.getDocument" => {
            let content = sg_workitem::docs::read(
                store,
                &str_param(params, "workItemId")?,
                &str_param(params, "name")?,
            )
            .map_err(store_err)?;
            Ok(json!({"content": content}))
        }
        "workitem.importDocument" => {
            let filename = str_param(params, "filename")?;
            let content = str_param(params, "content")?;
            let title = sg_workitem::docs::title_from_document(&filename, &content);
            let title = if title.is_empty() {
                "未命名需求".to_string()
            } else {
                title
            };
            let wi = sg_workitem::create(
                store,
                &str_param(params, "projectId")?,
                &title,
                "",
                None,
                &[],
            )
            .map_err(store_err)?;
            let doc_path =
                sg_workitem::docs::save(store, &wi.id, &filename, &content).map_err(store_err)?;
            if trace_writes_enabled() {
                sg_workitem::requirements::import_revision(
                    store,
                    &wi.id,
                    &filename,
                    &content,
                    "document",
                    "local-user",
                    "verified",
                )
                .map_err(store_err)?;
            }
            let mut value = serde_json::to_value(&wi).unwrap_or_default();
            value["requirementDoc"] = json!(doc_path);
            Ok(value)
        }
        "workitem.importIssue" => {
            let issue = state
                .gitlab
                .get_issue(
                    &str_param(params, "gitlabProjectId")?,
                    &str_param(params, "issueIid")?,
                )
                .map_err(|e| {
                    err(
                        if e.contains("forbidden") {
                            ErrorCode::GitlabUnconfigured
                        } else {
                            ErrorCode::GitlabUnreachable
                        },
                        e,
                    )
                })?;
            let wi = sg_workitem::create(
                store,
                &str_param(params, "projectId")?,
                &issue.title,
                &issue.body,
                Some(&issue.iid),
                &issue.labels,
            )
            .map_err(store_err)?;
            let doc = format!("# {}\n\n{}\n", issue.title, issue.body);
            let doc_path =
                sg_workitem::docs::save(store, &wi.id, "requirement.md", &doc).unwrap_or_default();
            if trace_writes_enabled() {
                sg_workitem::requirements::import_revision(
                    store,
                    &wi.id,
                    "requirement.md",
                    &doc,
                    "issue",
                    "local-user",
                    "verified",
                )
                .map_err(store_err)?;
            }
            let mut value = serde_json::to_value(&wi).unwrap_or_default();
            value["requirementDoc"] = json!(doc_path);
            Ok(value)
        }

        // --- 需求版本 / 追溯（ADR-030 M1）---
        "requirement.importRevision" => {
            let result = sg_workitem::requirements::import_revision(
                store,
                &str_param(params, "workItemId")?,
                &str_param(params, "filename")?,
                &str_param(params, "content")?,
                &opt_str_param(params, "sourceKind").unwrap_or_else(|| "document".into()),
                &opt_str_param(params, "createdBy").unwrap_or_else(|| "local-user".into()),
                "verified",
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(result).unwrap_or_default())
        }
        "requirement.revisions" => {
            let grouped =
                sg_workitem::requirements::revisions(store, &str_param(params, "workItemId")?)
                    .map_err(store_err)?;
            Ok(json!({"items": grouped.iter().map(|(doc, revs)| json!({
                "document": doc,
                "revisions": revs,
            })).collect::<Vec<_>>()}))
        }
        "requirement.items" => {
            let items = sg_workitem::requirements::items(store, &str_param(params, "revisionId")?)
                .map_err(store_err)?;
            Ok(json!({"items": items}))
        }
        "requirement.get" => {
            let revision_id = str_param(params, "revisionId")?;
            let items = sg_workitem::requirements::items(store, &revision_id).map_err(store_err)?;
            // 修订本体在 revisions 聚合中返回过；此处携带条目与谱系覆盖。
            let workitem_id = store
                .with_conn(|conn| {
                    conn.query_row(
                        "SELECT d.workitem_id FROM requirement_revisions r
                         JOIN requirement_documents d ON d.id = r.document_id WHERE r.id=?1",
                        [&revision_id],
                        |r| r.get::<_, String>(0),
                    )
                    .map_err(|_| Error::Message(format!("not_found: 修订 {revision_id}")))
                })
                .map_err(store_err)?;
            let coverage =
                sg_provenance::coverage(store, &workitem_id, &revision_id).map_err(store_err)?;
            Ok(
                json!({"revisionId": revision_id, "workItemId": workitem_id, "items": items, "coverage": coverage}),
            )
        }

        // --- 谱系查询（只读）---
        "trace.lineage" => {
            let direction = opt_str_param(params, "direction").unwrap_or_else(|| "both".into());
            if !matches!(direction.as_str(), "up" | "down" | "both") {
                return Err(err(ErrorCode::InvalidParams, "direction 须为 up/down/both"));
            }
            let depth = params.get("depth").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
            let result =
                sg_provenance::lineage(store, &str_param(params, "nodeId")?, &direction, depth)
                    .map_err(store_err)?;
            Ok(serde_json::to_value(result).unwrap_or_default())
        }
        "trace.coverage" => {
            let workitem_id = str_param(params, "workItemId")?;
            let revision_id = match opt_str_param(params, "revisionId") {
                Some(id) => id,
                None => sg_workitem::requirements::latest_revision_id(store, &workitem_id)
                    .map_err(store_err)?
                    .ok_or_else(|| {
                        err(
                            ErrorCode::NotFound,
                            format!("工作项 {workitem_id} 尚无需求修订"),
                        )
                    })?,
            };
            sg_provenance::coverage(store, &workitem_id, &revision_id).map_err(store_err)
        }
        "trace.gaps" => {
            sg_provenance::gaps(store, &str_param(params, "workItemId")?).map_err(store_err)
        }

        // --- B8 影响面（RDWS v1.4 WP-5，纯读；WP-6 审批绑定消费）---
        "impact.forProposal" => {
            let r = sg_provenance::impact::for_proposal(store, &str_param(params, "proposalId")?)
                .map_err(store_err)?;
            Ok(sg_provenance::impact::to_json(&r))
        }

        // --- 快照 / 回滚（ADR-030 M3）---
        "snapshot.get" => {
            let snap = sg_workitem::snapshot::get(store, &str_param(params, "snapshotId")?)
                .map_err(store_err)?
                .ok_or_else(|| err(ErrorCode::NotFound, "快照不存在"))?;
            let mut v = serde_json::to_value(&snap).unwrap_or_default();
            v["resources"] = serde_json::to_value(
                sg_workitem::snapshot::resources(store, &snap.id).map_err(store_err)?,
            )
            .unwrap_or_default();
            Ok(v)
        }
        "snapshot.list" => {
            let items = sg_workitem::snapshot::list(store, &str_param(params, "workItemId")?)
                .map_err(store_err)?;
            Ok(json!({ "items": items }))
        }
        "rollback.preview" => {
            let result = sg_workitem::rollback::preview(
                store,
                &str_param(params, "workItemId")?,
                &str_param(params, "targetSnapshotId")?,
                &release_policy_version(store),
            )
            .map_err(store_err)?;
            Ok(result)
        }
        "rollback.request" => {
            // E2E 钩子（仅显式设置生效）：RATIFLOW_APPROVAL_TTL_SECS 覆盖回滚审批有效期。
            let ttl = std::env::var("RATIFLOW_APPROVAL_TTL_SECS")
                .ok()
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or_else(|| assemble_policy_snapshot(store).0.approval_ttl_secs);
            let result = sg_workitem::rollback::request(
                store,
                &str_param(params, "workItemId")?,
                &str_param(params, "targetSnapshotId")?,
                &opt_str_param(params, "requestedBy").unwrap_or_else(|| "local-user".into()),
                &release_policy_version(store),
                ttl,
            )
            .map_err(store_err)?;
            Ok(result)
        }
        "rollback.decide" => {
            let approval_id = str_param(params, "approvalId")?;
            let decided_by = str_param(params, "decidedBy")?;
            let result = sg_workitem::rollback::decide(
                store,
                &approval_id,
                &str_param(params, "decision")?,
                &decided_by,
                &opt_str_param(params, "reason").unwrap_or_default(),
                &release_policy_version(store),
            )
            .map_err(store_err)?;
            sg_store::audit::append(
                store,
                &decided_by,
                &format!("rollback.{}", str_param(params, "decision")?),
                "approval",
                &approval_id,
                json!({}),
            )
            .map_err(store_err)?;
            Ok(result)
        }
        "rollback.get" => {
            sg_workitem::rollback::get(store, &str_param(params, "operationId")?).map_err(store_err)
        }
        "rollback.list" => {
            let items = sg_workitem::rollback::list(store, &str_param(params, "workItemId")?)
                .map_err(store_err)?;
            Ok(json!({ "items": items }))
        }

        // --- 工件 ---
        "artifact.list" => {
            let items = sg_artifact::list_artifacts(store, &str_param(params, "workItemId")?)
                .map_err(store_err)?;
            Ok(json!({"items": items}))
        }
        "artifact.create" => {
            let art = sg_artifact::create_artifact(
                store,
                &str_param(params, "workItemId")?,
                &str_param(params, "kind")?,
                &str_param(params, "title")?,
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(art).unwrap_or_default())
        }
        "artifact.createDraft" => {
            let artifact_id = str_param(params, "artifactId")?;
            let requirement_keys = str_list_param(params, "requirementKeys");
            // fail-closed：先解析需求 key 与工件归属，再落修订。
            let workitem_id: String = store
                .with_conn(|conn| {
                    conn.query_row(
                        "SELECT workitem_id FROM artifacts WHERE id=?1",
                        [&artifact_id],
                        |r| r.get::<_, String>(0),
                    )
                    .map_err(|_| Error::Message(format!("not_found: 工件 {artifact_id}")))
                })
                .map_err(store_err)?;
            let resolved_items = resolve_requirement_items(store, &workitem_id, &requirement_keys)
                .map_err(store_err)?;
            let rev =
                sg_artifact::create_draft(store, &artifact_id, &str_param(params, "content")?)
                    .map_err(store_err)?;
            if trace_writes_enabled() {
                sg_provenance::register_node(
                    store,
                    &sg_provenance::NodeInput {
                        project_id: "",
                        workitem_id: &workitem_id,
                        node_type: sg_provenance::node_type::ARTIFACT_REVISION,
                        entity_id: &rev.id,
                        content_digest: &rev.content_sha256,
                        verification_state: "verified",
                    },
                )
                .map_err(store_err)?;
                trace_link_items(
                    store,
                    &workitem_id,
                    sg_provenance::node_type::ARTIFACT_REVISION,
                    &rev.id,
                    sg_provenance::relation::SATISFIES,
                    &resolved_items,
                )
                .map_err(store_err)?;
            }
            // M2：修订即输出变化 → 该关 pending 放行失效（AC-SW-03 前置）。
            sg_workitem::release::invalidate_pending_if_drift(
                store,
                &workitem_id,
                workitem_gate(store, &workitem_id).as_str(),
                &release_policy_version(store),
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(rev).unwrap_or_default())
        }
        "artifact.updateDraft" => {
            let rev = sg_artifact::update_draft(
                store,
                &str_param(params, "revisionId")?,
                &str_param(params, "etag")?,
                &str_param(params, "content")?,
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(rev).unwrap_or_default())
        }
        "artifact.listRevisions" => {
            let items = sg_artifact::list_revisions(store, &str_param(params, "artifactId")?)
                .map_err(store_err)?;
            Ok(json!({"items": items}))
        }
        "artifact.revisionContent" => {
            let content = sg_artifact::revision_content(store, &str_param(params, "revisionId")?)
                .map_err(store_err)?;
            Ok(json!({"content": String::from_utf8_lossy(&content)}))
        }
        "artifact.addReview" => {
            sg_artifact::add_review(
                store,
                &str_param(params, "revisionId")?,
                &str_param(params, "reviewer")?,
                &str_param(params, "verdict")?,
                &opt_str_param(params, "comment").unwrap_or_default(),
                opt_str_param(params, "gitlabMrIid").as_deref(),
            )
            .map_err(store_err)?;
            Ok(json!({"status": "reviewed"}))
        }
        "artifact.freezeBaseline" => {
            let workitem_id = str_param(params, "workItemId")?;
            let gate_name = str_param(params, "gate")?;
            // M1-04：gate_id 按实例动态校验（取代六值枚举假设）。
            if !sg_workitem::gate_known(store, &workitem_id, &gate_name).map_err(store_err)? {
                return Err(err(ErrorCode::InvalidParams, "unknown gate"));
            }
            let revision_ids: Vec<String> = params
                .get("revisionIds")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(String::from))
                        .collect()
                })
                .ok_or_else(|| err(ErrorCode::InvalidParams, "revisionIds required"))?;
            // M2：基线绑定到当前关活跃 attempt（冻结即执行中工作）。
            let mut attempt = sg_workitem::attempt::ensure_active(store, &workitem_id, &gate_name)
                .map_err(store_err)?;
            if attempt.state == "prepared" {
                attempt = sg_workitem::attempt::transition(store, &attempt.id, "running")
                    .map_err(store_err)?;
            }
            let base = sg_artifact::freeze(
                store,
                &workitem_id,
                &gate_name,
                &revision_ids,
                &opt_str_param(params, "gitlabCommitSha").unwrap_or_default(),
                &attempt.id,
            )
            .map_err(store_err)?;
            // 新基线冻结 → 放行请求漂移失效（AC-SW-03）+ 下游 stale 传播（core 编排）。
            sg_workitem::release::invalidate_pending_if_drift(
                store,
                &workitem_id,
                &gate_name,
                &release_policy_version(store),
            )
            .map_err(store_err)?;
            let inputs = base.inputs_sha256.clone();
            let _ = sg_workitem::mark_stale_from(store, &workitem_id, &gate_name, &inputs);
            // 交付物自动入知识库（git 管理）：逐修订落 <repo>/knowledge/，失败逐条记录不阻塞审批。
            let project_id = sg_workitem::get(store, &workitem_id)
                .map_err(store_err)?
                .project_id;
            let knowledge_publish =
                publish_deliverables_to_knowledge(store, &project_id, &workitem_id, &base);
            let mut out = serde_json::to_value(base).unwrap_or_default();
            if let Some(obj) = out.as_object_mut() {
                obj.insert("knowledgePublish".into(), knowledge_publish);
            }
            Ok(out)
        }

        // --- Agent ---
        // 唯一入口（ADR-028）：建行（幂等）→ 立即返回 runId，循环在独立任务/连接上执行。
        // --- M4（EvoFlow / ADR-038）：Team / Context Policy / Middleware ---
        "agentTeam.list" => {
            let teams = sg_agent::team::list_teams(store).map_err(store_err)?;
            Ok(json!({ "items": teams }))
        }
        "agentTeam.create" => {
            let t = sg_agent::team::create_team(
                store,
                &str_param(params, "key")?,
                &str_param(params, "name")?,
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(t).unwrap_or_default())
        }
        "agentTeam.createVersion" => {
            let members: Vec<sg_agent::team::TeamMemberInput> = params
                .get("members")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .map(|m| sg_agent::team::TeamMemberInput {
                            role_key: m
                                .get("roleKey")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .into(),
                            profile_version_id: m
                                .get("profileVersionId")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .into(),
                            fallback_mode: m
                                .get("fallbackMode")
                                .and_then(|v| v.as_str())
                                .unwrap_or("generic")
                                .into(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let v = sg_agent::team::create_version(
                store,
                &str_param(params, "teamId")?,
                &str_param(params, "leadRoleKey")?,
                params
                    .get("maxConcurrency")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(3),
                &params
                    .get("requiredCapabilities")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect::<Vec<String>>()
                    })
                    .unwrap_or_default(),
                params
                    .get("reviewPolicy")
                    .and_then(|v| v.as_str())
                    .unwrap_or("none"),
                params
                    .get("fallbackMode")
                    .and_then(|v| v.as_str())
                    .unwrap_or("generic"),
                &members,
                params
                    .get("createdBy")
                    .and_then(|v| v.as_str())
                    .unwrap_or("local"),
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "agentTeam.activate" => {
            let v = sg_agent::team::activate(store, &str_param(params, "versionId")?)
                .map_err(store_err)?;
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "agentTeam.resolvePreview" => {
            // Role 选路预览：direct / fallback 证据 / fail_closed 明确失败。
            let version_id = str_param(params, "teamVersionId")?;
            let role_key = str_param(params, "roleKey")?;
            let r =
                sg_agent::team::resolve_role(store, &version_id, &role_key).map_err(store_err)?;
            Ok(serde_json::to_value(r).unwrap_or_default())
        }
        "contextPolicy.createVersion" => {
            let allowed: Vec<String> = params
                .get("allowedTools")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let v = sg_context::policy::create_version(
                store,
                &str_param(params, "key")?,
                opt_str_param(params, "gateId").as_deref(),
                &params
                    .get("sources")
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "[]".into()),
                &allowed,
                &params
                    .get("compaction")
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "{}".into()),
                params
                    .get("createdBy")
                    .and_then(|v| v.as_str())
                    .unwrap_or("local"),
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "contextPolicy.activate" => {
            let v = sg_context::policy::activate(store, &str_param(params, "versionId")?)
                .map_err(store_err)?;
            Ok(serde_json::to_value(v).unwrap_or_default())
        }
        "contextPolicy.activeList" => {
            // 服务端交集预览：EV-014 可视化断言面。
            let key = str_param(params, "key")?;
            let policy = sg_context::policy::active_by_key(store, &key)
                .map_err(store_err)?
                .ok_or_else(|| err(ErrorCode::InvalidParams, "context_policy_not_found"))?;
            let allowed: Vec<String> =
                serde_json::from_str(&policy.allowed_tools_json).unwrap_or_default();
            let registry_all: Vec<String> = sg_agent::tools::registry()
                .iter()
                .map(|d| d.name.to_string())
                .collect();
            let client: Vec<String> = params
                .get("clientRequest")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let resolved =
                sg_context::policy::resolve_tools(&registry_all, Some(&allowed), &client);
            Ok(json!({
                "policyVersionId": policy.id,
                "digest": policy.content_digest,
                "effective": resolved.effective,
                "excluded": resolved.excluded,
            }))
        }
        "middlewareProfile.createVersion" => {
            use sg_agent::middleware::MiddlewareStep;
            let steps: Vec<MiddlewareStep> = params
                .get("steps")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .map(|s| MiddlewareStep {
                            name: s.get("name").and_then(|v| v.as_str()).unwrap_or("").into(),
                            params: s.get("params").cloned().unwrap_or(json!({})),
                        })
                        .collect()
                })
                .unwrap_or_default();
            // M4-07：顺序校验（security 不可移除/重排，非内建拒绝）。
            sg_agent::middleware::validate_order(&steps)
                .map_err(|e| err(ErrorCode::InvalidParams, e.as_str()))?;
            let digest = sg_agent::middleware::profile_digest(&steps);
            let id = sg_store::ids::new_id("mpv");
            let now = sg_store::timefmt::now();
            let key = str_param(params, "key")?;
            store.with_conn(|conn| {
                conn.execute(
                    "INSERT INTO middleware_profile_versions(id, key, version_no, status, steps_json, content_digest, created_by, created_at, updated_at)
                     VALUES (?1,?2,(SELECT COALESCE(MAX(version_no),0)+1 FROM middleware_profile_versions WHERE key=?3),
                        'draft',?4,?5,?6,?7,?7)",
                    rusqlite::params![
                        id,
                        key,
                        key,
                        serde_json::to_string(&steps).unwrap_or_default(),
                        digest,
                        params.get("createdBy").and_then(|v| v.as_str()).unwrap_or("local"),
                        now
                    ],
                )
                .map_err(Error::from)?;
                Ok(())
            })
            .map_err(store_err)?;
            Ok(json!({"versionId": id, "digest": digest}))
        }
        "middlewareProfile.activate" => {
            let version_id = str_param(params, "versionId")?;
            store.with_conn(|conn| {
                let status: String = conn
                    .query_row(
                        "SELECT status FROM middleware_profile_versions WHERE id=?1",
                        [&version_id],
                        |r| r.get(0),
                    )
                    .map_err(Error::from)?;
                if status != "draft" {
                    return Err(Error::Message("middleware_profile_invalid: 仅 draft 可激活".into()));
                }
                conn.execute(
                    "UPDATE middleware_profile_versions SET status='deprecated', updated_at=?1
                     WHERE key=(SELECT key FROM middleware_profile_versions WHERE id=?2) AND status='active'",
                    rusqlite::params![sg_store::timefmt::now(), version_id],
                )?;
                conn.execute(
                    "UPDATE middleware_profile_versions SET status='active', updated_at=?1 WHERE id=?2",
                    rusqlite::params![sg_store::timefmt::now(), version_id],
                )?;
                Ok(())
            })
            .map_err(store_err)?;
            Ok(json!({"status": "active"}))
        }
        "middlewareProfile.validate" => {
            use sg_agent::middleware::MiddlewareStep;
            let steps: Vec<MiddlewareStep> = params
                .get("steps")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .map(|s| MiddlewareStep {
                            name: s.get("name").and_then(|v| v.as_str()).unwrap_or("").into(),
                            params: s.get("params").cloned().unwrap_or(json!({})),
                        })
                        .collect()
                })
                .unwrap_or_default();
            match sg_agent::middleware::validate_order(&steps) {
                Ok(()) => Ok(
                    json!({"valid": true, "digest": sg_agent::middleware::profile_digest(&steps)}),
                ),
                Err(e) => Ok(json!({"valid": false, "error": e})),
            }
        }
        "agent.start" => {
            let workitem_id = str_param(params, "workItemId")?;
            let goal = str_param(params, "goal")?;
            // ADR-032 M2：服务端创建/验证 manifest。旧客户端可暂传 contextManifestId，
            // 但必须通过归属验证并原样冻结；下一协议版本移除该参数（实施方案 §13 M2）。
            let manifest_id = match opt_str_param(params, "contextManifestId") {
                Some(id) => {
                    sg_context::manifest::require_manifest_for_workitem(store, &id, &workitem_id)
                        .map_err(store_err)?;
                    id
                }
                None => {
                    let wi_project: String = store
                        .with_conn(|conn| {
                            conn.query_row(
                                "SELECT project_id FROM workitems WHERE id=?1",
                                [&workitem_id],
                                |r| r.get::<_, String>(0),
                            )
                            .map_err(|_| Error::Message("not_found: workitem".into()))
                        })
                        .map_err(store_err)?;
                    let built = sg_context::build_manifest(
                        store,
                        &sg_context::BuildInput {
                            project_id: &wi_project,
                            workitem_id: &workitem_id,
                            goal: &goal,
                            selected_sources: &[],
                        },
                    )
                    .map_err(store_err)?;
                    built["id"].as_str().unwrap_or_default().to_string()
                }
            };
            let mut allowlist: Vec<String> = params
                .get("toolAllowlist")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_else(|| vec!["read_file".into()]);
            // M4-08（EV-014 / ADR-038 §6.10）：服务端工具交集——客户端只可收紧。
            // RATIFLOW_CONTEXT_POLICY_V2=1 且存在 active policy 时，
            // effective = registry ∩ policy ∩ client（越权请求被移除并记 excluded）。
            let mut ctx_policy_frozen: Option<String> = None;
            let mut ctx_excluded: serde_json::Value = serde_json::Value::Null;
            if std::env::var("RATIFLOW_CONTEXT_POLICY_V2").ok().as_deref() == Some("1") {
                let gate_for_policy = sg_workitem::get(store, &workitem_id)
                    .map(|w| w.current_gate)
                    .unwrap_or_default();
                let policy = sg_context::policy::active_by_key(store, &gate_for_policy)
                    .or_else(|_| sg_context::policy::active_by_key(store, "global"))
                    .unwrap_or(None);
                if let Some(p) = policy {
                    let allowed: Vec<String> =
                        serde_json::from_str(&p.allowed_tools_json).unwrap_or_default();
                    let registry_all: Vec<String> = sg_agent::tools::registry()
                        .iter()
                        .map(|d| d.name.to_string())
                        .collect();
                    let resolved = sg_context::policy::resolve_tools(
                        &registry_all,
                        Some(&allowed),
                        &allowlist,
                    );
                    allowlist = resolved.effective;
                    ctx_policy_frozen = Some(p.id);
                    ctx_excluded = serde_json::to_value(&resolved.excluded).unwrap_or_default();
                }
            }
            let budget: sg_agent::RunBudget = params
                .get("budget")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            let idem = opt_str_param(params, "idempotencyKey")
                .unwrap_or_else(|| format!("rpc-{}", sg_store::ids::new_id("idem")));
            let task_id = opt_str_param(params, "taskId").unwrap_or_default();
            let config = sg_agent::RunConfig {
                workitem_id: &workitem_id,
                task_id: &task_id,
                goal: &goal,
                manifest_id: &manifest_id,
                tool_allowlist: &allowlist,
                idempotency_key: &idem,
                budget: &budget,
                max_iterations: 20,
            };
            let (run, created) = sg_agent::create_run(store, &config).map_err(store_err)?;
            if created {
                // EvoFlow M3 评审修复：自治模式与 Grant 冻结进 Run（Ask 只读边界与
                // Grant 白名单在工具执行链逐动作校验）；任务级 attempt 绑定 =
                // 真实执行链入口（transition succeeded 需绑定 Run 终态证明）。
                let autonomy_mode = opt_str_param(params, "autonomyMode");
                if let Some(m) = &autonomy_mode {
                    if sg_policy::autonomy::AutonomyMode::parse(m).is_none() {
                        return Err(err(
                            ErrorCode::InvalidParams,
                            format!("autonomyMode 须为 ask|agent|plan，得到 {m}"),
                        ));
                    }
                }
                let autonomy_grant = opt_str_param(params, "autonomyGrantId");
                if let Some(g) = &autonomy_grant {
                    sg_policy::autonomy::validate_grant_status(store, g, &sg_store::timefmt::now())
                        .map_err(|e| {
                            err(
                                ErrorCode::InvalidParams,
                                format!("autonomyGrant 不可用: {e}"),
                            )
                        })?;
                }
                let plan_attempt = opt_str_param(params, "planTaskAttemptId");
                if let Some(pa) = &plan_attempt {
                    bind_plan_task_attempt(store, &run.id, &workitem_id, pa)
                        .map_err(|e| err(ErrorCode::InvalidParams, e))?;
                }
                // M4-08：冻结 context policy（可回查 digest 链，§3 不变量 8）。
                if let Some(pid) = &ctx_policy_frozen {
                    let _ = store.with_conn(|conn| {
                        conn.execute(
                            "UPDATE agent_runs SET context_policy_version_id=?1 WHERE id=?2",
                            rusqlite::params![pid, run.id],
                        )?;
                        Ok(())
                    });
                }
                if ctx_excluded.is_array() {
                    let _ = sg_store::outbox::emit(
                        store,
                        "workitem",
                        &workitem_id,
                        "context.tools_excluded",
                        json!({"runId": run.id, "excluded": ctx_excluded}),
                    );
                }
                if trace_writes_enabled() {
                    sg_provenance::register_node(
                        store,
                        &sg_provenance::NodeInput {
                            project_id: "",
                            workitem_id: &workitem_id,
                            node_type: sg_provenance::node_type::AGENT_RUN,
                            entity_id: &run.id,
                            content_digest: "",
                            verification_state: "verified",
                        },
                    )
                    .map_err(store_err)?;
                }
                let instructions = spawn_run_task(
                    state,
                    store,
                    &run.id,
                    autonomy_mode.as_deref(),
                    autonomy_grant.as_deref(),
                )?;
                return Ok(
                    json!({"runId": run.id, "status": run.status, "instructions": instructions}),
                );
            }
            Ok(json!({"runId": run.id, "status": run.status}))
        }
        "agent.get" => {
            let run_id = str_param(params, "runId")?;
            let run = sg_agent::get_run(store, &run_id).map_err(store_err)?;
            let mut v = serde_json::to_value(&run).unwrap_or_default();
            v["modelCalls"] = json!(sg_agent::count_model_calls(store, &run_id).unwrap_or(0));
            // M4-08：冻结面透出（allowlist 交集 + context/middleware/team 版本引用，
            // §3 不变量 8 可回查 digest 链）。
            {
                let row: (String, Option<String>, Option<String>, Option<String>) = store
                    .with_conn(|conn| {
                        conn.query_row(
                            "SELECT tool_allowlist, context_policy_version_id,
                                    middleware_profile_version_id, team_version_id
                             FROM agent_runs WHERE id=?1",
                            [&run_id],
                            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                        )
                        .map_err(Error::from)
                    })
                    .unwrap_or_default();
                v["toolAllowlist"] =
                    serde_json::from_str::<serde_json::Value>(&row.0).unwrap_or_else(|_| json!([]));
                v["contextPolicyVersionId"] = json!(row.1);
                v["middlewareProfileVersionId"] = json!(row.2);
                v["teamVersionId"] = json!(row.3);
            }
            // F10：真实权限快照（'default' 为旧占位）。
            let snap_raw: String = store
                .with_conn(|conn| {
                    Ok(conn
                        .query_row(
                            "SELECT policy_snapshot FROM agent_runs WHERE id=?1",
                            [&run_id],
                            |r| r.get::<_, String>(0),
                        )
                        .unwrap_or_default())
                })
                .unwrap_or_default();
            v["policySnapshot"] = json!(snap_raw);
            // M4：关卡绑定与选路记录透出（AC-SW-08 运行详情可见）。
            let bindings: (String, String, String, String) = store
                .with_conn(|conn| {
                    conn.query_row(
                        "SELECT stage_attempt_id, stage_activity_id, agent_selection_id, input_snapshot_id FROM agent_runs WHERE id=?1",
                        [&run_id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                    )
                    .map_err(Error::from)
                })
                .unwrap_or_default();
            v["stageAttemptId"] = json!(bindings.0);
            v["stageActivityId"] = json!(bindings.1);
            v["agentSelectionId"] = json!(bindings.2);
            v["inputSnapshotId"] = json!(bindings.3);
            if !bindings.2.is_empty() {
                if let Ok(sel) = sg_agent::router::get(store, &bindings.2) {
                    if let Ok(ver) =
                        sg_agent::profile::get_version(store, &sel.resolved_profile_version_id)
                    {
                        v["agentSelection"] = json!({
                            "selectionId": sel.id,
                            "resolvedProfileVersionId": sel.resolved_profile_version_id,
                            "profileId": ver.profile_id,
                            "versionNo": ver.version_no,
                            "contentDigest": ver.content_digest,
                            "sourceScope": sel.source_scope,
                            "fallbackUsed": sel.fallback_used,
                            "reasonCode": sel.reason_code,
                            "candidates": sel.candidate_report,
                        });
                    }
                }
            }
            // ADR-032 M2：本次 Run 实际采用的记忆证据（ID/bytes；正文永不出协议）。
            let run_manifest_id: String = store
                .with_conn(|conn| {
                    Ok(conn
                        .query_row(
                            "SELECT context_manifest_id FROM agent_runs WHERE id=?1",
                            [&run_id],
                            |r| r.get::<_, String>(0),
                        )
                        .unwrap_or_default())
                })
                .unwrap_or_default();
            if !run_manifest_id.is_empty() {
                if let Ok(ev) = sg_context::blocks::memory_evidence(store, &run_manifest_id) {
                    v["memory"] = ev;
                }
            }
            // rollout 摘要（F04）：行数/字节/路径；全文查看走 logs.* 既有域。
            let path = sg_agent::rollout::Rollout::path_for(&store.data_dir, &run_id);
            v["rollout"] = match std::fs::read_to_string(&path) {
                Ok(body) => json!({
                    "lines": body.lines().count(),
                    "bytes": body.len(),
                    "path": path.to_string_lossy(),
                }),
                Err(_) => Value::Null,
            };
            Ok(v)
        }
        "agent.list" => {
            let items = sg_agent::list_recent(
                store,
                &str_param(params, "workItemId")?,
                params.get("limit").and_then(|v| v.as_i64()).unwrap_or(8),
            )
            .map_err(store_err)?;
            Ok(json!({"items": items}))
        }
        "agent.trace" => {
            let run_id = str_param(params, "runId")?;
            sg_agent::trace(store, &run_id).map_err(store_err)
        }
        "agent.cancel" => {
            let run_id = str_param(params, "runId")?;
            cancel_run_shared(state, store, &run_id)
                .map_err(|e| RpcError::new(ErrorCode::InvalidParams, e.as_str()))
        }
        // --- M6：受控 MCP ToolProvider（ADR-035）---
        "mcp.serverAdd" => {
            if sg_settings::mcp_ext::mcp_disabled() {
                return Err(err(
                    ErrorCode::InvalidRequest,
                    "feature_disabled: RATIFLOW_MCP_MODE=disabled（MCP 已禁用）",
                ));
            }
            let args_list = str_list_param(params, "args");
            sg_settings::mcp_ext::server_add(
                store,
                &str_param(params, "name")?,
                &str_param(params, "command")?,
                &args_list,
            )
            .map_err(serr)
        }
        "mcp.serverApprove" => {
            if sg_settings::mcp_ext::mcp_disabled() {
                return Err(err(
                    ErrorCode::InvalidRequest,
                    "feature_disabled: RATIFLOW_MCP_MODE=disabled（MCP 已禁用）",
                ));
            }
            sg_settings::mcp_ext::server_approve(
                store,
                &str_param(params, "serverId")?,
                &str_param(params, "decidedBy")?,
            )
            .map_err(serr)
        }
        "mcp.serverList" => sg_settings::mcp_ext::server_list(store).map_err(serr),
        "mcp.serverRemove" => sg_settings::mcp_ext::server_revoke(
            store,
            &str_param(params, "serverId")?,
            &str_param(params, "decidedBy")?,
            &opt_str_param(params, "reason").unwrap_or_default(),
        )
        .map_err(serr),
        "mcp.serverRefresh" => {
            if sg_settings::mcp_ext::mcp_disabled() {
                return Err(err(
                    ErrorCode::InvalidRequest,
                    "feature_disabled: RATIFLOW_MCP_MODE=disabled（MCP 已禁用）",
                ));
            }
            sg_settings::mcp_ext::server_refresh(store, &str_param(params, "serverId")?)
                .map_err(serr)
        }
        "mcp.serverToggle" => {
            if sg_settings::mcp_ext::mcp_disabled() {
                return Err(err(
                    ErrorCode::InvalidRequest,
                    "feature_disabled: RATIFLOW_MCP_MODE=disabled（MCP 已禁用）",
                ));
            }
            sg_settings::mcp_ext::server_set_enabled(
                store,
                &str_param(params, "serverId")?,
                bool_param(params, "enabled")?,
            )
            .map_err(serr)
        }
        "mcp.toolsList" => mcp_tools_list(store, opt_str_param(params, "serverId").as_deref()),

        // --- Git 仓库导入 MCP（RDWS v1.4 WP-4 / ADR flag：RATIFLOW_MCP_GIT_IMPORT）---
        "mcp.importAdd" => with_rpc_receipt(
            store,
            &opt_str_param(params, "idempotencyKey").unwrap_or_default(),
            "mcp.importAdd",
            params,
            || {
                sg_settings::mcp_import::import_add(
                    store,
                    &store.data_dir,
                    &str_param(params, "repoUrl")?,
                    &str_param(params, "ref")?,
                    &opt_str_param(params, "createdBy").unwrap_or_else(|| "local-user".into()),
                )
                .map_err(store_err)
            },
        ),
        "mcp.importDecide" => with_rpc_receipt(
            store,
            &opt_str_param(params, "idempotencyKey").unwrap_or_default(),
            "mcp.importDecide",
            params,
            || {
                sg_settings::mcp_import::import_decide(
                    store,
                    &store.data_dir,
                    &str_param(params, "importId")?,
                    &str_param(params, "decision")?,
                    &str_param(params, "decidedBy")?,
                    &opt_str_param(params, "reason").unwrap_or_default(),
                )
                .map_err(store_err)
            },
        ),
        "mcp.importResume" => with_rpc_receipt(
            store,
            &opt_str_param(params, "idempotencyKey").unwrap_or_default(),
            "mcp.importResume",
            params,
            || {
                sg_settings::mcp_import::import_resume(
                    store,
                    &store.data_dir,
                    &str_param(params, "importId")?,
                )
                .map_err(store_err)
            },
        ),
        "mcp.importRevoke" => with_rpc_receipt(
            store,
            &opt_str_param(params, "idempotencyKey").unwrap_or_default(),
            "mcp.importRevoke",
            params,
            || {
                sg_settings::mcp_import::import_revoke(
                    store,
                    &store.data_dir,
                    &str_param(params, "importId")?,
                    &str_param(params, "decidedBy")?,
                    &opt_str_param(params, "reason").unwrap_or_default(),
                )
                .map_err(store_err)
            },
        ),
        "mcp.importList" => sg_settings::mcp_import::import_list(store).map_err(store_err),
        "mcp.importGet" => {
            sg_settings::mcp_import::import_get(store, &str_param(params, "importId")?)
                .map_err(store_err)
        }

        // M4：模型缓存与压缩观测（不含任何 reasoning 正文）。
        "model.usage" => {
            let run_id = opt_str_param(params, "runId");
            model_usage(state, store, run_id.as_deref())
        }
        "agent.proposals" => {
            let items =
                sg_agent::proposals(store, &str_param(params, "runId")?).map_err(store_err)?;
            Ok(json!({"items": items}))
        }

        // --- 门禁（M2/ADR-030：evaluate 只计算，绝不推进；放行走 gate.decideRelease）---
        "gate.evaluate" => {
            let workitem_id = str_param(params, "workItemId")?;
            let gate_name = str_param(params, "gate")?;
            // M1-04：gate_id 按实例动态校验。
            if !sg_workitem::gate_known(store, &workitem_id, &gate_name).map_err(store_err)? {
                return Err(err(ErrorCode::InvalidParams, "unknown gate"));
            }
            // 评估输入单一事实源（与放行的新鲜度重查共用 build_inputs，P0-2）。
            let inputs = sg_workitem::gate::build_inputs(store, &workitem_id, &gate_name)
                .map_err(store_err)?;
            let result =
                sg_workitem::gate::evaluate_and_record(store, &inputs).map_err(store_err)?;
            if result.passed {
                // attempt 投影推进（不碰 current_gate）。
                sg_workitem::attempt::advance_to_review_ready(store, &workitem_id, &gate_name)
                    .map_err(store_err)?;
            }
            Ok(serde_json::to_value(result).unwrap_or_default())
        }
        "gate.requestRelease" => {
            // 关卡放行审批不限时（人工评审无期限）；E2E 需要限时行为时显式设置
            // RATIFLOW_APPROVAL_TTL_SECS（秒）即可恢复过期语义。
            let ttl = std::env::var("RATIFLOW_APPROVAL_TTL_SECS")
                .ok()
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(0);
            let result = sg_workitem::release::request_release(
                store,
                &str_param(params, "workItemId")?,
                &str_param(params, "gate")?,
                &release_policy_version(store),
                ttl,
            )
            .map_err(store_err)?;
            Ok(result)
        }
        "gate.decideRelease" => gate_decide_release_rpc(store, params),
        "gate.getRelease" => {
            sg_workitem::release::get_release(store, &str_param(params, "releaseId")?)
                .map_err(store_err)
        }
        // --- WP-7 结构化验收：manual_confirm 人工确认链（决定走 approval.decide 路由）---
        "gate.requestManualConfirmation" => {
            let workitem_id = str_param(params, "workItemId")?;
            let gate_name = str_param(params, "gate")?;
            if !sg_workitem::gate_known(store, &workitem_id, &gate_name).map_err(store_err)? {
                return Err(err(ErrorCode::InvalidParams, "unknown gate"));
            }
            let element = params
                .get("element")
                .cloned()
                .ok_or_else(|| err(ErrorCode::InvalidParams, "missing param: element"))?;
            let requested_by = str_param(params, "requestedBy")?;
            let reason = opt_str_param(params, "reason").unwrap_or_default();
            let confirmation = sg_workitem::manual_confirm::request(
                store,
                &workitem_id,
                &gate_name,
                &element,
                &requested_by,
                &reason,
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(confirmation).unwrap_or_default())
        }
        "gate.manualConfirmations" => {
            let workitem_id = str_param(params, "workItemId")?;
            let gate_name = opt_str_param(params, "gate");
            let items = sg_workitem::manual_confirm::list(
                store,
                &workitem_id,
                gate_name.as_deref().filter(|g| !g.is_empty()),
            )
            .map_err(store_err)?;
            Ok(json!({ "items": items }))
        }
        // --- WP-8：gate.requestSkip——人工经审批跳关（flag=RATIFLOW_GATE_SKIP，
        //     默认 0；=0 禁止新建，存量 skipped 关照实返回 outcome 不退化）---
        "gate.requestSkip" => {
            if std::env::var("RATIFLOW_GATE_SKIP").ok().as_deref() != Some("1") {
                return Err(err(
                    ErrorCode::InvalidRequest,
                    "feature_disabled: RATIFLOW_GATE_SKIP 未开启",
                ));
            }
            let workitem_id = str_param(params, "workItemId")?;
            let gate_name = str_param(params, "gateId")?;
            if !sg_workitem::gate_known(store, &workitem_id, &gate_name).map_err(store_err)? {
                return Err(err(ErrorCode::InvalidParams, "unknown gate"));
            }
            // 策略（实例冻结版本）：无声明或 forbidden → 拒；部署/迁移类恒 forbidden
            //（创建面已拒，此处复核 = 纵深防御）。
            let instance = sg_workflow::instance::for_workitem(store, &workitem_id)
                .map_err(store_err)?
                .ok_or_else(|| err(ErrorCode::InvalidParams, "workflow instance missing"))?;
            let defs =
                sg_workflow::template::definitions_via_store(store, &instance.template_version_id)
                    .map_err(store_err)?;
            let def = defs
                .iter()
                .find(|d| d.gate_id == gate_name)
                .ok_or_else(|| err(ErrorCode::InvalidParams, "unknown gate"))?;
            let manual_allowed = def
                .skip_policy
                .as_ref()
                .map(|p| p.mode == sg_workflow::template::SkipMode::ManualApproval)
                .unwrap_or(false);
            let deployment_class = def
                .deliverables
                .iter()
                .any(|k| sg_workflow::template::is_deployment_class_kind(k));
            if !manual_allowed || deployment_class {
                return Err(err(
                    ErrorCode::ActionDenied,
                    format!(
                        "gate_skip_forbidden: 关 {gate_name} 的 skip 策略为 forbidden{}",
                        if deployment_class {
                            "（部署/迁移类恒 forbidden）"
                        } else {
                            ""
                        }
                    ),
                ));
            }
            // 替代证据必填且须在案（本工作项证据面）。
            let substitute_ids: Vec<String> = params
                .get("substituteEvidenceIds")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            if substitute_ids.is_empty() {
                return Err(err(
                    ErrorCode::InvalidParams,
                    "substitute_evidence_missing: 替代证据必填",
                ));
            }
            let known = sg_evidence::list(store, &workitem_id, None).map_err(store_err)?;
            for id in &substitute_ids {
                if !known.iter().any(|e| &e.id == id) {
                    return Err(err(
                        ErrorCode::InvalidParams,
                        format!("substitute_evidence_missing: 证据 {id} 不存在"),
                    ));
                }
            }
            let waiver = opt_str_param(params, "waiver").unwrap_or_default();
            // digest = sha256(gate_skip|workitem|gate|waiver|排序后替代证据 ids)；UNIQUE 幂等。
            let mut sorted_ids = substitute_ids.clone();
            sorted_ids.sort();
            let action_digest = format!("sha256:{}", {
                use sha2::Digest;
                sg_store::ids::hex(&sha2::Sha256::digest(
                    format!(
                        "gate_skip|{workitem_id}|{gate_name}|{waiver}|{}",
                        sorted_ids.join(",")
                    )
                    .as_bytes(),
                ))
            });
            // 幂等重放优先：同 digest 已有 gate_skip 审批 → 原样返回
            //（不受当前阶段状态影响——已批跳关的重放返回已批审批而非误报状态冲突）。
            if let Some(existing) =
                gate_skip_existing_approval(store, &action_digest).map_err(store_err)?
            {
                return Ok(existing);
            }
            // 阶段须未开工（仅 (NotStarted, Skipped) 迁移；已开工 → 状态已变）。
            let stage_state = sg_workitem::stages(store, &workitem_id)
                .map_err(store_err)?
                .into_iter()
                .find(|s| s.gate == gate_name)
                .map(|s| s.state)
                .unwrap_or_default();
            if stage_state != "not_started" {
                return Err(err(
                    ErrorCode::Conflict,
                    format!("gate_skip_state_changed: 关 {gate_name} 当前为 {stage_state}，仅未开工关可跳过"),
                ));
            }
            let attempt = sg_workitem::attempt::ensure_active(store, &workitem_id, &gate_name)
                .map_err(store_err)?;
            let approval = sg_policy::request_approval(
                store,
                "gate_skip",
                &attempt.id,
                &action_digest,
                sg_policy::Risk::High,
                &waiver,
                0,
                Some(&workitem_id),
                Some(&attempt.id),
            )
            .map_err(store_err)?;
            Ok(json!({
                "approvalId": approval.id,
                "actionDigest": action_digest,
                "state": "requested",
                "risk": approval.risk,
            }))
        }
        // --- WP-8 fast-track：六因素全真 → 建议落 shadow（不自动执行）；
        //     采纳后的缩减经 decideSuggestion 钩子应用（活动缩减+交付物豁免）---
        "gate.evaluateFastTrack" => {
            if std::env::var("RATIFLOW_GATE_SKIP").ok().as_deref() != Some("1") {
                return Err(err(
                    ErrorCode::InvalidRequest,
                    "feature_disabled: RATIFLOW_GATE_SKIP 未开启",
                ));
            }
            let workitem_id = str_param(params, "workItemId")?;
            let gate_name = str_param(params, "gate")?;
            if !sg_workitem::gate_known(store, &workitem_id, &gate_name).map_err(store_err)? {
                return Err(err(ErrorCode::InvalidParams, "unknown gate"));
            }
            let factors: sg_workflow::template::FastTrackFactors = params
                .get("factors")
                .cloned()
                .ok_or_else(|| err(ErrorCode::InvalidParams, "missing param: factors"))
                .and_then(|v| {
                    serde_json::from_value(v)
                        .map_err(|e| err(ErrorCode::InvalidParams, format!("factors 非法：{e}")))
                })?;
            let suggestion = sg_workitem::fast_track::evaluate_and_suggest(
                store,
                &workitem_id,
                &gate_name,
                &factors,
            )
            .map_err(|e| shadow_err(&e))?;
            Ok(serde_json::to_value(suggestion).unwrap_or_default())
        }
        // --- WP-9：A1 跨关返工五件套（flag=RATIFLOW_REWORK，默认 0）；
        //     decide 走 approval.decide 的 rework 主体路由到同一实现 ---
        "rework.preview" | "rework.request" | "rework.decide" | "rework.get" | "rework.list" => {
            if std::env::var("RATIFLOW_REWORK").ok().as_deref() != Some("1") {
                return Err(err(
                    ErrorCode::InvalidRequest,
                    "feature_disabled: RATIFLOW_REWORK 未开启",
                ));
            }
            match method {
                "rework.preview" => sg_workitem::rework::preview(
                    store,
                    &str_param(params, "workItemId")?,
                    &str_param(params, "targetGate")?,
                    &str_param(params, "reasonCode")?,
                    &opt_str_param(params, "note").unwrap_or_default(),
                    opt_str_param(params, "requestedBy")
                        .unwrap_or_else(|| "local".into())
                        .as_str(),
                )
                .map_err(store_err),
                "rework.request" => sg_workitem::rework::request(
                    store,
                    &str_param(params, "workItemId")?,
                    &str_param(params, "targetGate")?,
                    &str_param(params, "reasonCode")?,
                    &opt_str_param(params, "note").unwrap_or_default(),
                    opt_str_param(params, "requestedBy")
                        .unwrap_or_else(|| "local".into())
                        .as_str(),
                )
                .map_err(store_err),
                "rework.decide" => sg_workitem::rework::decide(
                    store,
                    &str_param(params, "approvalId")?,
                    &str_param(params, "decision")?,
                    &str_param(params, "decidedBy")?,
                    &opt_str_param(params, "reason").unwrap_or_default(),
                )
                .map_err(store_err),
                "rework.get" => sg_workitem::rework::get(store, &str_param(params, "operationId")?)
                    .map_err(store_err),
                _ => {
                    let items = sg_workitem::rework::list(store, &str_param(params, "workItemId")?)
                        .map_err(store_err)?;
                    Ok(json!({ "items": items }))
                }
            }
        }
        // --- WP-10：A5 指标投影（纯读）+ WP-11 Triage 纯读先行 ---
        "metrics.overview" => {
            let scope = str_param(params, "scope")?;
            sg_workflow::metrics::overview(store, &scope).map_err(store_err)
        }
        "triage.list" => sg_workflow::metrics::triage_list(store).map_err(store_err),
        "stage.attempts" => {
            let workitem_id = str_param(params, "workItemId")?;
            let attempts = sg_workitem::attempt::list(store, &workitem_id).map_err(store_err)?;
            let items: Vec<Value> = attempts
                .iter()
                .map(|a| {
                    let mut v = serde_json::to_value(a).unwrap_or_default();
                    v["activities"] = serde_json::to_value(
                        sg_workitem::attempt::activities(store, &a.id).unwrap_or_default(),
                    )
                    .unwrap_or_default();
                    v
                })
                .collect();
            Ok(json!({"items": items}))
        }
        "stage.startActivity" => {
            // M4 唯一关卡执行入口（SG-AGT-007）：attempt/快照/活动/选路/清单全部服务端装配。
            let workitem_id = str_param(params, "workItemId")?;
            let gate_name = str_param(params, "gate")?;
            // M1-04：gate_id 按实例动态校验。
            if !sg_workitem::gate_known(store, &workitem_id, &gate_name).map_err(store_err)? {
                return Err(err(ErrorCode::InvalidParams, "unknown gate"));
            }
            let goal = str_param(params, "goal")?;
            let required_caps = str_list_param(params, "requiredCapabilities");
            let task_override = opt_str_param(params, "profileVersionId");
            let idem = opt_str_param(params, "idempotencyKey")
                .unwrap_or_else(|| format!("sa-{}", sg_store::ids::new_id("idem")));
            // 只能在当前关启动活动（其他关的活跃 attempt 会被单活跃约束拒绝）。
            let wi_now = sg_workitem::get(store, &workitem_id).map_err(store_err)?;
            if wi_now.current_gate != gate_name {
                return Err(err(
                    ErrorCode::Conflict,
                    format!(
                        "attempt_active_exists: 工作项当前关为 {}，不能在 {gate_name} 启动活动",
                        wi_now.current_gate
                    ),
                ));
            }
            let attempt = sg_workitem::attempt::ensure_active(store, &workitem_id, &gate_name)
                .map_err(store_err)?;
            let activities =
                sg_workitem::attempt::activities(store, &attempt.id).map_err(store_err)?;
            let activity_key = match opt_str_param(params, "activityKey") {
                Some(key) => activities
                    .iter()
                    .find(|a| a.activity_key == key)
                    .ok_or_else(|| err(ErrorCode::InvalidParams, format!("未知活动 {key}")))?
                    .activity_key
                    .clone(),
                None => activities
                    .iter()
                    .find(|a| a.state == "pending")
                    .or_else(|| activities.first())
                    .ok_or_else(|| err(ErrorCode::Conflict, "attempt 无可用活动"))?
                    .activity_key
                    .clone(),
            };
            let activity_id = activities
                .iter()
                .find(|a| a.activity_key == activity_key)
                .map(|a| a.id.clone())
                .unwrap_or_default();
            let project_id: String = store
                .with_conn(|conn| {
                    conn.query_row(
                        "SELECT project_id FROM workitems WHERE id=?1",
                        [&workitem_id],
                        |r| r.get::<_, String>(0),
                    )
                    .map_err(|_| Error::Message("not_found: workitem".into()))
                })
                .map_err(store_err)?;
            // 选路（四级优先 + 回退语义），先于运行创建（Run 必须绑定 selection）。
            let selection = sg_agent::router::resolve(
                store,
                &sg_agent::router::ResolveContext {
                    project_id: &project_id,
                    gate: gate_name.as_str(),
                    activity_key: &activity_key,
                    stage_activity_id: &activity_id,
                    task_override_version_id: task_override.as_deref(),
                    required_capabilities: &required_caps,
                    persist: true,
                },
            )
            .map_err(store_err)?;
            sg_workitem::attempt::set_activity_state(store, &attempt.id, &activity_key, "running")
                .map_err(store_err)?;
            // 服务端装配上下文清单（客户端不再自报关键绑定）。
            let manifest = sg_context::build_manifest(
                store,
                &sg_context::BuildInput {
                    project_id: &project_id,
                    workitem_id: &workitem_id,
                    goal: &goal,
                    selected_sources: &[],
                },
            )
            .map_err(store_err)?;
            let manifest_id = manifest["id"].as_str().unwrap_or_default().to_string();
            let mut allowlist: Vec<String> = params
                .get("toolAllowlist")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_else(|| vec!["read_file".into()]);
            // M4-08（EV-014 / ADR-038 §6.10）：服务端工具交集——客户端只可收紧。
            // RATIFLOW_CONTEXT_POLICY_V2=1 且存在 active policy 时，
            // effective = registry ∩ policy ∩ client（越权请求被移除并记 excluded）。
            let mut ctx_policy_frozen: Option<String> = None;
            let mut ctx_excluded: serde_json::Value = serde_json::Value::Null;
            if std::env::var("RATIFLOW_CONTEXT_POLICY_V2").ok().as_deref() == Some("1") {
                let gate_for_policy = sg_workitem::get(store, &workitem_id)
                    .map(|w| w.current_gate)
                    .unwrap_or_default();
                let policy = sg_context::policy::active_by_key(store, &gate_for_policy)
                    .or_else(|_| sg_context::policy::active_by_key(store, "global"))
                    .unwrap_or(None);
                if let Some(p) = policy {
                    let allowed: Vec<String> =
                        serde_json::from_str(&p.allowed_tools_json).unwrap_or_default();
                    let registry_all: Vec<String> = sg_agent::tools::registry()
                        .iter()
                        .map(|d| d.name.to_string())
                        .collect();
                    let resolved = sg_context::policy::resolve_tools(
                        &registry_all,
                        Some(&allowed),
                        &allowlist,
                    );
                    allowlist = resolved.effective;
                    ctx_policy_frozen = Some(p.id);
                    ctx_excluded = serde_json::to_value(&resolved.excluded).unwrap_or_default();
                }
            }
            let budget: sg_agent::RunBudget = params
                .get("budget")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            let config = sg_agent::RunConfig {
                workitem_id: &workitem_id,
                task_id: &activity_id,
                goal: &goal,
                manifest_id: &manifest_id,
                tool_allowlist: &allowlist,
                idempotency_key: &idem,
                budget: &budget,
                max_iterations: 20,
            };
            let (run, created) = sg_agent::create_run(store, &config).map_err(store_err)?;
            if created {
                // M4-08：冻结 context policy（可回查 digest 链，§3 不变量 8）。
                if let Some(pid) = &ctx_policy_frozen {
                    let _ = store.with_conn(|conn| {
                        conn.execute(
                            "UPDATE agent_runs SET context_policy_version_id=?1 WHERE id=?2",
                            rusqlite::params![pid, run.id],
                        )?;
                        Ok(())
                    });
                }
                if ctx_excluded.is_array() {
                    let _ = sg_store::outbox::emit(
                        store,
                        "workitem",
                        &workitem_id,
                        "context.tools_excluded",
                        json!({"runId": run.id, "excluded": ctx_excluded}),
                    );
                }
                store
                    .with_conn(|conn| {
                        conn.execute(
                            "UPDATE agent_runs SET stage_attempt_id=?1, stage_activity_id=?2, agent_selection_id=?3, input_snapshot_id=?4 WHERE id=?5",
                            rusqlite::params![attempt.id, activity_id, selection.id, attempt.entry_snapshot_id, run.id],
                        )?;
                        Ok(())
                    })
                    .map_err(store_err)?;
                if trace_writes_enabled() {
                    sg_provenance::register_node(
                        store,
                        &sg_provenance::NodeInput {
                            project_id: "",
                            workitem_id: &workitem_id,
                            node_type: sg_provenance::node_type::AGENT_RUN,
                            entity_id: &run.id,
                            content_digest: "",
                            verification_state: "verified",
                        },
                    )
                    .map_err(store_err)?;
                }
                let instructions = spawn_run_task(state, store, &run.id, None, None)?;
                return Ok(json!({
                    "runId": run.id, "status": run.status, "instructions": instructions,
                    "attemptId": attempt.id, "activityKey": activity_key,
                    "selection": selection,
                }));
            }
            Ok(json!({
                "runId": run.id, "status": run.status,
                "attemptId": attempt.id, "activityKey": activity_key,
                "selection": serde_json::to_value(&selection).unwrap_or_default(),
            }))
        }
        "agentProfile.list" => {
            let project = opt_str_param(params, "projectId");
            let profiles =
                sg_agent::profile::list_profiles(store, project.as_deref()).map_err(store_err)?;
            let items: Vec<Value> = profiles
                .iter()
                .map(|p| {
                    let mut v = serde_json::to_value(p).unwrap_or_default();
                    v["versions"] = serde_json::to_value(
                        sg_agent::profile::versions(store, &p.id).unwrap_or_default(),
                    )
                    .unwrap_or_default();
                    v
                })
                .collect();
            Ok(json!({ "items": items }))
        }
        "agentProfile.create" => {
            let profile = sg_agent::profile::create_profile(
                store,
                opt_str_param(params, "projectId").as_deref(),
                &str_param(params, "name")?,
                &str_param(params, "adapterKind")?,
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(profile).unwrap_or_default())
        }
        "agentProfile.createVersion" => {
            let capabilities = str_list_param(params, "capabilities");
            let version = sg_agent::profile::create_version(
                store,
                &str_param(params, "profileId")?,
                &opt_str_param(params, "persona").unwrap_or_default(),
                &opt_str_param(params, "sop").unwrap_or_default(),
                &capabilities,
                &opt_str_param(params, "outputSchema").unwrap_or_default(),
                &opt_str_param(params, "modelRoute").unwrap_or_else(|| "{}".into()),
                &opt_str_param(params, "budget").unwrap_or_else(|| "{}".into()),
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(version).unwrap_or_default())
        }
        "agentProfile.setEnabled" => {
            sg_agent::profile::set_profile_enabled(
                store,
                &str_param(params, "profileId")?,
                params
                    .get("enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true),
            )
            .map_err(store_err)?;
            Ok(json!({ "status": "updated" }))
        }
        "agentBinding.list" => {
            let bindings = sg_agent::profile::list_bindings(
                store,
                opt_str_param(params, "projectId").as_deref(),
            )
            .map_err(store_err)?;
            Ok(json!({ "items": bindings }))
        }
        "agentBinding.set" => {
            sg_agent::profile::set_binding(
                store,
                opt_str_param(params, "projectId").as_deref(),
                &str_param(params, "gate")?,
                &str_param(params, "activityKey")?,
                &str_param(params, "profileVersionId")?,
                &opt_str_param(params, "fallbackMode").unwrap_or_else(|| "generic".into()),
                params.get("priority").and_then(|v| v.as_i64()).unwrap_or(0),
            )
            .map_err(store_err)?;
            Ok(json!({ "status": "bound" }))
        }
        "agentBinding.remove" => {
            sg_agent::profile::remove_binding(store, &str_param(params, "bindingId")?)
                .map_err(store_err)?;
            Ok(json!({ "status": "removed" }))
        }
        "agentBinding.resolvePreview" => {
            let preview = sg_agent::router::resolve_preview(
                store,
                &str_param(params, "projectId")?,
                &str_param(params, "gate")?,
                &str_param(params, "activityKey")?,
                &str_list_param(params, "requiredCapabilities"),
            )
            .map_err(store_err)?;
            Ok(preview)
        }
        "gate.deliverableStatus" => {
            let workitem_id = str_param(params, "workItemId")?;
            let gate_name = str_param(params, "gate")?;
            sg_workitem::deliverable::status(store, &workitem_id, &gate_name).map_err(store_err)
        }
        "stage.package" => {
            let workitem_id = str_param(params, "workItemId")?;
            let gate_name = str_param(params, "gate")?;
            let latest = sg_workitem::attempt::latest_for_gate(store, &workitem_id, &gate_name)
                .map_err(store_err)?;
            Ok(json!({
                "workItemId": workitem_id,
                "gate": gate_name,
                "latestAttempt": latest,
                "activeAttempt": sg_workitem::attempt::active_for_gate(store, &workitem_id, &gate_name)
                    .map_err(store_err)?,
                "releaseRequests": sg_workitem::release::list_for_gate(store, &workitem_id, &gate_name)
                    .map_err(store_err)?,
            }))
        }

        // --- 审批 ---
        "approval.list" => {
            let items = sg_policy::pending(
                store,
                params.get("limit").and_then(|v| v.as_i64()).unwrap_or(50),
            )
            .map_err(store_err)?;
            // WP-6（RDWS v1.4 A3）：tool_proposal 审批拼装展示三要素——提案的
            // rationale/confidence（模型自报，untrusted_display）+ 实时影响面
            // completeness（服务端权威，非模型自述）。缺行/无绑定时字段缺省，前端如实留空。
            let enriched: Vec<Value> = items
                .iter()
                .map(|a| {
                    let mut v = serde_json::to_value(a).unwrap_or(Value::Null);
                    if a.subject_type == "tool_proposal" {
                        let detail: Option<(String, String)> = store
                            .with_conn(|c| {
                                Ok(c.query_row(
                                    "SELECT COALESCE(rationale,''), COALESCE(confidence_json,'')
                                 FROM tool_proposals WHERE id=?1",
                                    [&a.subject_id],
                                    |r| Ok((r.get(0)?, r.get(1)?)),
                                )
                                .ok())
                            })
                            .unwrap_or(None);
                        if let Some((rationale, confidence_json)) = detail {
                            v["rationale"] = json!(rationale);
                            v["confidence"] = serde_json::from_str::<Value>(&confidence_json)
                                .ok()
                                .and_then(|c| c.get("value").cloned())
                                .unwrap_or(Value::Null);
                        }
                        if !a.impact_digest.is_empty() {
                            match sg_provenance::impact::for_proposal(store, &a.subject_id) {
                                Ok(impact) => {
                                    v["impactCompleteness"] = json!(impact.completeness.as_str());
                                    v["impactNodeCount"] = json!(impact.nodes.len());
                                }
                                Err(_) => {
                                    v["impactCompleteness"] = json!("unknown");
                                }
                            }
                        }
                    }
                    v
                })
                .collect();
            Ok(json!({"items": enriched}))
        }
        "approval.decide" => {
            let approval_id = str_param(params, "approvalId")?;
            let decision = str_param(params, "decision")?;
            let decided_by = str_param(params, "decidedBy")?;
            let reason = opt_str_param(params, "reason").unwrap_or_default();
            // subject_type 路由（评审 P1 修复）：gate_release / plan_revision 必须走各自
            // 治理链（gate 的 digest 漂移复检、plan 的状态迁移）；通用 decide 只裁决
            // 无漂移语义的主体（tool_proposal、deployment 等）。原实现统一 CAS 通过，
            // digest 复检被完全跳过，构成治理旁路。
            let subject = sg_policy::get(store, &approval_id).map_err(store_err)?;
            match subject.subject_type.as_str() {
                "gate_release" => {
                    return gate_decide_release_rpc(store, params);
                }
                // WP-6（RDWS v1.4 A3）：tool_proposal 双 digest 重算——impact 漂移/
                // 全图 facts 漂移使待审失效（legacy 空列跳过=回退语义；未知版本
                // fail-closed）。
                "tool_proposal" if !subject.impact_digest.is_empty() => {
                    verify_tool_proposal_binding(store, &subject)?;
                }
                "plan_revision" => {
                    if !matches!(decision.as_str(), "approved" | "rejected") {
                        return Err(err(
                            ErrorCode::InvalidParams,
                            "decision must be approved|rejected for plan_revision",
                        ));
                    }
                    let out = crate::plan_dispatch::decide_plan_governed(
                        store,
                        &subject.subject_id,
                        &decision,
                        &decided_by,
                        &reason,
                    )?;
                    sg_store::audit::append(
                        store,
                        &decided_by,
                        &format!("approval.{decision}"),
                        "approval",
                        &approval_id,
                        json!({"subjectId": subject.subject_id}),
                    )
                    .map_err(store_err)?;
                    return Ok(out);
                }
                // WP-8：gate_skip——批准 → 阶段落 skipped 终态（指针随 Passed|Skipped
                // 推进）；拒绝 → 仅审批落态。批准时阶段已开工 → gate_skip_state_changed。
                "gate_skip" => {
                    if !matches!(decision.as_str(), "approved" | "rejected") {
                        return Err(err(
                            ErrorCode::InvalidParams,
                            "decision must be approved|rejected for gate_skip",
                        ));
                    }
                    let workitem_id = subject.workitem_id.clone().ok_or_else(|| {
                        err(
                            ErrorCode::InternalError,
                            "gate_skip approval missing workitem",
                        )
                    })?;
                    let attempt_id = subject.stage_attempt_id.clone().ok_or_else(|| {
                        err(
                            ErrorCode::InternalError,
                            "gate_skip approval missing attempt",
                        )
                    })?;
                    let gate_name: String = store
                        .with_conn(|conn| {
                            conn.query_row(
                                "SELECT gate FROM stage_attempts WHERE id=?1",
                                [&attempt_id],
                                |r| r.get(0),
                            )
                            .map_err(Error::from)
                        })
                        .map_err(store_err)?;
                    if decision == "approved" {
                        // 仅当前关可跳（跳关语义 = 跳过眼前这关；未来关须依次到达）。
                        let wi = sg_workitem::get(store, &workitem_id).map_err(store_err)?;
                        if wi.current_gate != gate_name {
                            return Err(err(
                                ErrorCode::Conflict,
                                format!(
                                    "gate_skip_state_changed: 关 {gate_name} 非当前关（当前 {}）",
                                    wi.current_gate
                                ),
                            ));
                        }
                        let stage_state = sg_workitem::stages(store, &workitem_id)
                            .map_err(store_err)?
                            .into_iter()
                            .find(|s| s.gate == gate_name)
                            .map(|s| s.state)
                            .unwrap_or_default();
                        if stage_state != "not_started" {
                            return Err(err(
                                ErrorCode::Conflict,
                                format!("gate_skip_state_changed: 关 {gate_name} 当前为 {stage_state}，仅未开工关可跳过"),
                            ));
                        }
                        let appr =
                            sg_policy::decide(store, &approval_id, &decision, &decided_by, &reason)
                                .map_err(store_err)?;
                        sg_workitem::set_stage(
                            store,
                            &workitem_id,
                            &gate_name,
                            sg_workitem::StageState::Skipped,
                            "",
                        )
                        .map_err(store_err)?;
                        // 同步取消跳关关的未开工 attempt（单活跃约束：不阻塞后续关）。
                        sg_workitem::attempt::transition(store, &attempt_id, "cancelled")
                            .map_err(store_err)?;
                        sg_store::audit::append(
                            store,
                            &decided_by,
                            &format!("approval.{decision}"),
                            "approval",
                            &approval_id,
                            json!({"subjectId": subject.subject_id, "gate": gate_name, "outcome": "skipped_with_waiver"}),
                        )
                        .map_err(store_err)?;
                        return Ok(json!({
                            "approval": appr,
                            "skip": {"gate": gate_name, "outcome": "skipped_with_waiver"}
                        }));
                    }
                    let appr =
                        sg_policy::decide(store, &approval_id, &decision, &decided_by, &reason)
                            .map_err(store_err)?;
                    sg_store::audit::append(
                        store,
                        &decided_by,
                        &format!("approval.{decision}"),
                        "approval",
                        &approval_id,
                        json!({"subjectId": subject.subject_id, "gate": gate_name}),
                    )
                    .map_err(store_err)?;
                    return Ok(json!({ "approval": appr }));
                }
                // WP-9：rework——decide 治理链（CAS 复查/两步执行）与 rework.decide 同实现。
                "rework" => {
                    if !matches!(decision.as_str(), "approved" | "rejected") {
                        return Err(err(
                            ErrorCode::InvalidParams,
                            "decision must be approved|rejected for rework",
                        ));
                    }
                    return sg_workitem::rework::decide(
                        store,
                        &approval_id,
                        &decision,
                        &decided_by,
                        &reason,
                    )
                    .map_err(store_err);
                }
                // WP-7：manual_confirm 确认单落态（单向 requested → confirmed/rejected；
                // evaluator 只读 confirmed 行 + acceptance_item_digest 精确匹配）。
                "gate_manual_confirm" => {
                    if !matches!(decision.as_str(), "approved" | "rejected") {
                        return Err(err(
                            ErrorCode::InvalidParams,
                            "decision must be approved|rejected for gate_manual_confirm",
                        ));
                    }
                    let appr =
                        sg_policy::decide(store, &approval_id, &decision, &decided_by, &reason)
                            .map_err(store_err)?;
                    let confirmation = sg_workitem::manual_confirm::apply_approval_decision(
                        store,
                        &approval_id,
                        &decision,
                        &decided_by,
                    )
                    .map_err(store_err)?;
                    sg_store::audit::append(
                        store,
                        &decided_by,
                        &format!("approval.{decision}"),
                        "approval",
                        &approval_id,
                        json!({"subjectId": subject.subject_id, "confirmationId": confirmation.id}),
                    )
                    .map_err(store_err)?;
                    return Ok(json!({"approval": appr, "confirmation": confirmation}));
                }
                _ => {}
            }
            let appr = sg_policy::decide(store, &approval_id, &decision, &decided_by, &reason)
                .map_err(store_err)?;
            sg_store::audit::append(
                store,
                &decided_by,
                &format!("approval.{decision}"),
                "approval",
                &approval_id,
                json!({"subjectId": appr.subject_id}),
            )
            .map_err(store_err)?;
            // M1/F03 联动：审批主体是工具提案且其 Run 挂起 → 批准拉起恢复任务/拒绝收尾 failed。
            let mut run_link = Value::Null;
            if appr.subject_type == "tool_proposal" {
                let proposal =
                    sg_agent::get_proposal(store, &appr.subject_id).map_err(store_err)?;
                if let Ok(linked) = sg_agent::get_run(store, &proposal.run_id) {
                    if linked.status == "paused" {
                        match decision.as_str() {
                            "approved" => {
                                spawn_run_task(state, store, &linked.id, None, None)?;
                                run_link = json!({"runId": linked.id, "status": "resuming"});
                            }
                            _ => {
                                let message = format!(
                                    "审批拒绝：{}（{decided_by}）",
                                    if reason.is_empty() {
                                        "未提供理由"
                                    } else {
                                        reason.as_str()
                                    }
                                );
                                sg_agent::fail_paused_run(store, &linked.id, &message)
                                    .map_err(store_err)?;
                                run_link = json!({"runId": linked.id, "status": "failed"});
                            }
                        }
                    }
                }
            }
            let mut out = serde_json::to_value(appr).unwrap_or_default();
            out["run"] = run_link;
            Ok(out)
        }
        "approval.listByWorkItem" => {
            let workitem_id = str_param(params, "workItemId")?;
            let deployments = list_deployments_for(store, &workitem_id).map_err(store_err)?;
            let mut items = Vec::new();
            for dep in &deployments {
                items.extend(
                    sg_policy::list_by_subject(store, "deployment", dep).map_err(store_err)?,
                );
            }
            Ok(json!({"items": items}))
        }

        // --- 证据 / 文牒 ---
        "evidence.list" => {
            let items = sg_evidence::list(
                store,
                &str_param(params, "workItemId")?,
                opt_str_param(params, "gate").as_deref(),
            )
            .map_err(store_err)?;
            Ok(json!({"items": items}))
        }
        "evidence.record" => {
            let title = opt_str_param(params, "title").unwrap_or_default();
            let content = opt_str_param(params, "content");
            let payload = opt_str_param(params, "payload").unwrap_or_else(|| "{}".into());
            let source = opt_str_param(params, "source").unwrap_or_default();
            let workitem_id = str_param(params, "workItemId")?;
            let requirement_keys = str_list_param(params, "requirementKeys");
            // fail-closed：需求 key 先解析（不命中即 trace_incomplete，不落任何事实）。
            let resolved_items = resolve_requirement_items(store, &workitem_id, &requirement_keys)
                .map_err(store_err)?;
            let gate_param = str_param(params, "gate")?;
            let input = sg_evidence::RecordInput {
                workitem_id: &workitem_id,
                gate: &gate_param,
                kind: &str_param(params, "kind")?,
                title: &title,
                content: content.as_deref(),
                payload: &payload,
                source: &source,
            };
            let ev = sg_evidence::record(store, &input).map_err(store_err)?;
            if trace_writes_enabled() {
                sg_provenance::register_node(
                    store,
                    &sg_provenance::NodeInput {
                        project_id: "",
                        workitem_id: &workitem_id,
                        node_type: sg_provenance::node_type::EVIDENCE,
                        entity_id: &ev.id,
                        content_digest: &ev.object_sha256,
                        verification_state: "unverified",
                    },
                )
                .map_err(store_err)?;
                trace_link_items(
                    store,
                    &workitem_id,
                    sg_provenance::node_type::EVIDENCE,
                    &ev.id,
                    sg_provenance::relation::VERIFIES,
                    &resolved_items,
                )
                .map_err(store_err)?;
            }
            // M2：证据/核验态是放行包的一部分 → 漂移失效（AC-SW-03 前置）。
            sg_workitem::release::invalidate_pending_if_drift(
                store,
                &workitem_id,
                gate_param.as_str(),
                &release_policy_version(store),
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(ev).unwrap_or_default())
        }
        "evidence.verify" => {
            let evidence_id = str_param(params, "evidenceId")?;
            sg_evidence::verify(store, &evidence_id, &str_param(params, "verifiedBy")?)
                .map_err(store_err)?;
            if trace_writes_enabled() {
                sg_provenance::set_verification(
                    store,
                    sg_provenance::node_type::EVIDENCE,
                    &evidence_id,
                    "verified",
                )
                .map_err(store_err)?;
            }
            Ok(json!({"status": "verified"}))
        }
        "passport.issue" => {
            let workitem_id = str_param(params, "workItemId")?;
            let mut gates = Vec::new();
            // M1-04：护照按实例关卡序列签发。完成谓词（WP-8）：每关
            // outcome ∈ {passed, skipped_with_waiver}；skipped 关照实输出
            // passed:false（旧消费者保守视为未全通过 = fail-closed 方向）。
            for gref in sg_workitem::gate_refs(store, &workitem_id).map_err(store_err)? {
                let stage_state = sg_workitem::stages(store, &workitem_id)
                    .map_err(store_err)?
                    .into_iter()
                    .find(|s| s.gate == gref.gate_id)
                    .map(|s| s.state)
                    .unwrap_or_else(|| "not_started".into());
                if stage_state == "skipped" {
                    let waiver = gate_skip_waiver_approval_id(store, &workitem_id, &gref.gate_id)
                        .map_err(store_err)?;
                    gates.push(sg_evidence::GateSummary {
                        gate: gref.gate_id.clone(),
                        passed: false,
                        evidence_ids: vec![],
                        failed_inputs: vec![],
                        outcome: "skipped_with_waiver".into(),
                        waiver_approval_id: waiver.unwrap_or_default(),
                    });
                    continue;
                }
                match sg_workitem::gate::latest(store, &workitem_id, &gref.gate_id)
                    .map_err(store_err)?
                {
                    Some(result) if result.passed => {
                        let evidences = sg_evidence::list(store, &workitem_id, Some(&gref.gate_id))
                            .map_err(store_err)?;
                        gates.push(sg_evidence::GateSummary {
                            gate: gref.gate_id.clone(),
                            passed: true,
                            evidence_ids: evidences.iter().map(|e| e.id.clone()).collect(),
                            failed_inputs: vec![],
                            outcome: "passed".into(),
                            waiver_approval_id: String::new(),
                        });
                    }
                    _ => {
                        return Err(err(
                            ErrorCode::Conflict,
                            "passport_incomplete_gates: 实例关卡尚未全部通过",
                        ));
                    }
                }
            }
            let passport = sg_evidence::issue_passport(
                store,
                &workitem_id,
                &gates,
                &opt_str_param(params, "sharedSummary").unwrap_or_default(),
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(passport).unwrap_or_default())
        }
        "passport.latest" => {
            match sg_evidence::latest_passport(store, &str_param(params, "workItemId")?)
                .map_err(store_err)?
            {
                Some(p) => Ok(serde_json::to_value(p).unwrap_or_default()),
                None => Err(err(ErrorCode::NotFound, "尚无通关文牒")),
            }
        }

        // --- 部署 ---
        "deployment.create" => {
            let plan_value = params
                .get("plan")
                .cloned()
                .ok_or_else(|| err(ErrorCode::InvalidParams, "plan required"))?;
            let plan: sg_workflow::DeploymentPlan = serde_json::from_value(plan_value)
                .map_err(|e| err(ErrorCode::InvalidParams, format!("plan: {e}")))?;
            let dep = sg_workflow::create_plan(store, &str_param(params, "workItemId")?, &plan)
                .map_err(store_err)?;
            Ok(serde_json::to_value(dep).unwrap_or_default())
        }
        "deployment.get" => {
            let dep =
                sg_workflow::get(store, &str_param(params, "deploymentId")?).map_err(store_err)?;
            Ok(serde_json::to_value(dep).unwrap_or_default())
        }
        "deployment.submit" => {
            let id = str_param(params, "deploymentId")?;
            sg_workflow::submit_for_approval(store, &id).map_err(store_err)?;
            Ok(json!({"status": "awaiting_approval"}))
        }
        "deployment.deploy" => {
            let dep = sg_workflow::approve_and_deploy(
                store,
                &str_param(params, "deploymentId")?,
                state.ssh.as_ref(),
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(dep).unwrap_or_default())
        }
        "deployment.verify" => {
            let dep = sg_workflow::verify(
                store,
                &str_param(params, "deploymentId")?,
                state.ssh.as_ref(),
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(dep).unwrap_or_default())
        }
        "deployment.rollback" => {
            let dep = sg_workflow::rollback(
                store,
                &str_param(params, "deploymentId")?,
                state.ssh.as_ref(),
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(dep).unwrap_or_default())
        }

        // --- 时间线 / 备份 / 审计 ---
        "timeline.snapshot" => {
            let wi = opt_str_param(params, "workItemId");
            let after = params.get("afterSeq").and_then(|v| v.as_i64()).unwrap_or(0);
            let events =
                sg_timeline::snapshot(store, wi.as_deref(), after, 500).map_err(store_err)?;
            Ok(
                json!({"events": events, "latest": outbox::latest_sequence(store).map_err(store_err)?}),
            )
        }
        "backup.create" => {
            let snap = sg_store::backup::snapshot(store).map_err(store_err)?;
            Ok(snap.manifest)
        }
        "audit.list" => {
            let items = sg_store::audit::list(
                store,
                params.get("afterSeq").and_then(|v| v.as_i64()).unwrap_or(0),
                params.get("limit").and_then(|v| v.as_i64()).unwrap_or(50),
            )
            .map_err(store_err)?;
            Ok(json!({"items": items}))
        }

        // --- 项目记忆（ADR-032 / 实施方案 v1.0）：dispatch 仅做分流，
        // 全部逻辑在 sg-memory crate + memory_dispatch 适配层（§5.1 依赖方向）。 ---
        "memory.settingsGet"
        | "memory.settingsUpdate"
        | "memory.list"
        | "memory.get"
        | "memory.create"
        | "memory.update"
        | "memory.pin"
        | "memory.archive"
        | "memory.restore"
        | "memory.purgePreview"
        | "memory.purge"
        | "memory.search"
        | "memory.contextPreview"
        | "memory.import"
        | "memory.export"
        | "memory.captureStart"
        | "memory.captureGet"
        | "memory.candidateList"
        | "memory.candidateDecide"
        | "memory.syncFromRepo" => crate::memory_dispatch::dispatch(state, store, method, params),

        _ => Err(err(ErrorCode::MethodNotFound, format!("未知方法 {method}"))),
    }
}

fn list_deployments_for(store: &Store, workitem_id: &str) -> Result<Vec<String>, Error> {
    store.with_conn(|conn| {
        let mut stmt = conn.prepare("SELECT id FROM deployments WHERE workitem_id=?1")?;
        let rows = stmt.query_map([workitem_id], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

fn diagnostics(state: &AppState, store: &Store) -> RpcResult {
    let checks = diagnostics_checks(state, store);
    let ready = checks
        .iter()
        .all(|c| c["status"] != json!("error") && c["status"] != json!("needs_configuration"));
    Ok(json!({
        "generatedAt": sg_store::now(),
        "ready": ready,
        "checks": checks,
        // 兼容字段（旧 UI 消费）：由 checks 派生。
        "integrations": diagnostics_checks(state, store).iter().filter(|c| c["scope"] == json!("integration")).cloned().collect::<Vec<_>>(),
        "local": diagnostics_checks(state, store).iter().filter(|c| c["scope"] == json!("local")).cloned().collect::<Vec<_>>(),
    }))
}

/// S50：每个检查项含 checkId/scope/severity/status/durationMs/fixTarget（一键跳转配置页）。
fn diagnostics_checks(state: &AppState, store: &Store) -> Vec<Value> {
    let start = std::time::Instant::now();
    let store_ok = store.quick_check().is_ok();
    let schema = store.schema_version().unwrap_or(0);
    let dur = |s: std::time::Instant| s.elapsed().as_millis() as i64;

    let gl_profiles: i64 = store
        .with_conn(|conn| {
            Ok(conn
                .query_row("SELECT COUNT(*) FROM gitlab_profiles", [], |r| r.get(0))
                .unwrap_or(0))
        })
        .unwrap_or(0);
    let gl_env = std::env::var("RATIFLOW_GITLAB_URL")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let mp_profiles: i64 = store
        .with_conn(|conn| {
            Ok(conn
                .query_row("SELECT COUNT(*) FROM model_profiles", [], |r| r.get(0))
                .unwrap_or(0))
        })
        .unwrap_or(0);
    let mp_env = std::env::var("RATIFLOW_MODEL_API_KEY")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let ssh_targets: i64 = store
        .with_conn(|conn| {
            Ok(conn
                .query_row("SELECT COUNT(*) FROM ssh_targets", [], |r| r.get(0))
                .unwrap_or(0))
        })
        .unwrap_or(0);

    let mk = |check_id: &str,
              label: &str,
              scope: &str,
              status: &str,
              severity: &str,
              detail: String,
              fix: &str| {
        json!({
            "checkId": check_id, "label": label, "scope": scope, "status": status, "severity": severity,
            "detail": detail, "durationMs": dur(start), "fixTarget": fix, "required": true,
        })
    };

    vec![
        mk(
            "gitlab",
            "GitLab",
            "integration",
            if gl_profiles > 0 || gl_env {
                "ready"
            } else {
                "needs_configuration"
            },
            "blocking",
            if gl_profiles > 0 {
                format!("{gl_profiles} 个 Profile")
            } else if gl_env {
                "env 托管".into()
            } else {
                "未配置".into()
            },
            "/settings/gitlab",
        ),
        mk(
            "model",
            "公有模型",
            "integration",
            if mp_profiles > 0 || mp_env {
                "ready"
            } else {
                "needs_configuration"
            },
            "blocking",
            if mp_profiles > 0 {
                format!("{mp_profiles} 个 Profile")
            } else if mp_env {
                "env 托管".into()
            } else {
                "未配置".into()
            },
            "/settings/models",
        ),
        mk(
            "ssh",
            "SSH 目标机",
            "integration",
            if ssh_targets > 0 {
                "ready"
            } else {
                "needs_configuration"
            },
            "degraded",
            if ssh_targets > 0 {
                format!("{ssh_targets} 个目标")
            } else {
                "未配置".into()
            },
            "/settings/ssh",
        ),
        mk(
            "core",
            "Rust Core",
            "local",
            if store_ok { "ready" } else { "error" },
            "blocking",
            format!("schema v{schema}"),
            "",
        ),
        mk(
            "executor",
            "执行模式",
            "local",
            "ready",
            "info",
            format!("{:?}", state.executor_mode),
            "/settings/execution",
        ),
        mk(
            "sqlite",
            "SQLite WAL",
            "local",
            if store_ok { "ready" } else { "error" },
            "blocking",
            store.data_dir.display().to_string(),
            "/settings/backup",
        ),
    ]
}

pub fn diagnostics_run_pub(state: &AppState, store: &Store, check_id: &str) -> RpcResult {
    diagnostics_run(state, store, check_id)
}

/// diagnostics.run(checkId)：单项重查（S50）。
fn diagnostics_run(state: &AppState, store: &Store, check_id: &str) -> RpcResult {
    let checks = diagnostics_checks(state, store);
    let found = checks
        .into_iter()
        .find(|c| c["checkId"] == json!(check_id))
        .ok_or_else(|| err(ErrorCode::InvalidParams, format!("未知检查项 {check_id}")))?;
    Ok(found)
}

fn base64_decode(input: &str) -> Result<Vec<u8>, String> {
    // 轻量 base64（无外部依赖）。
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut lookup = [255u8; 256];
    for (i, c) in TABLE.iter().enumerate() {
        lookup[*c as usize] = i as u8;
    }
    let filtered: Vec<u8> = input
        .bytes()
        .filter(|b| !b.is_ascii_whitespace() && *b != b'=')
        .collect();
    let mut out = Vec::with_capacity(filtered.len() * 3 / 4);
    for chunk in filtered.chunks(4) {
        let mut buf = [0u8; 4];
        for (i, c) in chunk.iter().enumerate() {
            buf[i] = lookup[*c as usize];
            if buf[i] == 255 {
                return Err(format!("invalid base64 byte {c}"));
            }
        }
        let combined: u32 = ((buf[0] as u32) << 18)
            | ((buf[1] as u32) << 12)
            | ((buf[2] as u32) << 6)
            | buf[3] as u32;
        out.push((combined >> 16) as u8);
        if chunk.len() > 2 {
            out.push((combined >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(combined as u8);
        }
    }
    Ok(out)
}

/// F10/M3：装配运行时权限快照——注册表为工具权威（risk/data_level/max_result_bytes/timeout），
/// toolPolicy 全局行覆盖 enabled/requiresApproval/risk；enabled=false 即从规则集中移除（策略拒绝）。
fn assemble_policy_snapshot(store: &Store) -> (sg_policy::Snapshot, Value) {
    let overrides = sg_settings::policy_ext::tool_list(store).unwrap_or_default();
    let mut rules = Vec::new();
    let mut sources = Vec::new();
    for def in sg_agent::tools::registry() {
        let o = overrides
            .iter()
            .find(|o| o.tool_id == def.name && o.project_id.is_empty());
        if let Some(o) = o {
            if !o.enabled {
                sources.push(json!({"tool": def.name, "source": "settings", "enabled": false}));
                continue;
            }
        }
        let risk = match o.map(|o| o.risk.as_str()) {
            Some("low") => sg_policy::Risk::Low,
            Some("medium") => sg_policy::Risk::Medium,
            Some("high") => sg_policy::Risk::High,
            _ => def.risk,
        };
        let requires_approval = o
            .map(|o| o.requires_approval)
            .unwrap_or(def.risk == sg_policy::Risk::High);
        rules.push(sg_policy::ToolRule {
            tool: def.name.to_string(),
            risk,
            requires_approval,
            data_level: def.data_level.into(),
            max_result_bytes: def.max_result_bytes as i64,
            timeout_sec: def.timeout_sec,
        });
        sources.push(json!({
            "tool": def.name,
            "source": if o.is_some() { "settings" } else { "registry" },
            "requiresApproval": requires_approval,
        }));
    }
    // M6（ADR-035）：活跃 MCP 工具纳入策略快照——
    // read_only → Low/免审批；写 → High/必须审批；data_level=external；
    // 沙箱标注：本地 stdio 进程直启（沙箱包裹为后续硬化项），远端不受本机沙箱保护。
    for t in sg_settings::mcp_ext::active_tools(store) {
        let model_name = format!("mcp__{}__{}", t.server_name, t.tool_name);
        rules.push(sg_policy::ToolRule {
            tool: model_name.clone(),
            risk: if t.read_only {
                sg_policy::Risk::Low
            } else {
                sg_policy::Risk::High
            },
            requires_approval: !t.read_only,
            data_level: "external".into(),
            max_result_bytes: 64 * 1024,
            timeout_sec: 60,
        });
        sources.push(json!({
            "tool": model_name,
            "source": "mcp",
            "server": t.server_name,
            "schemaDigest": t.schema_digest,
            "readOnly": t.read_only,
            "sandboxed": false,
        }));
    }
    (
        sg_policy::Snapshot {
            tool_rules: rules,
            approval_ttl_secs: 3600,
        },
        json!({"sources": sources}),
    )
}

/// F10/M3：执行模式生效来源——env 覆盖（CI 优先）> 设置域 executionProfile（unsafe 需双确认）> 探测。
pub(crate) fn mode_str(m: sg_executor::Mode) -> &'static str {
    match m {
        sg_executor::Mode::Docker => "docker",
        sg_executor::Mode::KernelRestricted => "kernel_restricted",
        sg_executor::Mode::SafeRestricted => "safe_restricted",
        sg_executor::Mode::UnsafeExplicit => "unsafe_explicit",
        sg_executor::Mode::Disabled => "disabled",
    }
}

pub(crate) fn effective_executor_mode(
    state: &AppState,
    store: &Store,
) -> (sg_executor::Mode, &'static str) {
    if let Some(m) = std::env::var("RATIFLOW_EXEC_MODE")
        .ok()
        .and_then(|m| match m.as_str() {
            "docker" => Some(sg_executor::Mode::Docker),
            "kernel_restricted" => Some(sg_executor::Mode::KernelRestricted),
            "safe_restricted" => Some(sg_executor::Mode::SafeRestricted),
            "unsafe_explicit" => Some(sg_executor::Mode::UnsafeExplicit),
            "disabled" => Some(sg_executor::Mode::Disabled),
            _ => None,
        })
    {
        return (m, "env");
    }
    let prof = sg_settings::executor_ext::get(store).unwrap_or_else(|_| json!({}));
    let confirmed = prof["unsafeConfirmed"].as_bool() == Some(true);
    match prof["mode"].as_str() {
        Some("docker") if sg_executor::docker_available() => {
            (sg_executor::Mode::Docker, "settings")
        }
        // kernel_restricted 要求内核沙箱实际可用（不可用 → 探测回落，fail-closed）。
        Some("kernel_restricted") if sg_executor::sandbox::probe().backend != "unavailable" => {
            (sg_executor::Mode::KernelRestricted, "settings")
        }
        Some("safe_restricted") => (sg_executor::Mode::SafeRestricted, "settings"),
        Some("unsafe_explicit") if confirmed => (sg_executor::Mode::UnsafeExplicit, "settings"),
        Some("disabled") => (sg_executor::Mode::Disabled, "settings"),
        _ => (state.executor_mode, "detected"),
    }
}

/// 任务级绑定（EvoFlow 评审 P0-3 修复）：agent.start(planTaskAttemptId) 把 Run
/// 绑定到 TaskAttempt——真实执行链入口（planTask.transition succeeded 需绑定 Run
/// 终态证明）。约束：attempt 须属同一工作项（防跨任务拼接）、无其他活跃 Run 占用、
/// 状态 ready（此处经派发闸推进 running）或 running（崩溃遗留可重绑）。
fn bind_plan_task_attempt(
    store: &Store,
    run_id: &str,
    workitem_id: &str,
    attempt_id: &str,
) -> Result<(), String> {
    let (owner_wi, state): (String, String) = store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT pr.workitem_id, pa.state FROM plan_task_attempts pa
                 JOIN plan_tasks pt ON pt.id = pa.task_id
                 JOIN plan_revisions pr ON pr.id = pt.plan_revision_id
                 WHERE pa.id=?1",
                [attempt_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| sg_store::Error::Message("task_attempt_not_found".into()))
        })
        .map_err(|e| e.to_string())?;
    if owner_wi != workitem_id {
        return Err(format!(
            "task_attempt_scope_denied: attempt {attempt_id} 不属于工作项 {workitem_id}"
        ));
    }
    let busy: i64 = store
        .with_conn(|conn| {
            Ok(conn
                .query_row(
                    "SELECT COUNT(*) FROM agent_runs
                     WHERE plan_task_attempt_id=?1 AND status IN ('queued','running') AND id<>?2",
                    rusqlite::params![attempt_id, run_id],
                    |r| r.get(0),
                )
                .unwrap_or(0))
        })
        .unwrap_or(0);
    if busy > 0 {
        return Err("task_attempt_already_bound: 已有活跃 Run 绑定该 attempt".into());
    }
    match state.as_str() {
        "ready" => {
            crate::plan_runtime::start_running(store, attempt_id).map_err(|e| e.to_string())?;
        }
        "running" => {}
        other => {
            return Err(format!(
                "task_state_invalid: attempt 状态 {other} 不可绑定（须 ready/running）"
            ))
        }
    }
    store
        .with_conn(|conn| {
            conn.execute(
                "UPDATE agent_runs SET plan_task_attempt_id=?1 WHERE id=?2",
                rusqlite::params![attempt_id, run_id],
            )
            .map_err(sg_store::Error::from)?;
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    sg_store::outbox::emit(
        store,
        "workitem",
        workitem_id,
        "plan.task_bound",
        serde_json::json!({"runId": run_id, "taskAttemptId": attempt_id}),
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// 派发 Run 任务（agent.start 与审批恢复共用，M1/F03）：
/// 行配置重建 → ToolCtx/executor → rollout → tokio 任务（spawn_blocking 驱动循环，run_store 专属连接）。
/// autonomy_*：仅 agent.start 显式传入（None=恢复/派生路径沿用已存快照）。
fn spawn_run_task(
    state: &AppState,
    store: &Store,
    run_id: &str,
    autonomy_mode: Option<&str>,
    autonomy_grant_id: Option<&str>,
) -> Result<Value, RpcError> {
    let run = sg_agent::get_run(store, run_id).map_err(store_err)?;
    let (workitem_id, goal, manifest_id, allowlist, budget) =
        sg_agent::row_config(store, run_id).map_err(store_err)?;
    // 工具上下文（F05/M0-③）：项目根来自工作项所属项目的 local_root；
    // 工件草稿区 <dataDir>/artifacts/<runId>/。
    let (project_id, local_root): (String, String) = store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT p.id, COALESCE(p.local_root,'') FROM projects p
                 JOIN workitems w ON w.project_id = p.id WHERE w.id=?1",
                [&workitem_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| sg_store::Error::Message("workitem_not_found".into()))
        })
        .map_err(store_err)?;
    // M3+：Agent 工具执行在 Ratiflow 隔离 worktree（SG-RBK-005）；不可用时诚实回退 local_root。
    let worktree_info = sg_workitem::worktree::ensure(store, &workitem_id).ok();
    let mut work_dir = match &worktree_info {
        Some(info) => Some(std::path::PathBuf::from(&info.path)),
        None if !local_root.is_empty() => Some(std::path::PathBuf::from(&local_root)),
        _ => None,
    };
    // 任务级工作区优先（EvoFlow 评审 P0-3 修复）：Run 绑定 TaskAttempt 且工作区
    // 处于 ready/in_use 时，可写工作目录用 TaskWorkspace（attempt 级可归因可合并）。
    let plan_attempt_id: String = store
        .with_conn(|conn| {
            Ok(conn
                .query_row(
                    "SELECT COALESCE(plan_task_attempt_id,'') FROM agent_runs WHERE id=?1",
                    [run_id],
                    |r| r.get::<_, String>(0),
                )
                .unwrap_or_default())
        })
        .unwrap_or_default();
    let task_workspace_used = if plan_attempt_id.is_empty() {
        false
    } else {
        match sg_executor::workspace::get(store, &plan_attempt_id) {
            Ok(Some(w)) if matches!(w.state.as_str(), "ready" | "in_use") && !w.path.is_empty() => {
                work_dir = Some(std::path::PathBuf::from(&w.path));
                true
            }
            _ => false,
        }
    };
    // F10：权限快照与生效执行模式——已存快照（暂停恢复）原样复用（运行中不受设置变更影响），
    // 否则装配（注册表+toolPolicy 覆盖）并落库 policy_snapshot 列。
    let parse_mode = |v: &str| match v {
        "docker" => Some(sg_executor::Mode::Docker),
        "kernel_restricted" => Some(sg_executor::Mode::KernelRestricted),
        // 旧值兼容读取（ADR-034）：safe_restricted 仅 argv 只读白名单，无内核强制。
        "safe_restricted" => Some(sg_executor::Mode::SafeRestricted),
        "unsafe_explicit" => Some(sg_executor::Mode::UnsafeExplicit),
        "disabled" => Some(sg_executor::Mode::Disabled),
        _ => None,
    };
    let existing_snap: String = store
        .with_conn(|conn| {
            Ok(conn
                .query_row(
                    "SELECT policy_snapshot FROM agent_runs WHERE id=?1",
                    [run_id],
                    |r| r.get::<_, String>(0),
                )
                .unwrap_or_default())
        })
        .unwrap_or_default();
    let (policy_snapshot, snapshot_envelope, mode, mode_source) = if existing_snap.starts_with('{')
    {
        match serde_json::from_str::<Value>(&existing_snap)
            .ok()
            .and_then(|v| {
                Some((
                    serde_json::from_value::<sg_policy::Snapshot>(v["snapshot"].clone()).ok()?,
                    v,
                ))
            }) {
            Some((snap, v)) => {
                let m = v["mode"]
                    .as_str()
                    .and_then(parse_mode)
                    .unwrap_or(state.executor_mode);
                (snap, v, m, "stored")
            }
            None => {
                let (snap, _) = assemble_policy_snapshot(store);
                let (m, src) = effective_executor_mode(state, store);
                (snap, json!(null), m, src)
            }
        }
    } else {
        let (snap, sources) = assemble_policy_snapshot(store);
        let (m, src) = effective_executor_mode(state, store);
        // ADR-034 M3：内核沙箱能力与本次 Run 的策略 digest 固化进执行快照
        // （backend/version/digest；审计与放行对账可回查）。
        let sandbox_probe = sg_executor::sandbox::probe();
        let sandbox_policy = sg_executor::sandbox::SandboxPolicy {
            read_paths: work_dir
                .as_ref()
                .map(|p| vec![p.to_string_lossy().to_string()])
                .unwrap_or_default(),
            write_paths: vec![
                work_dir
                    .as_ref()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default(),
                store
                    .data_dir
                    .join("artifacts")
                    .join(run_id)
                    .to_string_lossy()
                    .to_string(),
            ],
            network_off: true,
        };
        let mut envelope = json!({
            "snapshot": serde_json::to_value(&snap).unwrap_or_default(),
            "sources": sources["sources"],
            "mode": mode_str(m),
            "modeSource": src,
            "sandbox": {
                "backend": sandbox_probe.backend,
                "version": sandbox_probe.version,
                "policyDigest": sandbox_policy.digest(),
                "blockedReason": sandbox_probe.blocked_reason,
            },
        });
        // 自治面冻结（评审 P0-1）：Ask 只读边界与 Grant 校验在工具执行链消费这两个字段。
        if let Some(m) = autonomy_mode {
            envelope["autonomyMode"] = json!(m);
        }
        if let Some(g) = autonomy_grant_id {
            envelope["autonomyGrantId"] = json!(g);
        }
        let _ = store.with_conn(|conn| {
            conn.execute(
                "UPDATE agent_runs SET policy_snapshot=?1 WHERE id=?2",
                rusqlite::params![envelope.to_string(), run_id],
            )?;
            Ok(())
        });
        (snap, envelope, m, src)
    };
    // 取消令牌提前创建并注册（缺陷审计 P1-9）：ToolCtx 持有令牌，
    // 在途工具子进程可被取消即时中止，而非等 timeout 自然结束。
    let token = Arc::new(sg_integrations::CancelToken::new());
    state.runs.register(run_id, token.clone());
    let ctx = sg_agent::tools::ToolCtx {
        mode,
        work_dir: work_dir.clone(),
        artifacts_dir: store.data_dir.join("artifacts").join(run_id),
        // P0-4：回退到主工作区 = 只读模式（run_command 被拒绝），可写执行必须发生在
        // 受管 worktree（workitem 级或任务级 TaskWorkspace）。
        read_only: worktree_info.is_none() && !task_workspace_used && !local_root.is_empty(),
        cancel: Some(token.clone()),
    };
    let executor =
        crate::tool_exec::make_executor(ctx, state.run_store.clone(), project_id.clone());

    // F06/F07/F08 上下文装配：分层指令文件（全局→项目根→docs/）+ manifest 内容块 → 知识层。
    let knowledge_settings = sg_settings::knowledge_defaults::get(store, Some(&project_id))
        .unwrap_or_else(|_| json!({}));
    let instr_settings = sg_agent::instructions::settings_from_json(&knowledge_settings);
    let (instr_text, layers, instr_warnings) =
        sg_agent::instructions::aggregate(&store.data_dir, work_dir.as_deref(), &instr_settings);
    let blocks = sg_context::blocks::manifest_blocks(store, &manifest_id, 64 << 10)
        .unwrap_or_else(|_| json!({"blocks": [], "totalBytes": 0, "includedCount": 0, "excludedCount": 0, "truncated": false}));
    // ADR-032 M2：记忆块按 manifest 冻结 revision 装载；purge/对象缺失 fail-closed 上报（§7.4）。
    let memory_blocks = sg_context::blocks::manifest_memory_blocks(store, &manifest_id, 32 << 10)
        .unwrap_or_else(|_| json!({"blocks": [], "totalBytes": 0, "missing": []}));
    let memory_blocks_text = memory_blocks["blocks"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|b| {
                    format!(
                        "### [memory:{}@{}] {}\n{}",
                        b["memoryId"].as_str().unwrap_or(""),
                        b["revisionNo"].as_i64().unwrap_or(0),
                        b["kind"].as_str().unwrap_or(""),
                        b["text"].as_str().unwrap_or(""),
                    )
                })
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_default();
    let memory_segment = sg_agent::prompt::memory_text(&memory_blocks_text);
    let blocks_text = blocks["blocks"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|b| {
                    format!(
                        "### {}\n{}",
                        b["name"].as_str().unwrap_or(""),
                        sg_agent::tools::truncate_output(
                            b["text"].as_str().unwrap_or(""),
                            16 << 10
                        )
                    )
                })
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_default();
    let compact_policy = sg_agent::CompactPolicy {
        threshold_tokens: knowledge_settings
            .get("autoCompactThresholdTokens")
            .and_then(|x| x.as_u64())
            .unwrap_or(24000) as usize,
        keep_turns: knowledge_settings
            .get("compactionKeepTurns")
            .and_then(|x| x.as_u64())
            .unwrap_or(2) as usize,
    };
    let knowledge = sg_agent::prompt::knowledge_text(&instr_text, &blocks_text);
    let env = sg_agent::prompt::PromptEnv {
        mode: Some(mode),
        work_dir_label: work_dir.as_ref().map(|p| p.to_string_lossy().to_string()),
        requires_approval_tools: policy_snapshot
            .tool_rules
            .iter()
            .filter(|r| r.requires_approval)
            .map(|r| r.tool.clone())
            .collect(),
    };
    // M4：AgentProfile developer 层（persona/SOP/输出契约；无 selection 时为 None）。
    // 技能段（启用即注入；全局技能恒注入，绑定技能仅该 Agent 的 Run）：拼入 profile 层之后。
    // _active_profile_id：已解析的 profile id，M4 Profile v2 冻结消费；当前仅用于技能注入过滤。
    let (profile_text, _active_profile_id): (Option<String>, Option<String>) = {
        let sel_id: String = store
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT agent_selection_id FROM agent_runs WHERE id=?1",
                    [run_id],
                    |r| r.get::<_, String>(0),
                )
                .map_err(Error::from)
            })
            .unwrap_or_default();
        let resolved = if sel_id.is_empty() {
            None
        } else {
            sg_agent::router::get(store, &sel_id)
                .ok()
                .and_then(|sel| sg_agent::profile::get_version(store, &sel.resolved_profile_version_id).ok())
                .map(|ver| {
                    let text = sg_agent::profile::version_texts(store, &ver).ok().map(|(persona, sop)| {
                        format!(
                            "【Agent 角色档案】\n角色说明：{}\n标准作业流程：{}\n（profile v{} · digest {}）",
                            if persona.is_empty() { "（未配置）" } else { persona.trim() },
                            if sop.is_empty() { "（未配置）" } else { sop.trim() },
                            ver.version_no,
                            ver.content_digest
                        )
                    });
                    (text, ver.profile_id)
                })
        };
        let (profile, profile_id) = match resolved {
            Some((text, pid)) => (text, Some(pid)),
            None => (None, None),
        };
        let skills_text =
            sg_settings::skills_ext::enabled_bodies_text(store, profile_id.as_deref())
                .unwrap_or_default();
        // 技能段追加到 profile 层文本（Run 内冻结、确定性拼接，保持前缀稳定）。
        let combined = match (profile, skills_text.is_empty()) {
            (None, true) => None,
            (None, false) => Some(skills_text),
            (Some(text), false) => Some(format!("{text}\n\n{skills_text}")),
            (Some(text), true) => Some(text),
        };
        (combined, profile_id)
    };
    let initial = sg_agent::prompt::assemble_with_profile(
        &env,
        &allowlist,
        &knowledge,
        &memory_segment,
        &goal,
        profile_text.as_deref(),
    );
    let summary = json!({
        "instructionLayers": sg_agent::instructions::layers_json(&layers),
        "instructionBytes": instr_text.len(),
        "warnings": instr_warnings,
        "knowledgeItems": blocks["includedCount"].clone(),
        "knowledgeBytes": blocks["totalBytes"].clone(),
        "knowledgeExcluded": blocks["excludedCount"].clone(),
        "memoryItems": memory_blocks["blocks"].as_array().map(|a| a.len()).unwrap_or(0),
        "memoryBytes": memory_blocks["totalBytes"].clone(),
        "memoryMissing": memory_blocks["missing"].clone(),
        "promptBytes": sg_agent::prompt::segment_bytes(&initial),
        "modeSource": mode_source,
        "policyRuleCount": policy_snapshot.tool_rules.len(),
        "policySources": if snapshot_envelope.is_null() {
            json!(null)
        } else {
            snapshot_envelope["sources"].clone()
        },
    });
    // rollout（F04）：打开失败不阻断 Run（观测数据可用性优先），记 stderr 继续。
    let mut rollout = sg_agent::rollout::Rollout::open(&store.data_dir, run_id).ok();
    if rollout.is_none() {
        eprintln!("{{\"level\":\"warn\",\"msg\":\"rollout open failed for {run_id}\"}}");
    }
    // ADR-032 §7.4：记忆块缺失（已清除/对象损坏）如实入 rollout；默认 memory optional 不阻断 Run。
    for missing in memory_blocks["missing"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        if let Some(r) = rollout.as_mut() {
            let _ = r.append("memory_block_missing", missing);
        }
    }
    // （取消令牌已提前到 ToolCtx 装配处创建并注册——避免此处二次注册
    //   把 ctx 持有的令牌从注册表顶掉，取消信号从此失联。）
    // M2：高频 UI delta 转发器（易失通道；不落库）。
    let forwarder: Arc<dyn sg_agent::modelgw::TurnDeltaForwarder> = Arc::new(
        crate::deltas::HubForwarder::new(state.deltas.clone(), run_id, &workitem_id),
    );
    let run_store = state.run_store.clone();
    let audit_store = state.run_store.clone();
    let gateway = state.model.clone();
    let policy = policy_snapshot.clone();
    let registry = state.runs.clone();
    let run_id_owned = run_id.to_string();
    let _ = run;
    let t_workitem = workitem_id;
    let t_goal = goal;
    let t_manifest = manifest_id;
    let t_allow = allowlist;
    let t_budget = budget;
    let initial = std::sync::Arc::new(initial);
    let initial_for_task = initial.clone();
    let compact_policy = std::sync::Arc::new(compact_policy);
    let compact_for_task = compact_policy.clone();
    let summary = std::sync::Arc::new(summary);
    let summary_for_task = summary.clone();
    state.handle.spawn(async move {
        let run_id_inner = run_id_owned.clone();
        let joined = tokio::task::spawn_blocking(move || {
            let mut rollout = rollout.take();
            let initial = initial_for_task;
            let compact_policy = compact_for_task;
            let summary = summary_for_task;
            let config = sg_agent::RunConfig {
                workitem_id: &t_workitem,
                task_id: "",
                goal: &t_goal,
                manifest_id: &t_manifest,
                tool_allowlist: &t_allow,
                idempotency_key: "",
                budget: &t_budget,
                max_iterations: 20,
            };
            if let Some(ro) = rollout.as_mut() {
                // F06/F07/F08：装配摘要（层数/知识项/告警）入 rollout。
                let _ = ro.append("instructions_summary", summary.as_ref().clone());
            }
            let out = sg_agent::execute_run(
                &run_store,
                &gateway,
                &policy,
                Some(executor.as_ref()),
                &config,
                &run_id_inner,
                Some(&token),
                rollout.take(),
                &initial,
                &compact_policy,
                Some(forwarder),
            );
            (out, rollout)
        })
        .await;
        let (result, rollout) = joined.unwrap_or_else(|_| {
            (
                Err(sg_store::Error::Message("run task panicked".into())),
                None,
            )
        });
        // rollout 收尾：fsync + sha256 → 审计行（路径/行数/哈希；合规证据链仍是 audit+evidence）。
        if let Some(ro) = rollout.and_then(|r| r.finish().ok()) {
            let (lines, sha256) = ro;
            let path = sg_agent::rollout::Rollout::path_for(&audit_store.data_dir, &run_id_owned);
            let _ = sg_store::audit::append(
                &audit_store,
                "system",
                "agent.rollout",
                "agent_run",
                &run_id_owned,
                json!({"path": path.to_string_lossy(), "lines": lines, "sha256": sha256}),
            );
        }
        // 终态/事件已由 execute_run 写库（失败也含在 result 中）；此处只做注册表清理。
        let _ = result;
        // P0-3：活动终态回写——Run 结束时其绑定的 stage activity 不得停留在 running。
        if let Ok(run_row) = sg_agent::get_run(&audit_store, &run_id_owned) {
            let activity_state = match run_row.status.as_str() {
                "completed_execution" => Some("done"),
                "failed" | "cancelled" => Some("failed"),
                _ => None,
            };
            if let Some(state) = activity_state {
                let activity_id: String = audit_store
                    .with_conn(|conn| {
                        conn.query_row(
                            "SELECT stage_activity_id FROM agent_runs WHERE id=?1",
                            [&run_id_owned],
                            |r| r.get::<_, String>(0),
                        )
                        .map_err(sg_store::Error::from)
                    })
                    .unwrap_or_default();
                if !activity_id.is_empty() {
                    let _ = audit_store.with_conn(|conn| {
                        conn.execute(
                            "UPDATE stage_activities SET state=?1, updated_at=?2 WHERE id=?3 AND state='running'",
                            rusqlite::params![state, sg_store::timefmt::now(), activity_id],
                        )?;
                        Ok(())
                    });
                }
            }
        }
        registry.unregister(&run_id_owned);
    });
    Ok(summary.as_ref().clone())
}

/// M4：model.usage 聚合——model_turns 的 token/缓存/延迟观测 + 压缩次数与前后估算。
fn model_usage(_state: &AppState, store: &Store, run_id: Option<&str>) -> RpcResult {
    let (calls, tokens_in, tokens_out, cached, reasoning, ttft_avg, ttft_p95, total_ms, compactions, compact_before, compact_after, daily) =
        store
            .with_conn(|conn| {
                let turns: (i64, i64, i64, i64, i64, Option<f64>, Option<i64>, i64) = conn
                    .query_row(
                        "SELECT COUNT(*), COALESCE(SUM(tokens_in),0), COALESCE(SUM(tokens_out),0), COALESCE(SUM(cached_tokens),0), COALESCE(SUM(reasoning_tokens),0), AVG(ttft_ms), MAX(ttft_ms), COALESCE(SUM(total_ms),0) FROM model_turns WHERE (?1 IS NULL OR agent_run_id=?1)",
                        rusqlite::params![run_id],
                        |r| {
                            Ok((
                                r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?,
                                r.get(5)?, r.get(6)?, r.get(7)?,
                            ))
                        },
                    )
                    .map_err(Error::from)?;
                let comp: (i64, i64, i64) = conn
                    .query_row(
                        "SELECT COUNT(*), COALESCE(MAX(json_extract(payload,'$.beforeEst')),-1), COALESCE(MAX(json_extract(payload,'$.afterEst')),-1) FROM events_outbox WHERE type='run.compacted' AND (?1 IS NULL OR aggregate_id=?1)",
                        rusqlite::params![run_id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .map_err(Error::from)?;
                let daily = model_usage_daily(conn, run_id)?;
                Ok((
                    turns.0, turns.1, turns.2, turns.3, turns.4,
                    turns.5, turns.6, turns.7,
                    comp.0, comp.1, comp.2,
                    daily,
                ))
            })
            .map_err(store_err)?;
    let hit_ratio = if tokens_in > 0 {
        Some((cached as f64 / tokens_in as f64 * 1000.0).round() / 1000.0)
    } else {
        None
    };
    Ok(json!({
        "scope": run_id,
        "calls": calls,
        "tokensIn": tokens_in,
        "tokensOut": tokens_out,
        "cachedTokens": cached,
        "cacheHitRatio": hit_ratio,
        "daily": daily,
        "reasoningTokens": reasoning,
        "ttftAvgMs": ttft_avg.map(|v| (v * 10.0).round() / 10.0),
        "ttftP95Ms": ttft_p95,
        "totalMs": total_ms,
        "compactions": compactions,
        "compactionBeforeEst": if compact_before >= 0 { json!(compact_before) } else { json!(null) },
        "compactionAfterEst": if compact_after >= 0 { json!(compact_after) } else { json!(null) },
        "costMicros": 0,
        "costNote": "未接价格表；成本恒 0（诚实口径）",
    }))
}

/// 交付物冻结基线 → 自动放入知识库（git 管理，RFC v1.0 manifest 平面）。
/// 逐修订以 opId 幂等（baseline-{基线id}-{工件id}）；失败逐条记录，不阻塞审批（诚实降级）。
fn publish_deliverables_to_knowledge(
    store: &Store,
    project_id: &str,
    workitem_id: &str,
    baseline: &sg_artifact::Baseline,
) -> Value {
    let root = match sg_knowledge::reconcile::project_root(store, project_id) {
        Ok(root) => root,
        Err(_) => {
            return json!({"published": false, "reason": "project_root_missing"});
        }
    };
    let mut items: Vec<Value> = Vec::new();
    let mut created = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;
    if let Some(map) = baseline.revision_map.as_object() {
        for (artifact_id, rev_id) in map {
            let rev_id = match rev_id.as_str() {
                Some(id) => id.to_string(),
                None => continue,
            };
            let title = match sg_artifact::get_artifact(store, artifact_id) {
                Ok(a) => a.title,
                Err(e) => {
                    items.push(
                        json!({"artifactId": artifact_id, "ok": false, "error": e.to_string()}),
                    );
                    failed += 1;
                    continue;
                }
            };
            let body = match sg_artifact::get_revision(store, &rev_id)
                .and_then(|rev| sg_store::objects::open(store, &rev.content_sha256))
            {
                Ok(bytes) => String::from_utf8_lossy(&bytes).to_string(),
                Err(e) => {
                    items.push(
                        json!({"artifactId": artifact_id, "ok": false, "error": e.to_string()}),
                    );
                    failed += 1;
                    continue;
                }
            };
            // 幂等键：同基线同工件重复冻结 → 同 opId 同指纹 → 原终态复用。
            let op_id = format!("baseline-{}-{artifact_id}", baseline.id);
            let params = json!({
                "projectId": project_id,
                "opId": op_id,
                "kind": "document",
                "name": title,
                "body": body,
                "expectedAbsent": true,
            });
            match sg_knowledge::manifest::manifest_create(store, &params) {
                Ok(v) => {
                    // 幂等：首次 projected，重复冻结 receipt_replay 原样返回。
                    let replayed = v["status"].as_str() != Some("projected");
                    if replayed {
                        skipped += 1
                    } else {
                        created += 1
                    }
                    items.push(json!({
                        "artifactId": artifact_id, "ok": true,
                        "sourceStableId": v["stableId"], "status": v["status"],
                    }));
                }
                Err(e) => {
                    failed += 1;
                    items.push(
                        json!({"artifactId": artifact_id, "ok": false, "error": e.to_string()}),
                    );
                }
            }
        }
    }
    // 有新增才需要 reconcile + 重建 generation（幂等）。
    let mut synced = false;
    if created > 0 {
        let sync = sg_knowledge::reconcile::sync_from_repo(store, project_id, &root);
        let gen = sg_knowledge::reconcile::build_and_activate_generation(store, project_id, &root);
        synced = sync.is_ok() && gen.is_ok();
    }
    json!({
        "published": failed == 0 && (created > 0 || skipped > 0),
        "workitemId": workitem_id,
        "created": created,
        "skipped": skipped,
        "failed": failed,
        "synced": synced,
        "note": if failed == 0 { "已写入 <repo>/knowledge/（git 提交由用户显式操作）" } else { "部分失败，详见 items" },
        "items": items,
    })
}

/// 近 30 天按 UTC 日零填充聚合：总 Token=in+out，命中率=cached/in（无输入日为 null）。供每日折线图。
fn model_usage_daily(
    conn: &rusqlite::Connection,
    run_id: Option<&str>,
) -> Result<Vec<Value>, Error> {
    let mut stmt = conn
        .prepare(
            "WITH RECURSIVE days(day) AS (
                 SELECT date('now','-29 days')
                 UNION ALL SELECT date(day,'+1 day') FROM days WHERE day < date('now')
             )
             SELECT days.day,
                    COALESCE(SUM(t.tokens_in),0), COALESCE(SUM(t.tokens_out),0), COALESCE(SUM(t.cached_tokens),0)
             FROM days
             LEFT JOIN model_turns t
               ON substr(t.created_at,1,10)=days.day AND (?1 IS NULL OR t.agent_run_id=?1)
             GROUP BY days.day ORDER BY days.day",
        )
        .map_err(Error::from)?;
    let rows = stmt
        .query_map(rusqlite::params![run_id], |r| {
            let day: String = r.get(0)?;
            let tin: i64 = r.get(1)?;
            let tout: i64 = r.get(2)?;
            let cached: i64 = r.get(3)?;
            Ok(json!({
                "day": day,
                "tokensIn": tin,
                "totalTokens": tin + tout,
                "cachedTokens": cached,
                "cacheHitRatio": if tin > 0 {
                    json!((cached as f64 / tin as f64 * 1000.0).round() / 1000.0)
                } else {
                    Value::Null
                },
            }))
        })
        .map_err(Error::from)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Error::from)
}

/// M6：工具清单（候选/活跃/撤销状态 + schema digest）。
fn mcp_tools_list(store: &Store, server_id: Option<&str>) -> RpcResult {
    let items: Vec<Value> = store
        .with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT t.id, t.server_id, s.name, t.tool_name, t.description, t.schema_json, t.schema_digest, t.read_only_hint, t.status
                 FROM mcp_server_tools t JOIN mcp_servers s ON s.id=t.server_id
                 WHERE (?1 IS NULL OR t.server_id=?1)
                 ORDER BY s.name, t.tool_name",
            )?;
            let rows = stmt.query_map(rusqlite::params![server_id], |r| {
                Ok(json!({
                    "toolId": r.get::<_, String>(0)?,
                    "serverId": r.get::<_, String>(1)?,
                    "serverName": r.get::<_, String>(2)?,
                    "toolName": r.get::<_, String>(3)?,
                    "description": r.get::<_, String>(4)?,
                    "schema": serde_json::from_str::<Value>(&r.get::<_, String>(5)?).unwrap_or(Value::Null),
                    "schemaDigest": r.get::<_, String>(6)?,
                    "readOnlyHint": r.get::<_, i64>(7)? == 1,
                    "status": r.get::<_, String>(8)?,
                    "modelName": format!("mcp__{}__{}", r.get::<_, String>(2)?, r.get::<_, String>(3)?),
                }))
            })?;
            let out = rows.flatten().collect::<Vec<_>>();
            Ok(out)
        })
        .map_err(store_err)?;
    Ok(json!({"items": items}))
}

#[cfg(test)]
mod m6_zero_change_tests {
    use super::*;

    /// M6 退出标准：默认无 MCP server 时零行为变化——
    /// 策略快照不含任何 mcp 来源；注册+批准后出现（写工具 High/必审批）。
    #[test]
    fn policy_snapshot_unchanged_without_mcp_servers() {
        let dir = std::env::temp_dir().join(format!("sg-mcp-zero-{}", sg_store::ids::new_id("t")));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir, "test").unwrap();
        let (snap, envelope) = assemble_policy_snapshot(&store);
        let any_mcp = envelope["sources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["source"] == "mcp");
        assert!(!any_mcp, "无 MCP server 时策略快照不得出现 mcp 来源");
        let before_count = snap.tool_rules.len();

        // 注册+批准（需要 python3 跑 fake server；缺失时跳过该半段）。
        let has_python = std::process::Command::new("python3")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if has_python {
            let v = sg_settings::mcp_ext::server_add(
                &store,
                "srvzero",
                "python3",
                &["/tmp/sg-mcp-e2e/fake_server.py".into(), "ok".into()],
            )
            .unwrap();
            sg_settings::mcp_ext::server_approve(&store, v["serverId"].as_str().unwrap(), "admin")
                .unwrap();
            let (snap2, envelope2) = assemble_policy_snapshot(&store);
            let mcp_rules: Vec<_> = snap2
                .tool_rules
                .iter()
                .filter(|r| r.tool.starts_with("mcp__"))
                .collect();
            assert_eq!(mcp_rules.len(), 2, "活跃 MCP 工具进策略快照");
            let write_rule = mcp_rules
                .iter()
                .find(|r| r.tool.ends_with("send_thing"))
                .unwrap();
            assert!(write_rule.requires_approval, "写工具必须审批");
            assert_eq!(write_rule.data_level, "external");
            assert!(envelope2["sources"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["source"] == "mcp" && s["sandboxed"] == json!(false)));
            assert_eq!(snap.tool_rules.len(), before_count, "基线规则集不变");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod model_usage_daily_tests {
    use super::*;

    fn daily_rows(store: &Store, run_id: Option<&str>) -> Vec<Value> {
        store
            .with_conn(|conn| model_usage_daily(conn, run_id))
            .unwrap()
    }

    /// 近 30 天窗口零填充 + 按 UTC 日分组 + run 过滤 + 命中率口径（cached/tokens_in）。
    #[test]
    fn daily_window_zero_filled_and_run_scoped() {
        let dir =
            std::env::temp_dir().join(format!("sg-usage-daily-{}", sg_store::ids::new_id("t")));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir, "test").unwrap();
        let today = sg_store::timefmt::now();
        let old = sg_store::timefmt::now_plus_minutes(-40 * 24 * 60);
        store
            .with_conn(|conn| {
                for (id, run, seq, tin, tout, cached, created) in [
                    ("mt_d1", "run1", 2_i64, 120_i64, 30_i64, 60_i64, today.as_str()),
                    ("mt_d2", "run2", 1, 10, 5, 0, today.as_str()),
                    ("mt_old", "run1", 1, 999, 999, 999, old.as_str()),
                ] {
                    conn.execute(
                        "INSERT INTO model_turns(id, agent_run_id, turn_seq, tokens_in, tokens_out, cached_tokens, created_at)
                         VALUES (?1,?2,?3,?4,?5,?6,?7)",
                        rusqlite::params![id, run, seq, tin, tout, cached, created],
                    )
                    .map_err(Error::from)?;
                }
                Ok(())
            })
            .unwrap();

        let all = daily_rows(&store, None);
        assert_eq!(all.len(), 30, "固定 30 天窗口");
        assert_eq!(
            all[0]["day"],
            json!(&sg_store::timefmt::now_plus_minutes(-29 * 24 * 60)[..10])
        );
        assert_eq!(all[29]["day"], json!(&today[..10]));
        assert_eq!(all[0]["totalTokens"], json!(0), "窗口外的旧行不参与");
        assert_eq!(
            all[0]["cacheHitRatio"],
            Value::Null,
            "无输入日命中率为 null"
        );
        assert_eq!(all[29]["totalTokens"], json!(165));
        assert_eq!(all[29]["cachedTokens"], json!(60));
        assert_eq!(all[29]["cacheHitRatio"], json!(0.462));

        let run1 = daily_rows(&store, Some("run1"));
        assert_eq!(
            run1.last().unwrap()["totalTokens"],
            json!(150),
            "run 过滤生效"
        );
        assert_eq!(run1.last().unwrap()["cacheHitRatio"], json!(0.5));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// M6-04：automation.* / goal.* RPC（RATIFLOW_AUTOMATIONS 门控创建/触发；
/// 查询面不受限——审计可见）。
fn automation_rpc(store: &Store, method: &str, params: &Value) -> RpcResult {
    let automation_flag = std::env::var("RATIFLOW_AUTOMATIONS").ok().as_deref() == Some("1");
    let invalid = |m: String| RpcError::new(ErrorCode::InvalidParams, m.as_str());
    let store_err =
        |e: sg_store::Error| RpcError::new(ErrorCode::InternalError, e.to_string().as_str());
    let str_param = |k: &str| -> Result<String, RpcError> {
        params
            .get(k)
            .and_then(|v| v.as_str())
            .map(String::from)
            .ok_or_else(|| invalid(format!("missing param: {k}")))
    };
    // WP-8a：shadow 域错误分类（确定性码，不落 InternalError=Transient 面）。
    let shadow_err = |e: &sg_store::Error| {
        let msg = e.to_string();
        let code = if msg.contains("shadow_decision_conflict") {
            ErrorCode::Conflict
        } else if msg.contains("shadow_suggestion_missing")
            || msg.contains("shadow_decision_missing")
        {
            ErrorCode::NotFound
        } else if msg.contains("shadow_decision_invalid")
            || msg.contains("shadow_suggestion_invalid")
        {
            ErrorCode::InvalidParams
        } else {
            ErrorCode::InternalError
        };
        RpcError::new(code, msg.as_str())
    };
    match method {
        "automation.create" => {
            if !automation_flag {
                return Err(RpcError::new(
                    ErrorCode::InvalidRequest,
                    "feature_disabled: RATIFLOW_AUTOMATIONS 未开启",
                ));
            }
            let interval_secs = params
                .get("intervalSecs")
                .and_then(|v| v.as_i64())
                .unwrap_or(3600);
            // 下限校验（缺陷审计）：interval<=0 会让每 tick 恒过期，触发洪泛。
            if interval_secs < 1 {
                return Err(RpcError::new(
                    ErrorCode::InvalidParams,
                    "interval_secs_invalid: intervalSecs 须 >= 1",
                ));
            }
            let a = sg_workflow::automation::create(
                store,
                &str_param("key")?,
                params.get("projectId").and_then(|v| v.as_str()),
                params.get("workItemId").and_then(|v| v.as_str()),
                &params
                    .get("intent")
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "{}".into()),
                interval_secs,
                params
                    .get("misfirePolicy")
                    .and_then(|v| v.as_str())
                    .unwrap_or("skip"),
                params
                    .get("overlapPolicy")
                    .and_then(|v| v.as_str())
                    .unwrap_or("skip"),
                params.get("autonomyGrantId").and_then(|v| v.as_str()),
                params
                    .get("createdBy")
                    .and_then(|v| v.as_str())
                    .unwrap_or("local"),
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(a).unwrap_or_default())
        }
        "automation.list" => {
            let items: Vec<Value> = store
                .with_conn(|conn| {
                    let mut stmt = conn.prepare(
                        "SELECT id, key, COALESCE(workitem_id,''), interval_secs, next_fire_at,
                                status, revision FROM automations ORDER BY created_at",
                    )?;
                    let rows = stmt.query_map([], |r| {
                        Ok(json!({
                            "id": r.get::<_, String>(0)?,
                            "key": r.get::<_, String>(1)?,
                            "workItemId": r.get::<_, String>(2)?,
                            "intervalSecs": r.get::<_, i64>(3)?,
                            "nextFireAt": r.get::<_, String>(4)?,
                            "status": r.get::<_, String>(5)?,
                            "revision": r.get::<_, i64>(6)?,
                        }))
                    })?;
                    let mut out = Vec::new();
                    for row in rows {
                        out.push(row?);
                    }
                    Ok(out)
                })
                .map_err(store_err)?;
            Ok(json!({ "items": items }))
        }
        "automation.pause" | "automation.resume" => {
            if !automation_flag {
                return Err(RpcError::new(
                    ErrorCode::InvalidRequest,
                    "feature_disabled: RATIFLOW_AUTOMATIONS 未开启",
                ));
            }
            let status = if method == "automation.pause" {
                "paused"
            } else {
                "active"
            };
            let a = sg_workflow::automation::set_status(
                store,
                &str_param("automationId")?,
                status,
                params
                    .get("expectedRevision")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0),
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(a).unwrap_or_default())
        }
        "automation.runNow" => {
            if !automation_flag {
                return Err(RpcError::new(
                    ErrorCode::InvalidRequest,
                    "feature_disabled: RATIFLOW_AUTOMATIONS 未开启",
                ));
            }
            let automation_id = str_param("automationId")?;
            let scheduled_for = params
                .get("scheduledFor")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(sg_store::timefmt::now);
            let (status, note) =
                crate::automation_dispatch::fire_one(store, &automation_id, &scheduled_for)
                    .map_err(store_err)?;
            Ok(
                json!({"automationId": automation_id, "scheduledFor": scheduled_for, "status": status, "note": note}),
            )
        }
        "automation.history" => {
            let automation_id = str_param("automationId")?;
            let items: Vec<Value> = store
                .with_conn(|conn| {
                    let mut stmt = conn.prepare(
                        "SELECT scheduled_for, receipt, status, note, created_at
                         FROM automation_runs WHERE automation_id=?1 ORDER BY created_at",
                    )?;
                    let rows = stmt.query_map([&automation_id], |r| {
                        Ok(json!({
                            "scheduledFor": r.get::<_, String>(0)?,
                            "receipt": r.get::<_, String>(1)?,
                            "status": r.get::<_, String>(2)?,
                            "note": r.get::<_, String>(3)?,
                            "createdAt": r.get::<_, String>(4)?,
                        }))
                    })?;
                    let mut out = Vec::new();
                    for row in rows {
                        out.push(row?);
                    }
                    Ok(out)
                })
                .map_err(store_err)?;
            Ok(json!({ "items": items }))
        }
        "autonomy.createGrant" => {
            if !automation_flag {
                return Err(RpcError::new(
                    ErrorCode::InvalidRequest,
                    "feature_disabled: RATIFLOW_AUTOMATIONS 未开启",
                ));
            }
            let id = sg_store::ids::new_id("agr");
            let now = sg_store::timefmt::now();
            let workitem_id = str_param("workItemId")?;
            let allowed_tools = params
                .get("allowedTools")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let allowed_risks = params
                .get("allowedRisks")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            // WP-1（RDWS v1.4）：限额声明进 limits_json，grant_usage_ledger 逐消费行
            // 校验（维度键 = model_calls/tokens_in/tokens_out/reasoning_tokens/
            // tool_calls/cost_micros；缺省/≤0 = 该维不限）。
            let limits = params.get("limits").cloned().filter(|v| v.is_object());
            if let Some(l) = &limits {
                for key in l
                    .as_object()
                    .map(|o| o.keys().cloned().collect::<Vec<_>>())
                    .unwrap_or_default()
                {
                    if !matches!(
                        key.as_str(),
                        "model_calls"
                            | "tokens_in"
                            | "tokens_out"
                            | "reasoning_tokens"
                            | "tool_calls"
                            | "cost_micros"
                    ) {
                        return Err(err(
                            ErrorCode::InvalidParams,
                            format!("autonomy_limits_invalid: 未知限额维度 {key}"),
                        ));
                    }
                }
            }
            store.with_conn(|conn| {
                conn.execute(
                    "INSERT INTO autonomy_grants(id, workitem_id, allowed_tools_json, allowed_risks_json,
                        limits_json, expires_at, granted_at, created_at, updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?7,?7)",
                    rusqlite::params![
                        id,
                        workitem_id,
                        serde_json::to_string(&allowed_tools).unwrap_or_default(),
                        serde_json::to_string(&allowed_risks).unwrap_or_default(),
                        limits.map(|l| l.to_string()).unwrap_or_else(|| "{}".into()),
                        params.get("expiresAt").and_then(|v| v.as_str()).unwrap_or(""),
                        now
                    ],
                )
                .map_err(Error::from)?;
                Ok(())
            })
            .map_err(store_err)?;
            Ok(json!({"grantId": id}))
        }
        "autonomy.revokeGrant" => {
            if !automation_flag {
                return Err(RpcError::new(
                    ErrorCode::InvalidRequest,
                    "feature_disabled: RATIFLOW_AUTOMATIONS 未开启",
                ));
            }
            // 撤销即时生效（缺陷审计修复：此前全库无撤销路径）。
            let grant = sg_policy::autonomy::revoke_grant(
                store,
                &str_param("grantId")?,
                params.get("reason").and_then(|v| v.as_str()).unwrap_or(""),
            )
            .map_err(store_err)?;
            sg_store::audit::append(
                store,
                "local",
                "autonomy.grant_revoked",
                "autonomy_grant",
                &grant.id,
                json!({}),
            )
            .map_err(store_err)?;
            Ok(json!({
                "grantId": grant.id,
                "status": grant.status,
                "workitemId": grant.workitem_id,
            }))
        }
        "goal.autoReleaseCheck" => {
            let check = sg_policy::autonomy::automatic_release_check(
                store,
                &str_param("workItemId")?,
                &str_param("grantId")?,
                &sg_store::timefmt::now(),
            )
            .map_err(store_err)?;
            Ok(serde_json::to_value(check).unwrap_or_default())
        }
        "notification.list" => {
            let items: Vec<Value> = store
                .with_conn(|conn| {
                    let mut stmt = conn.prepare(
                        "SELECT id, kind, COALESCE(workitem_id,''), COALESCE(automation_id,''),
                                payload_json, delivered, created_at
                         FROM notification_outbox ORDER BY created_at LIMIT 100",
                    )?;
                    let rows = stmt.query_map([], |r| {
                        Ok(json!({
                            "id": r.get::<_, String>(0)?,
                            "kind": r.get::<_, String>(1)?,
                            "workItemId": r.get::<_, String>(2)?,
                            "automationId": r.get::<_, String>(3)?,
                            "payload": serde_json::from_str::<Value>(&r.get::<_, String>(4)?).unwrap_or(json!({})),
                            "delivered": r.get::<_, i64>(5)? != 0,
                            "createdAt": r.get::<_, String>(6)?,
                        }))
                    })?;
                    let mut out = Vec::new();
                    for row in rows {
                        out.push(row?);
                    }
                    Ok(out)
                })
                .map_err(store_err)?;
            Ok(json!({ "items": items }))
        }
        // --- WP-8a：suggestion/observation 基础设施（写侧由领域路径生成：
        //     WP-8 fast-track 判定 / WP-12 automation tick；此处只暴露决定/复核/观察面）---
        "automation.decideSuggestion" => {
            if !automation_flag {
                return Err(RpcError::new(
                    ErrorCode::InvalidRequest,
                    "feature_disabled: RATIFLOW_AUTOMATIONS 未开启",
                ));
            }
            let out = sg_workflow::shadow::decide(
                store,
                &str_param("suggestionId")?,
                &str_param("decision")?,
                &str_param("decidedBy")?,
                &str_param("note")?,
            )
            .map_err(|e| shadow_err(&e))?;
            // WP-8：fast-track 建议被采纳 → 应用缩减（活动缩减；交付物豁免在
            // deliverable 检查时按已采纳状态消费）。未采纳/非 fast-track → 幂等跳过。
            let mut fast_track_applied = Value::Null;
            if out.decision == "accepted" {
                if let Ok(s) = sg_workflow::shadow::get(store, &out.suggestion_id) {
                    if s.source == "fast_track" {
                        if let Some(wi) = &s.workitem_id {
                            let gate = s.content.get("gate").and_then(|g| g.as_str()).unwrap_or("");
                            let applied =
                                sg_workitem::fast_track::apply_if_accepted(store, wi, gate)
                                    .map_err(|e| shadow_err(&e))?;
                            fast_track_applied =
                                json!({"workItemId": wi, "gate": gate, "applied": applied});
                            let _ = sg_store::audit::append(
                                store,
                                &str_param("decidedBy")?,
                                "fast_track.applied",
                                "workitem",
                                wi,
                                json!({"gate": gate, "applied": applied}),
                            );
                        }
                    }
                }
            }
            Ok(json!({ "decision": out, "fastTrack": fast_track_applied }))
        }
        "automation.reviewSuggestion" => {
            if !automation_flag {
                return Err(RpcError::new(
                    ErrorCode::InvalidRequest,
                    "feature_disabled: RATIFLOW_AUTOMATIONS 未开启",
                ));
            }
            let false_positive = params
                .get("falsePositive")
                .and_then(|v| v.as_bool())
                .ok_or_else(|| invalid("missing param: falsePositive".into()))?;
            let out = sg_workflow::shadow::review(
                store,
                &str_param("suggestionId")?,
                false_positive,
                &str_param("reviewer")?,
                &str_param("note")?,
            )
            .map_err(|e| shadow_err(&e))?;
            Ok(serde_json::to_value(out).unwrap_or_default())
        }
        "automation.observations" => {
            let source = opt_str_param(params, "source");
            let automation_id = opt_str_param(params, "automationId");
            let out = sg_workflow::shadow::observations(
                store,
                source.as_deref().filter(|s| !s.is_empty()),
                automation_id.as_deref().filter(|s| !s.is_empty()),
            )
            .map_err(|e| shadow_err(&e))?;
            Ok(serde_json::to_value(out).unwrap_or_default())
        }
        _ => Err(invalid(format!("unknown automation method: {method}"))),
    }
}

#[cfg(test)]
mod receipt_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-receipt-{}-{}",
            std::process::id(),
            sg_store::ids::new_id("t")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Store::open(&dir, "test").unwrap()
    }

    fn receipt_row(store: &Store, method: &str, key: &str) -> (String, String, i64, String) {
        store
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT lease_state, COALESCE(response_json,''), lease_revision, COALESCE(request_fingerprint,'')
                     FROM rpc_receipts WHERE method=?1 AND idem_key=?2",
                    [method, key],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .unwrap())
            })
            .unwrap()
    }

    #[test]
    fn missing_key_rejected_without_execution() {
        let store = setup();
        let executed = AtomicUsize::new(0);
        let out = with_rpc_receipt(&store, "", "m.x", &json!({"a":1}), || {
            executed.fetch_add(1, Ordering::SeqCst);
            Ok(json!({"ok": true}))
        });
        assert!(out.is_err());
        assert_eq!(
            out.unwrap_err().message,
            "idempotency_key_required",
            "缺 key 必须 idempotency_key_required"
        );
        assert_eq!(
            executed.load(Ordering::SeqCst),
            0,
            "缺 key 不得执行 mutation"
        );
    }

    #[test]
    fn replay_same_fingerprint_executes_once() {
        let store = setup();
        let executed = AtomicUsize::new(0);
        let run = || {
            with_rpc_receipt(
                &store,
                "k1",
                "m.x",
                &json!({"b": 2, "idempotencyKey": "k1"}),
                || {
                    executed.fetch_add(1, Ordering::SeqCst);
                    Ok(json!({"n": 1}))
                },
            )
            .unwrap()
        };
        let first = run();
        let second = run();
        assert_eq!(first, second, "同 key 同参重放返回首次响应");
        assert_eq!(executed.load(Ordering::SeqCst), 1, "重放不得重执行");
        let (state, body, rev, _) = receipt_row(&store, "m.x", "k1");
        assert_eq!(state, "completed");
        assert_eq!(rev, 2, "成功后 lease_revision+1");
        assert!(body.contains("\"n\":1"));
    }

    #[test]
    fn fingerprint_mismatch_rejected_before_execution() {
        let store = setup();
        let executed = AtomicUsize::new(0);
        let call = |params: Value| {
            with_rpc_receipt(&store, "k1", "m.x", &params, || {
                executed.fetch_add(1, Ordering::SeqCst);
                Ok(json!({"n": 1}))
            })
        };
        call(json!({"a": 1})).unwrap();
        let err = call(json!({"a": 2})).unwrap_err();
        assert_eq!(
            err.message, "receipt_fingerprint_mismatch",
            "同 key 异参执行前拒绝"
        );
        assert_eq!(executed.load(Ordering::SeqCst), 1, "指纹拒绝不得重执行");
    }

    #[test]
    fn key_scoped_per_method() {
        let store = setup();
        let executed = AtomicUsize::new(0);
        for method in ["m.a", "m.b"] {
            with_rpc_receipt(&store, "shared", method, &json!({"v": 1}), || {
                executed.fetch_add(1, Ordering::SeqCst);
                Ok(json!({"m": method}))
            })
            .unwrap();
        }
        assert_eq!(executed.load(Ordering::SeqCst), 2, "同 key 跨方法不串台");
    }

    #[test]
    fn deterministic_error_completed_and_replayable() {
        let store = setup();
        let executed = AtomicUsize::new(0);
        let fail = |executed: &AtomicUsize| {
            with_rpc_receipt(&store, "k2", "m.x", &json!({"a": 1}), || {
                executed.fetch_add(1, Ordering::SeqCst);
                Err(err(ErrorCode::InvalidParams, "参数非法"))
            })
        };
        let e1 = fail(&executed).unwrap_err();
        assert_eq!(executed.load(Ordering::SeqCst), 1);
        let (state, body, _, _) = receipt_row(&store, "m.x", "k2");
        assert_eq!(state, "completed", "deterministic 错误落 completed");
        assert!(body.contains("__sg_rpc_error"), "envelope 落库");
        let e2 = fail(&executed).unwrap_err();
        assert_eq!(e1.code, e2.code, "错误 envelope 原样重放");
        assert_eq!(e1.message, e2.message);
        assert_eq!(executed.load(Ordering::SeqCst), 1, "重放不重执行");
    }

    #[test]
    fn transient_error_releases_lease_for_reclaim() {
        let store = setup();
        let attempts = AtomicUsize::new(0);
        let call = || {
            with_rpc_receipt(&store, "k3", "m.x", &json!({"a": 1}), || {
                if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    Err(err(ErrorCode::InternalError, "io 抖动"))
                } else {
                    Ok(json!({"n": 2}))
                }
            })
        };
        assert!(call().is_err(), "首次 transient 失败");
        let (state, _, _, _) = receipt_row(&store, "m.x", "k3");
        assert_eq!(state, "retryable_failed", "transient 错误释放执行语义");
        let out = call().unwrap();
        assert_eq!(out["n"], 2, "后续请求重新认领并执行");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        let (state, _, _, _) = receipt_row(&store, "m.x", "k3");
        assert_eq!(state, "completed");
    }

    #[test]
    fn expired_lease_takeover_self_heals() {
        let store = setup();
        // 模拟崩溃残留：in_flight + 已过期租约 + 幽灵 owner。
        store
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO rpc_receipts(method, idem_key, request_fingerprint, owner,
                         lease_revision, lease_expires_at, lease_state, response_json,
                         created_at, updated_at)
                     VALUES ('m.x','k4','','ghost',1,'2000-01-01T00:00:00.000Z','in_flight','','2000-01-01T00:00:00.000Z','2000-01-01T00:00:00.000Z')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        let out = with_rpc_receipt(&store, "k4", "m.x", &json!({"a": 1}), || {
            Ok(json!({"healed": true}))
        })
        .expect("过期租约可接管");
        assert_eq!(out["healed"], true);
        let (state, _, rev, _) = receipt_row(&store, "m.x", "k4");
        assert_eq!(state, "completed");
        assert_eq!(rev, 3, "接管 revision+1、完成再 +1");
    }

    #[test]
    fn concurrent_claim_grants_single_owner() {
        let store = std::sync::Arc::new(setup());
        let executed = std::sync::Arc::new(AtomicUsize::new(0));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let store = store.clone();
            let executed = executed.clone();
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                with_rpc_receipt(&store, "k5", "m.x", &json!({"a": 1}), || {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    executed.fetch_add(1, Ordering::SeqCst);
                    Ok(json!({"winner": true}))
                })
            }));
        }
        let mut ok = 0;
        for h in handles {
            match h.join().unwrap() {
                Ok(v) => {
                    assert_eq!(v["winner"], true);
                    ok += 1;
                }
                Err(e) => assert_eq!(e.message, "conflict", "竞争失败方只允许 in_flight 冲突"),
            }
        }
        assert_eq!(
            executed.load(Ordering::SeqCst),
            1,
            "并发只一 owner 获得执行权"
        );
        assert!(ok >= 1);
    }

    #[test]
    fn tx_variant_commits_domain_and_receipt_atomically() {
        let store = setup();
        let out = with_rpc_receipt_tx(&store, "k6", "m.tx", &json!({"a": 1}), |conn| {
            conn.execute(
                "INSERT INTO app_meta(key, value) VALUES ('receipt_tx_probe','1')",
                [],
            )
            .map_err(|e| err(ErrorCode::InternalError, e.to_string()))?;
            Ok(json!({"committed": true}))
        })
        .unwrap();
        assert_eq!(out["committed"], true);
        let probe: i64 = store
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM app_meta WHERE key='receipt_tx_probe'",
                    [],
                    |r| r.get(0),
                )
                .unwrap())
            })
            .unwrap();
        assert_eq!(probe, 1, "领域写已随事务提交");
        let (state, _, _, _) = receipt_row(&store, "m.tx", "k6");
        assert_eq!(state, "completed");
    }

    #[test]
    fn tx_variant_rolls_back_domain_on_deterministic_error() {
        let store = setup();
        let attempts = AtomicUsize::new(0);
        let call = || {
            with_rpc_receipt_tx(&store, "k7", "m.tx", &json!({"a": 1}), |conn| {
                attempts.fetch_add(1, Ordering::SeqCst);
                conn.execute(
                    "INSERT INTO app_meta(key, value) VALUES ('rollback_probe','1')",
                    [],
                )
                .map_err(|e| err(ErrorCode::InternalError, e.to_string()))?;
                Err(err(ErrorCode::InvalidParams, "校验失败"))
            })
        };
        assert!(call().is_err());
        let probe: i64 = store
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT COUNT(*) FROM app_meta WHERE key='rollback_probe'",
                    [],
                    |r| r.get(0),
                )
                .unwrap())
            })
            .unwrap();
        assert_eq!(probe, 0, "closure 失败 → 领域事务回滚，状态零残留");
        let (state, body, _, _) = receipt_row(&store, "m.tx", "k7");
        assert_eq!(state, "completed", "deterministic envelope 回滚后独立落库");
        assert!(body.contains("__sg_rpc_error"));
        let e2 = call().unwrap_err();
        assert_eq!(e2.message, "invalid_params", "envelope 重放");
        assert_eq!(attempts.load(Ordering::SeqCst), 1, "重放不重执行");
    }
}

#[cfg(test)]
mod approval_binding_tests {
    use super::*;

    fn setup() -> Store {
        let dir = std::env::temp_dir().join(format!(
            "sg-apbind-{}-{}",
            std::process::id(),
            sg_store::ids::new_id("t")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Store::open(&dir, "test").unwrap()
    }

    fn seed(store: &Store) {
        store
            .with_conn(|c| {
                c.execute_batch(
                    "INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
                     VALUES ('pj','u','n','p','main','t');
                     INSERT INTO workitems(id, project_id, title, description, labels, current_gate, created_at, updated_at)
                     VALUES ('wi','pj','t','','[]','requirements','t','t');
                     INSERT INTO context_manifests(id, workitem_id, scope, data_policy, created_at)
                     VALUES ('ctx1','wi','{}','standard','t');
                     INSERT INTO agent_runs(id, workitem_id, task_id, goal, input_baseline_sha, context_manifest_id,
                         tool_allowlist, budget, policy_snapshot, idempotency_key, status, created_at, updated_at)
                     VALUES ('run1','wi','','g','sha','ctx1','[]','{}','default','ik','queued','t','t');
                     INSERT INTO tool_proposals(id, agent_run_id, tool, arguments, risk, action_digest,
                         requires_approval, decision, created_at)
                     VALUES ('tp1','run1','builtin:apply_patch','{}','high','d',1,'proposed','t');
                     INSERT INTO provenance_nodes(id, workitem_id, node_type, entity_id, content_digest, verification_state, created_at)
                     VALUES ('n1','wi','tool_proposal','tp1','cd1','verified','t');
                     INSERT INTO provenance_nodes(id, workitem_id, node_type, entity_id, content_digest, verification_state, created_at)
                     VALUES ('n2','wi','artifact_revision','ar1','cd2','verified','t');
                     INSERT INTO provenance_edges(id, workitem_id, from_node_id, relation, to_node_id, stage_attempt_id, created_by_run_id, edge_digest, created_at)
                     VALUES ('e1','wi','n1','produced_by','n2','','','ed1','t');",
                )
                .map_err(Error::from)?;
                Ok(())
            })
            .unwrap();
    }

    /// RDWS-010：双 digest 重算——命中通过；edge_digest 漂移 → 失效+过期；
    /// frontier 外 provenance 写入 → scope 漂移拦截（incomplete 审批）。
    #[test]
    fn tool_proposal_binding_drift_rejection() {
        let store = setup();
        seed(&store);
        let impact = sg_provenance::impact::for_proposal(&store, "tp1").unwrap();
        // complete 审批（无 scope 绑定）。
        let a = sg_policy::request_approval_binding(
            &store,
            "tool_proposal",
            "tp1",
            "d",
            sg_policy::Risk::High,
            "r",
            60,
            Some("wi"),
            None,
            &sg_policy::ImpactBinding {
                impact_digest: &impact.impact_digest,
                completeness: impact.completeness.as_str(),
                scope_facts_digest: &impact.workitem_facts_digest,
            },
        )
        .unwrap();
        verify_tool_proposal_binding(&store, &sg_policy::get(&store, &a.id).unwrap()).unwrap();
        // 行内漂移：edge_digest 篡改 → impact_digest_changed + 审批过期。
        store
            .with_conn(|c| {
                c.execute(
                    "UPDATE provenance_edges SET edge_digest='tampered' WHERE id='e1'",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        let subject = sg_policy::get(&store, &a.id).unwrap();
        let err = verify_tool_proposal_binding(&store, &subject).unwrap_err();
        assert!(
            err.message.contains("impact_digest_changed")
                || err.to_string().contains("impact_digest_changed"),
            "{err}"
        );
        let after = sg_policy::get(&store, &a.id).unwrap();
        assert_eq!(after.status, "expired", "漂移审批已失效");
        // scope 漂移：incomplete 审批 + frontier 外写入 → scope_facts_changed。
        store
            .with_conn(|c| {
                c.execute("UPDATE provenance_edges SET edge_digest='ed1' WHERE id='e1'", [])?;
                c.execute_batch(
                    "INSERT INTO provenance_nodes(id, workitem_id, node_type, entity_id, content_digest, verification_state, created_at)
                     VALUES ('n9','wi','evidence','ev9','cd9','verified','t');",
                )?;
                Ok(())
            })
            .unwrap();
        let impact2 = sg_provenance::impact::for_proposal(&store, "tp1").unwrap();
        let b = sg_policy::request_approval_binding(
            &store,
            "tool_proposal",
            "tp1",
            "d2",
            sg_policy::Risk::High,
            "r",
            60,
            Some("wi"),
            None,
            &sg_policy::ImpactBinding {
                impact_digest: &impact2.impact_digest,
                completeness: "incomplete",
                scope_facts_digest: &impact2.workitem_facts_digest,
            },
        )
        .unwrap();
        verify_tool_proposal_binding(&store, &sg_policy::get(&store, &b.id).unwrap()).unwrap();
        // frontier 外再写入（不影响 impact 子图）→ 仅 scope 拦截。
        store
            .with_conn(|c| {
                c.execute_batch(
                    "INSERT INTO provenance_nodes(id, workitem_id, node_type, entity_id, content_digest, verification_state, created_at)
                     VALUES ('n10','wi','evidence','ev10','cd10','verified','t');
                     INSERT INTO provenance_edges(id, workitem_id, from_node_id, relation, to_node_id, stage_attempt_id, created_by_run_id, edge_digest, created_at)
                     VALUES ('e10','wi','n10','verifies','n10','','','ed10','t');",
                )?;
                Ok(())
            })
            .unwrap();
        let subject_b = sg_policy::get(&store, &b.id).unwrap();
        let err = verify_tool_proposal_binding(&store, &subject_b).unwrap_err();
        assert!(
            err.to_string().contains("scope_facts_changed"),
            "{}",
            err.to_string()
        );
        // 未知版本 fail-closed。
        store
            .with_conn(|c| {
                c.execute(
                    "UPDATE approvals SET digest_schema_version=99 WHERE id=?1",
                    [&b.id],
                )?;
                Ok(())
            })
            .unwrap();
        let err = verify_tool_proposal_binding(&store, &sg_policy::get(&store, &b.id).unwrap())
            .unwrap_err();
        assert!(
            err.to_string().contains("approval_digest_version_unknown"),
            "{err}"
        );
    }
}
