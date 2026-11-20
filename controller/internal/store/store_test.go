package store

import (
	"path/filepath"
	"testing"
	"time"
)

func TestStorePersistenceAndEvents(t *testing.T) {
	dir := t.TempDir()
	dbPath := filepath.Join(dir, "guard.db")

	s, err := Open(dbPath)
	if err != nil {
		t.Fatalf("Open failed: %v", err)
	}
	defer s.Close()

	p, err := s.GetPolicy()
	if err != nil {
		t.Fatalf("GetPolicy failed: %v", err)
	}
	if p.DailyMaxMinutes != 90 || p.WeekdayStart != "15:30" || p.WeekendEnd != "21:30" {
		t.Fatalf("unexpected default policy: %+v", p)
	}

	p.DailyMaxMinutes = 120
	p.Paused = true
	if err := s.SavePolicy(p); err != nil {
		t.Fatalf("SavePolicy failed: %v", err)
	}

	u, err := s.GetOrCreateUsage("2026-11-20")
	if err != nil {
		t.Fatalf("GetOrCreateUsage failed: %v", err)
	}
	u.UsedSec = 1800
	u.BonusMinutes = 15
	u.Warned15m = true
	if err := s.SaveUsage(u); err != nil {
		t.Fatalf("SaveUsage failed: %v", err)
	}

	rt := RuntimeState{
		LastHeartbeatAt: time.Now().UTC().Format(time.RFC3339),
		LastTitleID:     "4D5307E6",
		LastMode:        "title",
		PendingShutdown: true,
		ReqKind:         "more_time",
		ReqMinutes:      15,
		ReqStatus:       "pending",
		ReqUpdatedAt:    time.Now().UTC().Format(time.RFC3339),
	}
	if err := s.SaveRuntime(rt); err != nil {
		t.Fatalf("SaveRuntime failed: %v", err)
	}

	if err := s.AddEvent(time.Now().UTC(), "more_time_requested", "Asked +15m"); err != nil {
		t.Fatalf("AddEvent failed: %v", err)
	}

	// Re-open to verify persistence on disk.
	_ = s.Close()
	s2, err := Open(dbPath)
	if err != nil {
		t.Fatalf("re-Open failed: %v", err)
	}
	defer s2.Close()

	p2, err := s2.GetPolicy()
	if err != nil || p2.DailyMaxMinutes != 120 || !p2.Paused {
		t.Fatalf("reloaded policy mismatch: %+v err=%v", p2, err)
	}
	u2, err := s2.GetOrCreateUsage("2026-11-20")
	if err != nil || u2.UsedSec != 1800 || u2.BonusMinutes != 15 || !u2.Warned15m {
		t.Fatalf("reloaded usage mismatch: %+v err=%v", u2, err)
	}
	rt2, err := s2.GetRuntime()
	if err != nil || rt2.LastTitleID != "4D5307E6" || !rt2.PendingShutdown || rt2.ReqStatus != "pending" {
		t.Fatalf("reloaded runtime mismatch: %+v err=%v", rt2, err)
	}
	evts, err := s2.ListEvents(10)
	if err != nil || len(evts) != 1 || evts[0].Kind != "more_time_requested" {
		t.Fatalf("reloaded events mismatch: %+v err=%v", evts, err)
	}
}
