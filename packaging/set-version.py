#!/usr/bin/env python3
"""Stamps release data into the build tree: the version into Cargo.toml and
Cargo.lock, the release entry into the AppStream metadata, and (when ADBDECK_URL
is set) the project address into both. Nothing is committed; the files in git keep
their placeholders.

The release workflow derives the version from the tag, so the tag is the only place
a release number is typed. `build-rpm.sh` then reads it back from Cargo.toml for the
spec and the tarball names, and `cargo --locked` keeps working because the lock file
carries the same number.

Usage: packaging/set-version.py MAJOR.MINOR.PATCH   (run from the repository root)
Environment: ADBDECK_URL (project address), SOURCE_DATE_EPOCH (release date, default today)
"""
import datetime
import os
import re
import sys

PACKAGE = "adbdeck"
PLACEHOLDER_URL = "https://example.invalid/adbdeck"
METAINFO = "packaging/dev.adbdeck.AdbDeck.metainfo.xml"


def stamp(path, pattern, replacement, flags=re.MULTILINE):
    with open(path, encoding="utf-8") as f:
        text = f.read()
    new, count = re.subn(pattern, replacement, text, count=1, flags=flags)
    if count != 1:
        sys.exit(f"{path}: could not find what to replace ({pattern})")
    with open(path, "w", encoding="utf-8") as f:
        f.write(new)


def replace_text(path, old, new):
    with open(path, encoding="utf-8") as f:
        text = f.read()
    with open(path, "w", encoding="utf-8") as f:
        f.write(text.replace(old, new))


def main():
    if len(sys.argv) != 2 or not re.fullmatch(r"\d+\.\d+\.\d+", sys.argv[1]):
        sys.exit("usage: set-version.py MAJOR.MINOR.PATCH")
    version = sys.argv[1]
    epoch = int(os.environ.get("SOURCE_DATE_EPOCH") or datetime.datetime.now(datetime.timezone.utc).timestamp())
    date = datetime.datetime.fromtimestamp(epoch, datetime.timezone.utc).strftime("%Y-%m-%d")

    # The first top-level `version =` line of Cargo.toml is the package's.
    stamp("Cargo.toml", r'^version = "[^"]*"', f'version = "{version}"')
    stamp("Cargo.lock", rf'(^name = "{PACKAGE}"\nversion = ")[^"]*(")', rf"\g<1>{version}\g<2>")

    url = os.environ.get("ADBDECK_URL", "").rstrip("/")
    if url:
        for path in ("Cargo.toml", METAINFO):
            replace_text(path, PLACEHOLDER_URL, url)
    release_url = f"{url or PLACEHOLDER_URL}/releases/tag/v{version}"
    stamp(
        METAINFO,
        r"<releases>.*?</releases>",
        f'<releases>\n    <release version="{version}" date="{date}">\n      <url type="details">{release_url}</url>\n    </release>\n  </releases>',
        flags=re.DOTALL,
    )
    print(f"{PACKAGE} {version} ({date})")


main()
