package store

import (
	"database/sql"
	"fmt"
	"time"

	_ "modernc.org/sqlite"
)

// Policy holds persisted parental schedule and quota settings.
type Policy struct {
	DailyMaxMinutes int    `json:"daily_max_minutes"`
	WeekdayStart    string `json:"weekday_start"`
	WeekdayEnd      string `json:"weekday_end"`
	WeekendStart    string `json:"weekend_start"`
	WeekendEnd      string `json:"weekend_end"`
	Paused          bool   `json:"paused"`
	UpdatedAt       string `json:"updated_at"`
}

// DailyUsage tracks consumed seconds and emitted warnings for one calendar date.
type DailyUsage struct {
	Day          string `json:"day"`
	UsedSec      int    `json:"used_sec"`
	BonusMinutes int    `json:"bonus_minutes"`
	Warned15m    bool   `json:"warned_15m"`
	Warned5m     bool   `json:"warned_5m"`
	Warned1m     bool   `json:"warned_1m"`
}

// RuntimeState tracks live console heartbeats and pending parent approvals.
type RuntimeState struct {
	LastHeartbeatAt string `json:"last_heartbeat_at"`
	LastTitleID     string `json:"last_title_id"`
	LastMode        string `json:"last_mode"`
	PendingShutdown bool   `json:"pending_shutdown"`
	ReqKind         string `json:"req_kind"`
	ReqMinutes      int    `json:"req_minutes"`
	ReqStatus       string `json:"req_status"`
	ReqUpdatedAt    string `json:"req_updated_at"`
}

// Event is an audit log entry surfaced in the Web UI.
type Event struct {
	ID     int64  `json:"id"`
	TS     string `json:"ts"`
	Kind   string `json:"kind"`
	Detail string `json:"detail"`
}

// Store wraps SQLite WAL persistence for xbox-guard.
type Store struct {
	db *sql.DB
}

// Open initializes the SQLite database in WAL mode and runs idempotent migrations.
func Open(path string) (*Store, error) {
	db, err := sql.Open("sqlite", path)
	if err != nil {
		return nil, fmt.Errorf("open sqlite %s: %w", path, err)
	}
	db.SetMaxOpenConns(1)

	pragmas := []string{
		`PRAGMA journal_mode=WAL;`,
		`PRAGMA busy_timeout=5000;`,
		`PRAGMA synchronous=NORMAL;`,
	}
	for _, p := range pragmas {
		if _, err := db.Exec(p); err != nil {
			_ = db.Close()
			return nil, fmt.Errorf("exec pragma %q: %w", p, err)
		}
	}

	s := &Store{db: db}
	if err := s.migrate(); err != nil {
		_ = db.Close()
		return nil, err
	}
	return s, nil
}

// Close closes the underlying SQLite database handle.
func (s *Store) Close() error {
	return s.db.Close()
}

