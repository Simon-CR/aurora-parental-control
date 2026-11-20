package mqtt

import (
	"encoding/json"
	"fmt"
	"strconv"
	"strings"
	"sync"
	"time"

	gomqtt "github.com/eclipse/paho.mqtt.golang"

	"github.com/Simon-CR/aurora-parental-control/controller/internal/policy"
)

// Transport abstracts an MQTT broker connection for testing without a live broker.
type Transport interface {
	Publish(topic string, qos byte, retained bool, payload []byte) error
	Subscribe(topic string, qos byte, handler func(topic string, payload []byte)) error
	Disconnect()
}

// Config controls MQTT broker connection and Home Assistant auto-discovery naming.
type Config struct {
	BrokerURL  string
	Username   string
	Password   string
	Prefix     string
	DeviceID   string
	DeviceName string
}

// Bridge publishes xbox-guard state to Home Assistant and processes control commands.
type Bridge struct {
	cfg       Config
	engine    *policy.Engine
	transport Transport
	mu        sync.Mutex
	lastState map[string]string
}

// NewPahoTransport dials an MQTT broker using Paho MQTT and waits cleanly for CONNACK.
func NewPahoTransport(cfg Config, onConnect func()) (Transport, error) {
	opts := gomqtt.NewClientOptions().
		AddBroker(cfg.BrokerURL).
		SetClientID(fmt.Sprintf("%s-%d", cfg.DeviceID, time.Now().Unix())).
		SetAutoReconnect(true).
		SetConnectRetry(true).
		SetConnectRetryInterval(5 * time.Second).
		SetWill(cfg.Prefix+"/availability", "offline", 1, true)
	if cfg.Username != "" {
		opts.SetUsername(cfg.Username)
	}
	if cfg.Password != "" {
		opts.SetPassword(cfg.Password)
	}
	if onConnect != nil {
		opts.SetOnConnectHandler(func(_ gomqtt.Client) { onConnect() })
	}

	client := gomqtt.NewClient(opts)
	tok := client.Connect()
	if !tok.WaitTimeout(10 * time.Second) {
		return nil, fmt.Errorf("mqtt connect timeout")
	}
	if err := tok.Error(); err != nil {
		return nil, fmt.Errorf("mqtt connect: %w", err)
	}
	return &pahoTransport{client: client}, nil
}

type pahoTransport struct {
	client gomqtt.Client
}

func (p *pahoTransport) Publish(topic string, qos byte, retained bool, payload []byte) error {
	tok := p.client.Publish(topic, qos, retained, payload)
	if !tok.WaitTimeout(5 * time.Second) {
		return fmt.Errorf("publish timeout on %s", topic)
	}
	return tok.Error()
}

func (p *pahoTransport) Subscribe(topic string, qos byte, handler func(string, []byte)) error {
	tok := p.client.Subscribe(topic, qos, func(_ gomqtt.Client, msg gomqtt.Message) {
		handler(msg.Topic(), msg.Payload())
	})
	if !tok.WaitTimeout(5 * time.Second) {
		return fmt.Errorf("subscribe timeout on %s", topic)
	}
	return tok.Error()
}

func (p *pahoTransport) Disconnect() {
	p.client.Disconnect(500)
}

// NewBridge wires policy.Engine and an MQTT Transport.
func NewBridge(cfg Config, engine *policy.Engine, transport Transport) *Bridge {
	if cfg.Prefix == "" {
		cfg.Prefix = "xbox_guard"
	}
	if cfg.DeviceID == "" {
		cfg.DeviceID = "xbox360_living_room"
	}
	if cfg.DeviceName == "" {
		cfg.DeviceName = "Xbox 360"
	}
	return &Bridge{
		cfg:       cfg,
		engine:    engine,
		transport: transport,
		lastState: make(map[string]string),
	}
}

