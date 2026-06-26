#!/usr/bin/env bash
# Hellbox one-command deploy.
#
#   ./deploy.sh
#
# Env: AWS_REGION, HELLBOX_STACK, HELLBOX_NAME, HELLBOX_REPO,
# HELLBOX_VERSION, HELLBOX_SKIP_ATTESTATION=1, HELLBOX_BIN.
set -euo pipefail

STACK="${HELLBOX_STACK:-Hellbox}"
REGION="${AWS_REGION:-${AWS_DEFAULT_REGION:-us-east-1}}"
NAME="${HELLBOX_NAME:-doom}"
REPO="${HELLBOX_REPO:-somoore/Hellbox}"
VERSION="${HELLBOX_VERSION:-latest}"
HOME_DIR="${HELLBOX_HOME:-$HOME/.hellbox}"
BIN_DIR="$HOME_DIR/bin"

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
say(){ printf '\n\033[1;36m==>\033[0m %s\n' "$*" >&2; }   # progress on stderr
die(){ printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }
warn(){ printf '\033[1;33mwarning:\033[0m %s\n' "$*" >&2; }

ext=""; case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) ext=".exe";; esac

detect_target(){
  local os arch
  case "$(uname -s)" in
    Linux) os=linux ;; Darwin) os=macos ;;
    MINGW*|MSYS*|CYGWIN*) os=windows ;;
    *) die "unsupported OS: $(uname -s)" ;;
  esac
  case "$(uname -m)" in
    x86_64|amd64) arch=x86_64 ;;
    arm64|aarch64) arch=arm64 ;;
    *) die "unsupported arch: $(uname -m)" ;;
  esac
  printf '%s-%s' "$os" "$arch"
}

