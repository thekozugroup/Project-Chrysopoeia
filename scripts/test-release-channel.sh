#!/usr/bin/env bash
# Tests the version-tag rules and release notes behind the Release workflow
# (scripts/release-channel.sh, scripts/release-notes.sh) without a registry,
# GitHub or Docker. Prints a dry run of which moving tags each release would
# take, then checks it.
#
#   scripts/test-release-channel.sh
#
# Needs bash 4+, git and awk. CI runs it in the lint job; `make test-release`
# runs it locally.

set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
channel="$here/release-channel.sh"
notes="$here/release-notes.sh"

failures=0
checks=0

check() { # check <description> <expected> <actual>
  checks=$((checks + 1))
  if [[ $2 == "$3" ]]; then
    printf '  ok    %s\n' "$1"
  else
    failures=$((failures + 1))
    printf '  FAIL  %s\n          expected: %s\n          actual:   %s\n' "$1" "$2" "$3"
  fi
}

oneline() { tr '\n' ' ' | sed 's/ $//'; }

# --- Which tags move ---------------------------------------------------------

# Every tag the repository has: releases, a prerelease, and tags the workflow
# must ignore. v0.10.0 sorts before v0.9.9 as text but is the newer version.
tags=$'v0.2.0\nv0.3.0\nv0.3.1-rc.1\nv0.10.0\nv0.9.9\nvfoo\nv1\nv1.2\nv01.2.3\nv0.4.0+build'

moving() { "$channel" moving-tags "$1" <<<"$tags" | oneline; }

echo "Tags in the repository: $(oneline <<<"$tags")"
echo
echo "Dry run: what each tag push would move"
printf '  %-14s %-8s %s\n' tag stable 'moving tags'
for tag in v0.2.0 v0.3.0 v0.3.1-rc.1 v0.10.0 v0.9.9; do
  m=$(moving "$tag")
  if [[ " $m " == *" stable "* ]]; then verdict=moves; else verdict=stays; fi
  printf '  %-14s %-8s %s\n' "$tag" "$verdict" "${m:-(none: exact version tag only)}"
done
echo

echo "Newest release"
check "v0.10.0 is the newest of the list" v0.10.0 "$("$channel" newest <<<"$tags")"
check "an empty list has none" '' "$("$channel" newest <<<'')"
check "prereleases alone are never newest" '' "$("$channel" newest <<<$'v1.0.0-rc.1\nv2.0.0-beta')"

