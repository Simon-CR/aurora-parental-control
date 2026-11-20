# Changelog

## v0.1.0 — Milestone 1: Foundation, Toolchain & Aurora Companion Tools

### New
- **Xbox 360 Plugins (`xbox360/`)** built with open-source `landaire/xenon-forge` (no legacy Microsoft 2010 XDK required):
  - **PeerPressure Title DLL** (`screentime_dll.xex`) for per-title sideloading.
  - **DashLaunch / RGH System Module** (`screentime_sys.xex`) for persistent background enforcement across games and dashboards.
  - Native `XNotifyQueueUI` warning toasts, `XamShowMessageBoxUI` Pause / Time's Up / Ask-for-More-Time modals, local 4-button parent PIN override (`XMB_PASSCODEMODE`) when offline, clean return to dashboard, and graceful shutdown (`HalReturnToFirmware`).
- **Companion Controller (`controller/`)**:
  - Pure-Go service (`xbox-guard`) with SQLite WAL daily quota ledger, weekday & weekend play-time windows, timezone support, Pause/Resume, `+15m`/`+30m` grants, and Ask-for-More-Time workflow.
  - Home Assistant **MQTT Auto-Discovery** integration exposing controls and live telemetry entities.
  - Grounded-Earth web dashboard (Warm Sand light & Warm Espresso dark modes) with mobile-ready quick controls and compact single-line status/history tables.
- **Home Assistant Blueprint (`homeassistant/blueprints/xbox360_more_time_approval.yaml`)**:
  - Pushes actionable mobile notifications (`Approve +15m`, `Approve +30m`, `Deny`) whenever the console asks for more time.
- **Aurora Companion Tools (`tools/`)**:
  - `sideload-sync`: Generates PeerPressure `<TitleID>.ini` sideload files from Aurora's `content.db` over FTP.
  - `aurora-art-sync`: Parallel XboxUnity cover fetcher that stages missing box covers into `/Game/User/Import/<TitleID>/cover.png` over a persistent passive FTP session.
  - `aurora-categories`: One-click Genre QuickViews installer (`QuickViewGenres/Main.lua`), `Uncategorized` filter script, and read-only `content.db` genre auditor.
