package policy

import (
	"fmt"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/Simon-CR/aurora-parental-control/controller/internal/store"
)

const (
	maxHeartbeatDeltaSec = 30
	onlineStaleThreshold = 90 * time.Second
)

// HeartbeatResult holds the response fields returned to the Xbox 360 and UI.
type HeartbeatResult struct {
	Action      string `json:"action"`
	Message     string `json:"message"`
	RemainingSec int   `json:"remaining_sec"`
	UsedSec     int    `json:"used_sec"`
	DailyMaxSec int    `json:"daily_max_sec"`
	WindowOpen  bool   `json:"window_open"`
	Paused      bool   `json:"paused"`
	ReqStatus   string `json:"req_status"`
}

// Snapshot captures full controller state for the Web UI and MQTT publisher.
type Snapshot struct {
	Day              string       `json:"day"`
	Timezone         string       `json:"timezone"`
	ServerTime       string       `json:"server_time"`
	State            string       `json:"state"`
	Online           bool         `json:"online"`
	TitleID          string       `json:"title_id"`
	Mode             string       `json:"mode"`
	UsedSec          int          `json:"used_sec"`
	UsedMinutes      int          `json:"used_minutes"`
	RemainingSec     int          `json:"remaining_sec"`
	RemainingMinutes int          `json:"remaining_minutes"`
	DailyMaxMinutes  int          `json:"daily_max_minutes"`
	BonusMinutes     int          `json:"bonus_minutes"`
	EffectiveMaxMin  int          `json:"effective_max_minutes"`
	WeekdayWindow    string       `json:"weekday_window"`
	WeekendWindow    string       `json:"weekend_window"`
	WindowOpen       bool         `json:"window_open"`
	Paused           bool         `json:"paused"`
	PendingRequest   string       `json:"pending_request"`
	ReqStatus        string       `json:"req_status"`
	LastHeartbeatAt  string       `json:"last_heartbeat_at"`
	Events           []store.Event `json:"events"`
}

// Engine evaluates screen-time rules and persists transitions via Store.
type Engine struct {
	mu       sync.Mutex
	store    *store.Store
	loc      *time.Location
	now      func() time.Time
	onChange func()
}

// New creates a policy Engine bound to store and an IANA timezone location.
func New(s *store.Store, loc *time.Location, clock func() time.Time) *Engine {
	if loc == nil {
		loc = time.UTC
	}
	if clock == nil {
		clock = time.Now
	}
	return &Engine{
		store: s,
		loc:   loc,
		now:   clock,
	}
}

// SetOnChange registers a callback invoked whenever state changes require MQTT publish.
func (e *Engine) SetOnChange(fn func()) {
	e.mu.Lock()
	defer e.mu.Unlock()
	e.onChange = fn
}

func (e *Engine) notifyChange() {
	if e.onChange != nil {
		e.onChange()
	}
}

