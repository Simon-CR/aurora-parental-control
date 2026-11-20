package httpapi

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"strconv"
	"strings"

	"github.com/Simon-CR/aurora-parental-control/controller/internal/catalog"
	"github.com/Simon-CR/aurora-parental-control/controller/internal/policy"
	"github.com/Simon-CR/aurora-parental-control/controller/internal/store"
	"github.com/Simon-CR/aurora-parental-control/controller/internal/webui"
)

// NewMux constructs the HTTP router for Xbox 360 poll endpoints and the parent Web UI.
func NewMux(engine *policy.Engine) http.Handler {
	return NewMuxWithCatalog(engine, nil, nil)
}

// NewMuxWithCatalog constructs the HTTP router including Shelf launcher catalog and state endpoints.
func NewMuxWithCatalog(engine *policy.Engine, cat *catalog.Store, st *store.Store) http.Handler {
	mux := http.NewServeMux()
	h := &handler{engine: engine, catalog: cat, store: st}

	mux.HandleFunc("GET /api/v1/xbox/heartbeat", h.handleHeartbeat)
	mux.HandleFunc("POST /api/v1/xbox/request-time", h.handleRequestTime)
	mux.HandleFunc("POST /api/v1/xbox/request-resume", h.handleRequestResume)

	mux.HandleFunc("GET /api/v1/launcher/catalog", h.handleLauncherCatalogGet)
	mux.HandleFunc("POST /api/v1/launcher/catalog", h.handleLauncherCatalogPost)
	mux.HandleFunc("GET /api/v1/launcher/poster/{id}", h.handleLauncherPoster)
	mux.HandleFunc("GET /api/v1/launcher/backdrop/{id}", h.handleLauncherBackdrop)
	mux.HandleFunc("GET /api/v1/launcher/userstate", h.handleLauncherUserstate)
	mux.HandleFunc("POST /api/v1/launcher/favorite", h.handleLauncherFavorite)
	mux.HandleFunc("POST /api/v1/launcher/played", h.handleLauncherPlayed)
	mux.HandleFunc("GET /api/v1/launcher/status", h.handleLauncherStatus)

	mux.HandleFunc("GET /api/v1/status", h.handleStatus)
	mux.HandleFunc("POST /api/v1/policy", h.handleUpdatePolicy)
	mux.HandleFunc("POST /api/v1/action", h.handleAction)
	mux.HandleFunc("GET /api/v1/events", h.handleEvents)

	mux.Handle("/", webui.Handler())
	return mux
}

type handler struct {
	engine  *policy.Engine
	catalog *catalog.Store
	store   *store.Store
}

func (h *handler) handleHeartbeat(w http.ResponseWriter, r *http.Request) {
	q := r.URL.Query()
	res, err := h.engine.Heartbeat(q.Get("title_id"), q.Get("mode"))
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	if wantsJSON(r) {
		writeJSON(w, http.StatusOK, res)
		return
	}

	w.Header().Set("Content-Type", "text/plain; charset=utf-8")
	fmt.Fprintf(
		w,
		"action=%s\nmessage=%s\nremaining_sec=%d\nused_sec=%d\ndaily_max_sec=%d\nwindow_open=%d\npaused=%d\nreq_status=%s\n",
		res.Action,
		strings.ReplaceAll(res.Message, "\n", " "),
		res.RemainingSec,
		res.UsedSec,
		res.DailyMaxSec,
		boolBit(res.WindowOpen),
		boolBit(res.Paused),
		res.ReqStatus,
	)
}

func (h *handler) handleRequestTime(w http.ResponseWriter, r *http.Request) {
	minStr := r.URL.Query().Get("minutes")
	minutes, err := strconv.Atoi(minStr)
	if err != nil || (minutes != 15 && minutes != 30) {
		http.Error(w, "minutes must be 15 or 30", http.StatusBadRequest)
		return
	}
	if err := h.engine.RequestTime(minutes); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	if wantsJSON(r) {
		writeJSON(w, http.StatusOK, map[string]any{
			"req_status":    "pending",
			"requested_min": minutes,
			"message":       "Request sent to parent",
		})
		return
	}
	w.Header().Set("Content-Type", "text/plain; charset=utf-8")
	fmt.Fprintf(w, "req_status=pending\nrequested_min=%d\nmessage=Request sent to parent\n", minutes)
}

