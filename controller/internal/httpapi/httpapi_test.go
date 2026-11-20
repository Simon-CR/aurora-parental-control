package httpapi

import (
	"bytes"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/Simon-CR/aurora-parental-control/controller/internal/catalog"
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

func setupLauncherHandler(t *testing.T) (http.Handler, string) {
	t.Helper()
	dir := t.TempDir()
	s, err := store.Open(filepath.Join(dir, "http.db"))
	if err != nil {
		t.Fatalf("Open store: %v", err)
	}
	t.Cleanup(func() { _ = s.Close() })
	catDir := filepath.Join(dir, "launcher-cache")
	cat, err := catalog.Open(catDir)
	if err != nil {
		t.Fatalf("Open catalog: %v", err)
	}
	loc, _ := time.LoadLocation("America/Toronto")
	clk := time.Date(2026, 11, 21, 12, 0, 0, 0, loc)
	eng := policy.New(s, loc, func() time.Time { return clk })
	return NewMuxWithCatalog(eng, cat, s), catDir
}

func TestLauncherEndpoints(t *testing.T) {
	mux, catDir := setupLauncherHandler(t)

	// 1. Empty catalog GET returns header-only TSV + ETag.
	recEmpty := httptest.NewRecorder()
	mux.ServeHTTP(recEmpty, httptest.NewRequest(http.MethodGet, "/api/v1/launcher/catalog", nil))
	if recEmpty.Code != http.StatusOK || !strings.HasPrefix(recEmpty.Body.String(), "SHELF_CATALOG\tv1\t0\t0") {
		t.Fatalf("empty catalog get failed: code=%d body=%q", recEmpty.Code, recEmpty.Body.String())
	}

	// 2. POST invalid catalog rejected with 400.
	recBadCat := httptest.NewRecorder()
	mux.ServeHTTP(recBadCat, httptest.NewRequest(http.MethodPost, "/api/v1/launcher/catalog", strings.NewReader("garbage")))
	if recBadCat.Code != http.StatusBadRequest {
		t.Fatalf("expected 400 on bad catalog, got %d", recBadCat.Code)
	}

	// 3. POST valid catalog and GET back + check 304 ETag.
	tsvPayload := strings.Join([]string{
		"SHELF_CATALOG\tv1\t1700000123\t1",
		"12\t45410912\t0E7A7EB3\tPortal 2\t2\t1\t1\tUsb0:\t\\games\\portal 2\tdefault.xex\tValve\t2011\t1\t1\tCo-op puzzle platformer.",
	}, "\n") + "\n"
	recPost := httptest.NewRecorder()
	mux.ServeHTTP(recPost, httptest.NewRequest(http.MethodPost, "/api/v1/launcher/catalog", strings.NewReader(tsvPayload)))
	if recPost.Code != http.StatusOK {
		t.Fatalf("catalog post failed: code=%d body=%s", recPost.Code, recPost.Body.String())
	}
	var st catalog.Status
	if err := json.Unmarshal(recPost.Body.Bytes(), &st); err != nil || st.GameCount != 1 {
		t.Fatalf("unexpected post status: %+v err=%v", st, err)
	}

	recCat := httptest.NewRecorder()
	mux.ServeHTTP(recCat, httptest.NewRequest(http.MethodGet, "/api/v1/launcher/catalog", nil))
	if recCat.Code != http.StatusOK || !strings.Contains(recCat.Body.String(), "Portal 2") {
		t.Fatalf("catalog get failed: code=%d body=%s", recCat.Code, recCat.Body.String())
	}
	etag := recCat.Header().Get("ETag")
	req304 := httptest.NewRequest(http.MethodGet, "/api/v1/launcher/catalog", nil)
	req304.Header.Set("If-None-Match", etag)
	rec304 := httptest.NewRecorder()
	mux.ServeHTTP(rec304, req304)
	if rec304.Code != http.StatusNotModified {
		t.Fatalf("expected 304 Not Modified, got %d", rec304.Code)
	}

	// 4. Poster & backdrop: 400 invalid ID, 404 missing, 200 present.
	recBadID := httptest.NewRecorder()
	mux.ServeHTTP(recBadID, httptest.NewRequest(http.MethodGet, "/api/v1/launcher/poster/nothex", nil))
	if recBadID.Code != http.StatusBadRequest {
		t.Fatalf("expected 400 on invalid poster id, got %d", recBadID.Code)
	}
	recTrav := httptest.NewRecorder()
	mux.ServeHTTP(recTrav, httptest.NewRequest(http.MethodGet, "/api/v1/launcher/backdrop/..%2Fsecret", nil))
	if recTrav.Code != http.StatusBadRequest {
		t.Fatalf("expected 400 on traversal backdrop id, got %d", recTrav.Code)
	}
	recMiss := httptest.NewRecorder()
	mux.ServeHTTP(recMiss, httptest.NewRequest(http.MethodGet, "/api/v1/launcher/poster/45410912.jpg", nil))
	if recMiss.Code != http.StatusNotFound {
		t.Fatalf("expected 404 on missing poster, got %d", recMiss.Code)
	}

	jpegStub := []byte{0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 'J', 'F', 'I', 'F', 0x00, 0xFF, 0xD9}
	if err := os.WriteFile(filepath.Join(catDir, "posters", "45410912.jpg"), jpegStub, 0o644); err != nil {
		t.Fatalf("write poster: %v", err)
	}
	if err := os.WriteFile(filepath.Join(catDir, "backdrops", "45410912.jpg"), jpegStub, 0o644); err != nil {
		t.Fatalf("write backdrop: %v", err)
	}
	recPoster := httptest.NewRecorder()
	mux.ServeHTTP(recPoster, httptest.NewRequest(http.MethodGet, "/api/v1/launcher/poster/45410912.jpg", nil))
	if recPoster.Code != http.StatusOK || recPoster.Header().Get("Content-Type") != "image/jpeg" {
		t.Fatalf("poster 200 failed: code=%d ct=%q", recPoster.Code, recPoster.Header().Get("Content-Type"))
	}
	recBack := httptest.NewRecorder()
	mux.ServeHTTP(recBack, httptest.NewRequest(http.MethodGet, "/api/v1/launcher/backdrop/45410912", nil))
	if recBack.Code != http.StatusOK || recBack.Header().Get("Content-Type") != "image/jpeg" {
		t.Fatalf("backdrop 200 failed: code=%d ct=%q", recBack.Code, recBack.Header().Get("Content-Type"))
	}

	// 5. Status JSON reflects poster & backdrop counts.
	recStatus := httptest.NewRecorder()
	mux.ServeHTTP(recStatus, httptest.NewRequest(http.MethodGet, "/api/v1/launcher/status", nil))
	if err := json.Unmarshal(recStatus.Body.Bytes(), &st); err != nil {
		t.Fatalf("launcher status unmarshal: %v", err)
	}
	if st.GameCount != 1 || st.PosterCount != 1 || st.BackdropCount != 1 {
		t.Fatalf("unexpected launcher status: %+v", st)
	}

	// 6. Favorite + played + userstate round-trip.
	recFavBad := httptest.NewRecorder()
	mux.ServeHTTP(recFavBad, httptest.NewRequest(http.MethodPost, "/api/v1/launcher/favorite?title_id=45410912&fav=maybe", nil))
	if recFavBad.Code != http.StatusBadRequest {
		t.Fatalf("expected 400 on invalid fav, got %d", recFavBad.Code)
	}
	recFav := httptest.NewRecorder()
	mux.ServeHTTP(recFav, httptest.NewRequest(http.MethodPost, "/api/v1/launcher/favorite?title_id=45410912&fav=1", nil))
	if recFav.Code != http.StatusOK || recFav.Body.String() != "ok\n" {
		t.Fatalf("favorite failed: code=%d body=%q", recFav.Code, recFav.Body.String())
	}
	recPlayed := httptest.NewRecorder()
	mux.ServeHTTP(recPlayed, httptest.NewRequest(http.MethodPost, "/api/v1/launcher/played?title_id=45410912", nil))
	if recPlayed.Code != http.StatusOK || recPlayed.Body.String() != "ok\n" {
		t.Fatalf("played failed: code=%d body=%q", recPlayed.Code, recPlayed.Body.String())
	}
	recUser := httptest.NewRecorder()
	mux.ServeHTTP(recUser, httptest.NewRequest(http.MethodGet, "/api/v1/launcher/userstate", nil))
	if recUser.Code != http.StatusOK {
		t.Fatalf("userstate failed: code=%d", recUser.Code)
	}
	lines := strings.Split(strings.TrimSpace(recUser.Body.String()), "\n")
	if len(lines) != 1 {
		t.Fatalf("expected 1 userstate line, got %q", recUser.Body.String())
	}
	cols := strings.Split(lines[0], "\t")
	if len(cols) != 4 || cols[0] != "45410912" || cols[1] != "1" || cols[3] != "1" || cols[2] == "0" {
		t.Fatalf("unexpected userstate row: %q", lines[0])
	}
}