// Start subscribes command topics, publishes HA auto-discovery configs, and syncs state.
func (b *Bridge) Start() error {
	prefix := b.cfg.Prefix
	subs := map[string]func([]byte) error{
		prefix + "/cmd/daily_max_minutes": func(p []byte) error {
			m, err := strconv.Atoi(strings.TrimSpace(string(p)))
			if err != nil {
				return err
			}
			return b.engine.SetDailyMax(m)
		},
		prefix + "/cmd/weekday_window": func(p []byte) error {
			return b.engine.SetWeekdayWindow(string(p))
		},
		prefix + "/cmd/weekend_window": func(p []byte) error {
			return b.engine.SetWeekendWindow(string(p))
		},
		prefix + "/cmd/paused": func(p []byte) error {
			v := strings.ToUpper(strings.TrimSpace(string(p)))
			return b.engine.SetPaused(v == "ON" || v == "TRUE" || v == "1")
		},
		prefix + "/cmd/action": func(p []byte) error {
			return b.handleAction(string(p))
		},
	}

	for topic, fn := range subs {
		handler := fn
		if err := b.transport.Subscribe(topic, 1, func(_ string, payload []byte) {
			_ = handler(payload)
		}); err != nil {
			return fmt.Errorf("subscribe %s: %w", topic, err)
		}
	}

	if err := b.PublishDiscovery(); err != nil {
		return err
	}
	_ = b.transport.Publish(prefix+"/availability", 1, true, []byte("online"))
	return b.PublishState(true)
}

func (b *Bridge) handleAction(raw string) error {
	switch strings.ToUpper(strings.TrimSpace(raw)) {
	case "ADD_15M":
		return b.engine.AddBonus(15)
	case "ADD_30M":
		return b.engine.AddBonus(30)
	case "APPROVE":
		return b.engine.Approve()
	case "DENY":
		return b.engine.Deny()
	case "RESET_TODAY":
		return b.engine.ResetToday()
	case "SHUTDOWN":
		return b.engine.TriggerShutdown()
	default:
		return fmt.Errorf("unknown action %q", raw)
	}
}

type haDevice struct {
	Identifiers  []string `json:"identifiers"`
	Name         string   `json:"name"`
	Manufacturer string   `json:"manufacturer"`
	Model        string   `json:"model"`
}

