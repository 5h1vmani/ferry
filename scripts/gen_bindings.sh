#!/usr/bin/env bash
#
# Build the engine and write the Swift and Kotlin bindings.
#
#     scripts/gen_bindings.sh              # writes into the repository
#     scripts/gen_bindings.sh <out-root>   # writes under <out-root> instead
#     FERRY_BINDINGS_ROOT=<dir> scripts/gen_bindings.sh
#
# The output root holds two paths, and the script always writes to both:
#
#     <out-root>/macos/Ferry/Generated          the Swift bindings
#     <out-root>/android/app/src/main/kotlin    the Kotlin bindings
#
# The default output root is the repository itself, so a plain run puts the
# files where the two apps read them. Passing another root is how a check run
# proves the generator works without touching the apps.
#
# `ktlint` formats the Kotlin file when it is installed. It is not required.
# The generator prints a warning and leaves the file unformatted without it.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

# Every cargo command in this project uses this target directory.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$repo_root/target/runtime}"

out_root="${1:-${FERRY_BINDINGS_ROOT:-$repo_root}}"
swift_out="$out_root/macos/Ferry/Generated"
kotlin_out="$out_root/android/app/src/main/kotlin"

# macOS builds a .dylib. Linux builds a .so. Nothing else is supported.
case "$(uname -s)" in
  Darwin) library="$CARGO_TARGET_DIR/debug/libferry_runtime.dylib" ;;
  Linux)  library="$CARGO_TARGET_DIR/debug/libferry_runtime.so" ;;
  *)
    echo "gen_bindings: this script runs on macOS or Linux only." >&2
    exit 1
    ;;
esac

echo "gen_bindings: building ferry-runtime"
cargo build -p ferry-runtime

if [ ! -f "$library" ]; then
  echo "gen_bindings: no library at $library" >&2
  exit 1
fi

mkdir -p "$swift_out" "$kotlin_out"

echo "gen_bindings: writing Swift bindings to $swift_out"
cargo run -p ferry-runtime --bin uniffi-bindgen -- \
  generate --library "$library" --language swift --out-dir "$swift_out"

echo "gen_bindings: writing Kotlin bindings to $kotlin_out"
cargo run -p ferry-runtime --bin uniffi-bindgen -- \
  generate --library "$library" --language kotlin --out-dir "$kotlin_out"

echo "gen_bindings: done"
