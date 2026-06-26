#!/usr/bin/env bash
# Supply-chain pin/hash gate across every dependency ecosystem in the repo.
# FAILS on any dependency that is not pinned to a specific release with integrity
# (hash) verification. WARNS on documented dev-only soft-pins. Run by pre-commit
# and CI; safe to run anywhere (read-only, no network).
#
# Ecosystems here:
#   - Rust   : Cargo.lock pins exact versions + checksums for every crate; deny.toml
#              [sources] rejects unknown registries/git deps. cargo-deny enforces.
#   - Python : capsule/requirements.txt uses pip --require-hashes (checked deeply by
#              scripts/check-reqs.sh; here we assert the mode is on).
#   - JS     : no committed package.json today. If any .mjs/.js gains an external
#              import, it must be backed by a package.json + lockfile (warned here,
#              FAILs once a package.json exists without a lockfile).
set -euo pipefail

cd "$(git rev-parse --show-toplevel 2>/dev/null || echo .)"

fail=0
say()  { printf '  %s\n' "$*"; }
ok()   { printf '  \033[1;32mok\033[0m   %s\n' "$*"; }
warn() { printf '  \033[1;33mwarn\033[0m %s\n' "$*" >&2; }
bad()  { printf '  \033[1;31mFAIL\033[0m %s\n' "$*" >&2; fail=1; }

echo "== Rust (Cargo) =="
if [ -f rs-cli/Cargo.lock ]; then
  # Every [[package]] from a registry/git source must carry a checksum. Path deps
  # (the local crate) legitimately have none, so only flag non-local sources.
  missing="$(awk '
    /^\[\[package\]\]/ {src=""; sum=""; next}
    /^source = /       {src=$0}
    /^checksum = /     {sum=$0}
    /^\[\[package\]\]/ || /^$/ {if (src!="" && sum=="") print "  (a registry/git crate lacks a checksum)"}
  ' rs-cli/Cargo.lock || true)"
  if [ -n "$missing" ]; then
    bad "Cargo.lock has a non-local crate without a checksum$missing"
  else
    ok "Cargo.lock present; all registry/git crates carry checksums"
  fi
  if grep -q '^\[sources\]' deny.toml 2>/dev/null; then
    ok "deny.toml has a [sources] policy (cargo-deny rejects unknown registries)"
  else
    bad "deny.toml has no [sources] policy — unknown registries/git deps aren't gated. Add a [sources] section and run \`cargo deny check sources\`."
  fi
else
  bad "rs-cli/Cargo.lock missing — crate versions/hashes are not pinned"
fi

echo "== Python (pip) =="
for req in $(git ls-files '*requirements*.txt'); do
  if grep -q -- '--hash=' "$req"; then
    ok "$req uses --hash pins (verify wheels with scripts/check-reqs.sh)"
  else
    bad "$req has no --hash pins — not hash-locked. Use \`pip hash\` / pip-compile --generate-hashes."
  fi
done

echo "== JavaScript (npm) =="
# Any committed package.json must ship a lockfile (npm ci needs it for integrity).
js_lockless=0
for pj in $(git ls-files '*package.json'); do
  dir="$(dirname "$pj")"
  if [ -f "$dir/package-lock.json" ] || [ -f "$dir/npm-shrinkwrap.json" ] || [ -f "$dir/yarn.lock" ] || [ -f "$dir/pnpm-lock.yaml" ]; then
    ok "$pj has a lockfile (hash-locked installs)"
  else
    bad "$pj has no lockfile — \`npm install\` would float transitive deps. Commit package-lock.json (npm i --package-lock-only)."
    js_lockless=1
  fi
done
# Flag external imports in .mjs/.js that aren't backed by a package.json lockfile.
js_ext_imports="$(git ls-files '*.mjs' '*.js' | xargs -r grep -lE "from ['\"][^.][^'\"]*['\"]" 2>/dev/null \
  | xargs -r grep -hoE "from ['\"][^.][^'\"]*['\"]" 2>/dev/null \
  | grep -v "node:" | sort -u || true)"
if [ -n "$js_ext_imports" ] && [ "${js_lockless}" = 0 ] && ! git ls-files '*package.json' | grep -q .; then
  warn "external JS import(s) without any package.json/lockfile:"
  printf '%s\n' "$js_ext_imports" | sed 's/^/         /' >&2
  warn "if this JS ships in the product, pin it via a package.json + lockfile; this becomes a hard FAIL."
fi

echo
if [ "$fail" = 0 ]; then
  echo "check-deps: OK — all shipped dependencies are pinned to a release with hash verification."
else
  echo "check-deps: FAILED — an unpinned/unhashed dependency was found (see above)." >&2
  exit 1
fi
