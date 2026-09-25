#!/usr/bin/env bash
#
# gate.sh: the one check every contributor and CI run before a push. It
# runs a mode's steps in order and stops at the first failure.
#
# Modes, given as the first argument:
#   rust        Format, lint, and test the Rust workspace with all features.
#   rust-quick  Format and lint, then run cargo test with your own arguments.
#               Put your arguments after "--", for example:
#               scripts/gate.sh rust-quick -- -p ferry-runtime --test two_engines
#   gen         Check the generated tokens, the generated errors, and the
#               generated app bindings all match their source.
#   mac         Build the Mac app in Debug with xcodegen and xcodebuild.
#   android     Build the Android app's Kotlin sources with the Gradle wrapper.
#   all         Run rust, then gen, then mac, then android, in that order.
#
# Each step prints "gate: <step> ok" or "gate: <step> FAIL (exit N)".
# Full step output goes to a log file, target/gate.log under the repo root
# by default. Set GATE_LOG to use a different path. On failure, the last
# 40 lines of the failing step's output print to stderr, or the whole
# file when the environment variable CI is set.

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
    if [ -n "${CI:-}" ]; then
      # CI keeps the whole step in its own log, so the full output is
      # cheap to show and worth more than the last few lines.
      cat "$step_file" >&2
    else
      tail -n 40 "$step_file" >&2
    fi
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

# ---- mac mode helper: no Screens or Components view may write one of
# EngineModel's six properties that used to be private(set). Swift cannot
# make a setter private to only the other files of one module, so this
# grep is the gate instead of the language. The six names live only here;
# EngineModel.swift's own comment on the properties points back to this
# check. `docs/audits/principles-fixes.md`, finding 9. ----
mac_no_view_write_names=(devices pairing presence roots downloadPath trustedNetworks)

mac_no_view_writes() {
  local name hit
  for name in "${mac_no_view_write_names[@]}"; do
    hit="$(grep -rnE "model\.${name}[[:space:]]*=[^=]" \
      "$repo_root/macos/Ferry/Screens" "$repo_root/macos/Ferry/Components" 2>/dev/null)"
    if [ -n "$hit" ]; then
      echo "a view writes model.${name} directly; only EngineModel and its own extensions may set it" >&2
      echo "$hit" >&2
      return 1
    fi
  done
  return 0
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

# ---- android mode helper: source env.sh, then the Gradle wrapper, in a
# subshell so the env and the cd never leak into a later step of an "all"
# run. The wrapper needs no separate Gradle install. The task
# ":app:compileDebugKotlin" exists under this repo's Gradle setup (AGP 9's
# built-in Kotlin support), confirmed with `./gradlew :app:tasks`. The
# compile task never merges the manifest, so a bad manifest element would
# pass it and fail at install. The manifest task runs first. ----
android_build() {
  (
    source "$repo_root/scripts/env.sh" &&
      cd "$repo_root/android" &&
      ./gradlew :app:processDebugMainManifest :app:compileDebugKotlin -q
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
  run_step "mac no view writes" mac_no_view_writes
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
