#!/usr/bin/env python3
"""Shrink a `cargo vendor` tree: crates that only matter on Windows are
replaced by empty stubs (Cargo still needs their manifests to resolve the
lock file, but they are never compiled on Linux)."""
import json, os, re, shutil, sys

root = sys.argv[1]
pat = re.compile(r"^(windows|winapi)([-_].*)?$")
removed = 0
for name in sorted(os.listdir(root)):
    d = os.path.join(root, name)
    base = re.sub(r"-\d+\.\d+\.\d+$", "", name)
    if not os.path.isdir(d) or not pat.match(base):
        continue
    keep = {"Cargo.toml", ".cargo-checksum.json"}
    for entry in os.listdir(d):
        p = os.path.join(d, entry)
        if entry in keep or entry.upper().startswith(("LICENSE", "COPYING")):
            continue
        shutil.rmtree(p) if os.path.isdir(p) else os.remove(p)
    os.makedirs(os.path.join(d, "src"), exist_ok=True)
    open(os.path.join(d, "src", "lib.rs"), "w").close()
    open(os.path.join(d, "build.rs"), "w").close()
    cs_path = os.path.join(d, ".cargo-checksum.json")
    cs = json.load(open(cs_path))
    cs["files"] = {}
    json.dump(cs, open(cs_path, "w"))
    removed += 1
print(f"stubbed {removed} Windows-only crates")
