package httpapi

import (
	"encoding/json"
	"fmt"
	"net/http"
	"strconv"
	"strings"

	"github.com/Simon-CR/aurora-parental-control/controller/internal/policy"
	"github.com/Simon-CR/aurora-parental-control/controller/internal/webui"
)

// NewMux constructs the HTTP router for Xbox 360 poll endpoints and the parent Web UI.
func NewMux(engine *policy.Engine) http.Handler {
	mux := http.NewServeMux()
	h := &handler{engine: engine}

	mux.HandleFunc("GET /api/v1/xbox/heartbeat", h.handleHeartbeat)
	mux.HandleFunc("POST /api/v1/xbox/request-time", h.handleRequestTime)
	mux.HandleFunc("POST /api/v1/xbox/request-resume", h.handleRequestResume)

	mux.HandleFunc("GET /api/v1/status", h.handleStatus)
	mux.HandleFunc("POST /api/v1/policy", h.handleUpdatePolicy)
	mux.HandleFunc("POST /api/v1/action", h.handleAction)
	mux.HandleFunc("GET /api/v1/events", h.handleEvents)

	mux.Handle("/", webui.Handler())
	return mux
}

type handler struct {
	engine *policy.Engine
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
