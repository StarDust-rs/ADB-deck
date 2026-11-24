# Architecture

ADB Deck is one Cargo package with a library (`adbdeck`) and a binary
(`adbdeck`): a modular monolith.

```
src/
  lib.rs          crate root (no UI dependency)
  error.rs        typed Error enum used at every module boundary
  process.rs      run/spawn external programs: explicit argv, timeout, bounded output, kill-on-drop
  pty.rs          pseudo-terminal children (adb shell, adb push/pull progress)
  jobs.rs         Spawner/Job/Slot: background futures polled by the UI, abort-on-drop
  tools.rs        discovery of adb, apksigner, emulator + install hints
  store.rs        settings.toml / data.json, atomic 0600 writes, damaged file recovery
  adb/            adb adapter: one module per feature (parsers are pure functions);
                  manage.rs = per-app privileged operations, control.rs = device-wide controls,
                  devices.rs = device watcher with the Reconnector state machine,
                  discover.rs = mDNS discovery of paired phones (`adb mdns services`) and the Discoverer planner
  adb/qr.rs       QR code connection: QrSession (random name + password, `WIFI:T:ADB` payload), scan → pair → connect flow
  adb/known.rs    saved-address probing of remembered phones (endpoints, tcpip port, gateways) and its Prober planner
  daemon.rs       windowless background service (`adbdeck --daemon`): watcher + saved data, single instance per user
  autostart.rs    XDG autostart entry that starts the service at login
  retry.rs        Backoff schedules, classification of transient adb failures
  mirror/         embedded scrcpy 3.1 client: server push, reverse tunnel, H.264 decode (openh264), input
  avd.rs signing.rs webview.rs desktop.rs   other external-tool adapters
  main.rs         CLI args, tokio runtime, eframe window
  ui/             egui panels (binary only)
tests/            integration tests + fixtures/fake-adb (simulated device)
```

## Decisions

* **GUI toolkit: egui/eframe (glow, Wayland and X11 backends).** Pure Rust, no
  system toolkit packages beyond GL/XKB/Wayland libraries, works natively on
  Wayland, fast to build. Trade-off: it does not look like a Qt/KDE app (it
  follows the system light/dark preference only) and its accessibility support
  is limited to AccessKit.
* **Tokio only for process and I/O supervision.** The UI thread never awaits:
  panels start a `Job` and poll it once per frame; completion wakes the UI
  through `request_repaint`. Dropping a `Job` aborts the future, and children
  are spawned with `kill_on_drop`, so closing a panel or the app cancels work.
* **Bounded everything.** Child stdout is capped (`max_stdout`), stderr is
  tailed, line streams use bounded channels, logcat keeps 20 000 entries and
  reads at most 1 500 lines per frame, transfers keep 40 history rows.
* **No shell strings from user input.** Host programs get an argv list. Device
  commands necessarily go through the device's shell (`adb shell` joins its
  arguments), so every dynamic value is single-quoted by `sh_quote` and
  validated first (package names, device paths, endpoints, hosts, proxy,
  tcpdump filters, AVD names).
* **Device tracking** uses `adb track-devices` only as a change signal; each
  signal triggers `adb devices` + `getprop`. If the stream ends the watcher
  reconnects after a pause.
* **Wi-Fi phones** come back three ways: remembered `ip:port` ids are retried with
  backoff; every 10 s `adb mdns services` is read and any `_adb-tls-connect` phone
  that is not connected is connected (60 s pause per phone after an attempt;
  phones the user disconnected are skipped by host, since Wireless debugging
  changes its port); a phone seen under a new port replaces its remembered entry
  (matched by serial number). One setting, `auto_reconnect`, covers all three.
* **Background service** (`daemon`) is a separate, windowless process that runs `watch_devices`
  with a `reload` function. It only *reads* `data.json`/`settings.toml` (`Store::peek_*`, which never
  move a damaged file aside), re-reading them on every scan; the window is the only writer, so the two
  never overwrite each other. What the window must tell the service travels through the saved data:
  `RemoteDevice.auto_connect` is false after the user disconnected a phone on purpose. The adb server
  holds the connections, so the window finds them in place. One instance per user through a pid file in
  `$XDG_RUNTIME_DIR`; a stale file (process gone, or another program name) is replaced. The window keeps
  `~/.config/autostart/adbdeck-daemon.desktop` in step with `Settings.background_service` on every start
  and after every Settings change.
* **Saved connection details**: `RemoteDevice` keeps `id` (last `ip:port`), `endpoints` (up to 6 recent
  `ip:port`, newest first), serial number, name, Android version/SDK, `last_seen` (refreshed at most hourly
  to avoid rewriting the file) and `auto_connect`. New fields have serde defaults, so files written by
  older versions load unchanged. Phones are matched by serial number, so a new port or address updates
  the entry instead of adding one.
* **QR code connection** (`adb::qr`, `ui::qr_connect`): the window shows `WIFI:T:ADB;S:<name>;P:<password>;;` with a
  random name (`adbdeck-` + 8 characters) and password (12 characters) read from `/dev/urandom` (no weaker fallback).
  After the phone scans it, the phone advertises `_adb-tls-pairing._tcp` under that instance name; the flow polls
  `adb mdns services` for exactly that name, runs `adb pair` (password on stdin, three attempts), then waits for the
  phone's `_adb-tls-connect._tcp` service (matched by the guid adb prints or by the paired address) and connects. A
  phone adb already holds is not connected twice (the `Discoverer` planner decides). The code lives 180 s; the
  phone is paired for good even if the connection step times out. Stages travel to the UI through a `watch` channel;
  cancelling drops the job, which kills any running `adb`. On success the phone's host is released from the
  disconnected-on-purpose list and its `auto_connect` is switched back on: scanning a code is asking for the connection.
  Entry points: the "no device" screen, the device menu and the top card of Connections. The code is drawn from the
  `qrcode` crate's module matrix (dark on white in every theme, four-module quiet zone, level M).
