package main

import (
	"context"
	"flag"
	"log"
	"net/http"
	"os"
	"os/signal"
	"syscall"
	"time"

	"github.com/Simon-CR/aurora-parental-control/controller/internal/httpapi"
	"github.com/Simon-CR/aurora-parental-control/controller/internal/mqtt"
	"github.com/Simon-CR/aurora-parental-control/controller/internal/policy"
	"github.com/Simon-CR/aurora-parental-control/controller/internal/store"
)

func envOr(key, fallback string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return fallback
}

func main() {
	var (
		listenAddr = flag.String("listen", envOr("XBOX_GUARD_LISTEN", ":8420"), "HTTP listen address")
		dbPath     = flag.String("db", envOr("XBOX_GUARD_DB", "./xbox-guard.db"), "SQLite WAL database path")
		tzName     = flag.String("tz", envOr("XBOX_GUARD_TZ", "America/Toronto"), "IANA timezone name")
		mqttBroker = flag.String("mqtt-broker", envOr("XBOX_GUARD_MQTT_BROKER", ""), "MQTT broker URL (optional)")
		mqttUser   = flag.String("mqtt-user", envOr("XBOX_GUARD_MQTT_USER", ""), "MQTT username")
		mqttPass   = flag.String("mqtt-pass", envOr("XBOX_GUARD_MQTT_PASS", ""), "MQTT password")
		mqttPrefix = flag.String("mqtt-prefix", envOr("XBOX_GUARD_MQTT_PREFIX", "xbox_guard"), "MQTT base topic prefix")
		deviceID   = flag.String("device-id", envOr("XBOX_GUARD_DEVICE_ID", "xbox360_living_room"), "Home Assistant device ID")
		deviceName = flag.String("device-name", envOr("XBOX_GUARD_DEVICE_NAME", "Xbox 360"), "Home Assistant device name")
	)
	flag.Parse()

	loc, err := time.LoadLocation(*tzName)
	if err != nil {
		log.Fatalf("load timezone %q: %v", *tzName, err)
	}

	st, err := store.Open(*dbPath)
	if err != nil {
		log.Fatalf("open db: %v", err)
	}
	defer st.Close()

	eng := policy.New(st, loc, time.Now)

	var mqttTransport mqtt.Transport
	if *mqttBroker != "" {
		cfg := mqtt.Config{
			BrokerURL:  *mqttBroker,
			Username:   *mqttUser,
			Password:   *mqttPass,
			Prefix:     *mqttPrefix,
			DeviceID:   *deviceID,
			DeviceName: *deviceName,
		}
		var bridge *mqtt.Bridge
		tr, err := mqtt.NewPahoTransport(cfg, func() {
			if bridge != nil {
				_ = bridge.PublishDiscovery()
				_ = bridge.PublishState(true)
			}
		})
		if err != nil {
			log.Fatalf("connect mqtt %s: %v", *mqttBroker, err)
		}
		mqttTransport = tr
		bridge = mqtt.NewBridge(cfg, eng, tr)
		eng.SetOnChange(func() {
			_ = bridge.PublishState(false)
		})
		if err := bridge.Start(); err != nil {
			log.Fatalf("start mqtt bridge: %v", err)
		}
		log.Printf("MQTT auto-discovery published under %s/* (device=%s)", *mqttPrefix, *deviceID)
	}

	srv := &http.Server{
		Addr:              *listenAddr,
		Handler:           httpapi.NewMux(eng),
		ReadHeaderTimeout: 5 * time.Second,
	}

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	go func() {
		<-ctx.Done()
		shCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		_ = srv.Shutdown(shCtx)
		if mqttTransport != nil {
			mqttTransport.Disconnect()
		}
	}()

	log.Printf("xbox-guard listening on %s (tz=%s db=%s)", *listenAddr, *tzName, *dbPath)
	if err := srv.ListenAndServe(); err != nil && err != http.ErrServerClosed {
		log.Fatalf("http server: %v", err)
	}
}
