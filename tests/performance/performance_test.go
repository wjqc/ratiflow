// Package performance 提供关键路径基准（CI 中以 -bench 运行，阈值守门在运行手册中约定）。
package performance

import (
	"context"
	"strings"
	"testing"

	"gitlab.com/sixgates/sixgates/internal/events"
	"gitlab.com/sixgates/sixgates/internal/gate"
	"gitlab.com/sixgates/sixgates/internal/scan"
	"gitlab.com/sixgates/sixgates/internal/store"
)

var sinkInt int

func BenchmarkGateEvaluate(b *testing.B) {
	inputs := gate.Inputs{
		WorkItemID: "wi", Gate: "testing",
		RequiredArtifactsFrozen: gate.InputPass,
		RequiredChecksPassed:    gate.InputPass,
		ApprovalsValid:          gate.InputPass,
		EvidenceComplete:        gate.InputPass,
		NoBlockingRisk:          gate.InputPass,
		InputsCurrent:           gate.InputPass,
	}
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		result := gate.Evaluate(inputs)
		sinkInt += len(result.FailedInputs)
	}
}

func BenchmarkScanMask(b *testing.B) {
	body := strings.Repeat("module demo\n\nconst password = supersecretvalue123\n", 200)
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		_, count := scan.Mask([]byte(body))
		sinkInt += count
	}
}

func BenchmarkOutboxEmit(b *testing.B) {
	ctx := context.Background()
	s, err := store.Open(ctx, b.TempDir(), store.Options{Version: "bench"})
	if err != nil {
		b.Fatal(err)
	}
	defer s.Close()
	outbox := events.NewOutbox(s, events.NewBus())
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		if _, err := outbox.Emit(ctx, "bench", "agg", "tick", i); err != nil {
			b.Fatal(err)
		}
	}
}

func BenchmarkOutboxReplay(b *testing.B) {
	ctx := context.Background()
	s, err := store.Open(ctx, b.TempDir(), store.Options{Version: "bench"})
	if err != nil {
		b.Fatal(err)
	}
	defer s.Close()
	outbox := events.NewOutbox(s, events.NewBus())
	for i := 0; i < 1000; i++ {
		if _, err := outbox.Emit(ctx, "bench", "agg", "tick", i); err != nil {
			b.Fatal(err)
		}
	}
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		events, err := outbox.Replay(ctx, int64(i%1000), 100)
		if err != nil {
			b.Fatal(err)
		}
		sinkInt += len(events)
	}
}
