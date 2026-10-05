#!/usr/bin/env bash
# Version-tag rules for the Release workflow (.github/workflows/release.yml).
# Kept in a script so they can be tested without a GitHub runner
# (scripts/test-release-channel.sh).
#
#   scripts/release-channel.sh classify <tag>
#       Validates a release tag (vMAJOR.MINOR.PATCH, optionally with a
#       prerelease such as -rc.1) and prints key=value lines for
#       $GITHUB_OUTPUT: version, major, minor, prerelease (true or false).
#
#   scripts/release-channel.sh moving-tags <tag>      (tag names on stdin)
#       Prints the moving image tags this release may take, one per line:
#         MAJOR.MINOR  only when <tag> is the newest release of that minor line
#         MAJOR        only when <tag> is the newest release of that major line,
#                      and only from 1.0 on (0.x has no major-only tag)
#         stable       only when <tag> is the newest release of all
#       "Release" means a non-prerelease tag, and "newest" is by semantic
#       version, so re-running an older tag never moves a tag backwards. A
#       prerelease takes none of them. <tag> counts as present even if the
#       list does not show it yet.
#
#   scripts/release-channel.sh newest                 (tag names on stdin)
#       Prints the newest release (non-prerelease) tag.
#
#   scripts/release-channel.sh previous <tag>         (tag names on stdin)
#       Prints the newest release tag whose version is lower than <tag>'s
#       (ignoring a prerelease suffix), or nothing when there is none. The
#       release notes list the commits since it.
#
#   scripts/release-channel.sh changelog <version> [file]
#       Prints the body of the "## [<version>]" section of CHANGELOG.md (or
#       <file>), without the heading. For a prerelease with no section of its
#       own, the section of the version it leads up to. Exit status 1 and no
#       output when there is none.
#
# Tags that are not strict semantic versions (v1, v1.2, vfoo, v01.2.3, build
# metadata such as v1.2.3+b) are ignored when listed and refused by classify.
# Numbers have at most nine digits, so shell arithmetic cannot overflow.

set -euo pipefail

NUM='0|[1-9][0-9]{0,8}'
IDENT='0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*'
TAG_RE="^v(${NUM})\\.(${NUM})\\.(${NUM})(-(${IDENT})(\\.(${IDENT}))*)?\$"

die() {
  echo "release-channel: $*" >&2
  exit 2
}

# parse_tag <tag>: sets MAJOR, MINOR, PATCH and PRERELEASE (empty for a
# release) and returns 0, or returns 1 when <tag> is not a valid release tag.
parse_tag() {
  local tag=$1
  # 100 characters keeps the image tag well inside Docker's 128.
  [[ ${#tag} -le 100 && $tag =~ $TAG_RE ]] || return 1
  MAJOR=${BASH_REMATCH[1]}
  MINOR=${BASH_REMATCH[2]}
  PATCH=${BASH_REMATCH[3]}
  PRERELEASE=${BASH_REMATCH[4]#-}
}

require_tag() {
  parse_tag "$1" || die "\"$1\" is not a release tag: expected vMAJOR.MINOR.PATCH, optionally followed by a prerelease such as -rc.1 (for example v1.2.3 or v1.2.0-rc.1)."
}

# releases [major [minor]]: tag names on stdin; prints the valid
# non-prerelease ones (optionally only of that major, or major and minor),
# oldest first, each once.
releases() {
  local want_major=${1:-} want_minor=${2:-} line
  while IFS= read -r line || [[ -n $line ]]; do
    line=${line%$'\r'}
    if parse_tag "$line" && [[ -z $PRERELEASE ]] \
      && [[ -z $want_major || $MAJOR == "$want_major" ]] \
      && [[ -z $want_minor || $MINOR == "$want_minor" ]]; then
      printf '%s %s %s %s\n' "$MAJOR" "$MINOR" "$PATCH" "$line"
    fi
  done | LC_ALL=C sort -u -k1,1n -k2,2n -k3,3n | cut -d' ' -f4
}

cmd_classify() {
  require_tag "$1"
  local version="${MAJOR}.${MINOR}.${PATCH}${PRERELEASE:+-$PRERELEASE}"
  printf 'version=%s\nmajor=%s\nminor=%s\n' "$version" "$MAJOR" "$MINOR"
  if [[ -n $PRERELEASE ]]; then echo 'prerelease=true'; else echo 'prerelease=false'; fi
}

cmd_moving_tags() {
  local tag=$1 all major minor newest
  require_tag "$tag"
  [[ -z $PRERELEASE ]] || return 0
  major=$MAJOR minor=$MINOR
  all=$(cat)
  all+=$'\n'$tag

  newest=$(releases "$major" "$minor" <<<"$all" | tail -n 1)
  [[ $newest != "$tag" ]] || echo "${major}.${minor}"
  if ((major >= 1)); then
    newest=$(releases "$major" <<<"$all" | tail -n 1)
    [[ $newest != "$tag" ]] || echo "$major"
  fi
  newest=$(releases <<<"$all" | tail -n 1)
  [[ $newest != "$tag" ]] || echo stable
}

cmd_newest() {
  releases | tail -n 1
}

cmd_previous() {
  local tag=$1 cand prev='' cmaj cmin cpat
  require_tag "$tag"
  cmaj=$MAJOR cmin=$MINOR cpat=$PATCH
  while IFS= read -r cand; do
    parse_tag "$cand"
    if ((MAJOR < cmaj || (MAJOR == cmaj && (MINOR < cmin || (MINOR == cmin && PATCH < cpat))))); then
      prev=$cand
    fi
  done < <(releases)
  [[ -z $prev ]] || printf '%s\n' "$prev"
}

# changelog_section <version> <file>: the lines under "## [<version>]" up to
# the next "## [" heading, minus trailing link definitions and blank lines.
changelog_section() {
  awk -v want="$1" '
    /^## \[/ {
      if (found) exit
      heading = $0
      sub(/^## \[/, "", heading)
      sub(/\].*$/, "", heading)
      if (heading == want) { found = 1 }
      next
    }
    found { lines[++n] = $0 }
    END {
      if (!found) exit 1
      while (n > 0 && (lines[n] ~ /^[[:space:]]*$/ || lines[n] ~ /^\[[^]]+\]: /)) n--
      for (i = 1; i <= n; i++) print lines[i]
    }
  ' "$2"
}

cmd_changelog() {
  local version=$1 file=${2:-CHANGELOG.md} candidate body
  [[ -f $file ]] || return 1
  for candidate in "$version" "${version%%-*}"; do
    # Leading blank lines are dropped, so the notes start with the first entry.
    body=$(changelog_section "$candidate" "$file" | sed '/./,$!d') || body=''
    if [[ -n $body ]]; then
      printf '%s\n' "$body"
      return 0
    fi
  done
  return 1
}

main() {
  local command=${1:-}
  shift || true
  case "$command" in
    classify | moving-tags | previous)
      [[ $# -eq 1 ]] || die "usage: release-channel.sh $command <tag>"
      "cmd_${command//-/_}" "$1"
      ;;
    newest)
      cmd_newest
      ;;
    changelog)
      [[ $# -ge 1 && $# -le 2 ]] || die "usage: release-channel.sh changelog <version> [file]"
      cmd_changelog "$@"
      ;;
    *)
      sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//' >&2
      exit 2
      ;;
  esac
}

main "$@"
