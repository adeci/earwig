#!/usr/bin/env bash
# Creates the GitHub release of a pushed tag unless it exists: "earwig <tag>", with the
# annotated tag's message as notes, or GitHub's generated notes for a lightweight tag.
# nixbot's release effect (nix/release.nix) and the Windows CI job both run it.
#
# Usage: GH_TOKEN=... github-release.sh OWNER/REPO TAG
set -euo pipefail

repo=$1
tag=$2

if gh release view "$tag" --repo "$repo" >/dev/null 2>&1; then
  echo "release $tag exists"
  exit 0
fi

notes=(--generate-notes)
annotated=$(gh api "repos/$repo/git/ref/tags/$tag" --jq 'select(.object.type == "tag").object.sha')
if [[ -n $annotated ]]; then
  # A signed tag's message ends in its signature.
  # shellcheck disable=SC2016 # $signature is jq's
  message=$(gh api "repos/$repo/git/tags/$annotated" \
    --jq '(.verification.signature // "") as $signature | .message | rtrimstr($signature)')
  if [[ $message =~ [^[:space:]] ]]; then
    notes=(--notes "$message")
  fi
fi

# Both runners may get here at once: only a release that is still missing is an error.
gh release create "$tag" --repo "$repo" --verify-tag --title "earwig $tag" "${notes[@]}" ||
  gh release view "$tag" --repo "$repo" >/dev/null