func (h *handler) handleRequestResume(w http.ResponseWriter, r *http.Request) {
	if err := h.engine.RequestResume(); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	if wantsJSON(r) {
		writeJSON(w, http.StatusOK, map[string]any{
			"req_status": "pending",
			"message":    "Resume request sent to parent",
		})
		return
	}
	w.Header().Set("Content-Type", "text/plain; charset=utf-8")
	fmt.Fprint(w, "req_status=pending\nmessage=Resume request sent to parent\n")
}

func (h *handler) handleStatus(w http.ResponseWriter, _ *http.Request) {
	snap, err := h.engine.Snapshot()
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	writeJSON(w, http.StatusOK, snap)
}

func (h *handler) handleEvents(w http.ResponseWriter, _ *http.Request) {
	snap, err := h.engine.Snapshot()
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	writeJSON(w, http.StatusOK, snap.Events)
}

type policyReq struct {
	DailyMaxMinutes int    `json:"daily_max_minutes"`
	WeekdayWindow   string `json:"weekday_window"`
	WeekendWindow   string `json:"weekend_window"`
}

func (h *handler) handleUpdatePolicy(w http.ResponseWriter, r *http.Request) {
	var body policyReq
	if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
		http.Error(w, "invalid json", http.StatusBadRequest)
		return
	}
	if err := h.engine.UpdatePolicy(body.DailyMaxMinutes, body.WeekdayWindow, body.WeekendWindow); err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	h.handleStatus(w, r)
}

type actionReq struct {
	Action string `json:"action"`
}

func (h *handler) handleAction(w http.ResponseWriter, r *http.Request) {
	var body actionReq
	if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
		http.Error(w, "invalid json", http.StatusBadRequest)
		return
	}
	var err error
	switch strings.ToLower(strings.TrimSpace(body.Action)) {
	case "pause":
		err = h.engine.SetPaused(true)
	case "resume":
		err = h.engine.SetPaused(false)
	case "add_15m":
		err = h.engine.AddBonus(15)
	case "add_30m":
		err = h.engine.AddBonus(30)
	case "approve":
		err = h.engine.Approve()
	case "deny":
		err = h.engine.Deny()
	case "reset_today":
		err = h.engine.ResetToday()
	case "shutdown":
		err = h.engine.TriggerShutdown()
	default:
		http.Error(w, "unsupported action", http.StatusBadRequest)
		return
	}
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	writeJSON(w, http.StatusOK, map[string]string{"status": "ok"})
}

func (h *handler) handleLauncherCatalogGet(w http.ResponseWriter, r *http.Request) {
	tsv := []byte("SHELF_CATALOG\tv1\t0\t0\n")
	etag := `"empty"`
	if h.catalog != nil {
		tsv, etag = h.catalog.CatalogTSV()
	}
	w.Header().Set("Content-Type", "text/tab-separated-values; charset=utf-8")
	w.Header().Set("ETag", etag)
	if r.Header.Get("If-None-Match") == etag {
		w.WriteHeader(http.StatusNotModified)
		return
	}
	w.Header().Set("Content-Length", strconv.Itoa(len(tsv)))
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write(tsv)
}

func (h *handler) handleLauncherCatalogPost(w http.ResponseWriter, r *http.Request) {
	if h.catalog == nil {
		http.Error(w, "launcher catalog store not configured", http.StatusServiceUnavailable)
		return
	}
	limited := http.MaxBytesReader(w, r.Body, catalog.MaxCatalogBytes+1024)
	body, err := io.ReadAll(limited)
	if err != nil {
		http.Error(w, fmt.Sprintf("read catalog body: %v", err), http.StatusBadRequest)
		return
	}
	st, err := h.catalog.SaveCatalogTSV(body)
	if err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	writeJSON(w, http.StatusOK, st)
}

func (h *handler) handleLauncherPoster(w http.ResponseWriter, r *http.Request) {
	h.serveLauncherAsset(w, r, false)
}

func (h *handler) handleLauncherBackdrop(w http.ResponseWriter, r *http.Request) {
	h.serveLauncherAsset(w, r, true)
}