// Heartbeat accounts elapsed play time and determines the next console directive.
func (e *Engine) Heartbeat(titleID, mode string) (HeartbeatResult, error) {
	e.mu.Lock()
	now := e.now().In(e.loc)
	day := now.Format("2006-01-02")

	p, err := e.store.GetPolicy()
	if err != nil {
		e.mu.Unlock()
		return HeartbeatResult{}, err
	}
	u, err := e.store.GetOrCreateUsage(day)
	if err != nil {
		e.mu.Unlock()
		return HeartbeatResult{}, err
	}
	rt, err := e.store.GetRuntime()
	if err != nil {
		e.mu.Unlock()
		return HeartbeatResult{}, err
	}

	windowOpen, activeStart, activeEnd := evalWindow(now, p)
	dailyMaxSec := (p.DailyMaxMinutes + u.BonusMinutes) * 60
	if dailyMaxSec < 0 {
		dailyMaxSec = 0
	}

	if rt.LastHeartbeatAt != "" && !p.Paused && windowOpen && u.UsedSec < dailyMaxSec {
		if prevTS, parseErr := time.Parse(time.RFC3339, rt.LastHeartbeatAt); parseErr == nil {
			prevLocal := prevTS.In(e.loc)
			if prevLocal.Format("2006-01-02") == day {
				delta := int(now.Sub(prevLocal).Seconds())
				if delta > 0 {
					if delta > maxHeartbeatDeltaSec {
						delta = maxHeartbeatDeltaSec
					}
					u.UsedSec += delta
					if u.UsedSec > dailyMaxSec {
						u.UsedSec = dailyMaxSec
					}
				}
			}
		}
	}

	remainingSec := dailyMaxSec - u.UsedSec
	if remainingSec < 0 {
		remainingSec = 0
	}

	titleID = sanitizeToken(titleID, "00000000")
	mode = sanitizeToken(mode, "title")
	titleChanged := titleID != rt.LastTitleID && titleID != "00000000"

	rt.LastHeartbeatAt = now.UTC().Format(time.RFC3339)
	rt.LastTitleID = titleID
	rt.LastMode = mode

	res := HeartbeatResult{
		Action:       "none",
		Message:      "",
		RemainingSec: remainingSec,
		UsedSec:      u.UsedSec,
		DailyMaxSec:  dailyMaxSec,
		WindowOpen:   windowOpen,
		Paused:       p.Paused,
		ReqStatus:    reqStatusOrNone(rt.ReqStatus),
	}

	if rt.PendingShutdown {
		rt.PendingShutdown = false
		res.Action = "shutdown"
		res.Message = "Shutting down console as requested by parent"
		_ = e.store.AddEvent(now, "shutdown_dispatched", fmt.Sprintf("Delivered to %s (%s)", titleID, mode))
	} else if p.Paused {
		res.Action = "pause"
		res.Message = "Screen time is paused by a parent"
	} else if !windowOpen {
		res.Action = "time_up"
		res.Message = fmt.Sprintf("Outside allowed play hours (%s - %s)", activeStart, activeEnd)
	} else if remainingSec == 0 {
		res.Action = "time_up"
		res.Message = "Daily screen time limit reached"
	} else if msg := checkWarning(&u, remainingSec); msg != "" {
		res.Action = "toast"
		res.Message = msg
		_ = e.store.AddEvent(now, "warning_toast", msg)
	}

	if titleChanged {
		_ = e.store.AddEvent(now, "title_active", fmt.Sprintf("Title %s (%s)", titleID, mode))
	}

	if err := e.store.SaveUsage(u); err != nil {
		e.mu.Unlock()
		return HeartbeatResult{}, err
	}
	if err := e.store.SaveRuntime(rt); err != nil {
		e.mu.Unlock()
		return HeartbeatResult{}, err
	}

	e.mu.Unlock()
	e.notifyChange()
	return res, nil
}

// RequestTime records a child request for +15 or +30 bonus minutes.
func (e *Engine) RequestTime(minutes int) error {
	if minutes != 15 && minutes != 30 {
		return fmt.Errorf("unsupported request minutes %d (allowed: 15 or 30)", minutes)
	}
	e.mu.Lock()
	now := e.now().UTC()
	rt, err := e.store.GetRuntime()
	if err != nil {
		e.mu.Unlock()
		return err
	}
	rt.ReqKind = "more_time"
	rt.ReqMinutes = minutes
	rt.ReqStatus = "pending"
	rt.ReqUpdatedAt = now.Format(time.RFC3339)
	if err := e.store.SaveRuntime(rt); err != nil {
		e.mu.Unlock()
		return err
	}
	_ = e.store.AddEvent(now, "request_time", fmt.Sprintf("Console requested +%d minutes", minutes))
	e.mu.Unlock()
	e.notifyChange()
	return nil
}

