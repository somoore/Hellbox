#!/usr/bin/env bash
# Remove Hellbox resources and local state.
#
#   ./uninstall.sh
set -uo pipefail

DEFAULT_REGION="${AWS_REGION:-${AWS_DEFAULT_REGION:-us-east-1}}"
HELLBOX_HOME_DIR="${HELLBOX_HOME:-$HOME/.hellbox}"
LEGACY_HOME_DIR="${LAMBDADOOM_HOME:-$HOME/.lambdadoom}"

say(){ printf '\n\033[1;36m==>\033[0m %s\n' "$*"; }
warn(){ printf '\033[1;33mwarning:\033[0m %s\n' "$*" >&2; }
FAILED=0

home_specs=("hellbox|$HELLBOX_HOME_DIR|${HELLBOX_NAME:-doom}")
if { [ -z "${HELLBOX_HOME:-}" ] || [ -n "${LAMBDADOOM_HOME:-}" ]; } && [ "$LEGACY_HOME_DIR" != "$HELLBOX_HOME_DIR" ]; then
  home_specs+=("lambdadoom|$LEGACY_HOME_DIR|${LAMBDADOOM_NAME:-doom}")
fi

if [ -n "${HELLBOX_STACK:-}" ] || [ -n "${LAMBDADOOM_STACK:-}" ]; then
  stack_specs=()
  [ -n "${HELLBOX_STACK:-}" ] && stack_specs+=("hellbox|$HELLBOX_HOME_DIR|$HELLBOX_STACK")
  [ -n "${LAMBDADOOM_STACK:-}" ] && stack_specs+=("lambdadoom|$LEGACY_HOME_DIR|$LAMBDADOOM_STACK")
else
  stack_specs=("hellbox|$HELLBOX_HOME_DIR|Hellbox")
  if { [ -z "${HELLBOX_HOME:-}" ] || [ -n "${LAMBDADOOM_HOME:-}" ]; } && [ "$LEGACY_HOME_DIR" != "$HELLBOX_HOME_DIR" ]; then
    stack_specs+=("lambdadoom|$LEGACY_HOME_DIR|LambdaDoom")
  fi
fi

region_for_home(){
  local d="$1" default_region="$2" r
  if [ -f "$d/config.toml" ]; then
    r="$(grep -E '^region' "$d/config.toml" | sed -E 's/.*"([^"]+)".*/\1/')"
    if [ -n "$r" ]; then
      printf '%s' "$r"
      return
    fi
  fi
  printf '%s' "$default_region"
}

stack_missing(){
  case "$1" in
    *"does not exist"*|*"doesn't exist"*) return 0 ;;
    *) return 1 ;;
  esac
}

cli_for_home(){
  local kind="$1" dir="$2" c
  local -a candidates
  if [ "$kind" = "lambdadoom" ]; then
    candidates=(
      "${LDOOM_BIN:-}"
      "${LAMBDADOOM_BIN:-}"
      "${HELLBOX_BIN:-}"
      "$dir/bin/ldoom"
      "$dir/bin/ldoom.exe"
      "$dir/bin/hellbox"
      "$dir/bin/hellbox.exe"
      "rs-cli/target/release/ldoom"
      "rs-cli/target/release/ldoom.exe"
      "rs-cli/target/release/hellbox"
      "rs-cli/target/release/hellbox.exe"
    )
  else
    candidates=(
      "${HELLBOX_BIN:-}"
      "${LAMBDADOOM_BIN:-}"
      "${LDOOM_BIN:-}"
      "$dir/bin/hellbox"
      "$dir/bin/hellbox.exe"
      "$dir/bin/ldoom"
      "$dir/bin/ldoom.exe"
      "rs-cli/target/release/hellbox"
      "rs-cli/target/release/hellbox.exe"
      "rs-cli/target/release/ldoom"
      "rs-cli/target/release/ldoom.exe"
    )
  fi
  for c in "${candidates[@]}"; do
    [ -n "$c" ] && [ -x "$c" ] && { printf '%s' "$c"; return; }
  done
}

# Stop a running loopback proxy (`hellbox open`) if one is still up — uninstall
# should be self-contained whether or not the user Ctrl-C'd it first. Targets only
# the hellbox proxy by command pattern; harmless if none is running.
if command -v pkill >/dev/null 2>&1; then
  if pkill -f 'hellbox(\.exe)? open' 2>/dev/null; then
    say "Stopped the running hellbox proxy"
  fi
fi

# MicroVM + image.
for spec in "${home_specs[@]}"; do
  IFS='|' read -r kind d name <<<"$spec"
  [ -d "$d" ] || continue
  DOOM="$(cli_for_home "$kind" "$d")"
  if [ -n "$DOOM" ]; then
    say "Removing the DOOM microvm + image using state in $d"
    HELLBOX_HOME="$d" LAMBDADOOM_HOME="$d" "$DOOM" rm --name "$name" \
      || say "(nothing to remove, or already gone)"
  else
    say "hellbox/ldoom CLI not found for $d — skipping microvm/image cleanup (delete the image manually if one exists)"
  fi
done

# Stack.
for spec in "${stack_specs[@]}"; do
  IFS='|' read -r kind d stack <<<"$spec"
  REGION="$(region_for_home "$d" "$DEFAULT_REGION")"
  describe_output=""
  if ! describe_output="$(aws cloudformation describe-stacks --region "$REGION" --stack-name "$stack" 2>&1)"; then
    if stack_missing "$describe_output"; then
      say "CloudFormation stack not found in $REGION, skipping: $stack"
    else
      warn "could not describe CloudFormation stack '$stack' in $REGION — leaving local state in place; AWS said: $describe_output"
      FAILED=1
    fi
    continue
  fi
  if ! BUCKET="$(aws cloudformation describe-stacks --region "$REGION" --stack-name "$stack" \
    --query "Stacks[0].Outputs[?OutputKey=='ArtifactBucket'].OutputValue" --output text 2>&1)"; then
    warn "could not read artifact bucket for '$stack' in $REGION — leaving local state in place; AWS said: $BUCKET"
    FAILED=1
    continue
  fi
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

if [ "$FAILED" = 1 ]; then
  warn "uninstall finished with errors — local state was left in place so cleanup can be retried after fixing AWS access."
  warn "verify in the AWS console that the stack, bucket, and any MicroVM/image are gone (they may still incur cost)."
  exit 1
fi

# Local state.
for spec in "${home_specs[@]}"; do
  IFS='|' read -r _ d _ <<<"$spec"
  say "Removing $d  (binary, config, state)"
  rm -rf "$d"
done

say "Hellbox removed. Delete your clone of the repo if you no longer need it."
