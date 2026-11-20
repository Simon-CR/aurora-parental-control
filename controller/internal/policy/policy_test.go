package policy

import (
	"path/filepath"
	"testing"
	"time"

	"github.com/Simon-CR/aurora-parental-control/controller/internal/store"
)

func newTestEngine(t *testing.T, initial time.Time) (*Engine, *time.Time) {
	t.Helper()
	s, err := store.Open(filepath.Join(t.TempDir(), "test.db"))
	if err != nil {
		t.Fatalf("Open store: %v", err)
	}
	t.Cleanup(func() { _ = s.Close() })

	now := initial
	loc, err := time.LoadLocation("America/Toronto")
	if err != nil {
		t.Fatalf("LoadLocation: %v", err)
	}
	e := New(s, loc, func() time.Time { return now })
	return e, &now
}

func TestWeekdayWeekendWindowsAndAccounting(t *testing.T) {
	loc, _ := time.LoadLocation("America/Toronto")
	// Friday 2026-11-20 16:00 (inside default 15:30-20:30 weekday window)
	fri := time.Date(2026, 11, 20, 16, 0, 0, 0, loc)
	eng, clk := newTestEngine(t, fri)

	r1, err := eng.Heartbeat("4D5307E6", "title")
	if err != nil {
		t.Fatalf("Heartbeat 1: %v", err)
	}
	if r1.Action != "none" || !r1.WindowOpen || r1.RemainingSec != 90*60 {
		t.Fatalf("unexpected r1: %+v", r1)
	}

	// Advance 10s -> counts 10s.
	*clk = clk.Add(10 * time.Second)
	r2, _ := eng.Heartbeat("4D5307E6", "title")
	if r2.UsedSec != 10 || r2.RemainingSec != 90*60-10 {
		t.Fatalf("expected 10s used, got %+v", r2)
	}

	// Huge gap (5 minutes) clamps cleanly to max 30s.
	*clk = clk.Add(5 * time.Minute)
	r3, _ := eng.Heartbeat("4D5307E6", "title")
	if r3.UsedSec != 40 {
		t.Fatalf("expected delta clamped to 30s (total 40s), got %d", r3.UsedSec)
	}

	// Before weekday window (Friday 10:00 AM -> closed on weekday).
	*clk = time.Date(2026, 11, 20, 10, 0, 0, 0, loc)
	rClosed, _ := eng.Heartbeat("4D5307E6", "title")
	if rClosed.Action != "time_up" || rClosed.WindowOpen {
		t.Fatalf("expected outside window time_up on weekday 10:00, got %+v", rClosed)
	}

	// Saturday 10:00 AM -> open in weekend window (09:00-21:30), fresh daily ledger.
	*clk = time.Date(2026, 11, 21, 10, 0, 0, 0, loc)
	rSat, _ := eng.Heartbeat("4D5307E6", "title")
	if rSat.Action != "none" || !rSat.WindowOpen || rSat.UsedSec != 0 {
		t.Fatalf("expected fresh Saturday open window, got %+v", rSat)
	}
}

func TestWarningsDeduplicationAndBonusReset(t *testing.T) {
	loc, _ := time.LoadLocation("America/Toronto")
	sat := time.Date(2026, 11, 21, 12, 0, 0, 0, loc)
	eng, clk := newTestEngine(t, sat)

	// Set 16 minute daily quota.
	if err := eng.UpdatePolicy(16, "00:00-24:00", "00:00-24:00"); err != nil {
		t.Fatalf("UpdatePolicy: %v", err)
	}

	// Poll every 20s to reach <= 15m (900s) -> consumes 60s on 4th poll.
	_, _ = eng.Heartbeat("4D5307E6", "title")
	var warned bool
	for i := 0; i < 4; i++ {
		*clk = clk.Add(20 * time.Second)
		r, _ := eng.Heartbeat("4D5307E6", "title")
		if r.Action == "toast" {
			warned = true
			if r.Message != "15 minutes of screen time remaining" {
				t.Fatalf("unexpected 15m warning: %q", r.Message)
			}
		}
	}
	if !warned {
		t.Fatalf("expected 15m warning toast")
	}

	// Next poll at same threshold must NOT re-emit 15m warning.
	*clk = clk.Add(5 * time.Second)
	rNext, _ := eng.Heartbeat("4D5307E6", "title")
	if rNext.Action != "none" {
		t.Fatalf("expected suppressed duplicate toast, got %+v", rNext)
	}

	// Simulate approaching 1m left directly via 16m cap + bonus check.
	if err := eng.AddBonus(15); err != nil {
		t.Fatalf("AddBonus: %v", err)
	}
	snap, _ := eng.Snapshot()
	if snap.EffectiveMaxMin != 31 || snap.BonusMinutes != 15 {
		t.Fatalf("unexpected bonus snapshot: %+v", snap)
	}
}

func TestPauseRequestsApprovalAndShutdown(t *testing.T) {
	loc, _ := time.LoadLocation("America/Toronto")
	sat := time.Date(2026, 11, 21, 12, 0, 0, 0, loc)
	eng, clk := newTestEngine(t, sat)

	if err := eng.SetPaused(true); err != nil {
		t.Fatalf("SetPaused: %v", err)
	}
	rp, _ := eng.Heartbeat("4D5307E6", "system")
	if rp.Action != "pause" || !rp.Paused {
		t.Fatalf("expected pause, got %+v", rp)
	}

	if err := eng.RequestResume(); err != nil {
		t.Fatalf("RequestResume: %v", err)
	}
	snap, _ := eng.Snapshot()
	if snap.PendingRequest != "resume" || snap.ReqStatus != "pending" {
		t.Fatalf("expected pending resume, got %+v", snap)
	}

	if err := eng.Approve(); err != nil {
		t.Fatalf("Approve: %v", err)
	}
	*clk = clk.Add(5 * time.Second)
	ra, _ := eng.Heartbeat("4D5307E6", "system")
	if ra.Action != "none" || ra.Paused || ra.ReqStatus != "approved" {
		t.Fatalf("expected unpaused after approve, got %+v", ra)
	}

	if err := eng.RequestTime(15); err != nil {
		t.Fatalf("RequestTime: %v", err)
	}
	if err := eng.Deny(); err != nil {
		t.Fatalf("Deny: %v", err)
	}
	rd, _ := eng.Heartbeat("4D5307E6", "system")
	if rd.ReqStatus != "denied" {
		t.Fatalf("expected denied, got %+v", rd)
	}

	if err := eng.TriggerShutdown(); err != nil {
		t.Fatalf("TriggerShutdown: %v", err)
	}
	rs1, _ := eng.Heartbeat("4D5307E6", "system")
	rs2, _ := eng.Heartbeat("4D5307E6", "system")
	if rs1.Action != "shutdown" || rs2.Action != "none" {
		t.Fatalf("expected one-shot shutdown, got rs1=%+v rs2=%+v", rs1, rs2)
	}
}
