#!/usr/bin/env bash
# Remove Hellbox resources and local state.
#
#   ./uninstall.sh
set -uo pipefail

STACK="${HELLBOX_STACK:-Hellbox}"
REGION="${AWS_REGION:-${AWS_DEFAULT_REGION:-us-east-1}}"
NAME="${HELLBOX_NAME:-doom}"
HOME_DIR="${HELLBOX_HOME:-$HOME/.hellbox}"

say(){ printf '\n\033[1;36m==>\033[0m %s\n' "$*"; }
warn(){ printf '\033[1;33mwarning:\033[0m %s\n' "$*" >&2; }
FAILED=0

# Prefer the deployed region from config.
if [ -f "$HOME_DIR/config.toml" ]; then
  r="$(grep -E '^region' "$HOME_DIR/config.toml" | sed -E 's/.*"([^"]+)".*/\1/')"
  [ -n "$r" ] && REGION="$r"
fi

# Locate hellbox for MicroVM cleanup.
DOOM=""
for c in "${HELLBOX_BIN:-}" "$HOME_DIR/bin/hellbox" "$HOME_DIR/bin/hellbox.exe" "rs-cli/target/release/hellbox" "rs-cli/target/release/hellbox.exe"; do
  [ -n "$c" ] && [ -x "$c" ] && { DOOM="$c"; break; }
done

# MicroVM + image.
if [ -n "$DOOM" ]; then
  say "Removing the DOOM microvm + image"
  "$DOOM" rm --name "$NAME" || say "(nothing to remove, or already gone)"
else
  say "hellbox CLI not found — skipping microvm/image cleanup (delete the image manually if one exists)"
fi

# Stack.
BUCKET="$(aws cloudformation describe-stacks --region "$REGION" --stack-name "$STACK" \
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
say "Deleting CloudFormation stack: $STACK"
if ! aws cloudformation delete-stack --region "$REGION" --stack-name "$STACK"; then
  warn "delete-stack call failed for '$STACK' in $REGION — check the CloudFormation console"
  FAILED=1
elif ! aws cloudformation wait stack-delete-complete --region "$REGION" --stack-name "$STACK"; then
  warn "stack '$STACK' did not finish deleting — check the CloudFormation console (resources may remain and still bill)"
  FAILED=1
fi

# Local state.
say "Removing $HOME_DIR  (binary, config, state)"
rm -rf "$HOME_DIR"

if [ "$FAILED" = 1 ]; then
  warn "uninstall finished with errors — verify in the AWS console that the stack, bucket, and any MicroVM/image are gone (they may still incur cost)."
  exit 1
fi
say "Hellbox removed. Delete your clone of the repo if you no longer need it."
