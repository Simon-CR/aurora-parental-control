package mqtt

import (
	"encoding/json"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/Simon-CR/aurora-parental-control/controller/internal/policy"
	"github.com/Simon-CR/aurora-parental-control/controller/internal/store"
)

type fakeTransport struct {
	mu       sync.Mutex
	retained map[string]string
	handlers map[string]func(string, []byte)
}

func newFakeTransport() *fakeTransport {
	return &fakeTransport{
		retained: make(map[string]string),
		handlers: make(map[string]func(string, []byte)),
	}
}

func (f *fakeTransport) Publish(topic string, _ byte, _ bool, payload []byte) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.retained[topic] = string(payload)
	return nil
}

func (f *fakeTransport) Subscribe(topic string, _ byte, handler func(string, []byte)) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.handlers[topic] = handler
	return nil
}

func (f *fakeTransport) Disconnect() {}

func (f *fakeTransport) inject(topic, payload string) {
	f.mu.Lock()
	h := f.handlers[topic]
	f.mu.Unlock()
	if h != nil {
		h(topic, []byte(payload))
	}
}

func TestMQTTDiscoveryAndCommands(t *testing.T) {
	s, err := store.Open(filepath.Join(t.TempDir(), "mqtt.db"))
	if err != nil {
		t.Fatalf("Open store: %v", err)
	}
	defer s.Close()

	loc, _ := time.LoadLocation("America/Toronto")
	clk := time.Date(2026, 11, 21, 12, 0, 0, 0, loc)
	eng := policy.New(s, loc, func() time.Time { return clk })

	tr := newFakeTransport()
	b := NewBridge(Config{
		Prefix:     "xbox_guard",
		DeviceID:   "xbox360_test",
		DeviceName: "Xbox 360 Test",
	}, eng, tr)

	eng.SetOnChange(func() {
		_ = b.PublishState(false)
	})

	if err := b.Start(); err != nil {
		t.Fatalf("Start: %v", err)
	}

	// Verify all 15 HA discovery configs were published.
	tr.mu.Lock()
	discCount := 0
	for topic, raw := range tr.retained {
		if strings.HasPrefix(topic, "homeassistant/") && strings.HasSuffix(topic, "/config") {
			discCount++
			var obj map[string]any
			if err := json.Unmarshal([]byte(raw), &obj); err != nil {
				t.Fatalf("invalid discovery json on %s: %v", topic, err)
			}
		}
	}
	tr.mu.Unlock()
	if discCount != 15 {
		t.Fatalf("expected 15 discovery configs, got %d", discCount)
	}

	// Command pause ON via MQTT.
	tr.inject("xbox_guard/cmd/paused", "ON")
	if got := tr.retained["xbox_guard/paused"]; got != "ON" {
		t.Fatalf("expected paused ON, got %q", got)
	}

	// Request +15m from console -> verify pending_request topic -> approve via MQTT.
	if err := eng.RequestTime(15); err != nil {
		t.Fatalf("RequestTime: %v", err)
	}
	if got := tr.retained["xbox_guard/pending_request"]; got != "+15m" {
		t.Fatalf("expected +15m pending_request, got %q", got)
	}
	tr.inject("xbox_guard/cmd/action", "APPROVE")
	if got := tr.retained["xbox_guard/pending_request"]; got != "none" {
		t.Fatalf("expected none after APPROVE, got %q", got)
	}
	snap, _ := eng.Snapshot()
	if snap.BonusMinutes != 15 {
		t.Fatalf("expected 15 bonus minutes after APPROVE, got %d", snap.BonusMinutes)
	}
}
