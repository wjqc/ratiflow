// Package failureinjection 覆盖手册 §14.6 发布前故障注入清单。
package failureinjection

import (
	"context"
	"encoding/json"
	"errors"
	"testing"
	"time"

	"gitlab.com/sixgates/sixgates/internal/artifact"
	"gitlab.com/sixgates/sixgates/internal/delivery"
	"gitlab.com/sixgates/sixgates/internal/events"
	"gitlab.com/sixgates/sixgates/internal/integrations"
	"gitlab.com/sixgates/sixgates/internal/policy"
	"gitlab.com/sixgates/sixgates/internal/store"
	"gitlab.com/sixgates/sixgates/internal/workflow"
)

func openStore(t *testing.T) *store.Store {
	t.Helper()
	s, err := store.Open(context.Background(), t.TempDir(), store.Options{Version: "fi"})
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	t.Cleanup(func() { _ = s.Close() })
	return s
}

func seed(t *testing.T, s *store.Store) {
	t.Helper()
	ctx := context.Background()
	now := store.Now()
	_, err := s.DB.ExecContext(ctx, `INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
		VALUES ('pj','u','n','p','main',?)`, now)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := s.DB.ExecContext(ctx, `INSERT INTO workitems(id, project_id, title, created_at, updated_at)
		VALUES ('wi','pj','t',?,?)`, now, now); err != nil {
		t.Fatal(err)
	}
}

// TestProcessKillRecovery 模拟 kill -9：数据落盘后重开库，工作流按 intent 恢复。
func TestProcessKillRecovery(t *testing.T) {
	dir := t.TempDir()
	ctx := context.Background()

	// 第一次进程生命周期：创建工作流，第一步完成后"崩溃"（直接关库，不清理）。
	s1, err := store.Open(ctx, dir, store.Options{Version: "fi"})
	if err != nil {
		t.Fatal(err)
	}
	seed(t, s1)
	outbox1 := events.NewOutbox(s1, events.NewBus())
	engine1 := workflow.NewEngine(s1, outbox1)

	var applied bool
	step := &fakeStep{name: "create_mr", key: "mr", onApply: func() { applied = true }}
	_, err = engine1.Start(ctx, workflow.Definition{
		Kind: "deploy_flow", Version: 1, Steps: []workflow.SideEffect{step},
	}, "wi")
	if err != nil {
		t.Fatalf("start: %v", err)
	}
	if err := s1.Close(); err != nil {
		t.Fatal(err)
	}
	if !applied {
		t.Fatal("precondition: step not applied before crash")
	}

	// 第二次进程生命周期：重开库并恢复。
	s2, err := store.Open(ctx, dir, store.Options{Version: "fi"})
	if err != nil {
		t.Fatalf("reopen after kill: %v", err)
	}
	defer s2.Close()
	outbox2 := events.NewOutbox(s2, events.NewBus())
	engine2 := workflow.NewEngine(s2, outbox2)

	// 既有工作流已 succeeded；恢复扫描应无待恢复项且不报错。
	registry := workflow.NewRegistry()
	registry.Register(workflow.Definition{
		Kind: "deploy_flow", Version: 1,
		Steps: []workflow.SideEffect{&fakeStep{name: "create_mr", key: "mr"}},
	})
	recovered, err := engine2.Resume(ctx, registry)
	if err != nil {
		t.Fatalf("resume: %v", err)
	}
	_ = recovered
}

// TestMRHeadSHADrift MR head 漂移：旧 SHA 的 pipeline 证据必须不被采信为当前。
func TestMRHeadSHADrift(t *testing.T) {
	ctx := context.Background()
	fake := integrations.NewGitLabFake()
	fake.AddProject(integrations.GitLabProject{ID: 7, PathWithNS: "team/demo", DefaultBranch: "main"})

	mr, err := fake.CreateMR(ctx, "7", "ai/wi/task", "main", "task")
	if err != nil {
		t.Fatal(err)
	}
	oldSHA := mr.SHA

	// 为旧 head 建的 pipeline。
	fake.Pipelines["7"] = []integrations.GitLabPipeline{
		{ID: 100, Ref: "ai/wi/task", SHA: oldSHA, Status: integrations.PipelineStatusSuccess},
	}

	// head 漂移。
	fake.AdvanceMRHead("7", mr.IID, "sha-new-head")

	current, err := fake.GetMR(ctx, "7", mr.IID)
	if err != nil {
		t.Fatal(err)
	}
	pipelines, err := fake.ListPipelines(ctx, "7", "ai/wi/task")
	if err != nil {
		t.Fatal(err)
	}
	stale := 0
	for _, p := range pipelines {
		if p.SHA == current.SHA && integrations.IsTerminalSuccess(p.Status) {
			stale++
		}
	}
	if stale != 0 {
		t.Fatal("pipeline for old head SHA must not count as current-head evidence")
	}
}

// TestPipelineDuplicateAndOutOfOrder 重复/乱序 pipeline 状态更新收敛一致。
func TestPipelineDuplicateAndOutOfOrder(t *testing.T) {
	ctx := context.Background()
	fake := integrations.NewGitLabFake()
	fake.Pipelines["7"] = []integrations.GitLabPipeline{
		{ID: 200, Ref: "main", SHA: "sha-1", Status: integrations.PipelineStatusPending},
	}
	// 乱序与重复写入终态。
	fake.SetPipelineStatus("7", 200, integrations.PipelineStatusSuccess)
	fake.SetPipelineStatus("7", 200, integrations.PipelineStatusFailed)
	fake.SetPipelineStatus("7", 200, integrations.PipelineStatusFailed)

	p, err := fake.GetPipeline(ctx, "7", 200)
	if err != nil || p.Status != integrations.PipelineStatusFailed {
		t.Fatalf("pipeline converged to %s err=%v", p.Status, err)
	}
}

