#!/usr/bin/env bash
# LambdaDoom — one command to play DOOM from an AWS Lambda MicroVM.
#
# Deploys the AWS prerequisites (S3 bucket + IAM roles) via CloudFormation, fetches the
# prebuilt `ldoom` CLI (no compiling), builds the MicroVM image, launches it, and opens
# the game in your browser. Run from a clone of this repo:
#
#   ./deploy.sh
#
# Override anything via env: AWS_REGION, LAMBDADOOM_STACK, LAMBDADOOM_NAME,
# LAMBDADOOM_REPO, LAMBDADOOM_VERSION, LDOOM_BIN (use a specific binary instead of
# downloading). Remove everything later with ./uninstall.sh.
set -euo pipefail

STACK="${LAMBDADOOM_STACK:-LambdaDoom}"
REGION="${AWS_REGION:-${AWS_DEFAULT_REGION:-us-east-1}}"
NAME="${LAMBDADOOM_NAME:-doom}"
REPO="${LAMBDADOOM_REPO:-somoore/LambdaDoom}"
VERSION="${LAMBDADOOM_VERSION:-latest}"
HOME_DIR="${LAMBDADOOM_HOME:-$HOME/.lambdadoom}"
BIN_DIR="$HOME_DIR/bin"

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
say(){ printf '\n\033[1;36m==>\033[0m %s\n' "$*" >&2; }   # progress on stderr
die(){ printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

ext=""; case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) ext=".exe";; esac

# Detect a release asset name like ldoom-linux-x86_64 / ldoom-macos-arm64 / ldoom-windows-x86_64.exe
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

# Resolve the ldoom binary: explicit override -> cached download -> local dev build -> download.
resolve_doom(){
  if [ -n "${LDOOM_BIN:-}" ]; then printf '%s' "$LDOOM_BIN"; return; fi
  if [ -x "$BIN_DIR/ldoom$ext" ]; then printf '%s' "$BIN_DIR/ldoom$ext"; return; fi
  if [ -x "rs-cli/target/release/ldoom$ext" ]; then printf '%s' "$(pwd)/rs-cli/target/release/ldoom$ext"; return; fi
  local asset url; asset="ldoom-$(detect_target)$ext"
  if [ "$VERSION" = "latest" ]; then url="https://github.com/$REPO/releases/latest/download/$asset"
  else url="https://github.com/$REPO/releases/download/$VERSION/$asset"; fi
  say "Downloading the ldoom CLI: $asset"
  mkdir -p "$BIN_DIR"
  # Public repos: the release URL works with plain curl. Private repos: that URL 403s,
  # so fall back to the authenticated GitHub CLI if it's available.
  if ! curl -fSL "$url" -o "$BIN_DIR/ldoom$ext" 2>/dev/null; then
    if command -v gh >/dev/null 2>&1; then
      say "(public download failed — fetching via gh; private repo?)"
      local rel="$VERSION"
      [ "$rel" = "latest" ] && rel="$(gh release list --repo "$REPO" --limit 1 --json tagName --jq '.[0].tagName' 2>/dev/null)"
      gh release download "$rel" --repo "$REPO" --pattern "$asset" --output "$BIN_DIR/ldoom$ext" --clobber \
        || die "could not download $asset from $REPO ($rel)"
    else
      die "could not download $url — is the repo public and a release published? (or set LDOOM_BIN=/path/to/ldoom)"
    fi
  fi
  chmod +x "$BIN_DIR/ldoom$ext"
  printf '%s' "$BIN_DIR/ldoom$ext"
}

# ---- preflight ----
command -v aws >/dev/null || die "the AWS CLI is required: https://aws.amazon.com/cli/"
command -v curl >/dev/null || die "curl is required"
aws sts get-caller-identity >/dev/null 2>&1 \
  || die "AWS credentials aren't working — configure the AWS CLI (or assume a role) first"
[ -f deploy/doom.yaml ] || die "run this from the repo root (deploy/doom.yaml not found)"
[ -d capsule ] || die "run this from the repo root (capsule/ not found)"

# ---- 1. infra: S3 bucket + IAM roles (CloudFormation; blocks until complete) ----
say "Deploying AWS prerequisites  (stack: $STACK, region: $REGION)"
aws cloudformation deploy \
  --region "$REGION" --stack-name "$STACK" \
  --template-file deploy/doom.yaml \
  --capabilities CAPABILITY_IAM \
  --no-fail-on-empty-changeset

# ---- 2. stack outputs -> ~/.lambdadoom/config.toml ----
out(){ aws cloudformation describe-stacks --region "$REGION" --stack-name "$STACK" \
  --query "Stacks[0].Outputs[?OutputKey=='$1'].OutputValue" --output text; }
BUCKET="$(out ArtifactBucket)"; BUILD_ROLE="$(out BuildRoleArn)"; EXEC_ROLE="$(out ExecutionRoleArn)"
[ -n "$BUCKET" ] && [ "$BUCKET" != "None" ] || die "could not read stack outputs"
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

# ---- 3. get the prebuilt CLI (no compiling) ----
DOOM="$(resolve_doom)"
say "Using ldoom CLI: $DOOM"

# ---- 4. build the image, launch it, open the game ----
say "Building the DOOM MicroVM image  (compiles the engine + fetches the WAD; a few minutes)"
"$DOOM" build --name "$NAME"
say "Launching the MicroVM"
"$DOOM" up --name "$NAME"
say "Opening DOOM  (http://127.0.0.1:6080)"
"$DOOM" open --name "$NAME"
