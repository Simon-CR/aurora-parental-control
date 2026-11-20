# Xbox 360 ↔ `xbox-guard` LAN Protocol

The Xbox 360 client speaks plain HTTP/1.0 over TCP so it requires zero TLS stack or heap JSON parser in title/system RAM.

## Endpoints

### 1. Heartbeat Poll
`GET /api/v1/xbox/heartbeat?title_id=<8-hex>&mode=<title|system>`

Example response (`text/plain; charset=utf-8`):
```text
action=toast
message=5 minutes left - save your game
remaining_sec=300
used_sec=3300
daily_max_sec=3600
window_open=1
paused=0
req_status=none
```

If `Accept: application/json` is sent (e.g., by web UI or scripts), the same endpoint returns JSON.

#### `action` Values
| `action` | Console Behavior |
|---|---|
| `none` | Normal gameplay; no UI shown |
| `toast` | Display slide-in `XNotifyQueueUI` banner with `message` |
| `pause` | Display persistent `XamShowMessageBoxUI` Pause modal (`Check Again`, `Ask to Resume`) |
| `time_up` | Display blocking `XamShowMessageBoxUI` Time's Up modal (`Ask +15 min`, `Ask +30 min`, `Shut Down`) |
| `exit_dash` | Return cleanly to Aurora (`Title` mode) or quiesce/lock (`System` mode) |
| `shutdown` | Graceful console power-off via `HalReturnToFirmware(HalPowerDownRoutine)` |

### 2. Ask for More Time
`POST /api/v1/xbox/request-time?minutes=15` (or `minutes=30`)

Response:
```text
req_status=pending
requested_min=15
message=Request sent to parent
```

While `req_status=pending`, `xbox-guard` publishes the request to MQTT (`pending_request` sensor), allowing Home Assistant to trigger an actionable mobile push notification.

### 3. Ask to Resume
`POST /api/v1/xbox/request-resume`

Response:
```text
req_status=pending
message=Resume request sent to parent
```

---

## Home Assistant MQTT Topics

Default base prefix: `xbox_guard` (configurable with `--mqtt-prefix`).

| Entity Type | Object ID | State Topic | Command Topic |
|---|---|---|---|
| `sensor` | `state` | `xbox_guard/state` | — |
| `sensor` | `used_minutes` | `xbox_guard/used_minutes` | — |
| `sensor` | `remaining_minutes` | `xbox_guard/remaining_minutes` | — |
| `sensor` | `title_id` | `xbox_guard/title_id` | — |
| `sensor` | `pending_request` | `xbox_guard/pending_request` | — |
| `number` | `daily_max_minutes` | `xbox_guard/daily_max_minutes` | `xbox_guard/cmd/daily_max_minutes` |
| `text` | `weekday_window` | `xbox_guard/weekday_window` | `xbox_guard/cmd/weekday_window` |
| `text` | `weekend_window` | `xbox_guard/weekend_window` | `xbox_guard/cmd/weekend_window` |
| `switch` | `paused` | `xbox_guard/paused` | `xbox_guard/cmd/paused` |
| `button` | `add_15m` | — | `xbox_guard/cmd/action` (`ADD_15M`) |
| `button` | `add_30m` | — | `xbox_guard/cmd/action` (`ADD_30M`) |
| `button` | `approve` | — | `xbox_guard/cmd/action` (`APPROVE`) |
| `button` | `deny` | — | `xbox_guard/cmd/action` (`DENY`) |
| `button` | `reset_today` | — | `xbox_guard/cmd/action` (`RESET_TODAY`) |
| `button` | `shutdown` | — | `xbox_guard/cmd/action` (`SHUTDOWN`) |
