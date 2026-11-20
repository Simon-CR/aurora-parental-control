# Architecture

## Design Principles

1. **Keep the Console Thin and Stateless**
   The Xbox 360 has no battery-backed RTC when Xbox Live time sync is blocked, and editing local config files on a USB drive is easy to bypass. Therefore, the `.xex` plugin does **not** hold schedules or daily usage totals—it only polls `xbox-guard` over the LAN and renders native Xbox 360 UI (`XNotifyQueueUI` and `XamShowMessageBoxUI`).
2. **All Schedules & Accounting Live on `xbox-guard`**
   `xbox-guard` owns the real wall clock (in your configured IANA timezone), SQLite WAL daily usage ledger, weekday/weekend play-time windows, warning deduplication (`15m`, `5m`, `1m`), and Ask-for-More-Time pending approval state.
3. **Dotted Line to Home Assistant via MQTT Auto-Discovery**
   `xbox-guard` publishes standard Home Assistant MQTT discovery payloads (`homeassistant/number/...`, `text/...`, `switch/...`, `button/...`, `sensor/...`) so Home Assistant gets first-class UI controls and actionable mobile notifications without embedding schedule state machines inside HA YAML.
4. **Fail-Closed Without Locking Out Parents**
   If the Xbox 360 loses LAN connectivity to `xbox-guard` for longer than `offline_grace_sec` (e.g., Ethernet cable unplugged), the plugin displays a blocking offline lock dialog. Parents can either reconnect the network or choose **Parent PIN** (`XMB_PASSCODEMODE`) using the 4-button controller code in `screentime.cfg`.

---

## PeerPressure vs. RGH / DashLaunch Modes

| Capability | PeerPressure Title DLL (`screentime_dll.xex`) | DashLaunch / RGH System Module (`screentime_sys.xex`) |
|---|---|---|
| Loaded via | `SideLoad.ini` / `<TitleID>.ini` | `launch.ini` (`plugin1 = ...`) |
| Memory space | Title process (`0x88000000`) | System process (`0x91c00000`, 4 KB pages) |
| Persistence | Reloaded on each game/dashboard launch | Stays resident across dashboard and games |
| `exit_dash` action | `XamLoaderLaunchTitle(NULL, 0)` | `HalReturnToFirmware(RebootQuiesce)` or Time's Up modal |
| `shutdown` action | `HalReturnToFirmware(HalPowerDownRoutine)` | `HalReturnToFirmware(HalPowerDownRoutine)` |
| Networking caller | `XNCALLER_TITLE` (`SO_INSECURE`) | `XNCALLER_SYSAPP` (`std::net::TcpStream` + `SO_INSECURE`) |
