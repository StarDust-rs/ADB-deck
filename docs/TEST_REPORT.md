# Test report

Host: Linux container (Ubuntu userland, Rust 1.97, 2 cores). Not Fedora, not KDE.

## Background reconnect pass (not verified)

This pass (saved connection details, `--daemon`, autostart, saved-address probing, offline-entry fix)
was written in a container with no Rust toolchain and no network. **Nothing below was compiled, linted,
formatted or run**: `cargo fmt`, `cargo check`, `cargo clippy` and `cargo test` must be run before the
tables that follow can be trusted for the new code. New tests: `adb::known` (route parser, candidates,
planner, TCP probe), `daemon` (pid file, switch-off), `autostart` (entry, sync), `store` (old records,
peeking), `ui::devices` (endpoints, last seen, auto-connect), `adb::devices` (`is_online`, registry) and
three fake-device integration tests. No real phone, USB tethering, Wayland session or reboot was tried.

## Fedora 44/45 packaging pass (not verified)

The spec, `build-rpm.sh`, the AppStream file, the man page, the systemd unit and the workflow were rewritten for
Fedora 44 and 45 (see `docs/PACKAGING_REPORT.md`). Nothing was built: no `rpmbuild`, `cargo-rpm-macros`, `rpmlint`,
`appstreamcli`, `desktop-file-validate`, `dnf` or Fedora container was available. Checked here: shell syntax, Python
compilation, the version and metainfo stamping on a copy, XML and YAML parsing.

## Release workflow (partly verified)

Run in the sandbox and passing: `packaging/set-version.py` (stamps Cargo.toml and Cargo.lock, changes
nothing else, rejects bad input), `packaging/release-notes.sh` against a throwaway repository (grouping,
merge commits skipped, first release, a tag with no new commits, previous-tag detection), and a YAML parse
of `.github/workflows/release.yml`. **Not run**: the workflow itself on GitHub, the Fedora container
build, `dnf install` of the RPM, the aarch64 runner, publishing to a release. The workflow has not been
linted with actionlint.

## UI polish pass (not verified)

Also written without a toolchain: not compiled, not rendered, not seen on screen. Contrast ratios of the
new palette were computed (script, WCAG formula) and are the only thing in this pass that was actually
measured. Window sizes (820x520 minimum), text scaling, keyboard focus and the light/dark switch were
not inspected.

## Static checks (run on the final tree, before the background reconnect pass)

| Check | Result |
|---|---|
| `cargo fmt --check` | pass |
| `cargo check --all-targets` | pass |
| `cargo clippy --all-targets --all-features -- -D warnings` | pass, no warnings |
| `cargo test` (debug), second pass | pass: 130 lib unit + 9 bin unit + 3 config/tools + 19 fake-device integration = 161 tests (fourth pass) |
| `cargo build --release`, second pass | see the end of this section |
| `cargo test --release`, RPM build, `cargo audit` | run on the first-pass tree only (144 tests, 2 advisories + 9 "unmaintained" warnings). **Not re-run after the second pass**, which added `flate2` as a direct dependency and the `image` `webp` feature (pulls in `image-webp`) |

### cargo audit findings (first-pass tree)

* `quick-xml 0.30.0` RUSTSEC-2026-0194 / -0195 (DoS on crafted XML). Reached only through
  `eframe → accesskit_unix → atspi-common → zbus-lockstep(-macros) → zbus_xml`.
  In `atspi-common` the `zbus_lockstep` library is referenced only under `#[cfg(test)]`; the
  proc-macro parses the crate's own bundled XML at compile time. We have no evidence it runs in
  the shipped binary, but it is not removable without dropping the `accesskit` feature or
  upgrading eframe. Left as is and disclosed.
* Unmaintained warnings: `paste`, `serial` (via portable-pty), `ttf-parser`, `zbus_names`,
  `zvariant`, `zvariant_derive`. No fix available from this package.

## Automated tests

Unit tests cover: adb output parsers (devices, props, overview, ls, packages/dumpsys, top, perf,
logcat, uiautomator XML, tcpdump pcap, unix sockets, DevTools JSON), `sh_quote` and every input
validator, argument construction for adb/apksigner/emulator, config load/save/damage,
key translation for the terminal, bounded buffers.

