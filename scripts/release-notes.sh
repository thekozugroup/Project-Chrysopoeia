#!/usr/bin/env bash
# Writes the notes of a GitHub Release to stdout (Markdown). Used by the
# Release workflow (.github/workflows/release.yml) and tested by
# scripts/test-release-channel.sh.
#
#   scripts/release-notes.sh <tag> [<previous-tag>]
#
# The notes hold, in this order:
#   1. the "## [<version>]" section of CHANGELOG.md, when there is one;
#   2. the commits from <previous-tag> (exclusive) to <tag>, merge commits
#      left out, newest first, at most 100, with a link to the full
#      comparison (everything up to <tag> when there is no previous tag; get
#      it from `scripts/release-channel.sh previous <tag>`);
#   3. which image tags the release is published under.
#
# Environment (all optional):
#   GITHUB_REPOSITORY  owner/name, for the links (default: omitted)
#   IMAGE              image name without a tag, e.g. ghcr.io/owner/app
#   PRERELEASE         "true" for a prerelease
#   STABLE_MOVED       "true" when this run moved the stable tag, "false" when
#                      it did not (a newer release exists, or a prerelease)
#   MOVING_TAGS        the moving tags this run set (stable, 1.2, 1), space separated
#
# Run it from the repository root with <tag> and <previous-tag> available
# (a full clone, not a shallow one).

set -euo pipefail

MAX_COMMITS=100

tag=${1:?usage: release-notes.sh <tag> [<previous-tag>]}
prev=${2:-}
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=${GITHUB_REPOSITORY:-}
image=${IMAGE:-}
version=${tag#v}

git rev-parse --verify --quiet "refs/tags/${tag}^{commit}" >/dev/null \
  || { echo "release-notes: tag $tag does not exist in this clone (fetch the tags and full history)." >&2; exit 1; }
if [[ -n $prev ]]; then
  git rev-parse --verify --quiet "refs/tags/${prev}^{commit}" >/dev/null \
    || { echo "release-notes: tag $prev does not exist in this clone (fetch the tags and full history)." >&2; exit 1; }
fi

if section=$("$here/release-channel.sh" changelog "$version" CHANGELOG.md); then
  printf '%s\n\n' "$section"
fi

if [[ -n $prev ]]; then
  range="${prev}..${tag}"
  echo "## Changes since ${prev}"
else
  range=$tag
  echo '## Changes'
fi
echo

# One line per commit: "- subject (`abc1234`)" (\x60 is a backtick).
log_format=$'- %s (\x60%h\x60)'
mapfile -t commits < <(git log --no-merges --max-count=$((MAX_COMMITS + 1)) --pretty=format:"$log_format" "$range")
if ((${#commits[@]} == 0)); then
  echo "No commits of their own; see the full comparison."
else
  printf '%s\n' "${commits[@]:0:MAX_COMMITS}"
  if ((${#commits[@]} > MAX_COMMITS)); then
    echo "- and earlier commits: see the full comparison."
  fi
fi
echo

if [[ -n $repo ]]; then
  if [[ -n $prev ]]; then
    echo "**Full comparison:** https://github.com/${repo}/compare/${prev}...${tag}"
  else
    echo "**All commits:** https://github.com/${repo}/commits/${tag}"
  fi
  if [[ -f CHANGELOG.md ]]; then
    echo
    echo "**Changelog:** https://github.com/${repo}/blob/${tag}/CHANGELOG.md"
  fi
  echo
fi

if [[ -n $image ]]; then
  echo '## Docker image'
  echo
  printf '    docker pull %s:%s\n\n' "$image" "$version"
  read -ra moving <<<"${MOVING_TAGS:-}"
  listed=''
  for t in "${moving[@]}"; do listed+="${listed:+, }\`:${t}\`"; done
  if [[ ${PRERELEASE:-false} == true ]]; then
    echo "This is a prerelease: it is published as \`:${version}\` only. \`:stable\` and \`:latest\` are unchanged."
  else
    [[ -z $listed ]] || echo "Moving tags that now point at this release: ${listed}."
    case ${STABLE_MOVED:-} in
      true) echo "The stable channel (\`:stable\`) now offers this release." ;;
      false) echo "\`:stable\` was not moved: a newer release exists, and the stable channel never moves backwards." ;;
      *) ;;
    esac
  fi
fi
