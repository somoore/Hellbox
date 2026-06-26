#!/usr/bin/env bash
# Remove Hellbox resources and local state.
#
#   ./uninstall.sh
set -uo pipefail

REGION="${AWS_REGION:-${AWS_DEFAULT_REGION:-us-east-1}}"
NAME="${HELLBOX_NAME:-${LAMBDADOOM_NAME:-doom}}"
HELLBOX_HOME_DIR="${HELLBOX_HOME:-$HOME/.hellbox}"
LEGACY_HOME_DIR="${LAMBDADOOM_HOME:-$HOME/.lambdadoom}"

say(){ printf '\n\033[1;36m==>\033[0m %s\n' "$*"; }
warn(){ printf '\033[1;33mwarning:\033[0m %s\n' "$*" >&2; }
FAILED=0

home_dirs=("$HELLBOX_HOME_DIR")
if [ -z "${HELLBOX_HOME:-}" ] && [ "$LEGACY_HOME_DIR" != "$HELLBOX_HOME_DIR" ]; then
  home_dirs+=("$LEGACY_HOME_DIR")
fi

if [ -n "${HELLBOX_STACK:-}" ]; then
  stacks=("$HELLBOX_STACK")
elif [ -n "${LAMBDADOOM_STACK:-}" ]; then
  stacks=("$LAMBDADOOM_STACK")
else
  stacks=("Hellbox" "LambdaDoom")
fi

# Prefer the deployed region from config, checking new and legacy homes.
for d in "${home_dirs[@]}"; do
  if [ -f "$d/config.toml" ]; then
    r="$(grep -E '^region' "$d/config.toml" | sed -E 's/.*"([^"]+)".*/\1/')"
    [ -n "$r" ] && { REGION="$r"; break; }
  fi
done

# Stop a running loopback proxy (`hellbox open`) if one is still up — uninstall
# should be self-contained whether or not the user Ctrl-C'd it first. Targets only
# the hellbox proxy by command pattern; harmless if none is running.
if command -v pkill >/dev/null 2>&1; then
  if pkill -f 'hellbox(\.exe)? open' 2>/dev/null; then
    say "Stopped the running hellbox proxy"
  fi
fi

# Locate hellbox for MicroVM cleanup.
DOOM=""
for c in \
  "${HELLBOX_BIN:-}" \
  "${LAMBDADOOM_BIN:-}" \
  "$HELLBOX_HOME_DIR/bin/hellbox" \
  "$HELLBOX_HOME_DIR/bin/hellbox.exe" \
  "$LEGACY_HOME_DIR/bin/hellbox" \
  "$LEGACY_HOME_DIR/bin/hellbox.exe" \
  "$LEGACY_HOME_DIR/bin/ldoom" \
  "$LEGACY_HOME_DIR/bin/ldoom.exe" \
  "rs-cli/target/release/hellbox" \
  "rs-cli/target/release/hellbox.exe" \
  "rs-cli/target/release/ldoom" \
  "rs-cli/target/release/ldoom.exe"; do
  [ -n "$c" ] && [ -x "$c" ] && { DOOM="$c"; break; }
done

# MicroVM + image.
if [ -n "$DOOM" ]; then
  for d in "${home_dirs[@]}"; do
    [ -d "$d" ] || continue
    say "Removing the DOOM microvm + image using state in $d"
    HELLBOX_HOME="$d" LAMBDADOOM_HOME="$d" "$DOOM" rm --name "$NAME" \
      || say "(nothing to remove, or already gone)"
  done
else
  say "hellbox/ldoom CLI not found — skipping microvm/image cleanup (delete the image manually if one exists)"
fi

# Stack.
for stack in "${stacks[@]}"; do
  if ! aws cloudformation describe-stacks --region "$REGION" --stack-name "$stack" >/dev/null 2>&1; then
    say "CloudFormation stack not found, skipping: $stack"
    continue
  fi
  BUCKET="$(aws cloudformation describe-stacks --region "$REGION" --stack-name "$stack" \
    --query "Stacks[0].Outputs[?OutputKey=='ArtifactBucket'].OutputValue" --output text 2>/dev/null || true)"
  if [ -n "$BUCKET" ] && [ "$BUCKET" != "None" ]; then
    say "Emptying artifact bucket: $BUCKET"
    # Surface the real error: a non-empty bucket blocks stack deletion, and a
    # silently-failed empty leaves billing resources behind.
    if ! aws s3 rm "s3://$BUCKET" --recursive >/dev/null; then
      warn "could not empty s3://$BUCKET — delete its objects manually, else stack deletion will fail"
      FAILED=1
    fi
  fi
  say "Deleting CloudFormation stack: $stack"
  if ! aws cloudformation delete-stack --region "$REGION" --stack-name "$stack"; then
    warn "delete-stack call failed for '$stack' in $REGION — check the CloudFormation console"
    FAILED=1
  elif ! aws cloudformation wait stack-delete-complete --region "$REGION" --stack-name "$stack"; then
    warn "stack '$stack' did not finish deleting — check the CloudFormation console (resources may remain and still bill)"
    FAILED=1
  fi
done

# Local state.
for d in "${home_dirs[@]}"; do
  say "Removing $d  (binary, config, state)"
  rm -rf "$d"
done

if [ "$FAILED" = 1 ]; then
  warn "uninstall finished with errors — verify in the AWS console that the stack, bucket, and any MicroVM/image are gone (they may still incur cost)."
  exit 1
fi
say "Hellbox removed. Delete your clone of the repo if you no longer need it."