* **Finding a phone without mDNS** (`adb::known`): a phone in `adb tcpip` mode, behind a network that
  drops multicast, or tethered over USB is looked for at its saved endpoints, `host:5555`, and
  `gateway:port` for every gateway in `/proc/net/route`. Candidates get a parallel TCP connect (800 ms)
  first so a missing address never costs an `adb connect` timeout; after `adb connect` the serial number
  must match. A phone already online (by address or by serial, e.g. plugged in) is not probed, and
  failed rounds are spaced 30 s apart per phone.
* **Interactive transfers/shell use a PTY** because adb prints progress and
  behaves like a terminal program only on a TTY.
* **Secrets**: signing passwords go to apksigner through environment
  variables (`env:NAME`), never argv; optional persistence is the desktop
  Secret Service (KWallet/GNOME Keyring), never the config files.
* **Persistence**: `~/.config/adbdeck/settings.toml`,
  `~/.local/share/adbdeck/data.json`. Writes are atomic (temp file + rename)
  with mode 0600; a file that cannot be parsed is renamed to `*.bad` and
  defaults are used with a visible warning. The RPM never touches them.
* **Dialogs** use the XDG desktop portal (`rfd`), so KDE's native file dialogs
  are used on Plasma and sandboxing rules are respected.
* **Nothing is downloaded or executed from the network**; `ureq` is built
  without TLS and only talks to `127.0.0.1` (WebView DevTools).

## Threading model

```
UI thread (eframe) ── polls Job/Slot/LineStream each frame ──┐
                                                             │ oneshot / bounded mpsc
tokio workers (3) ── spawn adb/scrcpy/apksigner, read pipes ─┘
reader thread per PTY ── bounded std→tokio channel
```


## UI design system

The look follows macOS conventions and is defined in two modules; panels contain no colours,
sizes or fonts of their own. Surfaces are neutral greys with no colour cast (window, sidebar, grouped
cards with hairline separators), the accent is the system blue, switches are 38x22 like a macOS switch,
sidebar section headers are sentence case, and controls keep the arrow cursor (a hand is not used for
buttons or rows on macOS). Text, accent and status colours were measured against the surfaces they sit
on: all pairs in use reach 4.5:1 (white on the filled accent: 4.67 light, 4.51 dark).

* `ui/theme.rs`: palettes (`DARK`, `LIGHT`) with semantic tokens (window layer, card, hairline
  strokes, one blue accent plus soft tinted success/warning/danger fills), radii 6/10/12 px,
  30 px controls, type scale in Inter (Regular/Medium/SemiBold) and JetBrains Mono, and the
  Phosphor icon font. The theme is installed per egui `Theme`, so "Follow system" switches
  live. Inter is subset without private-use glyphs so it cannot shadow Phosphor code points;
  family order is Inter, then Phosphor.
* `ui/kit.rs`: the widget kit used by every panel: `Btn` (secondary / primary / subtle /
  danger, optional icon, tooltip), cards (`card`, `card_titled`, `card_sized`, `card_pair`),
  pills, banners, empty states, search field, segmented control, toggle, slider, meters,
  stat tiles, key/value grids, dialogs, code blocks and the busy indicator.
* `ui/mod.rs`: shell with brand header, device picker popup, grouped navigation, status bar,
  toasts and a page header with title and subtitle.

Layouts adapt down to the 820x520 minimum window: toolbars switch to icon-only buttons,
the Files preview pane hides itself, and the Remote control cards stack.

### App icons and names

Android has no command that returns an app's icon or label, so they come from the Play Store through the open
source metadata API at `play.rajkumaar.co.in` (MIT; `github.com/rajkumaar23/playstore-api`).

* `appicon::PlayStore` makes the requests with the system's `curl` (no TLS stack in the program; the system's
  certificates and crypto policy apply): `GET https://play.rajkumaar.co.in/json?id=<package>` for the listing
  (`name`, `logo`), then the `logo` address for the icon. Only validated package names are sent (letters,
  digits, `.`, `_`, `-`, at most 200). The icon address is only followed when it is an `https` address on
  `play-lh.googleusercontent.com` made of plain URL characters, and curl is limited to `https`
  (`--proto =https --proto-redir =https`), three redirects, 15 s and a size cap. Status 404 and 400 mean "not on the
  Play Store"; anything else, and any curl failure, is a failed lookup.
* `appicon::fetch_batch` asks the cache first, then the service for what is missing or old, at most three requests
  at a time. Files per package under `$XDG_CACHE_HOME/adbdeck/icons/play`: `<package>.png` + `.label` (kept 30
  days; an old icon is still shown when the refresh fails), `.none` (not on the Play Store, one day), `.failed`
  (ten minutes; "Retry" ignores it).
* `ui::app_icons` queues only the rows on screen, runs one batch of nine at a time and draws the 96 px icon (a
  48 px variant in rows). The store title is used as the app's name in the details panel. Without an icon the
  letter badge stays; when no lookup of a batch got an answer the Apps panel shows a banner with Retry.
* `Settings.fetch_app_icons` (default on) switches the network lookups off; the cache is still read. Toggling it
  lets apps that had no answer ask again.

Fonts are OFL-1.1 (`assets/fonts/*LICENSE*`); the icon font comes with the `egui-phosphor` crate
(MIT). Both are declared in the RPM (`License`, `Provides: bundled(...)`, `%license`).
