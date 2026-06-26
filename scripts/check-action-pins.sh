#!/usr/bin/env bash
# Fail if any GitHub Action `uses:` ref is not pinned to a full 40-char commit SHA.
# Mutable tags/branches (e.g. @v4, @stable) let a compromised/retargeted third-party
# Action run inside our trusted release job (CWE-829/494). Local actions (./...) and
# reusable workflows in this repo are exempt. Run by CI and pre-commit.
set -euo pipefail

cd "$(git rev-parse --show-toplevel 2>/dev/null || echo .)"

fail=0
# Match lines like:  - uses: owner/repo@<ref>   (ignoring trailing "# comment").
while IFS= read -r line; do
  file="${line%%:*}"
  rest="${line#*:}"
  ref="$(printf '%s' "$rest" | sed -E 's/.*uses:[[:space:]]*//; s/[[:space:]]*#.*//')"
  # Exempt local actions / reusable workflows in this repo.
  case "$ref" in
    ./*|"") continue ;;
  esac
  pinned_part="${ref##*@}"
  # A full commit SHA is exactly 40 lowercase hex chars.
  if ! printf '%s' "$pinned_part" | grep -qE '^[0-9a-f]{40}$'; then
    echo "UNPINNED: $file -> $ref (pin to a full commit SHA, keep '# <tag>' as a comment)" >&2
    fail=1
  fi
done < <(grep -rnE '^[[:space:]]*-?[[:space:]]*uses:' .github/workflows/ 2>/dev/null)

if [ "$fail" = 0 ]; then
  echo "check-action-pins: OK — all workflow actions are pinned to commit SHAs"
else
  echo "check-action-pins: FAILED — unpinned action ref(s) above." >&2
  echo "Resolve the SHA with: gh api repos/<owner>/<repo>/commits/<tag> --jq .sha" >&2
  exit 1
fi