Integration tests (`tests/fake_device.rs`, `tests/config_and_tools.rs`) run the real adapters
against `tests/fixtures/fake-adb` (a shell script that simulates adb and an Android shell with
path mapping into a temp directory) and fake apksigner (and a fake scrcpy server speaking the real wire protocol in the mirror tests):

* missing adb with install guidance; spawn failure; non-zero exit; invalid/garbage output
* device vanishing and unauthorized; adb timeout kills the child; job cancellation kills the child;
  dropped logcat stream kills the child; watcher reconnects after the track stream ends;
  runtime shutdown during an active operation
* describe device, overview, apps, processes, performance, app actions, install success/failure,
  file operations and PTY transfers with progress, screenshot/layout/capture, ports/connect/pair
* damaged settings file → defaults and `.bad` backup; unwritable config location

## Manual GUI checks

Performed under Xvfb (1280x800, software GL, X11 backend) with the fake adb, by driving the real
window with xdotool and reading screenshots. Every panel (overview, files, apps, processes,
performance, shell, layout, screenshot, logcat, capture, WebView, remote, mirror, connections,
virtual devices, signing, settings, about) was captured in **both dark and light themes** at
1180x760 after the UI redesign, and Files, Remote control and Logcat were re-checked at the
820x520 minimum size. This found and fixed: clipped toolbars (Files, Logcat), overflowing
Remote control cards, an overlapping status bar, and an over-wide Files preview pane.
The device picker popup (dark) and the New folder dialog (light) were captured after the restyle;
toast notifications and the remaining dialogs were not visually re-checked.

**Not tested:** KDE Plasma, Wayland, XDG-portal file dialogs (they return nothing without a portal),
clipboard image copy, `xdg-open`, real adb, a physical device, a real emulator, the scrcpy server on a real phone,
real apksigner, Secret Service/KWallet, high-DPI scaling, screen readers.


## Second pass: bug hunt, regressions, UI polish

Defects found by reading the code and by driving the GUI, each fixed, with the evidence kept:

| Defect | Fix | Evidence |
|---|---|---|
| Pulling a file from an app's private storage (`run-as`) truncated the user's existing local file when the pull failed | written to `<dest>.part`, renamed on success, removed on failure | regression test `a_failed_run_as_pull_keeps_the_existing_local_file` |
| Export APK could overwrite an existing file and joined an unvalidated package name into a path | `unused_path` picks `name (n).apk`; name validated first | unit test `exports_never_reuse_an_existing_name` |
| Files: downloads/uploads asked once per file about overwriting; table rows stayed clickable while a stale listing was shown | one batched prompt; rows inert until the listing matches the path | GUI check; no automated test |
| Connections: port list could show the previous device's mappings; "Forget" did nothing for connected devices | refresh keyed on the device; Forget disabled with a tooltip while online | GUI check; no automated test |
| Network capture: a failed save moved the state to Idle and lost the capture | stays Stopped until the save succeeds | code review; no automated test |
| Apps: the details panel's buttons did not wrap, widened the panel past its frame, clipped them ("Per miss ions") and hid the Version/Updated columns | two-column button grid | before/after screenshots |
| Apps/Processes/Connections toolbars and cards overflowed the window at the 820 px minimum | icon-only navigation rail below 1100 px, narrower search fields, stacked Connections cards | screenshots at 820x520 |
| Settings/About rows misaligned (`allocate_ui_with_layout` shrinks to content) | `kit::fixed_cell` | screenshots |

App icons and names (third pass): the first implementation parsed APKs on the host; it was replaced
by a port of ADBPlus' approach (on-device helper `aya.dex`, see `docs/ARCHITECTURE.md`).

* Unit tests (`aya`, `appicon`): Protobuf encoding against hand-computed bytes, round trip incl.
  two-byte lengths, partial messages at every cut point, unknown fields, malformed/oversized
  input, defensive JSON parsing, the icon-path allow-list (traversal, spaces, other directories),
  decode of PNGs of several sizes, corrupt images, cache round trip and half-written entries.