func (s *Store) migrate() error {
	const schema = `
CREATE TABLE IF NOT EXISTS policy (
	id INTEGER PRIMARY KEY CHECK (id = 1),
	daily_max_minutes INTEGER NOT NULL,
	weekday_start TEXT NOT NULL,
	weekday_end TEXT NOT NULL,
	weekend_start TEXT NOT NULL,
	weekend_end TEXT NOT NULL,
	paused INTEGER NOT NULL,
	updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS daily_usage (
	day TEXT PRIMARY KEY,
	used_sec INTEGER NOT NULL,
	bonus_minutes INTEGER NOT NULL,
	warned_15m INTEGER NOT NULL,
	warned_5m INTEGER NOT NULL,
	warned_1m INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS runtime_state (
	id INTEGER PRIMARY KEY CHECK (id = 1),
	last_heartbeat_at TEXT NOT NULL,
	last_title_id TEXT NOT NULL,
	last_mode TEXT NOT NULL,
	pending_shutdown INTEGER NOT NULL,
	req_kind TEXT NOT NULL,
	req_minutes INTEGER NOT NULL,
	req_status TEXT NOT NULL,
	req_updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS events (
	id INTEGER PRIMARY KEY AUTOINCREMENT,
	ts TEXT NOT NULL,
	kind TEXT NOT NULL,
	detail TEXT NOT NULL
);
`
	if _, err := s.db.Exec(schema); err != nil {
		return fmt.Errorf("migrate schema: %w", err)
	}

	now := time.Now().UTC().Format(time.RFC3339)
	if _, err := s.db.Exec(`
INSERT OR IGNORE INTO policy (
	id, daily_max_minutes, weekday_start, weekday_end, weekend_start, weekend_end, paused, updated_at
) VALUES (1, 90, '15:30', '20:30', '09:00', '21:30', 0, ?)`, now); err != nil {
		return fmt.Errorf("seed default policy: %w", err)
	}

	if _, err := s.db.Exec(`
INSERT OR IGNORE INTO runtime_state (
	id, last_heartbeat_at, last_title_id, last_mode, pending_shutdown, req_kind, req_minutes, req_status, req_updated_at
) VALUES (1, '', '', '', 0, '', 0, 'none', '')`); err != nil {
		return fmt.Errorf("seed runtime state: %w", err)
	}

	return nil
}

// GetPolicy loads the current parental policy.
func (s *Store) GetPolicy() (Policy, error) {
	var p Policy
	var paused int
	err := s.db.QueryRow(`
SELECT daily_max_minutes, weekday_start, weekday_end, weekend_start, weekend_end, paused, updated_at
FROM policy WHERE id = 1`).Scan(
		&p.DailyMaxMinutes,
		&p.WeekdayStart,
		&p.WeekdayEnd,
		&p.WeekendStart,
		&p.WeekendEnd,
		&paused,
		&p.UpdatedAt,
	)
	if err != nil {
		return p, fmt.Errorf("select policy: %w", err)
	}
	p.Paused = paused != 0
	return p, nil
}

// SavePolicy persists the updated policy.
func (s *Store) SavePolicy(p Policy) error {
	_, err := s.db.Exec(`
UPDATE policy SET
	daily_max_minutes = ?,
	weekday_start = ?,
	weekday_end = ?,
	weekend_start = ?,
	weekend_end = ?,
	paused = ?,
	updated_at = ?
WHERE id = 1`,
		p.DailyMaxMinutes,
		p.WeekdayStart,
		p.WeekdayEnd,
		p.WeekendStart,
		p.WeekendEnd,
		boolToInt(p.Paused),
		p.UpdatedAt,
	)
	if err != nil {
		return fmt.Errorf("update policy: %w", err)
	}
	return nil
}

// GetOrCreateUsage loads usage for day, creating a zero row if absent.
func (s *Store) GetOrCreateUsage(day string) (DailyUsage, error) {
	if _, err := s.db.Exec(`
INSERT OR IGNORE INTO daily_usage (day, used_sec, bonus_minutes, warned_15m, warned_5m, warned_1m)
VALUES (?, 0, 0, 0, 0, 0)`, day); err != nil {
		return DailyUsage{}, fmt.Errorf("insert usage %s: %w", day, err)
	}

	var u DailyUsage
	var w15, w5, w1 int
	err := s.db.QueryRow(`
SELECT day, used_sec, bonus_minutes, warned_15m, warned_5m, warned_1m
FROM daily_usage WHERE day = ?`, day).Scan(
		&u.Day,
		&u.UsedSec,
		&u.BonusMinutes,
		&w15,
		&w5,
		&w1,
	)
	if err != nil {
		return DailyUsage{}, fmt.Errorf("select usage %s: %w", day, err)
	}
	u.Warned15m = w15 != 0
	u.Warned5m = w5 != 0
	u.Warned1m = w1 != 0
	return u, nil
}