func (h *handler) serveLauncherAsset(w http.ResponseWriter, r *http.Request, backdrop bool) {
	tid, ok := catalog.ValidateTitleID(r.PathValue("id"))
	if !ok {
		http.Error(w, "invalid title_id", http.StatusBadRequest)
		return
	}
	if h.catalog == nil {
		http.Error(w, "asset not found", http.StatusNotFound)
		return
	}
	var path string
	var found bool
	if backdrop {
		path, found = h.catalog.BackdropPath(tid)
	} else {
		path, found = h.catalog.PosterPath(tid)
	}
	if !found {
		http.Error(w, "asset not found", http.StatusNotFound)
		return
	}
	data, err := os.ReadFile(path)
	if err != nil {
		http.Error(w, "asset unavailable", http.StatusNotFound)
		return
	}
	info, err := os.Stat(path)
	if err != nil {
		http.Error(w, "asset unavailable", http.StatusNotFound)
		return
	}
	etag := fmt.Sprintf(`"%x-%x"`, info.Size(), info.ModTime().Unix())
	w.Header().Set("Content-Type", "image/jpeg")
	w.Header().Set("ETag", etag)
	if r.Header.Get("If-None-Match") == etag {
		w.WriteHeader(http.StatusNotModified)
		return
	}
	http.ServeContent(w, r, tid+".jpg", info.ModTime(), bytes.NewReader(data))
}

func (h *handler) handleLauncherUserstate(w http.ResponseWriter, _ *http.Request) {
	if h.store == nil {
		http.Error(w, "launcher state store not configured", http.StatusServiceUnavailable)
		return
	}
	states, err := h.store.ListLauncherGameStates()
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	var b strings.Builder
	for _, st := range states {
		fmt.Fprintf(&b, "%s\t%d\t%d\t%d\n", st.TitleID, boolBit(st.Favorite), st.LastPlayedAt, st.PlayCount)
	}
	w.Header().Set("Content-Type", "text/plain; charset=utf-8")
	w.Header().Set("Content-Length", strconv.Itoa(b.Len()))
	w.WriteHeader(http.StatusOK)
	_, _ = io.WriteString(w, b.String())
}

func (h *handler) handleLauncherFavorite(w http.ResponseWriter, r *http.Request) {
	if h.store == nil {
		http.Error(w, "launcher state store not configured", http.StatusServiceUnavailable)
		return
	}
	q := r.URL.Query()
	tid, ok := store.NormalizeTitleID(q.Get("title_id"))
	if !ok {
		http.Error(w, "invalid title_id", http.StatusBadRequest)
		return
	}
	favRaw := strings.ToLower(strings.TrimSpace(q.Get("fav")))
	var fav bool
	switch favRaw {
	case "1", "true":
		fav = true
	case "0", "false":
		fav = false
	default:
		http.Error(w, "fav must be 0 or 1", http.StatusBadRequest)
		return
	}
	if err := h.store.SetLauncherFavorite(tid, fav); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	w.Header().Set("Content-Type", "text/plain; charset=utf-8")
	_, _ = io.WriteString(w, "ok\n")
}

func (h *handler) handleLauncherPlayed(w http.ResponseWriter, r *http.Request) {
	if h.store == nil {
		http.Error(w, "launcher state store not configured", http.StatusServiceUnavailable)
		return
	}
	tid, ok := store.NormalizeTitleID(r.URL.Query().Get("title_id"))
	if !ok {
		http.Error(w, "invalid title_id", http.StatusBadRequest)
		return
	}
	if err := h.store.RecordLauncherPlayed(tid, 0); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	w.Header().Set("Content-Type", "text/plain; charset=utf-8")
	_, _ = io.WriteString(w, "ok\n")
}

func (h *handler) handleLauncherStatus(w http.ResponseWriter, _ *http.Request) {
	var st catalog.Status
	if h.catalog != nil {
		st = h.catalog.Status()
	} else {
		st = catalog.Status{CatalogVersion: "empty"}
	}
	writeJSON(w, http.StatusOK, st)
}

func wantsJSON(r *http.Request) bool {
	return strings.Contains(strings.ToLower(r.Header.Get("Accept")), "application/json")
}

func writeJSON(w http.ResponseWriter, code int, v any) {
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.WriteHeader(code)
	_ = json.NewEncoder(w).Encode(v)
}

func boolBit(b bool) int {
	if b {
		return 1
	}
	return 0
}
