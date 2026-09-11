plugins {
    alias(libs.plugins.android.application)
    // No org.jetbrains.kotlin.android: AGP 9's built-in Kotlin compiles the
    // Kotlin sources. Only the Compose compiler plugin is still needed.
    alias(libs.plugins.kotlin.compose)
}

android {
    namespace = "app.ferry"
    compileSdk = 36

    defaultConfig {
        applicationId = "app.ferry"
        minSdk = 31
        targetSdk = 36
        versionCode = 1
        versionName = "1.0"

        // arm64 only. The Rust runtime is built for arm64-v8a and nothing
        // else, so the app cannot run on another architecture whatever
        // else is packaged. JNA's aar carries a helper library for six
        // architectures; this drops the five that are dead weight.
        ndk {
            abiFilters += "arm64-v8a"
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            // Debug signing only. Ferry has no release keystore in phase 1.
            signingConfig = signingConfigs.getByName("debug")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
        // No kotlinOptions.jvmTarget: with AGP's built-in Kotlin, that value
        // defaults to targetCompatibility above.
    }

    buildFeatures {
        compose = true
    }
}

// Builds the Rust runtime for the phone before the app compiles.
//
// `cargo-ndk` writes libferry_runtime.so straight into src/main/jniLibs,
// which the Android plugin packages into the APK. It reads ANDROID_NDK_HOME
// from the environment, which scripts/env.sh sets, so this task passes the
// whole environment through instead of writing a path down.
//
// arm64-v8a only. The phone is a Pixel 3 XL, which is arm64. Building a
// second architecture would double the time and ship bytes nobody runs.
//
// CARGO_TARGET_DIR is set to target/android so this cross build does not
// fight the host build in target/ over the same lock.
val repoRoot = rootProject.layout.projectDirectory.dir("..")
val jniLibsDir = layout.projectDirectory.dir("src/main/jniLibs")

val buildRustForPhone = tasks.register<Exec>("buildRustForPhone") {
    group = "build"
    description = "Builds ferry-runtime for arm64-v8a into src/main/jniLibs."
    workingDir = repoRoot.asFile
    environment("CARGO_TARGET_DIR", repoRoot.dir("target/android").asFile.absolutePath)
    commandLine(
        "cargo", "ndk",
        "-t", "arm64-v8a",
        "-o", jniLibsDir.asFile.absolutePath,
        "build", "--release",
        "-p", "ferry-runtime",
    )
    // Gradle skips the task when no Rust source changed and the library is
    // already in place.
    inputs.dir(repoRoot.dir("crates"))
    inputs.file(repoRoot.file("Cargo.toml"))
    inputs.file(repoRoot.file("Cargo.lock"))
    outputs.dir(jniLibsDir)
}

tasks.named("preBuild") {
    dependsOn(buildRustForPhone)
}

dependencies {
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.activity.compose)
    implementation(platform(libs.androidx.compose.bom))
    implementation(libs.androidx.ui)
    implementation(libs.androidx.ui.graphics)
    implementation(libs.androidx.ui.tooling.preview)
    implementation(libs.androidx.material3)
    implementation(libs.androidx.material.icons.extended)
    // The camera, for pairing by scan. One screen reads it, and CameraX is
    // the platform's own answer, so nothing here wraps it.
    implementation(libs.androidx.camera.core)
    implementation(libs.androidx.camera.camera2)
    implementation(libs.androidx.camera.lifecycle)
    implementation(libs.androidx.camera.view)
    // Reads the QR code out of a camera frame. The bundled model, so the
    // first pairing of a new phone does not depend on the network — the one
    // moment Ferry may have none. See the note in libs.versions.toml.
    implementation(libs.mlkit.barcode.scanning)
    // The generated Kotlin bindings in uniffi/ferry_runtime load
    // libferry_runtime.so through JNA. The Android build of JNA ships as an
    // aar, so it is asked for by that classifier.
    implementation(variantOf(libs.jna) { artifactType("aar") })
    // Dispatchers.IO, for the engine calls that block on disk or network.
    implementation(libs.kotlinx.coroutines.android)
    debugImplementation(libs.androidx.ui.tooling)
}
