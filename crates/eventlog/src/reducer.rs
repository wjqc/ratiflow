//! 确定性 reducer 与三层投影（ADR-031 C3 r5）：
//! - 事实投影 = reducer(事件集)：纯函数、逐字节可复现；放行结论一律为 claimed；
//! - 发布层 = reducer(事件集, 见证)：published/revoked/unknown；
//! - 有效治理投影 = 按模式化谓词取证，唯一能产出 passed。
//!
//! fork.resolved 的闭包不信任 payload：由 reducer 从 DAG 复算并要求完全相等（C6 r4）。

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::dag::DagView;
use crate::{Decision, Envelope, Error, Payload, TrustLevel};

/// 一致性模式（ADR-031 C5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    LocalOnly,
    RepositoryBacked,
}

/// 发布见证（witness）：远端验证的留痕输入（C3）。同一见证集下输出确定；
/// 见证按输入序后者覆盖前者。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PublicationWitness {
    Verified {
        event_id: String,
        ref_name: String,
        commit_sha: String,
    },
    Revoked {
        event_id: String,
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationState {
    Unpublished,
    Published,
    Revoked,
    /// 断网/远端不可达/验证失败：fail-closed（C3 r3）。
    Unknown,
}

/// 发布层：事件 → 状态。无任何见证 = Unknown（不得拿缓存冒充验证结果）。
pub fn publication_state(witnesses: &[PublicationWitness], event_id: &str) -> PublicationState {
    let mut state = PublicationState::Unknown;
    for w in witnesses {
        match w {
            PublicationWitness::Verified { event_id: e, .. } if e == event_id => {
                state = PublicationState::Published;
            }
            PublicationWitness::Revoked { event_id: e, .. } if e == event_id => {
                state = PublicationState::Revoked;
            }
            _ => {}
        }
    }
    state
}

// --- 事实投影 ---

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AttemptFact {
    pub gate: String,
    pub state: String,
    pub transitions: Vec<String>,
}

/// 放行**声明**：事实层的唯一放行呈现形态（C3 r4）。是否生效由有效治理谓词判定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReleaseClaim {
    pub event_id: String,
    pub attempt_id: String,
    pub digest: String,
    pub decision: Decision,
    pub reviewer: String,
    pub trust: TrustLevel,
    /// 事实层恒为 claimed——恶意/未验证的 release.decided 不得呈现为 passed。
    pub status: &'static str,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FactProjection {
    pub attempts: BTreeMap<String, AttemptFact>,
    pub release_claims: Vec<ReleaseClaim>,
    /// 被有效裁决/显式 supersede 剔除的 attempt。
    pub superseded_attempts: BTreeSet<String>,
    /// 未裁决分叉波及的 attempt：forked/blocked，不得放行（C6）。
    pub forked_blocked_attempts: BTreeSet<String>,
    /// 校验失败的 ForkResolved 事件（裁决无效，分叉保持 blocked）。
    pub invalid_resolutions: Vec<String>,
}

#[derive(Debug)]
pub enum Projection {
    /// 完整性校验失败：整体 fail-closed，无任何可用投影（C2）。
    Failed(crate::Error),
    Facts(FactProjection),
}

/// 便捷装配：装载事件 → 校验 → 事实投影。完整性失败时整体 `Projection::Failed`
/// （fail-closed，不产半投影，C2）。
pub fn project(loaded: Vec<(String, Envelope)>) -> Projection {
    match DagView::build(loaded) {
        Ok(view) => fact_projection(&view),
        Err(e) => Projection::Failed(e),
    }
}

