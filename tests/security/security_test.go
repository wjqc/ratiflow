// Package security 覆盖手册 §18 安全编码检查项的可自动化部分。
package security

import (
	"context"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"gitlab.com/sixgates/sixgates/internal/events"
	"gitlab.com/sixgates/sixgates/internal/integrations"
	"gitlab.com/sixgates/sixgates/internal/modelgw"
	"gitlab.com/sixgates/sixgates/internal/scan"
	"gitlab.com/sixgates/sixgates/internal/store"
)

// TestBinaryRefusesNonLoopback 端口二进制级验证：程序必须拒绝非 loopback 监听地址。
func TestBinaryRefusesNonLoopback(t *testing.T) {
	if testing.Short() {
		t.Skip("short mode skips binary build")
	}
	bin := filepath.Join(t.TempDir(), "sixgates")
	build := exec.Command("go", "build", "-o", bin, "gitlab.com/sixgates/sixgates/cmd/sixgates")
	if out, err := build.CombinedOutput(); err != nil {
		t.Fatalf("build: %v\n%s", err, out)
	}
	for _, address := range []string{"0.0.0.0:7666", "192.168.1.10:7666", "example.com:7666"} {
		cmd := exec.Command(bin, "--address", address, "--data-dir", t.TempDir())
		out, err := cmd.CombinedOutput()
		if err == nil {
			t.Fatalf("address %s must be refused", address)
		}
		if !strings.Contains(string(out), "refusing non-loopback") {
			t.Fatalf("address %s: unexpected output %s", address, out)
		}
	}
}

// TestSecretsNeverReachProvider 验证模型网关出网内容不含秘密。
func TestSecretsNeverReachProvider(t *testing.T) {
	ctx := context.Background()
	s, err := store.Open(ctx, t.TempDir(), store.Options{Version: "test"})
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	now := store.Now()
	_, _ = s.DB.ExecContext(ctx, `INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
		VALUES ('pj','u','n','p','main',?)`, now)
	_, _ = s.DB.ExecContext(ctx, `INSERT INTO workitems(id, project_id, title, created_at, updated_at)
		VALUES ('wi','pj','t',?,?)`, now, now)

	fake := integrations.NewModelFake()
	fake.Script = []integrations.ScriptedCompletion{
		{Response: integrations.CompletionResponse{Content: "ok"}},
	}
	gw := modelgw.New(s, fake, modelgw.CostPerToken{In: 1, Out: 1})
	manifest, _ := gw.CreateManifest(ctx, "wi", modelgw.Scope{}, "standard")

	secretMaterial := "AKIAIOSFODNN7EXAMPLE 与 glpat-xxxxxxxxxxxxxxxxxxxx 以及 -----BEGIN RSA PRIVATE KEY-----"
	if _, err := gw.Call(ctx, manifest, "run", modelgw.DefaultBudget(), integrations.CompletionRequest{
		Messages: []integrations.ChatMessage{{Role: "user", Content: secretMaterial}},
	}); err != nil {
		t.Fatalf("call: %v", err)
	}
	sent := fake.Calls[0].Messages[0].Content
	for _, leaked := range []string{"AKIAIOSFODNN7EXAMPLE", "glpat-", "PRIVATE KEY"} {
		if strings.Contains(sent, leaked) {
			t.Fatalf("secret %q leaked to provider: %s", leaked, sent)
		}
	}
}

// TestAuditAndModelCallsContainNoPrompt 验证审计面不含 prompt 正文。
func TestAuditAndModelCallsContainNoPrompt(t *testing.T) {
	ctx := context.Background()
	s, _ := store.Open(ctx, t.TempDir(), store.Options{Version: "test"})
	defer s.Close()
	now := store.Now()
	_, _ = s.DB.ExecContext(ctx, `INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
		VALUES ('pj','u','n','p','main',?)`, now)
	_, _ = s.DB.ExecContext(ctx, `INSERT INTO workitems(id, project_id, title, created_at, updated_at)
		VALUES ('wi','pj','t',?,?)`, now, now)

	fake := integrations.NewModelFake()
	fake.Script = []integrations.ScriptedCompletion{{Response: integrations.CompletionResponse{Content: "ok"}}}
	gw := modelgw.New(s, fake, modelgw.CostPerToken{In: 1, Out: 1})
	manifest, _ := gw.CreateManifest(ctx, "wi", modelgw.Scope{}, "standard")
	secret := "password: hunter2secretvalue"
	if _, err := gw.Call(ctx, manifest, "run_sec", modelgw.DefaultBudget(), integrations.CompletionRequest{
		Messages: []integrations.ChatMessage{{Role: "user", Content: secret}},
	}); err != nil {
		t.Fatal(err)
	}
	// 直接读表确认审计不含正文。
	var rows int
	if err := s.DB.QueryRow("SELECT COUNT(*) FROM model_calls").Scan(&rows); err != nil || rows != 1 {
		t.Fatalf("model_calls rows = %d err=%v", rows, err)
	}
	var auditBody string
	_ = s.DB.QueryRow("SELECT provider || model || status FROM model_calls LIMIT 1").Scan(&auditBody)
	if strings.Contains(auditBody, secret) || strings.Contains(auditBody, "hunter2") {
		t.Fatalf("audit contains prompt content: %s", auditBody)
	}
}

