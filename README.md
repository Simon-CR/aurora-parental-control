# Aurora Parental Control (Xbox 360)

Native Xbox 360 screen-time enforcement, in-game notifications, remote Pause/Resume, and **Ask for More Time** mobile approvals for **Aurora Dashboard** — built with the open-source [`landaire/xenon-forge`](https://github.com/landaire/xenon-forge) toolchain (no Microsoft 2010 XDK required) and integrated with **Home Assistant** over MQTT Auto-Discovery.

---

## Features

- **Native In-Game Xbox 360 UI**
  - Slide-in Xbox 360 toast banners (`15 min left`, `5 min left — save your game`, `+15 min approved!`).
  - Native Xbox 360 modal dialogs for **Pause**, **Time's Up**, and **Ask for More Time** (`Ask +15 min`, `Ask +30 min`, `Shut Down`).
  - **Fail-Closed + Parent PIN Fallback**: Unplugging Ethernet triggers an offline lock screen after a configurable grace window, with a local 4-button controller passcode fallback (`parent_pin`) so parents are never locked out if the server is offline.
- **Supports Both Mod Types**
  - **PeerPressure Title DLL** (`xbox360/dll`, loaded per game via `SideLoad.ini` / `<TitleID>.ini`).
  - **DashLaunch / RGH3 / ABadAvatar System Module** (`xbox360/system-module`, loaded globally at boot via `launch.ini`).
- **Companion Controller + Grounded-Earth Web UI (`controller/`)**
  - Pure-Go daemon (`CGO_ENABLED=0`) + SQLite WAL daily usage ledger.
  - Configurable **Daily Max** minutes, **Weekday** and **Weekend** play-time windows (with IANA timezone support), **Pause / Resume**, **+15 / +30 min bonus**, and **Reset Today**.
  - Warm Sand / Warm Espresso Grounded-Earth web dashboard for quick phone or desktop control.
- **Home Assistant Dotted Line (MQTT Auto-Discovery)**
  - Auto-registers a single **Xbox 360 Parental Control** device with `number`, `text`, `switch`, `button`, and `sensor` entities.
  - Includes a ready-to-import Home Assistant blueprint ([`homeassistant/blueprints/xbox360_more_time_approval.yaml`](homeassistant/blueprints/xbox360_more_time_approval.yaml)) for actionable mobile phone notifications (`Approve +15m`, `Approve +30m`, `Deny`).
- **Aurora Companion Utilities (`tools/`) & Custom Plex-Style Launcher (`xbox360/launcher/`)**
  - `xbox360/launcher/` (**Shelf** — `shelf_launcher.xex`): Open-source, Grounded-Earth Warm Espresso Plex-style Xbox 360 dashboard built on `xenon-forge` with a persistent 14-genre category sidebar, 5-column poster wall, hero backdrop strip, offline `Hdd1:` thumbnail cache, and built-in **Xbox Guard** screen-time lock/PIN integration. See [`docs/LAUNCHER.md`](docs/LAUNCHER.md).
  - `tools/launcher-sync/`: Safely reads Aurora's `content.db`, generates `catalog.tsv` plus pre-sized `180x248` posters & `640x360` backdrops, pushes updates to `xbox-guard`, and optionally seeds `Hdd1:` over FTP.
  - `tools/sideload-sync/`: Auto-generate PeerPressure `<TitleID>.ini` sideload files for Aurora + your entire game library over FTP.
  - `tools/aurora-art-sync/`: Parallel XboxUnity boxart pre-downloader that stages missing covers into Aurora's `/Game/User/Import/<TitleID>/cover.png` directory so Aurora imports hundreds of covers in seconds instead of stalling on its single-threaded downloader.
  - `tools/aurora-categories/`: One-click **Genre QuickViews** Aurora Utility Lua script + `Uncategorized` filter + `content.db` genre auditor so large libraries (500+ games) can be browsed cleanly by category instead of scrolling through one huge horizontal list.

---

## Architecture Overview

```text
+-------------------------------------------------------------------+
| Home Assistant                                                    |
|  - MQTT Auto-Discovery (Daily Max, Windows, Pause, Bonus, State)  |
|  - Mobile Actionable Notifications ("Approve +15m / +30m / Deny") |
+-------------------------------^-----------------------------------+
                                | MQTT
                                v
+-------------------------------------------------------------------+
| xbox-guard Controller (Go + SQLite WAL + Web UI)                  |
|  - Real wall clock & timezone-aware daily ledger                  |
|  - Play-time window & daily quota evaluator                       |
|  - LAN HTTP API (/api/v1/xbox/heartbeat, /request-time, etc.)     |
+-------------------------------^-----------------------------------+
                                | LAN HTTP (key=value)
                                v
+-------------------------------------------------------------------+
| Xbox 360 Plugin (screentime_dll.xex / screentime_sys.xex)         |
|  - Polls xbox-guard on a background thread                        |
|  - Shows XNotifyQueueUI toasts & XamShowMessageBoxUI prompts      |
|  - Returns to Aurora or cleanly powers down when time expires     |
+-------------------------------------------------------------------+
```

For full protocol details, see [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) and [`docs/PROTOCOL.md`](docs/PROTOCOL.md).

---

## Quick Start

### 1. Run `xbox-guard` Controller

```bash
cd controller
go test -race ./...
go build -o xbox-guard ./cmd/xbox-guard
./xbox-guard --listen :8420 --db ./xbox-guard.db --tz America/Toronto \
  --mqtt-broker tcp://192.168.1.50:1883 --mqtt-user homeassistant --mqtt-pass secret
```

Open `http://<server-ip>:8420` to adjust the daily limit, weekday/weekend windows, or approve requests from your browser.

### 2. Import the Home Assistant Blueprint

Import [`homeassistant/blueprints/xbox360_more_time_approval.yaml`](homeassistant/blueprints/xbox360_more_time_approval.yaml) in **Settings → Automations & Scenes → Blueprints** to get instant mobile push notifications when the console asks for more time.

### 3. Build the Xbox 360 `.xex` Plugins (No Microsoft XDK Required)

On Ubuntu 24.04 (or Windows with Visual Studio Build Tools + `clang`):

```bash
cd xbox360
./bootstrap.sh
./bin/buck2 build -m release //dll:screentime_dll //system-module:screentime_sys //launcher:shelf_launcher --show-output
```

See [`docs/LAUNCHER.md`](docs/LAUNCHER.md) for setting up **Shelf** (`shelf_launcher.xex`) safely alongside Aurora.

Copy [`xbox360/screentime.cfg.example`](xbox360/screentime.cfg.example) to `screentime.cfg`, set `server_ip` to your `xbox-guard` host IP, and deploy:
- **RGH / JTAG / ABadAvatar (`launch.ini`)**: place `screentime_sys.xex` + `screentime.cfg` on `Hdd1:\` and load `screentime_sys.xex` as a DashLaunch plugin (`plugin1 = Hdd1:\screentime_sys.xex`).
- **PeerPressure (`SideLoad.ini`)**: place `screentime_dll.xex` + `screentime.cfg` in `Hdd1:\PeerPressure\SideLoading\` and run `python tools/sideload-sync/sideload_sync.py --host <xbox-ip> --apply` to generate `<TitleID>.ini` manifests for your scanned titles.

> **Important:** Always test on a single `TitleID` first before deploying across your full library.

---

## Speeding Up Aurora Artwork & Browsing by Category

### Bulk Artwork Sync (`aurora-art-sync`)
Aurora's built-in asset queue downloads assets one at a time inside the dashboard. `aurora_art_sync.py` downloads **covers, backgrounds, icons, banners, and screenshots** (excluding videos) in parallel from XboxUnity and the Xbox CDN:

```bash
# Pack native .asset containers directly into /Game/Data/GameData/ (Windows with asset_packer)
python tools/aurora-art-sync/aurora_art_sync.py --host <xbox-ip> --mode asset --packer aurora_asset_packer.exe --apply

# Or target newly installed TitleID(s) only without scanning the full library
python tools/aurora-art-sync/aurora_art_sync.py --host <xbox-ip> --title-id 45410912,4D5307E6 --mode asset --packer aurora_asset_packer.exe --apply

# Or stage raw images into /Game/User/Import/<TitleID>/ for Settings -> Assets -> Import
python tools/aurora-art-sync/aurora_art_sync.py --host <xbox-ip> --mode import --apply
```

### Browsing Large Libraries by Category (`aurora-categories`)
Aurora natively supports **QuickViews** (`LB` / `RB`), **Grid / Multi-Row view** (`B` → `View`), and **A–Z letter jumping** (`LT` / `RT`). To add 1-click **Genre QuickViews** (`Family`, `Action & Adventure`, `Racing & Flying`, `Shooter`, `Platformer`, `Sports & Recreation`, etc.) plus an `Uncategorized` filter:

1. Copy `tools/aurora-categories/QuickViewGenres/Main.lua` to `Game:\User\Scripts\Utility\QuickViewGenres\Main.lua`.
2. Copy `tools/aurora-categories/Filters/Uncategorized.lua` to `Game:\User\Scripts\Content\Filters\Uncategorized.lua`.
3. In Aurora press **Back → Scripts → Utility → Genre QuickView Installer** and click **Install All Genre QuickViews (+ Uncategorized)**, then allow Aurora to reload.

---

## License

[MIT](LICENSE)