sha256_file(){
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

# Transport-integrity check only: the .sha256 sidecar ships from the same release
# as the binary, so an attacker who can swap the asset can swap its sidecar too.
# This guards against truncated/corrupt downloads, NOT tampering. The cryptographic
# trust anchor is verify_attestation (GitHub build provenance), which is bound to the
# release workflow identity and cannot be forged by replacing release files. To avoid
# trusting any prebuilt artifact, build from source and pass HELLBOX_BIN.
verify_sha256(){
  local file sumfile expected actual
  file="$1"; sumfile="$2"
  expected="$(awk '{print $1}' "$sumfile")"
  [ -n "$expected" ] || die "empty checksum file: $sumfile"
  actual="$(sha256_file "$file")"
  [ "$actual" = "$expected" ] || die "SHA256 mismatch for $(basename "$file"): expected $expected, got $actual"
}

verify_attestation(){
  local file
  file="$1"
  if [ "${HELLBOX_SKIP_ATTESTATION:-0}" = "1" ]; then
    [ "$VERSION" != "latest" ] \
      || die "HELLBOX_SKIP_ATTESTATION=1 requires a pinned HELLBOX_VERSION, not latest"
    warn "skipping GitHub artifact attestation verification for pinned release $VERSION"
    return
  fi
  command -v gh >/dev/null 2>&1 \
    || die "gh is required to verify GitHub artifact attestation for $(basename "$file") (install gh, build from source with HELLBOX_BIN, or set HELLBOX_SKIP_ATTESTATION=1 with a pinned HELLBOX_VERSION)"
  gh attestation verify "$file" --repo "$REPO" \
    --signer-workflow "github.com/$REPO/.github/workflows/release.yml" >/dev/null \
    || die "GitHub artifact attestation verification failed for $(basename "$file")"
}

resolve_release_tag(){
  if [ "$VERSION" != "latest" ]; then printf '%s' "$VERSION"; return; fi
  if command -v gh >/dev/null 2>&1; then
    gh release view --repo "$REPO" --json tagName --jq .tagName 2>/dev/null \
      || die "could not resolve latest release for $REPO"
  else
    curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
      | sed -n 's/.*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' \
      | head -1
  fi
}

download_release_asset(){
  local rel asset out url
  rel="$1"; asset="$2"; out="$3"
  url="https://github.com/$REPO/releases/download/$rel/$asset"
  if curl -fSL "$url" -o "$out" 2>/dev/null; then
    return
  fi
  if command -v gh >/dev/null 2>&1; then
    say "(public download failed — fetching $asset via gh; private repo?)"
    gh release download "$rel" --repo "$REPO" --pattern "$asset" --output "$out" --clobber \
      || die "could not download $asset from $REPO ($rel)"
  else
    die "could not download $url — is the repo public and a release published? (or set HELLBOX_BIN=/path/to/hellbox)"
  fi
}

# Resolve CLI: override -> cache -> local release build -> release download.
resolve_hellbox(){
  if [ -n "${HELLBOX_BIN:-}" ]; then printf '%s' "$HELLBOX_BIN"; return; fi
  if [ -x "$BIN_DIR/hellbox$ext" ] && [ -f "$BIN_DIR/hellbox$ext.sha256" ]; then
    verify_sha256 "$BIN_DIR/hellbox$ext" "$BIN_DIR/hellbox$ext.sha256"
    verify_attestation "$BIN_DIR/hellbox$ext"
    printf '%s' "$BIN_DIR/hellbox$ext"
    return
  fi
  if [ -x "rs-cli/target/release/hellbox$ext" ]; then printf '%s' "$(pwd)/rs-cli/target/release/hellbox$ext"; return; fi
  local asset rel tmp_bin tmp_sum; asset="hellbox-$(detect_target)$ext"
  rel="$(resolve_release_tag)"
  [ -n "$rel" ] || die "could not resolve release tag for $REPO"
  say "Downloading the hellbox CLI: $asset ($rel)"
  mkdir -p "$BIN_DIR"
  tmp_bin="$BIN_DIR/$asset"
  tmp_sum="$BIN_DIR/$asset.sha256"
  download_release_asset "$rel" "$asset" "$tmp_bin"
  download_release_asset "$rel" "$asset.sha256" "$tmp_sum"
  verify_sha256 "$tmp_bin" "$tmp_sum"
  say "Verified SHA256 for $asset"
  verify_attestation "$tmp_bin"
  [ "${HELLBOX_SKIP_ATTESTATION:-0}" = "1" ] \
    || say "Verified GitHub artifact attestation for $asset"
  mv "$tmp_bin" "$BIN_DIR/hellbox$ext"
  mv "$tmp_sum" "$BIN_DIR/hellbox$ext.sha256"
  chmod +x "$BIN_DIR/hellbox$ext"
  printf '%s' "$BIN_DIR/hellbox$ext"
}

# Preflight.
command -v aws >/dev/null || die "the AWS CLI is required: https://aws.amazon.com/cli/"
command -v curl >/dev/null || die "curl is required"
command -v awk >/dev/null || die "awk is required"
if ! command -v sha256sum >/dev/null 2>&1 && ! command -v shasum >/dev/null 2>&1; then
  die "sha256sum or shasum is required"
fi
aws sts get-caller-identity >/dev/null 2>&1 \
  || die "AWS credentials aren't working — configure the AWS CLI (or assume a role) first"
[ -f deploy/doom.yaml ] || die "run this from the repo root (deploy/doom.yaml not found)"
[ -d capsule ] || die "run this from the repo root (capsule/ not found)"

# Infra.
say "Deploying AWS prerequisites  (stack: $STACK, region: $REGION)"
aws cloudformation deploy \
  --region "$REGION" --stack-name "$STACK" \
  --template-file deploy/doom.yaml \
  --capabilities CAPABILITY_IAM \
  --no-fail-on-empty-changeset

# Stack outputs -> config.
out(){ aws cloudformation describe-stacks --region "$REGION" --stack-name "$STACK" \
  --query "Stacks[0].Outputs[?OutputKey=='$1'].OutputValue" --output text; }
BUCKET="$(out ArtifactBucket)"; BUILD_ROLE="$(out BuildRoleArn)"; EXEC_ROLE="$(out ExecutionRoleArn)"
if [ -z "$BUCKET" ] || [ "$BUCKET" = "None" ]; then
  die "could not read stack outputs"
fi
mkdir -p "$HOME_DIR"
cat > "$HOME_DIR/config.toml" <<EOF
region             = "$REGION"
artifact_bucket    = "$BUCKET"
build_role_arn     = "$BUILD_ROLE"
execution_role_arn = "$EXEC_ROLE"
base_image_arn     = "arn:aws:lambda:$REGION:aws:microvm-image:al2023-1"
display            = "h264"
EOF
say "Wrote $HOME_DIR/config.toml"

# CLI.
HELLBOX="$(resolve_hellbox)"
say "Using hellbox CLI: $HELLBOX"

# Build, launch, open.
say "Building the DOOM MicroVM image  (compiles the engine + fetches the WAD; a few minutes)"
"$HELLBOX" build --name "$NAME"
say "Launching the MicroVM"
"$HELLBOX" up --name "$NAME"
say "Opening DOOM  (http://127.0.0.1:6080)"
"$HELLBOX" open --name "$NAME"
