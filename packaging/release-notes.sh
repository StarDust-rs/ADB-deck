#!/usr/bin/env bash
# Prints release notes for a tag: the titles of the commits since the previous
# release tag, newest first. Commits that follow Conventional Commits (feat:, fix:)
# are grouped; any other title goes under "Other changes" (or "Changes" when no
# commit uses a prefix). Merge commits are left out, their commits are listed.
#
# Usage: packaging/release-notes.sh TAG [PREVIOUS_TAG]
# Without PREVIOUS_TAG the nearest earlier v* tag is used; with none, every commit
# up to TAG is listed. Run it inside the repository with full history.
set -euo pipefail

tag=${1:?usage: release-notes.sh TAG [PREVIOUS_TAG]}
prev=${2-}
if [ -z "$prev" ]; then
  prev=$(git describe --tags --abbrev=0 --match 'v[0-9]*' --exclude "$tag" "$tag" 2>/dev/null || true)
fi
range=$tag
if [ -n "$prev" ]; then
  range="$prev..$tag"
fi

features=""
fixes=""
other=""
while IFS=$'\t' read -r subject hash; do
  [ -n "$subject" ] || continue
  line="- ${subject} (\`${hash}\`)"$'\n'
  case "$subject" in
    feat:* | feat\(*) features+=$line ;;
    fix:* | fix\(*) fixes+=$line ;;
    *) other+=$line ;;
  esac
done < <(git log --no-merges --pretty=tformat:'%s%x09%h' "$range")

if [ -z "$features$fixes$other" ]; then
  echo "No changes since ${prev:-the start of the history}."
else
  other_title="Other changes"
  if [ -z "$features$fixes" ]; then
    other_title="Changes"
  fi
  [ -z "$features" ] || printf '### Features\n\n%s\n' "$features"
  [ -z "$fixes" ] || printf '### Fixes\n\n%s\n' "$fixes"
  [ -z "$other" ] || printf '### %s\n\n%s\n' "$other_title" "$other"
fi

if [ -n "$prev" ] && [ -n "${GITHUB_REPOSITORY-}" ]; then
  printf '**Full changelog:** %s/%s/compare/%s...%s\n' "${GITHUB_SERVER_URL:-https://github.com}" "$GITHUB_REPOSITORY" "$prev" "$tag"
fi