/// 事实投影：两遍法——先复算校验 ForkResolved 闭包，再按因果序应用未被剔除事件。
/// 同一事件集（任意装载顺序）产出 `serde_json` 逐字节一致的结果（C7-1 判据）。
pub fn fact_projection(view: &DagView) -> Projection {
    let order = view.causal_order();

    // 第一遍：闭包复算校验。
    let mut superseded_events: BTreeSet<String> = BTreeSet::new();
    let mut superseded_attempts: BTreeSet<String> = BTreeSet::new();
    let mut invalid_resolutions: Vec<String> = Vec::new();
    for env in &order {
        let Payload::ForkResolved {
            conflicting_heads,
            winner,
            superseded_descendants,
            ..
        } = &env.payload
        else {
            continue;
        };
        match validate_resolution(
            view,
            conflicting_heads,
            winner,
            superseded_descendants,
            &env.event_id,
        ) {
            Ok(losers) => {
                for id in &losers {
                    if let Some(loser) = view.events.get(id) {
                        superseded_attempts.insert(loser.attempt_id.clone());
                    }
                }
                superseded_events.extend(losers);
            }
            Err(()) => invalid_resolutions.push(env.event_id.clone()),
        }
    }

    // 未被有效裁决消解的分叉头 → 波及其分叉双侧 attempt（C6：forked/blocked）。
    // 裁决事件给出的是冲突 head 集；被消解的是它们的共同 parent 分叉头。
    let mut resolved_heads: BTreeSet<&str> = BTreeSet::new();
    for env in &order {
        if let Payload::ForkResolved {
            conflicting_heads, ..
        } = &env.payload
        {
            if invalid_resolutions.contains(&env.event_id) {
                continue;
            }
            if let Some(h) = conflicting_heads.first() {
                if let Some(e) = view.events.get(h) {
                    resolved_heads.insert(e.parent_head.as_str());
                }
            }
        }
    }
    let mut forked_blocked: BTreeSet<String> = BTreeSet::new();
    for head in view.fork_heads() {
        if resolved_heads.contains(head) {
            continue;
        }
        if let Some(children) = view.children.get(head) {
            for c in children {
                if let Some(env) = view.events.get(c) {
                    forked_blocked.insert(env.attempt_id.clone());
                }
            }
        }
    }

    // 第二遍：应用未被剔除事件。
    let mut proj = FactProjection {
        superseded_attempts,
        forked_blocked_attempts: forked_blocked,
        ..Default::default()
    };
    for env in &order {
        if superseded_events.contains(&env.event_id) {
            continue;
        }
        apply(&mut proj, env);
    }
    proj.invalid_resolutions = invalid_resolutions;
    Projection::Facts(proj)
}

/// 校验一条 ForkResolved：winner ∈ 冲突集、冲突集确为同一 parent 的真实分叉、
/// payload 闭包与 DAG 复算完全相等（不删无关事实、不漏败方后代）。
/// 成功返回败方事件闭包（含败方 head 自身）。
fn validate_resolution(
    view: &DagView,
    conflicting_heads: &[String],
    winner: &str,
    superseded_descendants: &[String],
    resolver_event_id: &str,
) -> Result<BTreeSet<String>, ()> {
    if conflicting_heads.len() < 2 || !conflicting_heads.contains(&winner.to_string()) {
        return Err(());
    }
    let parents: BTreeSet<&str> = conflicting_heads
        .iter()
        .filter_map(|h| view.events.get(h).map(|e| e.parent_head.as_str()))
        .collect();
    if parents.len() != 1 {
        return Err(());
    }
    let parent = parents.into_iter().next().ok_or(())?;
    let actual_children: BTreeSet<&str> = view
        .children
        .get(parent)
        .map(|v| v.iter().map(|s| s.as_str()).collect())
        .unwrap_or_default();
    if conflicting_heads
        .iter()
        .any(|h| !actual_children.contains(h.as_str()))
    {
        return Err(());
    }
    let losers: Vec<String> = conflicting_heads
        .iter()
        .filter(|h| h.as_str() != winner)
        .cloned()
        .collect();
    let computed = view.descendants(&losers);
    let claimed: BTreeSet<String> = superseded_descendants.iter().cloned().collect();
    if computed != claimed {
        return Err(());
    }
    if computed.contains(resolver_event_id) {
        return Err(());
    }
    Ok(computed)
}

fn apply(proj: &mut FactProjection, env: &Envelope) {
    if proj.superseded_attempts.contains(&env.attempt_id) {
        return;
    }
    match &env.payload {
        Payload::AttemptStarted { gate } => {
            proj.attempts
                .entry(env.attempt_id.clone())
                .or_insert_with(|| AttemptFact {
                    gate: gate.clone(),
                    state: "running".into(),
                    transitions: Vec::new(),
                });
        }
        Payload::StageTransition { to, .. } => {
            if let Some(a) = proj.attempts.get_mut(&env.attempt_id) {
                a.state = to.clone();
                a.transitions.push(to.clone());
            }
        }
        Payload::ReleaseDecided {
            digest,
            decision,
            reviewer,
            trust,
            ..
        } => {
            proj.release_claims.push(ReleaseClaim {
                event_id: env.event_id.clone(),
                attempt_id: env.attempt_id.clone(),
                digest: digest.clone(),
                decision: *decision,
                reviewer: reviewer.clone(),
                trust: *trust,
                status: "release_decided_claimed",
            });
        }
        Payload::Supersede { target_attempt_id } => {
            proj.superseded_attempts.insert(target_attempt_id.clone());
        }
        Payload::ForkResolved { .. }
        | Payload::OutputFrozen { .. }
        | Payload::ReleaseRequested { .. }
        | Payload::PlanFrozen { .. } => {}
    }
}

