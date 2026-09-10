// Root build file. It declares the plugins every module may use, but
// applies none of them here (`apply false`), so the version is resolved
// once and each module opts in for itself.
//
// No org.jetbrains.kotlin.android plugin: AGP 9 compiles Kotlin itself
// (https://developer.android.com/build/releases/agp-9-0-0-release-notes
// #android-gradle-plugin-built-in-kotlin), and applying it fails the build.
// The Compose compiler plugin is a separate concern and is still needed.
plugins {
    alias(libs.plugins.android.application) apply false
    alias(libs.plugins.kotlin.compose) apply false
}