* Integration tests against the fake device, with a stand-in helper that speaks the real wire
  protocol: the bundled dex is pushed unchanged and started through `app_process`, one
  `getPackageInfos` call serves a whole batch, icons are read through `adb exec-out`, an app with
  no icon and an app the helper cannot read are handled, a second lookup is answered from the cache
  without starting the helper, and a helper that dies on start is reported in well under the
  10 s timeout and is not retried or cached.
* GUI (screenshots, fake adb plus a scratch Python stand-in for the helper): icons in rows and
  details header, label as the details title, and the fallback note and letter badges when the
  helper does not answer.

**Not verified:** the real `aya.dex` has never been executed by this project. No Android device,
emulator, `app_process`, SELinux policy or real `adb forward localabstract:` was available, so
whether it starts and answers on real phones (and on which Android versions) is untested. The
Kotlin source in `assets/aya/source` was not rebuilt, so that it matches the bundled dex is taken
from ADBPlus.

UI revamp (anti-slop pass): removed icon-before-heading, icon tiles on cards, the accent bar on the
navigation, per-card pill badges and unused helpers; headings and card titles are plain text.
All 18 panels were captured in dark and light at 1180x760 after the changes; Apps, Processes,
Connections, Overview and Logcat were captured at 820x520 after the final layout fix. Files,
Remote control and Settings at 820x520 were not re-captured after the navigation rail change.

Fourth pass (robustness and efficiency; the user confirmed icons appear on a real phone):

* Helper start-up: a stale or hung helper is killed before a new one is launched (pattern
  `io.liriliri.aya.Serve[r]`, which does not match its own `pkill`), the start-up wait is 15 s, and
  a failed start appends the last `aya`/`AndroidRuntime` lines of the device log to the message.
  These device-side behaviours are UNVERIFIED against a real device (only the fake adb ran them).
* A failed helper is now a visible warning banner in the Apps panel with Retry, not a tooltip.
  Checked by screenshot: the banner appears after the 15 s wait; icons, names and the compact rail
  were re-captured at 820x520 against the stand-in helper.
* Apps list rows are rebuilt only when the packages, filter, sort or system toggle change (it was
  every frame). Filters in Apps, Processes, Files and Logcat use `text::contains_ci`, which does
  not allocate for ASCII text (it did 2 allocations per entry per frame in Logcat); 2 unit tests.
* Settings and About are pinned to the bottom of the navigation rail; only the panel list scrolls.

Known limitations: the icon cache is never pruned; a helper left running after ADB Deck is
killed stays on the phone until it restarts.


## Fifth pass: embedded mirror, app actions, device controls, connection resilience

Gates run on the final tree (all against the fake device; nothing ran on a real phone):
`cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test`:
224 tests pass (172 library, 14 binary, 2 config, 36 fake-device integration).

Pass 6 (network discovery): mDNS output parsing, the Discoverer planner (cooldown, already
connected by address / mDNS name / USB serial, user-disconnected host), a phone on a new port
replacing its remembered entry, and fake-adb integration tests (discovered phone connected, no
repeated connects for a phone that does not come up, user disconnect respected on a new port, no
scan when the setting is off). Mutation-checked: removing the cooldown or the suppression check
makes the matching integration test fail.

New integration tests: app management operations (permissions, ops, buckets, suspend, hide,
compile, kill, remove for user), names that fail validation never reach the device, cache clearing
by Android version and system-wide, device controls read back what was applied, service commands
and reboots, adb hiccups retried while a dead link still fails, a dropped Wi-Fi phone is
reconnected, a user-disconnected phone stays disconnected, remembered phones connect at start.

GUI: Apps (Permissions/Advanced tabs), Device controls and Overview were captured in light and dark
at 1180x760 and at 820x520 under Xvfb. Layout defects found and fixed: side panel growing every
frame, clipped right edge, reversed button order in right-to-left rows.

**Not verified:** scrcpy server on real Android, scroll direction/scale, H.264 profiles from real
phones with openh264, pm/appops/cache/svc/wm/dumpsys behaviour on real Android versions, Wi-Fi
reconnect and mDNS discovery on a real network (the `adb mdns services` row format and the
`adb-<serial>-<x>._adb-tls-connect._tcp` device names are from adb's documented behaviour, not
checked against a real adb), KDE/Wayland, release-build performance, RPM build with openh264
(needs gcc-c++ and nasm, written into the spec but not built).
