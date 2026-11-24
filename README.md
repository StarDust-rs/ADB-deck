# ADB Deck

A native Rust desktop application for managing and debugging Android devices
through `adb`, built for Fedora Linux with KDE Plasma (Wayland first, X11 also
supported). It reimplements the *workflows* of the ADBPlus project in Rust with no
Electron/JS runtime. App icons and names come from the Play Store through an open source
metadata service (see "App icons").

Features: device overview, file browser (upload/download with progress, preview),
app manager (install queue, drag-and-drop, uninstall, clear, permissions,
export APK), processes, performance charts, interactive shell, layout inspector,
screenshots, logcat, network capture (tcpdump), WebView inspector, remote key
pad, Wi-Fi connect/pairing, port forwarding/reverse, virtual devices (emulator),
APK signing (apksigner), embedded screen mirroring (scrcpy protocol, no external program), per-app
privileged actions (permissions, app ops, standby bucket, suspend, hide, compile, per-app and
system-wide cache clearing), device controls (radios, display, developer toggles, battery and
doze simulation, reboot targets), automatic Wi-Fi reconnect with retry/backoff, automatic connection to paired phones found on the
local network (mDNS) or at their saved addresses (Wi-Fi, USB tethering, hotspot), a windowless background service that does
this at login, and settings.
See `docs/FEATURE_INVENTORY.md` for the details and status of each.

## External tools

| Tool | Needed for | Fedora package |
|---|---|---|
| `adb` | everything | `android-tools` (required) |
| `apksigner` | APK signing | Android SDK build-tools (optional) |
| `emulator` | Virtual devices | Android SDK emulator (optional) |

Missing tools are detected at start-up and explained in *Settings → Tools*.
Explicit paths can be set there.

## Build from source

```sh
sudo dnf install cargo rust gcc          # build dependencies
cargo build --release                    # target/release/adbdeck
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check
```

Run it with `target/release/adbdeck [file.apk ...]` (APK arguments are queued
for installation on the selected device). `--help` and `--version` are supported.

## Releases

Publishing a GitHub release with a new tag such as `v1.0.0` builds the Fedora 45 x86_64 RPM from `main`, attaches
it to the release and writes the notes from the commit titles since the last release. See
[docs/RELEASING.md](docs/RELEASING.md).

## App icons

Android has no command that returns an app's icon, so ADB Deck looks apps up on the Play Store through the
open source metadata API at [play.rajkumaar.co.in](https://play.rajkumaar.co.in) (MIT,
[source](https://github.com/rajkumaar23/playstore-api)): `GET /json?id=<package>` returns the store title and the
address of the icon, which is then downloaded from Google's image server. **Only the package names of the apps shown
are sent.** Apps that are not on the Play Store (system apps, sideloaded builds) keep a letter badge. The requests are
made by the system's `curl`; results are cached under `~/.cache/adbdeck/icons/play` (icons for 30 days, unknown apps
for a day, failures for ten minutes). *Settings → Fetch app icons and names from the Play Store* switches the
lookups off; icons already cached are still shown.

## Keeping a phone connected (background service)

Connect a phone once (Connections: Wi-Fi address, pairing code, a QR code scanned with the phone, or "Switch a USB device to Wi-Fi").
ADB Deck saves it: serial number, every `ip:port` it was reached at, Android version, last time seen,
and whether it should be reconnected automatically.

After a restart, `adbdeck --daemon` runs without a window and brings the phone back, whether or not
the window is open:

1. remembered `ip:port` addresses are retried with growing pauses;
2. every 10 s, phones advertising Wireless debugging (Android 11+, `_adb-tls-connect` over mDNS) are
   connected, whatever port they picked this time;
3. every 30 s, phones that are still missing are looked for at their saved addresses, on the standard
   tcpip port (5555) of those hosts, and at the gateways of this computer's networks, which is where a
   USB-tethered or hotspot phone sits. A connection is kept only if the phone's serial number matches.

The service is started at login by an XDG autostart entry (`~/.config/autostart/adbdeck-daemon.desktop`)
that ADB Deck writes while *Settings → Keep devices connected in the background* is on (the default).
Turn it off there and the entry is removed and a running service stops within 30 s. To start it before
login instead, use the systemd user unit: `systemctl --user enable --now adbdeck-daemon`, and
`loginctl enable-linger $USER` so it runs without a login session.

Pressing *Disconnect* on a remembered phone switches its automatic connection off (saved, so the service
respects it); *Connect* switches it on again. *Forget* removes the entry only: a phone that still
advertises Wireless debugging for this computer is found again by mDNS. Revoke the computer's
authorisation in the phone's Developer options to stop that.

## RPM

```sh
sudo dnf install rpm-build redhat-rpm-config cargo-rpm-macros cargo rust gcc gcc-c++ nasm \
  desktop-file-utils appstream systemd-rpm-macros python3 xz
packaging/build-rpm.sh            # .fc<your Fedora release>; or pass a dist tag, e.g. .fc45
```

The spec follows the Fedora Rust guidelines: `cargo-rpm-macros` (`%cargo_prep -v vendor`, `%cargo_build`,
`%cargo_test`), the locked crates as a separate vendor tarball with a generated
`cargo-vendor.txt` and `LICENSE.dependencies`, debuginfo, a man page, AppStream and desktop file
validation in `%check`, and the user unit scriptlets. The tarballs and the build time are reproducible
(`SOURCE_DATE_EPOCH`). Output: `dist/rpmbuild/RPMS/<arch>/adbdeck-<version>-1.fc<N>.<arch>.rpm`
and the matching `.src.rpm`. See [docs/PACKAGING_REPORT.md](docs/PACKAGING_REPORT.md).

```sh
sudo dnf install ./adbdeck-*.rpm      # install
adbdeck                               # launch (or from the application menu)
sudo dnf remove adbdeck               # uninstall
```

Removal leaves `~/.config/adbdeck` and `~/.local/share/adbdeck` in place.

## Test limits (read this)

* Automated tests use a **simulated** device (`tests/fixtures/fake-adb`), not
  real adb or Android. Nothing was tested against a physical device or a real
  emulator.
* The GUI was exercised under Xvfb with software GL (X11 backend) against the
  simulated device. It was **not** run on KDE Plasma or under a Wayland session.
* XDG-portal file dialogs, clipboard image copy, `xdg-open`, apksigner
  and the Secret Service were not exercised end to end.
* The RPM was built on a non-Fedora host with `rpmbuild`; it was not built in
  mock or on Fedora, and runtime dependency names were not verified against
  Fedora repositories. See `docs/PACKAGING_REPORT.md`.

## Licence

GPL-3.0-or-later (see `LICENSE`). The scrcpy server it bundles for screen mirroring is Apache-2.0
(`assets/scrcpy/`), the fonts are OFL-1.1 (`assets/fonts/`). Third-party crates keep their own
licences; the RPM's `License:` field lists them.
