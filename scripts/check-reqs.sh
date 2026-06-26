#!/usr/bin/env bash
# Verify capsule/requirements.txt resolves + hash-matches against the SAME platform
# the capsule Dockerfile installs on (cp311, aarch64, all manylinux glibc tiers).
#
# This is the M3 catcher: a single-glibc wheel hash silently breaks the ~6-min cloud
# build when the build host pulls a newer-glibc wheel. `pip download` with explicit
# --platform/--python-version/--abi resolves the exact wheels the Dockerfile sees, on
# ANY host OS — so a Mac or x86 dev (or CI) catches it in seconds instead of in the cloud.
#
# Why pip download and not `pip install --dry-run`: a plain dry-run on a dev machine
# resolves the HOST's wheels (e.g. macOS/cp314), which fail for the wrong reason and tell
# you nothing about the Linux/aarch64 build. The --platform flags pin the real target.
set -euo pipefail

REQ="${1:-capsule/requirements.txt}"
PYVER="311"                 # capsule uses python3.11
ABI="cp311"
# Match the Dockerfile's base (AL2023 aarch64). pip accepts a wheel matching ANY listed
# platform, so list every glibc tier a build host might resolve to.
PLATFORMS=(
  manylinux2014_aarch64
  manylinux_2_17_aarch64
  manylinux_2_28_aarch64
  manylinux_2_34_aarch64
)

[ -f "$REQ" ] || { echo "check-reqs: no such file: $REQ" >&2; exit 2; }

PY="$(command -v python3 || command -v python)" \
  || { echo "check-reqs: python3 not found" >&2; exit 2; }

plat_args=()
for p in "${PLATFORMS[@]}"; do plat_args+=(--platform "$p"); done

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# --no-deps: requirements.txt is fully pinned (incl. transitive deps), so resolve exactly
# what's listed. --only-binary + --require-hashes mirror the Dockerfile's pip invocation.
if "$PY" -m pip download \
      --no-deps --only-binary=:all: --require-hashes \
      "${plat_args[@]}" \
      --python-version "$PYVER" --implementation cp --abi "$ABI" \
      -r "$REQ" -d "$tmp" >"$tmp/log" 2>&1; then
  echo "check-reqs: OK — all wheels in $REQ resolve + hash-match for cp$PYVER/aarch64"
else
  rc=$?
  echo "check-reqs: FAILED for $REQ (cp$PYVER/aarch64, glibc ${PLATFORMS[*]})" >&2
  echo "----- pip output -----" >&2
  # Show the hash mismatch / resolution error and the correct hash to paste in.
  grep -iE 'do not match|expected sha256|got |no matching distribution|could not find' "$tmp/log" >&2 \
    || tail -25 "$tmp/log" >&2
  echo "----------------------" >&2
  echo "Fix: add the 'Got' hash above to the package's --hash lines in $REQ" >&2
  echo "(multi-hash is fine — pip accepts a wheel matching ANY listed hash)." >&2
  exit "$rc"
fi
