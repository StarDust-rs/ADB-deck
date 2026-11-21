# Packaging report

Target: Fedora 44 and 45 (45 was released on 2026-11-03; Fedora 43 ends on 2026-12-09), x86_64 and aarch64.
Built by `packaging/build-rpm.sh`, in CI by `.github/workflows/release.yml` inside the Fedora containers.

**Nothing in this report was built or run.** The spec, scripts and workflow were rewritten without a Fedora
system, `rpmbuild`, `cargo` or network. The checks that were possible here are listed at the end.

## What the package follows

| Practice | In this package |
|---|---|
| Rust guidelines | `BuildRequires: cargo-rpm-macros >= 25`; `%cargo_prep -v vendor` in `%prep`, `%cargo_build`, `%cargo_install`, `%cargo_test` (so the distribution's `%build_rustflags`, hardening and debuginfo apply); no `%cargo_generate_buildrequires`, which is not allowed with vendored crates |
| Vendored crates | separate `Source1` vendor tarball; `%{cargo_vendor_manifest}` writes `cargo-vendor.txt`, from which the RPM generator derives the `bundled(crate(...))` (and, from Fedora 45, `purl(...)`) Provides; `%{cargo_license_summary}` in the log and `%{cargo_license}` as `LICENSE.dependencies`, both `%license` files |
| License tag | SPDX expression built from `cargo metadata` of the locked set plus the bundled scrcpy server and fonts |
| Debuginfo | on (the old `debug_package` opt-out is gone) |
| Tests | `%bcond check 1`, `%cargo_test` in `%check` |
| Metadata checks | `desktop-file-validate` and `appstreamcli validate --no-net --explain` in `%check` (the `appstream` package, not the deprecated `appstream-util`) |
| AppStream | developer, homepage and bug tracker URLs, keywords, branding colours, display requirement, release entry stamped from the tag |
| Man page | `adbdeck(1)` |
| systemd user unit | installed in `%{_userunitdir}`; `%systemd_user_post` / `%systemd_user_preun`; not enabled by default (the login autostart entry that Settings manages does that) |
| Icons and desktop file | `hicolor-icon-theme` dependency; no scriptlets, the distribution's file triggers update the caches |
| Reproducibility | sorted, root-owned, `SOURCE_DATE_EPOCH`-stamped tarballs (`gzip -n`, `xz -T1`), `clamp_mtime_to_source_date_epoch`, `use_source_date_epoch_as_buildtime`, a fixed build host, a changelog date taken from the epoch |
| Dependencies | `Requires: android-tools` and the libraries that winit/glow load with `dlopen`; weak `Recommends` for the portal and `xdg-utils`. rpmlint reports `explicit-lib-dependency` for these on purpose |

## Not done, and why

* **Package signing.** RPM 6 changed how packages are signed (native OpenPGP, `--rpmv6`), and a key handling flow that works for both
  Fedora 44 (RPM 6.0) and 45 (RPM 6.1) could not be checked here. Releases carry SHA256SUMS and a GitHub build provenance attestation instead.
* **RPM v6 package format.** Fedora 46 moves to it; Fedora 44 and 45 build the default (v4) format.
* **`%autorelease` / `%autochangelog`.** They need the packaging repository's history; the spec generated here has a fixed `Release: 1` and a one-line changelog.
* **Fedora main repositories.** The package could not be submitted there as it stands: it bundles a scrcpy server binary and builds the
  openh264 source (H.264 is not allowed in Fedora's repositories without Cisco's binary), and its crates are vendored. It is meant for
  GitHub releases or a COPR.
* **The `.src.rpm` is built once** (Fedora 45, x86_64); it contains everything to rebuild offline.

## What was checked in the sandbox

`sh -n` on the build script, `py_compile` on the Python helpers, the version/metainfo stamping on a copy of the files, an XML
parse of the AppStream file, a YAML parse of the workflow. Not checked: any `rpmbuild`, `rpmlint`, `appstreamcli`, `desktop-file-validate`,
`dnf install`, `man` rendering, or the Fedora package names in `Requires`.
