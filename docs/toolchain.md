# The toolchain

What is installed on the development Mac, how it got there, and how to get it
again on a fresh machine. Nothing here needs `sudo` or a password.

## Rust

The pinned toolchain is in `rust-toolchain.toml`. `rustup` reads it. Two extra
targets are needed for Android:

```bash
rustup target add aarch64-linux-android x86_64-linux-android
```

`cargo-ndk` finds the NDK and sets the linker for those targets, so no NDK
path is ever written into the repository:

```bash
cargo install cargo-ndk
```

## macOS app

Xcode, from the App Store. The project file is generated, not hand edited:

```bash
brew install xcodegen
```

`macos/project.yml` is the single source of truth for the Xcode project. Run
`xcodegen` in `macos/` after changing it. The generated `.xcodeproj` is not
committed.

## Android app

Java, from Homebrew's formula rather than a cask, because the cask writes to
`/Library/Java` and needs a password:

```bash
brew install openjdk
```

The Android command-line tools, then the SDK pieces. The `sdkmanager` tool
prints a deprecation notice about a newer `android` command. It still works.

```bash
brew install --cask android-commandlinetools
source scripts/env.sh
yes | sdkmanager --licenses
sdkmanager "platform-tools" "platforms;android-36" "build-tools;36.1.0" "ndk;30.0.16248370"
```

Those are the versions installed on 10 September 2026. Newer ones are fine.
Check with `sdkmanager --list` and pick the newest that is not marked `rc`.

The whole Android SDK is about 3.3 GB.

## Environment

Every shell that builds the Android side needs the variables from
`scripts/env.sh`. Source it; do not run it.

```bash
source scripts/env.sh
```

It derives every path from `brew --prefix`, so it holds no machine-specific
value.

## Checking it works

```bash
source scripts/env.sh
java -version
adb version
cargo ndk --version
xcodegen --version
```

All four should print a version.