// RequestResume records a child request to unpause the console.
func (e *Engine) RequestResume() error {
	e.mu.Lock()
	now := e.now().UTC()
	rt, err := e.store.GetRuntime()
	if err != nil {
		e.mu.Unlock()
		return err
	}
	rt.ReqKind = "resume"
	rt.ReqMinutes = 0
	rt.ReqStatus = "pending"
	rt.ReqUpdatedAt = now.Format(time.RFC3339)
	if err := e.store.SaveRuntime(rt); err != nil {
		e.mu.Unlock()
		return err
	}
	_ = e.store.AddEvent(now, "request_resume", "Console requested to resume from pause")
	e.mu.Unlock()
	e.notifyChange()
	return nil
}

// Approve grants the pending request (adding bonus minutes or unpausing).
func (e *Engine) Approve() error {
	e.mu.Lock()
	now := e.now().In(e.loc)
	day := now.Format("2006-01-02")

	rt, err := e.store.GetRuntime()
	if err != nil {
		e.mu.Unlock()
		return err
	}
	if rt.ReqStatus != "pending" {
		e.mu.Unlock()
		return nil
	}

	if rt.ReqKind == "resume" {
		p, err := e.store.GetPolicy()
		if err != nil {
			e.mu.Unlock()
			return err
		}
		p.Paused = false
		p.UpdatedAt = now.UTC().Format(time.RFC3339)
		if err := e.store.SavePolicy(p); err != nil {
			e.mu.Unlock()
			return err
		}
		_ = e.store.AddEvent(now, "request_approved", "Approved resume request")
	} else {
		add := rt.ReqMinutes
		if add <= 0 {
			add = 15
		}
		if err := e.addBonusLocked(day, add, now, "request_approved"); err != nil {
			e.mu.Unlock()
			return err
		}
	}

	rt.ReqStatus = "approved"
	rt.ReqUpdatedAt = now.UTC().Format(time.RFC3339)
	if err := e.store.SaveRuntime(rt); err != nil {
		e.mu.Unlock()
		return err
	}

	e.mu.Unlock()
	e.notifyChange()
	return nil
}

// Deny rejects the current pending request without unlocking or adding time.
func (e *Engine) Deny() error {
	e.mu.Lock()
	now := e.now().UTC()
	rt, err := e.store.GetRuntime()
	if err != nil {
		e.mu.Unlock()
		return err
	}
	if rt.ReqStatus != "pending" {
		e.mu.Unlock()
		return nil
	}
	rt.ReqStatus = "denied"
	rt.ReqUpdatedAt = now.Format(time.RFC3339)
	if err := e.store.SaveRuntime(rt); err != nil {
		e.mu.Unlock()
		return err
	}
	_ = e.store.AddEvent(now, "request_denied", "Parent denied console request")
	e.mu.Unlock()
	e.notifyChange()
	return nil
}

// AddBonus immediately grants bonus minutes for today and resets cleared warning thresholds.
func (e *Engine) AddBonus(minutes int) error {
	if minutes <= 0 || minutes > 600 {
		return fmt.Errorf("invalid bonus minutes %d", minutes)
	}
	e.mu.Lock()
	now := e.now().In(e.loc)
	day := now.Format("2006-01-02")
	if err := e.addBonusLocked(day, minutes, now, "bonus_added"); err != nil {
		e.mu.Unlock()
		return err
	}
	e.mu.Unlock()
	e.notifyChange()
	return nil
}

func (e *Engine) addBonusLocked(day string, minutes int, now time.Time, eventKind string) error {
	p, err := e.store.GetPolicy()
	if err != nil {
		return err
	}
	u, err := e.store.GetOrCreateUsage(day)
	if err != nil {
		return err
	}
	u.BonusMinutes += minutes
	remainingSec := (p.DailyMaxMinutes+u.BonusMinutes)*60 - u.UsedSec
	if remainingSec > 900 {
		u.Warned15m = false
	}
	if remainingSec > 300 {
		u.Warned5m = false
	}
	if remainingSec > 60 {
		u.Warned1m = false
	}
	if err := e.store.SaveUsage(u); err != nil {
		return err
	}
	_ = e.store.AddEvent(now, eventKind, fmt.Sprintf("Added +%d bonus minutes", minutes))
	return nil
}

