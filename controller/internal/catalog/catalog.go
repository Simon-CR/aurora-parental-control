package catalog

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"
	"unicode/utf8"
)

const (
	// MaxCatalogBytes caps incoming catalog.tsv uploads at 4 MiB.
	MaxCatalogBytes = 4 * 1024 * 1024

	emptyCatalogTSV = "SHELF_CATALOG\tv1\t0\t0\n"
)

// Status summarizes the synced Shelf launcher catalog and artwork cache.
type Status struct {
	GameCount      int    `json:"game_count"`
	PosterCount    int    `json:"poster_count"`
	BackdropCount  int    `json:"backdrop_count"`
	CatalogVersion string `json:"catalog_version"`
	UpdatedAt      string `json:"updated_at"`
}

// Store manages cached Shelf launcher metadata and resized JPEG artwork on disk.
type Store struct {
	mu      sync.RWMutex
	dir     string
	tsv     []byte
	etag    string
	games   int
	updated string
}

// Open initializes dir (creating posters/ and backdrops/ subdirectories) and loads catalog.tsv if present.
func Open(dir string) (*Store, error) {
	if strings.TrimSpace(dir) == "" {
		return nil, errors.New("catalog dir must not be empty")
	}
	absDir, err := filepath.Abs(dir)
	if err != nil {
		return nil, fmt.Errorf("resolve catalog dir %q: %w", dir, err)
	}
	for _, sub := range []string{"", "posters", "backdrops"} {
		p := filepath.Join(absDir, sub)
		if err := os.MkdirAll(p, 0o755); err != nil {
			return nil, fmt.Errorf("create catalog dir %s: %w", p, err)
		}
	}

	s := &Store{dir: absDir}
	tsvPath := filepath.Join(absDir, "catalog.tsv")
	raw, err := os.ReadFile(tsvPath)
	if err != nil {
		if !os.IsNotExist(err) {
			return nil, fmt.Errorf("read %s: %w", tsvPath, err)
		}
		s.setRawLocked([]byte(emptyCatalogTSV), "", 0)
		return s, nil
	}

	games, updated, parseErr := validateCatalogTSV(raw)
	if parseErr != nil {
		return nil, fmt.Errorf("existing catalog %s invalid: %w", tsvPath, parseErr)
	}
	info, statErr := os.Stat(tsvPath)
	if updated == "" && statErr == nil {
		updated = info.ModTime().UTC().Format(time.RFC3339)
	}
	s.setRawLocked(raw, updated, games)
	return s, nil
}

// ValidateTitleID strips an optional .jpg/.jpeg suffix and accepts strictly 8 ASCII hex digits.
func ValidateTitleID(raw string) (string, bool) {
	s := strings.TrimSpace(raw)
	if s == "" {
		return "", false
	}
	lower := strings.ToLower(s)
	if strings.HasSuffix(lower, ".jpeg") {
		s = s[:len(s)-5]
	} else if strings.HasSuffix(lower, ".jpg") {
		s = s[:len(s)-4]
	}
	if len(s) != 8 {
		return "", false
	}
	for i := 0; i < 8; i++ {
		c := s[i]
		if !((c >= '0' && c <= '9') || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F')) {
			return "", false
		}
	}
	return strings.ToUpper(s), true
}

// CatalogTSV returns a copy of the cached catalog TSV payload and its quoted ETag.
func (s *Store) CatalogTSV() ([]byte, string) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	out := make([]byte, len(s.tsv))
	copy(out, s.tsv)
	return out, s.etag
}

// SaveCatalogTSV validates data, writes catalog.tsv atomically, and updates status metadata.
func (s *Store) SaveCatalogTSV(data []byte) (Status, error) {
	games, headerUpdated, err := validateCatalogTSV(data)
	if err != nil {
		return Status{}, err
	}

	normalized := bytes.ReplaceAll(data, []byte("\r\n"), []byte("\n"))
	if len(normalized) == 0 || normalized[len(normalized)-1] != '\n' {
		normalized = append(normalized, '\n')
	}

	updatedAt := headerUpdated
	if updatedAt == "" {
		updatedAt = time.Now().UTC().Format(time.RFC3339)
	}

	s.mu.Lock()
	defer s.mu.Unlock()

	tmpPath := filepath.Join(s.dir, "catalog.tsv.tmp")
	finalPath := filepath.Join(s.dir, "catalog.tsv")
	if err := os.WriteFile(tmpPath, normalized, 0o644); err != nil {
		return Status{}, fmt.Errorf("write temp catalog: %w", err)
	}
	if err := os.Rename(tmpPath, finalPath); err != nil {
		_ = os.Remove(tmpPath)
		return Status{}, fmt.Errorf("replace catalog.tsv: %w", err)
	}

	s.setRawLocked(normalized, updatedAt, games)
	return s.statusLocked(), nil
}

