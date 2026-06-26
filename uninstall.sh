#!/usr/bin/env bash
# LambdaDoom — remove everything this project created, in your account and on your machine.
#
#   ./uninstall.sh
#
# Terminates the MicroVM, deletes the image, deletes the CloudFormation stack (S3 bucket +
# IAM roles), and removes the downloaded `doom` binary plus all local state (~/.lambdadoom).
# Your clone of the repo is left untouched. Best-effort: it keeps going past anything
# already gone.
set -uo pipefail

STACK="${LAMBDADOOM_STACK:-LambdaDoom}"
REGION="${AWS_REGION:-${AWS_DEFAULT_REGION:-us-east-1}}"
NAME="${LAMBDADOOM_NAME:-doom}"
HOME_DIR="${LAMBDADOOM_HOME:-$HOME/.lambdadoom}"

say(){ printf '\n\033[1;36m==>\033[0m %s\n' "$*"; }

# Prefer the region recorded in config (so we hit the same region we deployed to).
if [ -f "$HOME_DIR/config.toml" ]; then
  r="$(grep -E '^region' "$HOME_DIR/config.toml" | sed -E 's/.*"([^"]+)".*/\1/')"
  [ -n "$r" ] && REGION="$r"
fi

# Locate the ldoom CLI (needed for the MicroVMs API: terminate + delete image).
DOOM=""
for c in "${LDOOM_BIN:-}" "$HOME_DIR/bin/ldoom" "$HOME_DIR/bin/ldoom.exe" "rs-cli/target/release/ldoom" "rs-cli/target/release/ldoom.exe"; do
  [ -n "$c" ] && [ -x "$c" ] && { DOOM="$c"; break; }
done

# 1. terminate the microvm + delete the image
if [ -n "$DOOM" ]; then
  say "Removing the DOOM microvm + image"
  "$DOOM" rm --name "$NAME" || say "(nothing to remove, or already gone)"
else
  say "ldoom CLI not found — skipping microvm/image cleanup (delete the image manually if one exists)"
fi

# 2. empty + delete the CloudFormation stack (a non-empty bucket blocks stack deletion)
BUCKET="$(aws cloudformation describe-stacks --region "$REGION" --stack-name "$STACK" \
  --query "Stacks[0].Outputs[?OutputKey=='ArtifactBucket'].OutputValue" --output text 2>/dev/null || true)"
if [ -n "$BUCKET" ] && [ "$BUCKET" != "None" ]; then
  say "Emptying artifact bucket: $BUCKET"
  aws s3 rm "s3://$BUCKET" --recursive >/dev/null 2>&1 || true
fi
say "Deleting CloudFormation stack: $STACK"
aws cloudformation delete-stack --region "$REGION" --stack-name "$STACK" 2>/dev/null || true
aws cloudformation wait stack-delete-complete --region "$REGION" --stack-name "$STACK" 2>/dev/null || true

# 3. remove the downloaded binary + all local state (one directory)
say "Removing $HOME_DIR  (binary, config, state)"
rm -rf "$HOME_DIR"

say "LambdaDoom removed. Delete your clone of the repo if you no longer need it."