// SetPaused pauses or unpauses Xbox 360 play.
func (e *Engine) SetPaused(paused bool) error {
	e.mu.Lock()
	now := e.now().UTC()
	p, err := e.store.GetPolicy()
	if err != nil {
		e.mu.Unlock()
		return err
	}
	p.Paused = paused
	p.UpdatedAt = now.Format(time.RFC3339)
	if err := e.store.SavePolicy(p); err != nil {
		e.mu.Unlock()
		return err
	}
	rt, err := e.store.GetRuntime()
	if err == nil && !paused && rt.ReqKind == "resume" && rt.ReqStatus == "pending" {
		rt.ReqStatus = "approved"
		rt.ReqUpdatedAt = now.Format(time.RFC3339)
		_ = e.store.SaveRuntime(rt)
	}
	stateText := "paused"
	if !paused {
		stateText = "resumed"
	}
	_ = e.store.AddEvent(now, "pause_changed", "Console "+stateText+" by parent")
	e.mu.Unlock()
	e.notifyChange()
	return nil
}

// ResetToday clears today's consumed seconds, bonus minutes, warnings, and request state.
func (e *Engine) ResetToday() error {
	e.mu.Lock()
	now := e.now().In(e.loc)
	day := now.Format("2006-01-02")

	u := store.DailyUsage{Day: day}
	if err := e.store.SaveUsage(u); err != nil {
		e.mu.Unlock()
		return err
	}
	rt, err := e.store.GetRuntime()
	if err != nil {
		e.mu.Unlock()
		return err
	}
	rt.ReqKind = ""
	rt.ReqMinutes = 0
	rt.ReqStatus = "none"
	rt.ReqUpdatedAt = now.UTC().Format(time.RFC3339)
	if err := e.store.SaveRuntime(rt); err != nil {
		e.mu.Unlock()
		return err
	}
	_ = e.store.AddEvent(now, "day_reset", "Daily timer reset to 0")
	e.mu.Unlock()
	e.notifyChange()
	return nil
}

// TriggerShutdown queues a one-shot shutdown command delivered on the next console poll.
func (e *Engine) TriggerShutdown() error {
	e.mu.Lock()
	now := e.now().UTC()
	rt, err := e.store.GetRuntime()
	if err != nil {
		e.mu.Unlock()
		return err
	}
	rt.PendingShutdown = true
	if err := e.store.SaveRuntime(rt); err != nil {
		e.mu.Unlock()
		return err
	}
	_ = e.store.AddEvent(now, "shutdown_queued", "Queued shutdown for next console poll")
	e.mu.Unlock()
	e.notifyChange()
	return nil
}

// UpdatePolicy validates and persists schedule/quota changes.
func (e *Engine) UpdatePolicy(dailyMaxMin int, weekdayWin, weekendWin string) error {
	if dailyMaxMin < 0 || dailyMaxMin > 1440 {
		return fmt.Errorf("daily_max_minutes must be between 0 and 1440")
	}
	ws, we, err := ParseWindow(weekdayWin)
	if err != nil {
		return fmt.Errorf("weekday_window: %w", err)
	}
	ks, ke, err := ParseWindow(weekendWin)
	if err != nil {
		return fmt.Errorf("weekend_window: %w", err)
	}

	e.mu.Lock()
	now := e.now().UTC()
	p, err := e.store.GetPolicy()
	if err != nil {
		e.mu.Unlock()
		return err
	}
	p.DailyMaxMinutes = dailyMaxMin
	p.WeekdayStart = ws
	p.WeekdayEnd = we
	p.WeekendStart = ks
	p.WeekendEnd = ke
	p.UpdatedAt = now.Format(time.RFC3339)
	if err := e.store.SavePolicy(p); err != nil {
		e.mu.Unlock()
		return err
	}
	_ = e.store.AddEvent(now, "policy_updated", fmt.Sprintf("Max %dm | Wk %s-%s | Wkend %s-%s", dailyMaxMin, ws, we, ks, ke))
	e.mu.Unlock()
	e.notifyChange()
	return nil
}

