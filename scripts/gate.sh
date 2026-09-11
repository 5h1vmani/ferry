#!/usr/bin/env bash
#
# gate.sh: the one check every builder and the orchestrator run before a
# push. It runs a mode's steps in order and stops at the first failure.
#
# Modes, given as the first argument:
#   rust        Format, lint, and test the Rust workspace with all features.
#   rust-quick  Format and lint, then run cargo test with your own arguments.
#               Put your arguments after "--", for example:
#               scripts/gate.sh rust-quick -- -p ferry-runtime --test two_engines
#   gen         Check the generated tokens, the generated errors, and the
#               generated app bindings all match their source.
#   mac         Build the Mac app in Debug with xcodegen and xcodebuild.
#   android     Build the Android app's Kotlin sources with Gradle.
#   all         Run rust, then gen, then mac, then android, in that order.
#
# Each step prints "gate: <step> ok" or "gate: <step> FAIL (exit N)".
# Full step output goes to a log file, target/gate.log under the repo root
# by default. Set GATE_LOG to use a different path. On failure, the last
# 40 lines of the failing step's output print to stderr.

set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

GATE_LOG="${GATE_LOG:-$repo_root/target/gate.log}"
mkdir -p "$repo_root/target" "$(dirname "$GATE_LOG")"

# Start every run with an empty log, so it reflects only this run.
: > "$GATE_LOG"

# A per-step temp dir. Each step's output lands in its own file here first,
# so a failure can show that step's own last lines, not another step's.
step_tmp_dir="$(mktemp -d "$repo_root/target/gate-step.XXXXXX")"
trap 'rm -rf "$step_tmp_dir"' EXIT

step_count=0

# run_step NAME CMD [ARGS...]
#
# Runs CMD with its own output captured to a private file, never through a
# pipe, so the exit code checked below is always CMD's own exit code.
run_step() {
  local name="$1"
  shift
  step_count=$((step_count + 1))
  local step_file="$step_tmp_dir/$step_count.log"

  "$@" >"$step_file" 2>&1
  local status=$?

  cat "$step_file" >>"$GATE_LOG"

  if [ "$status" -eq 0 ]; then
    echo "gate: $name ok"
  else
    echo "gate: $name FAIL (exit $status)" >&2
    tail -n 40 "$step_file" >&2
    exit "$status"
  fi
}

# ---- gen mode helper: the bindings diff from ci.yml, into a target/ temp
# dir instead of /tmp, so two worktrees never share it. ----
gen_bindings_check() {
  local out_root
  out_root="$(mktemp -d "$repo_root/target/gate-bindings.XXXXXX")"
  local status=0

  scripts/gen_bindings.sh "$out_root" || status=$?

  if [ "$status" -eq 0 ]; then
    local f
    for f in ferry_runtime.swift ferry_runtimeFFI.h ferry_runtimeFFI.modulemap; do
      diff "$out_root/macos/Ferry/Generated/$f" "macos/Ferry/Generated/$f" || {
        status=$?
        break
      }
    done
  fi

  if [ "$status" -eq 0 ]; then
    diff -r "$out_root/android/app/src/main/kotlin/uniffi" "android/app/src/main/kotlin/uniffi" || status=$?
  fi

  rm -rf "$out_root"
  return "$status"
}

# ---- mac mode helper: xcodegen then xcodebuild, in a subshell so the cd
# never leaks into a later step of an "all" run. ----
mac_build() {
  (
    cd "$repo_root/macos" &&
      xcodegen generate -q &&
      xcodebuild -project Ferry.xcodeproj -scheme Ferry -configuration Debug -derivedDataPath build -quiet build
  )
}

# ---- android mode helper: source env.sh, then gradle, in a subshell so
# the env and the cd never leak into a later step of an "all" run. The
# task ":app:compileDebugKotlin" exists under this repo's Gradle setup
# (AGP 9's built-in Kotlin support), confirmed with `gradle :app:tasks`. ----
android_build() {
  (
    source "$repo_root/scripts/env.sh" &&
      cd "$repo_root/android" &&
      gradle :app:compileDebugKotlin -q
  )
}

mode_rust() {
  run_step "cargo fmt" cargo fmt --all --check
  run_step "cargo clippy" cargo clippy --all-targets --all-features -- -D warnings
  run_step "cargo test" cargo test --all --all-features
}

mode_rust_quick() {
  run_step "cargo fmt" cargo fmt --all --check
  run_step "cargo clippy" cargo clippy --all-targets --all-features -- -D warnings
  if [ "${#extra_test_args[@]}" -gt 0 ]; then
    run_step "cargo test" cargo test "${extra_test_args[@]}"
  else
    run_step "cargo test" cargo test
  fi
}

mode_gen() {
  run_step "gen tokens" python3 scripts/gen_tokens.py --check
  run_step "gen errors" python3 scripts/gen_errors.py --check
  run_step "gen bindings" gen_bindings_check
}

mode_mac() {
  run_step "mac build" mac_build
}

mode_android() {
  run_step "android build" android_build
}

mode_all() {
  mode_rust
  mode_gen
  mode_mac
  mode_android
}

mode="${1:-}"

case "$mode" in
  rust)
    mode_rust
    ;;
  rust-quick)
    shift
    if [ "${1:-}" = "--" ]; then
      shift
    fi
    extra_test_args=("$@")
    mode_rust_quick
    ;;
  gen)
    mode_gen
    ;;
  mac)
    mode_mac
    ;;
  android)
    mode_android
    ;;
  all)
    mode_all
    ;;
  *)
    echo "gate: usage: $0 {rust|rust-quick|gen|mac|android|all} [-- cargo test args]" >&2
    exit 2
    ;;
esac

exit 0