echo "Moving tags (stable only for the newest release)"
check "v0.10.0 moves stable" "0.10 stable" "$(moving v0.10.0)"
check "v0.9.9 does not move stable" "0.9" "$(moving v0.9.9)"
check "v0.3.0 does not move stable" "0.3" "$(moving v0.3.0)"
check "v0.2.0 does not move stable" "0.2" "$(moving v0.2.0)"
check "a prerelease moves nothing" "" "$(moving v0.3.1-rc.1)"
check "a prerelease newer than every release moves nothing" "" "$("$channel" moving-tags v9.0.0-rc.1 <<<"$tags" | oneline)"
check "v0.11.0 is newer than v0.10.0 even before the list shows it" "0.11 stable" "$("$channel" moving-tags v0.11.0 <<<"$tags" | oneline)"
check "the first release moves stable" "0.2 stable" "$("$channel" moving-tags v0.2.0 <<<'' | oneline)"
check "a re-run of the newest release moves stable again" "0.10 stable" "$("$channel" moving-tags v0.10.0 <<<"$tags" | oneline)"
check "a CRLF tag list works" "0.10 stable" "$("$channel" moving-tags v0.10.0 <<<"${tags//$'\n'/$'\r\n'}" | oneline)"

echo "Major tag from 1.0 on, and backports"
list=$'v1.0.0\nv1.2.0\nv1.2.3\nv1.3.0\nv2.0.0'
check "1.2.3 is no longer the newest 1.2.x after 1.2.4" "" "$("$channel" moving-tags v1.2.3 <<<"$list"$'\nv1.2.4' | oneline)"
check "a backport 1.2.4 moves 1.2, not 1 or stable" "1.2" "$("$channel" moving-tags v1.2.4 <<<"$list"$'\nv1.2.4' | oneline)"
check "1.3.0 is the newest 1.x: 1.3 and 1 move, stable does not (2.0.0 exists)" "1.3 1" "$("$channel" moving-tags v1.3.0 <<<"$list" | oneline)"
check "2.0.0 moves 2.0, 2 and stable" "2.0 2 stable" "$("$channel" moving-tags v2.0.0 <<<"$list" | oneline)"
check "a re-run of 1.0.0 moves only 1.0, never 1 or stable (newer releases exist)" "1.0" "$("$channel" moving-tags v1.0.0 <<<"$list" | oneline)"
check "1.1.0 moves 1.1 only (1.3.0 is newer in 1.x)" "1.1" "$("$channel" moving-tags v1.1.0 <<<"$list" | oneline)"

echo "Previous release (for the release notes)"
prev() { "$channel" previous "$1" <<<"$tags"; }
check "before v0.10.0 comes v0.9.9" v0.9.9 "$(prev v0.10.0)"
check "before v0.9.9 comes v0.3.0" v0.3.0 "$(prev v0.9.9)"
check "before v0.3.0 comes v0.2.0" v0.2.0 "$(prev v0.3.0)"
check "before v0.2.0 comes nothing" "" "$(prev v0.2.0)"
check "before v0.3.1-rc.1 comes v0.3.0, not a prerelease" v0.3.0 "$(prev v0.3.1-rc.1)"
check "before v0.3.0-rc.1 comes v0.2.0, not v0.3.0 itself" v0.2.0 "$(prev v0.3.0-rc.1)"

echo "Tag validation and versions"
cls() { "$channel" classify "$1" 2>/dev/null | oneline; }
check "v1.2.3" "version=1.2.3 major=1 minor=2 prerelease=false" "$(cls v1.2.3)"
check "v0.10.0" "version=0.10.0 major=0 minor=10 prerelease=false" "$(cls v0.10.0)"
check "v1.2.0-rc.1 is a prerelease" "version=1.2.0-rc.1 major=1 minor=2 prerelease=true" "$(cls v1.2.0-rc.1)"
check "v1.0.0-alpha-2 is a prerelease" "version=1.0.0-alpha-2 major=1 minor=0 prerelease=true" "$(cls v1.0.0-alpha-2)"
for bad in vfoo v1 v1.2 v01.2.3 v1.02.3 v1.2.3.4 V1.2.3 1.2.3 v1.2.3-01 v1.2.3- v1.2.3+meta v1.2.3-rc..1 "v1.2.3 " v1234567890.0.0; do
  if "$channel" classify "$bad" >/dev/null 2>&1; then
    check "\"$bad\" is refused" refused accepted
  else
    check "\"$bad\" is refused" refused refused
  fi
done

# --- CHANGELOG sections --------------------------------------------------------

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

cat >"$tmp/CHANGELOG.md" <<'EOF'
# Changelog

## [Unreleased]

### Added

- Coming soon.

## [0.3.0] - 2026-11-01

### Fixed

- A fix.

## [0.2.0] - 2026-10-04

### Added

- The first release.

[Unreleased]: https://example.invalid/compare/v0.3.0...HEAD
[0.3.0]: https://example.invalid/compare/v0.2.0...v0.3.0
[0.2.0]: https://example.invalid/releases/tag/v0.2.0
EOF

echo "CHANGELOG sections"
check "the 0.2.0 section, without the link definitions" $'### Added\n\n- The first release.' "$("$channel" changelog 0.2.0 "$tmp/CHANGELOG.md")"
check "the 0.3.0 section stops at the next heading" $'### Fixed\n\n- A fix.' "$("$channel" changelog 0.3.0 "$tmp/CHANGELOG.md")"
check "a prerelease uses the section of its version" $'### Fixed\n\n- A fix.' "$("$channel" changelog 0.3.0-rc.1 "$tmp/CHANGELOG.md")"
if "$channel" changelog 9.9.9 "$tmp/CHANGELOG.md" >/dev/null; then r=found; else r=none; fi
check "a version without a section has none" none "$r"
if "$channel" changelog 0.2.0 "$tmp/missing.md" >/dev/null; then r=found; else r=none; fi
check "a missing CHANGELOG.md has none" none "$r"

# --- Release notes -------------------------------------------------------------

echo "Release notes"
repo="$tmp/repo"
mkdir "$repo"
g() { git -C "$repo" -c user.name=Test -c user.email=test@example.invalid -c commit.gpgsign=false -c tag.gpgsign=false "$@"; }
g init -q -b main
commit() { g commit -q --allow-empty -m "$1"; }
commit "feat: first"
commit "fix: second"
g tag v0.2.0
commit "feat: third"
g checkout -q -b side
commit "fix: on a branch"
g checkout -q main
g merge -q --no-ff side -m "Merge branch side"
g tag v0.3.0
cp "$tmp/CHANGELOG.md" "$repo/CHANGELOG.md"

notes_for() { (cd "$repo" && GITHUB_REPOSITORY=o/r IMAGE=ghcr.io/o/app "$notes" "$@"); }

out=$(PRERELEASE=false STABLE_MOVED=true MOVING_TAGS="0.3 stable" notes_for v0.3.0 v0.2.0)
check "notes list the commits since the previous release" "yes" "$([[ $out == *"- feat: third ("* && $out == *"- fix: on a branch ("* ]] && echo yes || echo no)"
check "notes leave out the commits before it" "no" "$([[ $out == *"feat: first"* || $out == *"fix: second"* ]] && echo yes || echo no)"
check "notes leave out the merge commit" "no" "$([[ $out == *"Merge branch"* ]] && echo yes || echo no)"
check "notes start with the CHANGELOG section" "yes" "$([[ $out == $'### Fixed\n\n- A fix.\n\n## Changes since v0.2.0'* ]] && echo yes || echo no)"
check "notes link the comparison" "yes" "$([[ $out == *"https://github.com/o/r/compare/v0.2.0...v0.3.0"* ]] && echo yes || echo no)"
check "notes say stable moved" "yes" "$([[ $out == *"now offers this release"* && $out == *"\`:0.3\`, \`:stable\`"* ]] && echo yes || echo no)"
check "notes give the exact pull command" "yes" "$([[ $out == *"docker pull ghcr.io/o/app:0.3.0"* ]] && echo yes || echo no)"

out=$(PRERELEASE=false STABLE_MOVED=false MOVING_TAGS="0.2" notes_for v0.2.0)
check "a first release lists every commit" "yes" "$([[ $out == *"## Changes"$'\n'* && $out == *"- feat: first ("* && $out == *"- fix: second ("* ]] && echo yes || echo no)"
check "notes say stable stayed" "yes" "$([[ $out == *"was not moved"* ]] && echo yes || echo no)"

out=$(PRERELEASE=true notes_for v0.3.0 v0.2.0)
check "a prerelease says it moves nothing" "yes" "$([[ $out == *"This is a prerelease"* ]] && echo yes || echo no)"

if (cd "$repo" && "$notes" v9.9.9 >/dev/null 2>&1); then r=ok; else r=refused; fi
check "notes for a tag that does not exist fail" refused "$r"

echo
if ((failures > 0)); then
  echo "$failures of $checks checks failed."
  exit 1
fi
echo "All $checks checks passed."