// --- 有效治理投影（模式化谓词，C3 r5）---

/// effective(local-only) = 事实 + local-trust 声明；
/// effective(repository-backed) = 事实 + 发布见证 + 协作者/门禁级信任证明。
/// 谓词不满足 → false（fail-closed，不产出 passed）。
///
/// `release_event` 为该放行的 release.decided 事件本体——证明是验证层输入，
/// 不因落在事实层而采信，必须独立满足来源可验证且绑定到该事件（C4 r3）。
pub fn effective_passed(
    mode: Mode,
    proj: &FactProjection,
    release_event: &Envelope,
    witnesses: &[PublicationWitness],
) -> Result<bool, Error> {
    let event_id = &release_event.event_id;
    let Payload::ReleaseDecided { proof, trust, .. } = &release_event.payload else {
        return Err(Error::BadEventId(event_id.clone()));
    };
    let Some(claim) = proj.release_claims.iter().find(|c| c.event_id == *event_id) else {
        return Err(Error::BadEventId(event_id.clone()));
    };
    if proj.superseded_attempts.contains(&claim.attempt_id)
        || proj.forked_blocked_attempts.contains(&claim.attempt_id)
        || claim.decision != Decision::Approved
    {
        return Ok(false);
    }
    match mode {
        Mode::LocalOnly => Ok(*trust == TrustLevel::Local),
        Mode::RepositoryBacked => {
            if publication_state(witnesses, event_id) != PublicationState::Published {
                return Ok(false);
            }
            let Some(p) = proof else { return Ok(false) };
            Ok(p.verified
                && matches!(p.trust, TrustLevel::Collaborator | TrustLevel::Gate)
                && p.trust == claim.trust
                && p.subject_event_id == *event_id
                && !p.protected_ref.is_empty()
                && !p.commit_sha.is_empty())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{dag::DagView, GovernanceProof, ProofSource, ROOT_PARENT};

    fn env(num: u128, parent: &str, attempt: &str, payload: Payload) -> Envelope {
        let mut e = Envelope::new("wi", attempt, parent, payload, "t");
        e.event_id = crate::encode_ulid(num);
        e
    }

    fn started(num: u128, parent: &str, attempt: &str) -> Envelope {
        env(
            num,
            parent,
            attempt,
            Payload::AttemptStarted { gate: "dev".into() },
        )
    }

    fn decided(
        num: u128,
        parent: &str,
        attempt: &str,
        trust: TrustLevel,
        proof: Option<GovernanceProof>,
    ) -> Envelope {
        env(
            num,
            parent,
            attempt,
            Payload::ReleaseDecided {
                digest: "d".into(),
                decision: Decision::Approved,
                reviewer: "a@x".into(),
                trust,
                proof,
            },
        )
    }

    fn gate_proof(subject: &str, trust: TrustLevel, verified: bool) -> GovernanceProof {
        GovernanceProof {
            trust,
            verified,
            source: ProofSource::ServerSignedAttestation,
            protected_ref: "refs/heads/main".into(),
            commit_sha: "c0ffee".into(),
            subject_event_id: subject.into(),
        }
    }

    fn view(list: Vec<Envelope>) -> DagView {
        DagView::build(list.into_iter().map(|e| (e.event_id.clone(), e)).collect()).unwrap()
    }

    fn verified(eid: &str) -> PublicationWitness {
        PublicationWitness::Verified {
            event_id: eid.to_string(),
            ref_name: "refs/heads/main".into(),
            commit_sha: "abc".into(),
        }
    }

    #[test]
    fn facts_are_claimed_not_passed_and_deterministic() {
        let a = started(1, ROOT_PARENT, "at1");
        let d = decided(2, &a.event_id, "at1", TrustLevel::Local, None);
        let p1 = fact_projection(&view(vec![a.clone(), d.clone()]));
        let p2 = fact_projection(&view(vec![d, a]));
        let Projection::Facts(f1) = p1 else { panic!() };
        let Projection::Facts(f2) = p2 else { panic!() };
        let s1 = serde_json::to_string(&f1).unwrap();
        let s2 = serde_json::to_string(&f2).unwrap();
        assert_eq!(s1, s2, "任意装载顺序投影逐字节一致（C7-1 判据）");
        assert_eq!(f1.release_claims[0].status, "release_decided_claimed");
    }

    #[test]
    fn audit_trust_never_effective_anywhere() {
        let a = started(1, ROOT_PARENT, "at1");
        let d = decided(2, &a.event_id, "at1", TrustLevel::Audit, None);
        let Projection::Facts(f) = fact_projection(&view(vec![a, d.clone()])) else {
            panic!()
        };
        assert!(!effective_passed(Mode::LocalOnly, &f, &d, &[]).unwrap());
        assert!(
            !effective_passed(Mode::RepositoryBacked, &f, &d, &[verified(&d.event_id)]).unwrap()
        );
    }

    #[test]
    fn effective_local_only_requires_local_trust() {
        let a = started(1, ROOT_PARENT, "at1");
        let d = decided(2, &a.event_id, "at1", TrustLevel::Local, None);
        let Projection::Facts(f) = fact_projection(&view(vec![a, d.clone()])) else {
            panic!()
        };
        assert!(effective_passed(Mode::LocalOnly, &f, &d, &[]).unwrap());
    }

    #[test]
    fn effective_repo_backed_requires_published_and_verified_proof() {
        let a = started(1, ROOT_PARENT, "at1");
        let mut d = decided(2, &a.event_id, "at1", TrustLevel::Gate, None);
        d.payload = Payload::ReleaseDecided {
            digest: "d".into(),
            decision: Decision::Approved,
            reviewer: "a@x".into(),
            trust: TrustLevel::Gate,
            proof: Some(gate_proof(&d.event_id, TrustLevel::Gate, true)),
        };
        let Projection::Facts(f) = fact_projection(&view(vec![a, d.clone()])) else {
            panic!()
        };
        // 断网/无见证 → Unknown → fail-closed（C7-17）
        assert!(!effective_passed(Mode::RepositoryBacked, &f, &d, &[]).unwrap());
        // 已验证见证 → effective
        let w = [verified(&d.event_id)];
        assert!(effective_passed(Mode::RepositoryBacked, &f, &d, &w).unwrap());
        // force-push 撤销 → 阻断（C7-12）
        let w2 = [PublicationWitness::Revoked {
            event_id: d.event_id.clone(),
            reason: "force-push".into(),
        }];
        assert!(!effective_passed(Mode::RepositoryBacked, &f, &d, &w2).unwrap());
    }

    #[test]
    fn forged_or_unbound_proof_not_effective() {
        let w_gen = |eid: &str| [verified(eid)];
        // ① 来源未验证（字段伪造）：
        let a = started(1, ROOT_PARENT, "at1");
        let mut d = decided(2, &a.event_id, "at1", TrustLevel::Gate, None);
        d.payload = Payload::ReleaseDecided {
            digest: "d".into(),
            decision: Decision::Approved,
            reviewer: "a@x".into(),
            trust: TrustLevel::Gate,
            proof: Some(gate_proof(&d.event_id, TrustLevel::Gate, false)),
        };
        let Projection::Facts(f) = fact_projection(&view(vec![a, d.clone()])) else {
            panic!()
        };
        assert!(
            !effective_passed(Mode::RepositoryBacked, &f, &d, &w_gen(&d.event_id)).unwrap(),
            "verified=false 不可采信（C7-11）"
        );
        // ② 证明绑定到别的事件（挪用）：
        let a2 = started(10, ROOT_PARENT, "at2");
        let d2 = decided(
            11,
            &a2.event_id,
            "at2",
            TrustLevel::Gate,
            Some(gate_proof("OTHER", TrustLevel::Gate, true)),
        );
        let Projection::Facts(f2) = fact_projection(&view(vec![a2, d2.clone()])) else {
            panic!()
        };
        assert!(
            !effective_passed(Mode::RepositoryBacked, &f2, &d2, &w_gen(&d2.event_id)).unwrap(),
            "证明未绑定该事件 → 不生效"
        );
    }

    #[test]
    fn local_trust_not_valid_in_repository_backed() {
        let a = started(1, ROOT_PARENT, "at1");
        let d = decided(2, &a.event_id, "at1", TrustLevel::Local, None);
        let Projection::Facts(f) = fact_projection(&view(vec![a, d.clone()])) else {
            panic!()
        };
        assert!(
            !effective_passed(Mode::RepositoryBacked, &f, &d, &[verified(&d.event_id)]).unwrap()
        );
    }

    #[test]
    fn valid_fork_resolution_removes_losers_keeps_facts() {
        let base = started(1, ROOT_PARENT, "at1");
        let b1 = started(10, &base.event_id, "atA");
        let b2 = started(20, &base.event_id, "atB");
        let d1 = decided(30, &b1.event_id, "atA", TrustLevel::Local, None);
        let res = env(
            40,
            &d1.event_id,
            "atA",
            Payload::ForkResolved {
                conflicting_heads: vec![b1.event_id.clone(), b2.event_id.clone()],
                winner: b1.event_id.clone(),
                superseded_descendants: vec![b2.event_id.clone()],
                reason: "B 基线漂移".into(),
            },
        );
        let Projection::Facts(f) = fact_projection(&view(vec![base, b1, b2.clone(), d1, res]))
        else {
            panic!()
        };
        assert!(f.superseded_attempts.contains("atB"), "败方分支被剔除");
        assert!(f.attempts.contains_key("atA"));
        assert!(f.invalid_resolutions.is_empty());
        assert!(
            !f.forked_blocked_attempts.contains("atA"),
            "有效裁决后不再 blocked"
        );
    }

    #[test]
    fn forged_closure_rejected_fork_stays_blocked_no_fact_loss() {
        let base = started(1, ROOT_PARENT, "at1");
        let b1 = started(10, &base.event_id, "atA");
        let b2 = started(20, &base.event_id, "atB");
        let victim = started(35, &b2.event_id, "atC"); // 败方分支后代（计算闭包含它）
        let d1 = decided(30, &b1.event_id, "atA", TrustLevel::Local, None);
        // 伪造：声称闭包 = {b2, d1}——把胜方分支事实 d1 塞进去删掉（payload 与复算不符）。
        let res = env(
            40,
            &d1.event_id,
            "atA",
            Payload::ForkResolved {
                conflicting_heads: vec![b1.event_id.clone(), b2.event_id.clone()],
                winner: b1.event_id.clone(),
                superseded_descendants: vec![b2.event_id.clone(), d1.event_id.clone()],
                reason: "恶意闭包".into(),
            },
        );
        let Projection::Facts(f) =
            fact_projection(&view(vec![base, b1, b2, victim.clone(), d1.clone(), res]))
        else {
            panic!();
        };
        assert_eq!(
            f.invalid_resolutions.len(),
            1,
            "伪造闭包 → 裁决无效（C7-19）"
        );
        assert!(f.attempts.contains_key("atC"), "败方真实后代不被静默删除");
        assert!(
            f.release_claims.iter().any(|c| c.event_id == d1.event_id),
            "胜方分支事实不丢"
        );
        assert!(
            f.forked_blocked_attempts.contains("atA") && f.forked_blocked_attempts.contains("atB"),
            "未裁决分叉保持 blocked"
        );
    }

    #[test]
    fn winner_outside_conflict_set_rejected() {
        let base = started(1, ROOT_PARENT, "at1");
        let b1 = started(10, &base.event_id, "atA");
        let b2 = started(20, &base.event_id, "atB");
        let res = env(
            40,
            &b1.event_id,
            "atA",
            Payload::ForkResolved {
                conflicting_heads: vec![b1.event_id.clone(), b2.event_id.clone()],
                winner: "NOT_IN_SET".into(),
                superseded_descendants: vec![b2.event_id.clone()],
                reason: "越界 winner".into(),
            },
        );
        let Projection::Facts(f) = fact_projection(&view(vec![base, b1, b2, res])) else {
            panic!()
        };
        assert_eq!(f.invalid_resolutions.len(), 1);
    }

    #[test]
    fn integrity_failure_fails_closed_whole_projection() {
        let a = started(1, "GHOST", "at1"); // 未知 parent
        let p = project(
            vec![a]
                .into_iter()
                .map(|e| (e.event_id.clone(), e))
                .collect(),
        );
        assert!(
            matches!(p, Projection::Failed(_)),
            "完整性失败 → 整体 fail-closed，不产半投影"
        );
    }
}