// SetDailyMax updates only the daily quota limit.
func (e *Engine) SetDailyMax(minutes int) error {
	e.mu.Lock()
	p, err := e.store.GetPolicy()
	e.mu.Unlock()
	if err != nil {
		return err
	}
	return e.UpdatePolicy(minutes, p.WeekdayStart+"-"+p.WeekdayEnd, p.WeekendStart+"-"+p.WeekendEnd)
}

// SetWeekdayWindow updates only the weekday HH:MM-HH:MM window.
func (e *Engine) SetWeekdayWindow(win string) error {
	e.mu.Lock()
	p, err := e.store.GetPolicy()
	e.mu.Unlock()
	if err != nil {
		return err
	}
	return e.UpdatePolicy(p.DailyMaxMinutes, win, p.WeekendStart+"-"+p.WeekendEnd)
}

// SetWeekendWindow updates only the weekend HH:MM-HH:MM window.
func (e *Engine) SetWeekendWindow(win string) error {
	e.mu.Lock()
	p, err := e.store.GetPolicy()
	e.mu.Unlock()
	if err != nil {
		return err
	}
	return e.UpdatePolicy(p.DailyMaxMinutes, p.WeekdayStart+"-"+p.WeekdayEnd, win)
}

// Snapshot returns a consistent view of today's screen-time state and recent events.
func (e *Engine) Snapshot() (Snapshot, error) {
	e.mu.Lock()
	defer e.mu.Unlock()

	now := e.now().In(e.loc)
	day := now.Format("2006-01-02")

	p, err := e.store.GetPolicy()
	if err != nil {
		return Snapshot{}, err
	}
	u, err := e.store.GetOrCreateUsage(day)
	if err != nil {
		return Snapshot{}, err
	}
	rt, err := e.store.GetRuntime()
	if err != nil {
		return Snapshot{}, err
	}
	evts, err := e.store.ListEvents(40)
	if err != nil {
		return Snapshot{}, err
	}

	windowOpen, _, _ := evalWindow(now, p)
	effMaxMin := p.DailyMaxMinutes + u.BonusMinutes
	dailyMaxSec := effMaxMin * 60
	remSec := dailyMaxSec - u.UsedSec
	if remSec < 0 {
		remSec = 0
	}

	online := false
	if rt.LastHeartbeatAt != "" {
		if ts, parseErr := time.Parse(time.RFC3339, rt.LastHeartbeatAt); parseErr == nil {
			online = now.Sub(ts.In(e.loc)) <= onlineStaleThreshold
		}
	}

	state := "offline"
	if online {
		switch {
		case p.Paused:
			state = "paused"
		case !windowOpen:
			state = "outside_hours"
		case remSec == 0:
			state = "time_up"
		default:
			state = "playing"
		}
	}

	pendingLabel := "none"
	if rt.ReqStatus == "pending" {
		if rt.ReqKind == "resume" {
			pendingLabel = "resume"
		} else if rt.ReqMinutes > 0 {
			pendingLabel = fmt.Sprintf("+%dm", rt.ReqMinutes)
		}
	}

	return Snapshot{
		Day:              day,
		Timezone:         e.loc.String(),
		ServerTime:       now.Format("2006-01-02 15:04:05 MST"),
		State:            state,
		Online:           online,
		TitleID:          sanitizeToken(rt.LastTitleID, "00000000"),
		Mode:             rt.LastMode,
		UsedSec:          u.UsedSec,
		UsedMinutes:      u.UsedSec / 60,
		RemainingSec:     remSec,
		RemainingMinutes: (remSec + 59) / 60,
		DailyMaxMinutes:  p.DailyMaxMinutes,
		BonusMinutes:     u.BonusMinutes,
		EffectiveMaxMin:  effMaxMin,
		WeekdayWindow:    p.WeekdayStart + "-" + p.WeekdayEnd,
		WeekendWindow:    p.WeekendStart + "-" + p.WeekendEnd,
		WindowOpen:       windowOpen,
		Paused:           p.Paused,
		PendingRequest:   pendingLabel,
		ReqStatus:        reqStatusOrNone(rt.ReqStatus),
		LastHeartbeatAt:  rt.LastHeartbeatAt,
		Events:           evts,
	}, nil
}