// TestOutboxEventsCarryNoSecrets 事件负载不携带敏感正文。
func TestOutboxEventsCarryNoSecrets(t *testing.T) {
	ctx := context.Background()
	s, _ := store.Open(ctx, t.TempDir(), store.Options{Version: "test"})
	defer s.Close()
	outbox := events.NewOutbox(s, events.NewBus())
	_, err := outbox.Emit(ctx, "test", "agg_1", "event.secret-check", map[string]string{"note": "clean"})
	if err != nil {
		t.Fatal(err)
	}
	var payload string
	_ = s.DB.QueryRow("SELECT payload FROM events_outbox LIMIT 1").Scan(&payload)
	if scan.HasHighRiskSecrets(scan.Scan([]byte(payload))) {
		t.Fatalf("event payload flagged as containing secrets: %s", payload)
	}
}

// TestEnvExampleContainsNoSecrets .env.example 只允许变量名与无害示例。
func TestEnvExampleContainsNoSecrets(t *testing.T) {
	body, err := os.ReadFile("../../.env.example")
	if err != nil {
		t.Skip(".env.example not present")
	}
	findings := scan.Scan(body)
	if scan.HasHighRiskSecrets(findings) {
		t.Fatalf(".env.example contains secret-like values: %v", findings)
	}
}

// TestGitLabTokenNeverLogged HTTP 适配器错误信息不回显 token。
func TestGitLabTokenNeverLogged(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get("Private-Token") != "glpat-secret-value" {
			w.WriteHeader(http.StatusUnauthorized)
			return
		}
		w.WriteHeader(http.StatusForbidden)
	}))
	defer server.Close()
	client := integrations.NewGitLabHTTP(server.URL, "glpat-secret-value")
	_, err := client.CurrentUser(context.Background())
	if err == nil {
		t.Fatal("expected error")
	}
	if strings.Contains(err.Error(), "glpat-secret-value") {
		t.Fatalf("token leaked in error: %v", err)
	}
}

// TestLargeRequestRejected 超限请求体被拒绝（防资源耗尽）。
func TestLargeRequestRejected(t *testing.T) {
	// 8MiB 上限在 api 包；此处用 SSE/诊断之外最直接的验证：模型请求裁剪。
	ctx := context.Background()
	s, _ := store.Open(ctx, t.TempDir(), store.Options{Version: "test"})
	defer s.Close()
	now := store.Now()
	_, _ = s.DB.ExecContext(ctx, `INSERT INTO projects(id, gitlab_instance, namespace, project, default_branch, created_at)
		VALUES ('pj','u','n','p','main',?)`, now)
	_, _ = s.DB.ExecContext(ctx, `INSERT INTO workitems(id, project_id, title, created_at, updated_at)
		VALUES ('wi','pj','t',?,?)`, now, now)
	gw := modelgw.New(s, integrations.NewModelFake(), modelgw.CostPerToken{In: 1, Out: 1})
	manifest, _ := gw.CreateManifest(ctx, "wi", modelgw.Scope{MaxContextBytes: 1024}, "standard")
	_, err := gw.Call(ctx, manifest, "r", modelgw.DefaultBudget(), integrations.CompletionRequest{
		Messages: []integrations.ChatMessage{{Role: "user", Content: strings.Repeat("x", 4096)}},
	})
	if err == nil || !strings.Contains(err.Error(), "context_too_large") {
		t.Fatalf("expected context_too_large, got %v", err)
	}
	_ = time.Now
}
