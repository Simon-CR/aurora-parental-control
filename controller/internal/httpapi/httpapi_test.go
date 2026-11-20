package httpapi

import (
	"bytes"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/Simon-CR/aurora-parental-control/controller/internal/policy"
	"github.com/Simon-CR/aurora-parental-control/controller/internal/store"
)

func setupHandler(t *testing.T) http.Handler {
	t.Helper()
	s, err := store.Open(filepath.Join(t.TempDir(), "http.db"))
	if err != nil {
		t.Fatalf("Open store: %v", err)
	}
	t.Cleanup(func() { _ = s.Close() })
	loc, _ := time.LoadLocation("America/Toronto")
	clk := time.Date(2026, 11, 21, 12, 0, 0, 0, loc)
	eng := policy.New(s, loc, func() time.Time { return clk })
	return NewMux(eng)
}

func TestXboxFlatAndJSONProtocolEndpoints(t *testing.T) {
	mux := setupHandler(t)

	// 1. Flat key=value response for Xbox 360 C/Rust client.
	req := httptest.NewRequest(http.MethodGet, "/api/v1/xbox/heartbeat?title_id=4D5307E6&mode=title", nil)
	rec := httptest.NewRecorder()
	mux.ServeHTTP(rec, req)
	if rec.Code != http.StatusOK {
		t.Fatalf("expected 200, got %d", rec.Code)
	}
	body := rec.Body.String()
	for _, want := range []string{"action=none\n", "remaining_sec=5400\n", "window_open=1\n", "paused=0\n", "req_status=none\n"} {
		if !strings.Contains(body, want) {
			t.Fatalf("flat body missing %q:\n%s", want, body)
		}
	}

	// 2. JSON response when Accept: application/json is passed.
	reqJ := httptest.NewRequest(http.MethodGet, "/api/v1/xbox/heartbeat?title_id=4D5307E6&mode=title", nil)
	reqJ.Header.Set("Accept", "application/json")
	recJ := httptest.NewRecorder()
	mux.ServeHTTP(recJ, reqJ)
	var hb policy.HeartbeatResult
	if err := json.Unmarshal(recJ.Body.Bytes(), &hb); err != nil {
		t.Fatalf("unmarshal heartbeat json: %v", err)
	}
	if hb.DailyMaxSec != 5400 || !hb.WindowOpen {
		t.Fatalf("unexpected json heartbeat: %+v", hb)
	}

	// 3. Request +15m -> Approve via Web API -> check bonus minutes.
	reqT := httptest.NewRequest(http.MethodPost, "/api/v1/xbox/request-time?minutes=15", nil)
	recT := httptest.NewRecorder()
	mux.ServeHTTP(recT, reqT)
	if recT.Code != http.StatusOK || !strings.Contains(recT.Body.String(), "req_status=pending") {
		t.Fatalf("request-time failed: code=%d body=%s", recT.Code, recT.Body.String())
	}

	reqA := httptest.NewRequest(http.MethodPost, "/api/v1/action", bytes.NewBufferString(`{"action":"approve"}`))
	recA := httptest.NewRecorder()
	mux.ServeHTTP(recA, reqA)
	if recA.Code != http.StatusOK {
		t.Fatalf("action approve failed: %d", recA.Code)
	}

	recS := httptest.NewRecorder()
	mux.ServeHTTP(recS, httptest.NewRequest(http.MethodGet, "/api/v1/status", nil))
	var snap policy.Snapshot
	if err := json.Unmarshal(recS.Body.Bytes(), &snap); err != nil {
		t.Fatalf("status unmarshal: %v", err)
	}
	if snap.BonusMinutes != 15 || snap.PendingRequest != "none" {
		t.Fatalf("unexpected snap after approve: %+v", snap)
	}

	// 4. Embedded Grounded-Earth UI served cleanly at /.
	recUI := httptest.NewRecorder()
	mux.ServeHTTP(recUI, httptest.NewRequest(http.MethodGet, "/", nil))
	if recUI.Code != http.StatusOK || !strings.Contains(recUI.Body.String(), "Aurora Parental Control") {
		t.Fatalf("embedded UI failed: code=%d", recUI.Code)
	}
}