// ParseWindow validates an "HH:MM-HH:MM" time range.
func ParseWindow(raw string) (string, string, error) {
	parts := strings.Split(strings.TrimSpace(raw), "-")
	if len(parts) != 2 {
		return "", "", fmt.Errorf("expected HH:MM-HH:MM, got %q", raw)
	}
	sNorm, _, err := parseHHMM(parts[0], false)
	if err != nil {
		return "", "", err
	}
	eNorm, _, err := parseHHMM(parts[1], true)
	if err != nil {
		return "", "", err
	}
	return sNorm, eNorm, nil
}

func parseHHMM(s string, allow2400 bool) (string, int, error) {
	s = strings.TrimSpace(s)
	parts := strings.Split(s, ":")
	if len(parts) != 2 {
		return "", 0, fmt.Errorf("expected HH:MM, got %q", s)
	}
	h, err := strconv.Atoi(parts[0])
	if err != nil || h < 0 || h > 24 {
		return "", 0, fmt.Errorf("invalid hour in %q", s)
	}
	m, err := strconv.Atoi(parts[1])
	if err != nil || m < 0 || m > 59 {
		return "", 0, fmt.Errorf("invalid minute in %q", s)
	}
	if h == 24 && (!allow2400 || m != 0) {
		return "", 0, fmt.Errorf("24:xx is only valid as 24:00 end")
	}
	return fmt.Sprintf("%02d:%02d", h, m), h*60 + m, nil
}

func evalWindow(now time.Time, p store.Policy) (bool, string, string) {
	startStr, endStr := p.WeekdayStart, p.WeekdayEnd
	wd := now.Weekday()
	if wd == time.Saturday || wd == time.Sunday {
		startStr, endStr = p.WeekendStart, p.WeekendEnd
	}
	_, startMin, err1 := parseHHMM(startStr, false)
	_, endMin, err2 := parseHHMM(endStr, true)
	if err1 != nil || err2 != nil {
		return false, startStr, endStr
	}
	nowMin := now.Hour()*60 + now.Minute()
	if startMin == endMin {
		return false, startStr, endStr
	}
	if startMin < endMin {
		return nowMin >= startMin && nowMin < endMin, startStr, endStr
	}
	return nowMin >= startMin || nowMin < endMin, startStr, endStr
}

func checkWarning(u *store.DailyUsage, remainingSec int) string {
	switch {
	case remainingSec <= 60 && !u.Warned1m:
		u.Warned1m = true
		u.Warned5m = true
		u.Warned15m = true
		return "1 minute left - save your game now"
	case remainingSec <= 300 && !u.Warned5m:
		u.Warned5m = true
		u.Warned15m = true
		return "5 minutes left - save your game"
	case remainingSec <= 900 && !u.Warned15m:
		u.Warned15m = true
		return "15 minutes of screen time remaining"
	default:
		return ""
	}
}

func sanitizeToken(raw, fallback string) string {
	s := strings.TrimSpace(raw)
	if s == "" {
		return fallback
	}
	if len(s) > 24 {
		s = s[:24]
	}
	return s
}

func reqStatusOrNone(s string) string {
	if s == "" {
		return "none"
	}
	return s
}