// SaveUsage persists a DailyUsage row.
func (s *Store) SaveUsage(u DailyUsage) error {
	_, err := s.db.Exec(`
INSERT INTO daily_usage (day, used_sec, bonus_minutes, warned_15m, warned_5m, warned_1m)
VALUES (?, ?, ?, ?, ?, ?)
ON CONFLICT(day) DO UPDATE SET
	used_sec = excluded.used_sec,
	bonus_minutes = excluded.bonus_minutes,
	warned_15m = excluded.warned_15m,
	warned_5m = excluded.warned_5m,
	warned_1m = excluded.warned_1m`,
		u.Day,
		u.UsedSec,
		u.BonusMinutes,
		boolToInt(u.Warned15m),
		boolToInt(u.Warned5m),
		boolToInt(u.Warned1m),
	)
	if err != nil {
		return fmt.Errorf("save usage %s: %w", u.Day, err)
	}
	return nil
}

// GetRuntime reads runtime state.
func (s *Store) GetRuntime() (RuntimeState, error) {
	var r RuntimeState
	var shutdown int
	err := s.db.QueryRow(`
SELECT last_heartbeat_at, last_title_id, last_mode, pending_shutdown, req_kind, req_minutes, req_status, req_updated_at
FROM runtime_state WHERE id = 1`).Scan(
		&r.LastHeartbeatAt,
		&r.LastTitleID,
		&r.LastMode,
		&shutdown,
		&r.ReqKind,
		&r.ReqMinutes,
		&r.ReqStatus,
		&r.ReqUpdatedAt,
	)
	if err != nil {
		return r, fmt.Errorf("select runtime state: %w", err)
	}
	r.PendingShutdown = shutdown != 0
	return r, nil
}

// SaveRuntime updates runtime state.
func (s *Store) SaveRuntime(r RuntimeState) error {
	_, err := s.db.Exec(`
UPDATE runtime_state SET
	last_heartbeat_at = ?,
	last_title_id = ?,
	last_mode = ?,
	pending_shutdown = ?,
	req_kind = ?,
	req_minutes = ?,
	req_status = ?,
	req_updated_at = ?
WHERE id = 1`,
		r.LastHeartbeatAt,
		r.LastTitleID,
		r.LastMode,
		boolToInt(r.PendingShutdown),
		r.ReqKind,
		r.ReqMinutes,
		r.ReqStatus,
		r.ReqUpdatedAt,
	)
	if err != nil {
		return fmt.Errorf("save runtime state: %w", err)
	}
	return nil
}

// AddEvent appends an audit log entry and trims history beyond 500 entries.
func (s *Store) AddEvent(ts time.Time, kind, detail string) error {
	if _, err := s.db.Exec(`INSERT INTO events (ts, kind, detail) VALUES (?, ?, ?)`,
		ts.UTC().Format(time.RFC3339), kind, detail); err != nil {
		return fmt.Errorf("insert event: %w", err)
	}
	_, _ = s.db.Exec(`DELETE FROM events WHERE id NOT IN (SELECT id FROM events ORDER BY id DESC LIMIT 500)`)
	return nil
}

// ListEvents returns up to limit most recent events, newest first.
func (s *Store) ListEvents(limit int) ([]Event, error) {
	if limit <= 0 {
		limit = 50
	}
	rows, err := s.db.Query(`SELECT id, ts, kind, detail FROM events ORDER BY id DESC LIMIT ?`, limit)
	if err != nil {
		return nil, fmt.Errorf("query events: %w", err)
	}
	defer rows.Close()

	events := make([]Event, 0, limit)
	for rows.Next() {
		var e Event
		if err := rows.Scan(&e.ID, &e.TS, &e.Kind, &e.Detail); err != nil {
			return nil, fmt.Errorf("scan event: %w", err)
		}
		events = append(events, e)
	}
	return events, rows.Err()
}

func boolToInt(b bool) int {
	if b {
		return 1
	}
	return 0
}
