#!/usr/bin/env python3
"""Prints the combined License expression of the crates compiled on Linux, from
`cargo metadata` of the locked, vendored dependency set. Run it where
`.cargo/config.toml` points cargo at ./vendor (packaging/build-rpm.sh does this).

The per-crate `bundled(crate(...))` Provides are not generated here: the RPM
generator reads them from cargo-vendor.txt, which the spec's %build writes."""
import json
import subprocess
import sys

meta = json.loads(subprocess.check_output(
    ["cargo", "metadata", "--format-version", "1", "--locked", "--offline",
     "--filter-platform", "x86_64-unknown-linux-gnu"], text=True))
ids = {n["id"] for n in meta["resolve"]["nodes"]}
pkgs = sorted((p for p in meta["packages"] if p["id"] in ids and p["name"] != "adbdeck"),
              key=lambda p: (p["name"], p["version"]))


def norm(expr):
    return expr.replace("/", " OR ").replace("  ", " ").strip()


licenses = set()
for p in pkgs:
    lic = p.get("license")
    if not lic:
        sys.exit(f"{p['name']} {p['version']} has no SPDX license field; review manually")
    licenses.add(norm(lic))


def wrap(l):
    return f"({l})" if (" OR " in l or " AND " in l) else l


print(" AND ".join(["GPL-3.0-or-later"] + [wrap(l) for l in sorted(licenses)]))