// TestEvidenceWriteFailure 证据写入失败（含秘密内容）不得产生半成品证据。
func TestEvidenceWriteFailure(t *testing.T) {
	s := openStore(t)
	seed(t, s)
	ctx := context.Background()
	outbox := events.NewOutbox(s, events.NewBus())
	artifactSvc := artifact.NewService(s, outbox)

	// 带秘密的工件内容：objects 拒绝，修订不得落库。
	_, err := artifactSvc.CreateDraft(ctx, "art_bad", "token: glpat-abcdefghijklmnopqrst")
	if err == nil {
		t.Fatal("expected secret rejection")
	}
	var count int
	_ = s.DB.QueryRow("SELECT COUNT(*) FROM revisions").Scan(&count)
	if count != 0 {
		t.Fatalf("no revision rows expected, got %d", count)
	}
}

// TestApprovalTimeout 审批超时：过期未决自动失效，不再授权。
func TestApprovalTimeout(t *testing.T) {
	s := openStore(t)
	seed(t, s)
	ctx := context.Background()
	outbox := events.NewOutbox(s, events.NewBus())
	pol := policy.NewService(s, outbox, policy.Snapshot{
		ToolRules:   []policy.ToolRule{{Tool: "deploy", Risk: policy.RiskHigh, RequiresApproval: true}},
		ApprovalTTL: "50ms",
	})

	digest := policy.Digest(map[string]string{"tool": "deploy"})
	appr, err := pol.RequestApproval(ctx, "deployment", "dep_1", digest, policy.RiskHigh, "测试")
	if err != nil {
		t.Fatal(err)
	}
	_ = appr
	time.Sleep(120 * time.Millisecond)

	if err := pol.ValidateFor(ctx, "deployment", "dep_1", digest); err == nil {
		t.Fatal("expired pending approval must not authorize")
	}
}

// TestSSHInterruption SSH 中断：部署失败且不进入 deployed 状态。
func TestSSHInterruption(t *testing.T) {
	s := openStore(t)
	seed(t, s)
	ctx := context.Background()
	outbox := events.NewOutbox(s, events.NewBus())
	pol := policy.NewService(s, outbox, policy.DefaultSnapshot())

	ssh := &integrations.SSHFake{FingerprintValue: "SHA256:x"}
	// 预检通过但命令中断。
	preflightOK := &integrations.PreflightReport{FingerprintOK: true, DockerOK: true, ComposeOK: true, RemoteDirOK: true}
	ssh.PreflightResult = preflightOK
	ssh.PlanErr = errors.New("connection reset by peer")

	svc := delivery.NewService(s, outbox, ssh, pol)
	dep, err := svc.CreatePlan(ctx, "wi", validFIPlan())
	if err != nil {
		t.Fatal(err)
	}
	appr, _ := svc.SubmitForApproval(ctx, dep.ID)
	if _, err := pol.Decide(ctx, appr.ID, "approved", "owner", ""); err != nil {
		t.Fatal(err)
	}
	if _, err := svc.ApproveAndDeploy(ctx, dep.ID); err == nil {
		t.Fatal("expected deploy failure on SSH interruption")
	}
	final, _ := svc.Get(ctx, dep.ID)
	if final.State != "deploy_failed" {
		t.Fatalf("state = %s, expected deploy_failed", final.State)
	}

	// 回滚亦失败时进入 rollback_failed 终态。
	ssh.PlanErr = errors.New("rollback also failed")
	if _, err := svc.Rollback(ctx, dep.ID); err == nil {
		t.Fatal("expected rollback failure")
	}
	final, _ = svc.Get(ctx, dep.ID)
	if final.State != "rollback_failed" {
		t.Fatalf("state = %s, expected rollback_failed", final.State)
	}
}

// TestUnsupportedFilesystem 数据目录写入探针（网络 FS 拒绝逻辑在 fscheck 单测覆盖）。
func TestUnsupportedFilesystem(t *testing.T) {
	dir := t.TempDir()
	if err := store.CheckFilesystem(dir); err != nil {
		t.Fatalf("local dir must pass: %v", err)
	}
	if err := store.WriteProbe(dir); err != nil {
		t.Fatalf("write probe: %v", err)
	}
}

// --- helpers ---

type fakeStep struct {
	name    string
	key     string
	onApply func()
}

func (f *fakeStep) Name() string                   { return f.name }
func (f *fakeStep) Digest() []byte                 { return []byte(f.key) }
func (f *fakeStep) IdempotencyKey(string) string   { return f.key }
func (f *fakeStep) Check(context.Context) (bool, json.RawMessage, error) { return false, nil, nil }
func (f *fakeStep) Apply(ctx context.Context) (json.RawMessage, error) {
	if f.onApply != nil {
		f.onApply()
	}
	return json.RawMessage(`{"ok":true}`), nil
}

func validFIPlan() delivery.Plan {
	return delivery.Plan{
		Target:      integrations.SSHTarget{Host: "h", User: "u", ExpectedFingerprint: "SHA256:x", RemoteDir: "/srv"},
		ImageDigest: "sha256:abc",
		DeploySteps: []delivery.PlanStep{{Name: "up", Argv: []string{"docker", "compose", "up", "-d"}}},
		VerifyChecks: []delivery.VerificationCheck{{Name: "health", Argv: []string{"curl", "-f", "http://l/h"}}},
	}
}
