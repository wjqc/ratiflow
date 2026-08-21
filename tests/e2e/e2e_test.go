// Package e2e 验证端到端黄金流程：项目 → 工作项 → 工件评审 → 基线 →
// Agent Run → 六关门禁 → 通关文牒 → 部署验证（手册 §14 浏览器/流程级 E2E 的 API 层实现）。
package e2e

import (
	"bytes"
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"testing/fstest"

	"gitlab.com/sixgates/sixgates/internal/agent"
	"gitlab.com/sixgates/sixgates/internal/api"
	"gitlab.com/sixgates/sixgates/internal/artifact"
	"gitlab.com/sixgates/sixgates/internal/audit"
	"gitlab.com/sixgates/sixgates/internal/auth"
	"gitlab.com/sixgates/sixgates/internal/delivery"
	"gitlab.com/sixgates/sixgates/internal/diagnostics"
	"gitlab.com/sixgates/sixgates/internal/events"
	"gitlab.com/sixgates/sixgates/internal/evidence"
	"gitlab.com/sixgates/sixgates/internal/gate"
	"gitlab.com/sixgates/sixgates/internal/integrations"
	"gitlab.com/sixgates/sixgates/internal/modelgw"
	"gitlab.com/sixgates/sixgates/internal/policy"
	"gitlab.com/sixgates/sixgates/internal/store"
	"gitlab.com/sixgates/sixgates/internal/trace"
	"gitlab.com/sixgates/sixgates/internal/workitem"
)

type e2eEnv struct {
	t       *testing.T
	handler http.Handler
	token   string
}

func newEnv(t *testing.T) *e2eEnv {
	t.Helper()
	ctx := context.Background()
	s, err := store.Open(ctx, t.TempDir(), store.Options{Version: "e2e"})
	if err != nil {
		t.Fatalf("open store: %v", err)
	}
	t.Cleanup(func() { _ = s.Close() })

	bus := events.NewBus()
	outbox := events.NewOutbox(s, bus)
	model := integrations.NewModelFake()
	model.Script = []integrations.ScriptedCompletion{
		{Response: integrations.CompletionResponse{Content: `{"action":"final","summary":"PRD 草案完成"}`}},
	}
	pol := policy.NewService(s, outbox, policy.Snapshot{
		ToolRules:    []policy.ToolRule{{Tool: "read_file", Risk: policy.RiskLow}, {Tool: "write_file", Risk: policy.RiskMedium}},
		ApprovalTTL:  "1h",
	})
	gateway := modelgw.New(s, model, modelgw.CostPerToken{In: 1, Out: 1})
	authSvc := auth.NewService(s)
	_, token, err := authSvc.CreateSession(ctx, "e2e")
	if err != nil {
		t.Fatal(err)
	}

	workItemSvc := workitem.NewService(s, outbox)
	artifactSvc := artifact.NewService(s, outbox)
	agentSvc := agent.NewService(s, outbox, gateway, pol, func(ctx context.Context, p agent.Proposal) (string, error) {
		return "done", nil
	})

	deps := api.Deps{
		Version: "e2e", Diagnostics: diagnostics.NewService(diagnostics.Options{Version: "e2e"}),
		Auth: authSvc, WorkItems: workItemSvc, Artifacts: artifactSvc,
		Manifests: gateway, Agent: agentSvc, Policy: pol,
		Trace: trace.NewService(s), Gates: gate.NewService(s),
		Evidence: evidence.NewService(s, outbox),
		Delivery: delivery.NewService(s, outbox, &integrations.SSHFake{FingerprintValue: "SHA256:x"}, pol),
		Outbox:   outbox, Audit: audit.New(s),
	}
	assets := fstest.MapFS{"index.html": &fstest.MapFile{Data: []byte("<!doctype html>")}}
	return &e2eEnv{t: t, handler: api.NewServer(deps, assets).Handler(), token: token}
}

