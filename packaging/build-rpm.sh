#!/bin/sh
# Builds the source tarball, the vendor tarball, the spec and the RPMs.
# Usage: packaging/build-rpm.sh [dist-tag, default .fc<running Fedora, else 45>]
#
# Reproducible: both tarballs are sorted, owned by root and stamped with
# SOURCE_DATE_EPOCH (default: the last commit, else now), and rpmbuild clamps file
# times and the build time to it. The spec's changelog date comes from it too.
# RPMBUILD_ARGS="--nodeps" is for non-Fedora hosts whose RPM database cannot satisfy
# BuildRequires.
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
out="$here/dist"
cd "$here"

fedora=$(rpm --eval '%{?fedora}' 2>/dev/null || true)
dist=${1:-.fc${fedora:-45}}

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
url=$(sed -n 's/^repository = "\(.*\)"/\1/p' Cargo.toml | head -1)
if [ -z "${SOURCE_DATE_EPOCH:-}" ]; then
  SOURCE_DATE_EPOCH=$(git log -1 --format=%ct 2>/dev/null || date +%s)
fi
export SOURCE_DATE_EPOCH
changelog_date=$(LC_ALL=C date -u -d "@$SOURCE_DATE_EPOCH" '+%a %b %d %Y')

top="$out/rpmbuild"
rm -rf "$out"
mkdir -p "$top"/SOURCES "$top"/SPECS "$top"/BUILD "$top"/RPMS "$top"/SRPMS

stage="$out/stage/adbdeck-$version"
mkdir -p "$stage"
# the working tree without build output, history and previous packaging products
tar --exclude=./target --exclude=./dist --exclude=./.git --exclude=./.github --exclude=./vendor -cf - . | tar -xf - -C "$stage"

(cd "$stage" && cargo vendor --locked vendor >/dev/null)
python3 "$here/packaging/prune-vendor.py" "$stage/vendor"

# the license expression comes from the vendored, locked dependency set
mkdir -p "$stage/.cargo"
cat > "$stage/.cargo/config.toml" <<'CFG'
[source.crates-io]
replace-with = "vendored-sources"
[source.vendored-sources]
directory = "vendor"
CFG
license=$(cd "$stage" && python3 "$here/packaging/gen-spec-data.py")
rm -rf "$stage/.cargo"

reproducible_tar() {
  tar --sort=name --owner=0 --group=0 --numeric-owner --mtime="@$SOURCE_DATE_EPOCH" -cf - "$@"
}
(cd "$out/stage/adbdeck-$version" && reproducible_tar vendor | xz -T1 -9e > "$top/SOURCES/adbdeck-$version-vendor.tar.xz")
rm -rf "$stage/vendor"
(cd "$out/stage" && reproducible_tar "adbdeck-$version" | gzip -n -9 > "$top/SOURCES/adbdeck-$version.tar.gz")
rm -rf "$out/stage"

LICENSE_EXPR="$license" VERSION="$version" PROJECT_URL="$url" CHANGELOG_DATE="$changelog_date" python3 - <<'PY' > "$top/SPECS/adbdeck.spec"
import os
t = open("packaging/adbdeck.spec.in").read()
for key, value in (("@LICENSE@", os.environ["LICENSE_EXPR"]), ("@VERSION@", os.environ["VERSION"]),
                   ("@URL@", os.environ["PROJECT_URL"]), ("@CHANGELOG_DATE@", os.environ["CHANGELOG_DATE"])):
    t = t.replace(key, value)
print(t, end="")
PY

rpmbuild -ba ${RPMBUILD_ARGS:-} \
  --define "_topdir $top" --define "dist $dist" \
  --define "_buildhost reproducible" \
  --define "use_source_date_epoch_as_buildtime 1" --define "clamp_mtime_to_source_date_epoch 1" \
  "$top/SPECS/adbdeck.spec"
find "$top/RPMS" "$top/SRPMS" -name '*.rpm'