// PublishDiscovery publishes retained Home Assistant MQTT discovery payloads for all entities.
func (b *Bridge) PublishDiscovery() error {
	dev := haDevice{
		Identifiers:  []string{b.cfg.DeviceID},
		Name:         b.cfg.DeviceName,
		Manufacturer: "Simon-CR",
		Model:        "Aurora Parental Control",
	}
	avail := b.cfg.Prefix + "/availability"
	p := b.cfg.Prefix

	type entitySpec struct {
		component string
		objectID  string
		fields    map[string]any
	}

	specs := []entitySpec{
		{"sensor", "state", map[string]any{
			"name": "Screen Time State", "state_topic": p + "/state", "icon": "mdi:controller",
		}},
		{"sensor", "used_minutes", map[string]any{
			"name": "Used Time Today", "state_topic": p + "/used_minutes",
			"unit_of_measurement": "min", "state_class": "total_increasing", "icon": "mdi:timer-outline",
		}},
		{"sensor", "remaining_minutes", map[string]any{
			"name": "Remaining Time Today", "state_topic": p + "/remaining_minutes",
			"unit_of_measurement": "min", "icon": "mdi:timer-sand",
		}},
		{"sensor", "title_id", map[string]any{
			"name": "Active Title ID", "state_topic": p + "/title_id", "icon": "mdi:gamepad-variant",
		}},
		{"sensor", "pending_request", map[string]any{
			"name": "Pending Request", "state_topic": p + "/pending_request", "icon": "mdi:hand-back-right",
		}},
		{"number", "daily_max_minutes", map[string]any{
			"name": "Daily Limit", "state_topic": p + "/daily_max_minutes",
			"command_topic": p + "/cmd/daily_max_minutes", "min": 0, "max": 720, "step": 5,
			"unit_of_measurement": "min", "mode": "box", "icon": "mdi:clock-edit-outline",
		}},
		{"text", "weekday_window", map[string]any{
			"name": "Weekday Play Window", "state_topic": p + "/weekday_window",
			"command_topic": p + "/cmd/weekday_window", "pattern": `^\d{2}:\d{2}-\d{2}:\d{2}$`,
			"icon": "mdi:calendar-week",
		}},
		{"text", "weekend_window", map[string]any{
			"name": "Weekend Play Window", "state_topic": p + "/weekend_window",
			"command_topic": p + "/cmd/weekend_window", "pattern": `^\d{2}:\d{2}-\d{2}:\d{2}$`,
			"icon": "mdi:calendar-weekend",
		}},
		{"switch", "paused", map[string]any{
			"name": "Paused", "state_topic": p + "/paused",
			"command_topic": p + "/cmd/paused", "payload_on": "ON", "payload_off": "OFF",
			"icon": "mdi:pause-circle-outline",
		}},
		{"button", "add_15m", map[string]any{
			"name": "Grant +15 Minutes", "command_topic": p + "/cmd/action",
			"payload_press": "ADD_15M", "icon": "mdi:plus-circle-outline",
		}},
		{"button", "add_30m", map[string]any{
			"name": "Grant +30 Minutes", "command_topic": p + "/cmd/action",
			"payload_press": "ADD_30M", "icon": "mdi:plus-circle",
		}},
		{"button", "approve", map[string]any{
			"name": "Approve Request", "command_topic": p + "/cmd/action",
			"payload_press": "APPROVE", "icon": "mdi:check-circle-outline",
		}},
		{"button", "deny", map[string]any{
			"name": "Deny Request", "command_topic": p + "/cmd/action",
			"payload_press": "DENY", "icon": "mdi:close-circle-outline",
		}},
		{"button", "reset_today", map[string]any{
			"name": "Reset Today's Timer", "command_topic": p + "/cmd/action",
			"payload_press": "RESET_TODAY", "icon": "mdi:restore",
		}},
		{"button", "shutdown", map[string]any{
			"name": "Shut Down Console", "command_topic": p + "/cmd/action",
			"payload_press": "SHUTDOWN", "icon": "mdi:power",
		}},
	}

	for _, s := range specs {
		uid := fmt.Sprintf("%s_%s", b.cfg.DeviceID, s.objectID)
		payloadMap := map[string]any{
			"unique_id":          uid,
			"default_entity_id":  fmt.Sprintf("%s.%s", s.component, uid),
			"availability_topic": avail,
			"device":             dev,
		}
		for k, v := range s.fields {
			payloadMap[k] = v
		}
		data, err := json.Marshal(payloadMap)
		if err != nil {
			return err
		}
		topic := fmt.Sprintf("homeassistant/%s/%s/config", s.component, uid)
		if err := b.transport.Publish(topic, 1, true, data); err != nil {
			return fmt.Errorf("publish discovery %s: %w", topic, err)
		}
	}
	return nil
}

// PublishState pushes updated state topics when values change or force is true.
func (b *Bridge) PublishState(force bool) error {
	snap, err := b.engine.Snapshot()
	if err != nil {
		return err
	}
	pausedVal := "OFF"
	if snap.Paused {
		pausedVal = "ON"
	}

	values := map[string]string{
		b.cfg.Prefix + "/state":             snap.State,
		b.cfg.Prefix + "/used_minutes":      strconv.Itoa(snap.UsedMinutes),
		b.cfg.Prefix + "/remaining_minutes": strconv.Itoa(snap.RemainingMinutes),
		b.cfg.Prefix + "/title_id":          snap.TitleID,
		b.cfg.Prefix + "/pending_request":   snap.PendingRequest,
		b.cfg.Prefix + "/daily_max_minutes": strconv.Itoa(snap.DailyMaxMinutes),
		b.cfg.Prefix + "/weekday_window":    snap.WeekdayWindow,
		b.cfg.Prefix + "/weekend_window":    snap.WeekendWindow,
		b.cfg.Prefix + "/paused":            pausedVal,
	}

	b.mu.Lock()
	defer b.mu.Unlock()
	for topic, val := range values {
		if !force && b.lastState[topic] == val {
			continue
		}
		if err := b.transport.Publish(topic, 1, true, []byte(val)); err != nil {
			return err
		}
		b.lastState[topic] = val
	}
	return nil
}