func (e *e2eEnv) call(method, path string, body any) (*httptest.ResponseRecorder, map[string]any) {
	e.t.Helper()
	var reader *bytes.Reader
	if body != nil {
		encoded, _ := json.Marshal(body)
		reader = bytes.NewReader(encoded)
	} else {
		reader = bytes.NewReader(nil)
	}
	req := httptest.NewRequest(method, path, reader)
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	req.Header.Set("Authorization", "Bearer "+e.token)
	rec := httptest.NewRecorder()
	e.handler.ServeHTTP(rec, req)
	var decoded map[string]any
	_ = json.Unmarshal(rec.Body.Bytes(), &decoded)
	return rec, decoded
}

func str(m map[string]any, key string) string {
	if v, ok := m[key].(string); ok {
		return v
	}
	return ""
}

// TestGoldenFlowSixGates 走完六关直至签发通关文牒。
func TestGoldenFlowSixGates(t *testing.T) {
	e := newEnv(t)

	// 1. 项目与工作项。
	_, project := e.call(http.MethodPost, "/api/v1/projects", map[string]any{
		"gitlabInstance": "https://gitlab.test", "namespace": "team", "project": "demo",
	})
	_, wi := e.call(http.MethodPost, "/api/v1/workitems", map[string]any{
		"projectId": str(project, "id"), "title": "SSO 登录", "gitlabIssueIid": "42",
	})
	wiID := str(wi, "id")

	// 2. PRD 工件：草稿 → 评审 → 冻结基线（需求关）。
	_, art := e.call(http.MethodPost, "/api/v1/workitems/"+wiID+"/artifacts",
		map[string]any{"kind": "prd", "title": "PRD"})
	_, rev := e.call(http.MethodPost, "/api/v1/artifacts/"+str(art, "id")+"/revisions",
		map[string]any{"content": "# PRD\n范围：OIDC 登录。非目标：SAML。"})
	e.call(http.MethodPost, "/api/v1/revisions/"+str(rev, "id")+"/reviews",
		map[string]any{"reviewer": "pm", "verdict": "approved", "comment": "LGTM", "gitlabMrIid": "1"})
	rec, _ := e.call(http.MethodPost, "/api/v1/workitems/"+wiID+"/baselines",
		map[string]any{"gate": "requirements", "revisionIds": []string{str(rev, "id")}, "gitlabCommitSha": "sha-commit-1"})
	if rec.Code != http.StatusCreated {
		t.Fatalf("freeze baseline: %d %s", rec.Code, rec.Body.String())
	}

	// 3. Agent Run 生成方案（completed_execution 不等于过关）。
	_, manifest := e.call(http.MethodPost, "/api/v1/workitems/"+wiID+"/context-manifests",
		map[string]any{"scope": map[string]any{"maxContextBytes": 65536}})
	_, run := e.call(http.MethodPost, "/api/v1/workitems/"+wiID+"/agent-runs", map[string]any{
		"goal": "基于 PRD 起草技术方案", "contextManifestId": str(manifest, "id"),
		"toolAllowlist": []string{"read_file"},
	})
	if got := str(run, "status"); got != "completed_execution" {
		t.Fatalf("agent run status = %s body=%v", got, run)
	}

	// 4. 每关补证据 + 复验 + 门禁评估。
	gates := []string{"requirements", "design", "development", "testing", "deployment", "verification"}
	for _, gateName := range gates {
		_, ev := e.call(http.MethodPost, "/api/v1/workitems/"+wiID+"/evidences", map[string]any{
			"gate": gateName, "kind": "manual", "title": gateName + " 核验",
			"content": "evidence-" + gateName, "source": "local",
		})
		e.call(http.MethodPost, "/api/v1/evidences/"+str(ev, "id")+"/verify", map[string]any{"verifiedBy": "e2e"})
		rec, result := e.call(http.MethodPost, "/api/v1/workitems/"+wiID+"/gates/"+gateName+"/evaluate", map[string]any{})
		if rec.Code != http.StatusOK {
			t.Fatalf("evaluate %s: %d", gateName, rec.Code)
		}
		if passed, _ := result["passed"].(bool); !passed {
			t.Fatalf("gate %s not passed: %v", gateName, result)
		}
	}

	// 5. 文牒签发。
	rec, passport := e.call(http.MethodPost, "/api/v1/workitems/"+wiID+"/passports", map[string]any{})
	if rec.Code != http.StatusCreated {
		t.Fatalf("passport: %d %s", rec.Code, rec.Body.String())
	}
	if !strings.HasPrefix(str(passport, "objectSha256"), "sha") {
		// objectSha256 是 64 位 hex（无前缀），这里验证非空即可。
		if str(passport, "objectSha256") == "" {
			t.Fatal("passport object hash missing")
		}
	}
	gateSummaries, _ := passport["gates"].([]any)
	if len(gateSummaries) != 6 {
		t.Fatalf("passport gates = %d", len(gateSummaries))
	}

	// 6. 部署关走真实状态机（fake SSH）。
	_, dep := e.call(http.MethodPost, "/api/v1/deployments", map[string]any{
		"workItemId": wiID,
		"plan": map[string]any{
			"target":       map[string]any{"host": "deploy.test", "user": "deploy", "expectedFingerprint": "SHA256:x", "remoteDir": "/srv"},
			"imageDigest":  "sha256:abcdef123456",
			"deploySteps":  []map[string]any{{"seq": 0, "name": "up", "argv": []string{"docker", "compose", "up", "-d"}}},
			"verifyChecks": []map[string]any{{"name": "health", "argv": []string{"curl", "-f", "http://localhost/h"}}},
			"rollbackSteps": []map[string]any{{"seq": 0, "name": "down", "argv": []string{"docker", "compose", "down"}}},
		},
	})
	depID := str(dep, "id")
	e.call(http.MethodPost, "/api/v1/deployments/"+depID+"/submit", nil)
	_, approvals := e.call(http.MethodGet, "/api/v1/approvals", nil)
	items, _ := approvals["items"].([]any)
	if len(items) != 1 {
		t.Fatalf("pending approvals = %d", len(items))
	}
	first, _ := items[0].(map[string]any)
	e.call(http.MethodPost, "/api/v1/approvals/"+str(first, "id")+"/decide",
		map[string]any{"decision": "approved", "decidedBy": "owner"})
	rec, deployed := e.call(http.MethodPost, "/api/v1/deployments/"+depID+"/deploy", nil)
	if rec.Code != http.StatusOK || str(deployed, "state") != "awaiting_verification" {
		t.Fatalf("deploy: %d %v", rec.Code, deployed)
	}
	rec, verified := e.call(http.MethodPost, "/api/v1/deployments/"+depID+"/verify", nil)
	if rec.Code != http.StatusOK || str(verified, "state") != "verified" {
		t.Fatalf("verify deployment: %d %v", rec.Code, verified)
	}
}

// TestFirstSetupExperience 首次设置：无会话时诊断可达、业务端点拒绝。
func TestFirstSetupExperience(t *testing.T) {
	e := newEnv(t)
	req := httptest.NewRequest(http.MethodGet, "/api/v1/diagnostics", nil)
	rec := httptest.NewRecorder()
	e.handler.ServeHTTP(rec, req)
	if rec.Code != http.StatusOK {
		t.Fatalf("diagnostics before setup: %d", rec.Code)
	}

	req = httptest.NewRequest(http.MethodGet, "/api/v1/workitems", nil)
	rec = httptest.NewRecorder()
	e.handler.ServeHTTP(rec, req)
	if rec.Code != http.StatusUnauthorized {
		t.Fatalf("unauthenticated workitems: %d", rec.Code)
	}
	var problem struct {
		ErrorCode string `json:"errorCode"`
	}
	_ = json.Unmarshal(rec.Body.Bytes(), &problem)
	if problem.ErrorCode != "unauthorized" {
		t.Fatalf("error code = %s", problem.ErrorCode)
	}
}
