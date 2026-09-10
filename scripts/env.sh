# Source this file. Do not run it.
#
#     source scripts/env.sh
#
# It points the Android and Java tools at the copies Homebrew installed. No
# path is written down. Everything is derived from `brew --prefix`, so the
# same file works on any Mac that used docs/toolchain.md.

_brew="$(brew --prefix 2>/dev/null)"
if [ -z "$_brew" ]; then
  echo "ferry: Homebrew is not installed. See docs/toolchain.md." >&2
  return 1 2>/dev/null || exit 1
fi

export JAVA_HOME="$_brew/opt/openjdk"
export ANDROID_HOME="$_brew/share/android-commandlinetools"
export ANDROID_SDK_ROOT="$ANDROID_HOME"

# The newest installed NDK. cargo-ndk reads ANDROID_NDK_HOME.
_ndk="$(ls -d "$ANDROID_HOME"/ndk/* 2>/dev/null | sort -V | tail -1)"
if [ -n "$_ndk" ]; then
  export ANDROID_NDK_HOME="$_ndk"
fi

export PATH="$JAVA_HOME/bin:$ANDROID_HOME/platform-tools:$ANDROID_HOME/cmdline-tools/latest/bin:$PATH"

unset _brew _ndk