// PosterPath returns the validated local JPEG path for titleID's poster thumbnail if present.
func (s *Store) PosterPath(titleID string) (string, bool) {
	return s.assetPath("posters", titleID)
}

// BackdropPath returns the validated local JPEG path for titleID's backdrop image if present.
func (s *Store) BackdropPath(titleID string) (string, bool) {
	return s.assetPath("backdrops", titleID)
}

// Status returns current catalog and local artwork cache counts.
func (s *Store) Status() Status {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.statusLocked()
}

func (s *Store) statusLocked() Status {
	version := strings.Trim(s.etag, `"`)
	return Status{
		GameCount:      s.games,
		PosterCount:    countJpegs(filepath.Join(s.dir, "posters")),
		BackdropCount:  countJpegs(filepath.Join(s.dir, "backdrops")),
		CatalogVersion: version,
		UpdatedAt:      s.updated,
	}
}

func (s *Store) setRawLocked(raw []byte, updatedAt string, games int) {
	buf := make([]byte, len(raw))
	copy(buf, raw)
	sum := sha256.Sum256(buf)
	s.tsv = buf
	s.etag = `"` + hex.EncodeToString(sum[:8]) + `"`
	s.games = games
	s.updated = updatedAt
}

func (s *Store) assetPath(subdir, rawTitleID string) (string, bool) {
	tid, ok := ValidateTitleID(rawTitleID)
	if !ok {
		return "", false
	}
	baseDir := filepath.Join(s.dir, subdir)
	for _, ext := range []string{".jpg", ".jpeg"} {
		candidate := filepath.Join(baseDir, tid+ext)
		info, err := os.Stat(candidate)
		if err == nil && !info.IsDir() {
			return candidate, true
		}
	}
	return "", false
}

func validateCatalogTSV(data []byte) (int, string, error) {
	if len(data) == 0 {
		return 0, "", errors.New("catalog body is empty")
	}
	if len(data) > MaxCatalogBytes {
		return 0, "", fmt.Errorf("catalog exceeds %d bytes", MaxCatalogBytes)
	}
	if !utf8.Valid(data) {
		return 0, "", errors.New("catalog must be valid UTF-8")
	}

	text := strings.ReplaceAll(string(data), "\r\n", "\n")
	lines := strings.Split(text, "\n")

	headerSeen := false
	updatedAt := ""
	games := 0

	for idx, rawLine := range lines {
		line := strings.TrimRight(rawLine, "\r")
		if strings.TrimSpace(line) == "" {
			continue
		}
		if !headerSeen {
			fields := strings.Split(line, "\t")
			if len(fields) < 4 || fields[0] != "SHELF_CATALOG" || fields[1] != "v1" {
				return 0, "", errors.New("missing SHELF_CATALOG v1 header")
			}
			headerSeen = true
			if unixTS, err := parsePositiveInt64(fields[2]); err == nil && unixTS > 0 {
				updatedAt = time.Unix(unixTS, 0).UTC().Format(time.RFC3339)
			}
			continue
		}

		fields := strings.Split(line, "\t")
		if len(fields) != 15 {
			return 0, "", fmt.Errorf("line %d: expected 15 TSV columns, got %d", idx+1, len(fields))
		}
		if _, ok := ValidateTitleID(fields[1]); !ok {
			return 0, "", fmt.Errorf("line %d: invalid title_id %q", idx+1, fields[1])
		}
		games++
	}

	if !headerSeen {
		return 0, "", errors.New("missing SHELF_CATALOG v1 header")
	}
	return games, updatedAt, nil
}

func parsePositiveInt64(raw string) (int64, error) {
	var n int64
	if raw == "" {
		return 0, errors.New("empty")
	}
	for i := 0; i < len(raw); i++ {
		c := raw[i]
		if c < '0' || c > '9' {
			return 0, errors.New("non-digit")
		}
		n = n*10 + int64(c-'0')
	}
	return n, nil
}

func countJpegs(dir string) int {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return 0
	}
	n := 0
	for _, entry := range entries {
		if entry.IsDir() {
			continue
		}
		lower := strings.ToLower(entry.Name())
		if strings.HasSuffix(lower, ".jpg") || strings.HasSuffix(lower, ".jpeg") {
			n++
		}
	}
	return n
}
