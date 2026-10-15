import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
}

// The version of rustls-platform-verifier's Android part must be the one
// the engine was built with: read from the workspace's Cargo.lock.
fun cargoLockVersion(crate: String): String {
    val lines = rootDir.resolve("../../Cargo.lock").readLines()
    val i = lines.indexOfFirst { it.trim() == "name = \"$crate\"" }
    require(i >= 0) { "$crate is not in Cargo.lock" }
    return lines.drop(i + 1).first { it.trim().startsWith("version = ") }.substringAfter('"').substringBefore('"')
}

// Every AudioNet part has the workspace's version (Cargo.toml); the version
// code follows it (1.2.4 is 10204), so each release is an update.
val workspaceVersion: String = rootDir.resolve("../../Cargo.toml").readLines()
    .first { it.startsWith("version = ") }.substringAfter('"').substringBefore('"')
val workspaceVersionCode: Int = workspaceVersion.split(".").map { it.toInt() }
    .let { (major, minor, patch) -> major * 10000 + minor * 100 + patch }

// Release signing: a keystore kept outside the repository, named in
// ~/.audionet-release/android-signing.properties (storeFile, storePassword,
// keyAlias, keyPassword). Without it, release builds are unsigned.
val signingFile = File(System.getProperty("user.home"), ".audionet-release/android-signing.properties")
val signing = Properties().apply { if (signingFile.isFile) signingFile.inputStream().use { load(it) } }

android {
    namespace = "com.matthewdovi.audionet"
    compileSdk = 37

    defaultConfig {
        applicationId = "com.matthewdovi.audionet"
        // AAudio (cpal's Android audio) needs Android 8.0.
        minSdk = 26
        targetSdk = 37
        versionCode = workspaceVersionCode
        versionName = workspaceVersion
        buildConfigField("String", "DEFAULT_SERVER", "\"${project.findProperty("audionetDefaultServer") ?: ""}\"")
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        // Only where the engine is built (build-core.ps1): JNA brings
        // libraries for more, which would let a phone without the engine
        // install the app.
        ndk { abiFilters += listOf("arm64-v8a", "x86_64") }
    }

    signingConfigs {
        if (signing.getProperty("storeFile") != null) {
            create("release") {
                storeFile = File(signingFile.parentFile, signing.getProperty("storeFile"))
                storePassword = signing.getProperty("storePassword")
                keyAlias = signing.getProperty("keyAlias")
                keyPassword = signing.getProperty("keyPassword")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.findByName("release")
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    buildFeatures {
        compose = true
        buildConfig = true
    }
    packaging {
        // The engine is stored uncompressed and page-aligned (16 KB pages).
        jniLibs { useLegacyPackaging = false }
    }
}

dependencies {
    val composeBom = platform("androidx.compose:compose-bom:2026.09.00")
    implementation(composeBom)
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-core")
    implementation("androidx.activity:activity-compose:1.13.0")
    implementation("androidx.core:core-ktx:1.19.1")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.11.0")
    // UniFFI's Kotlin bindings call the engine through JNA.
    implementation("net.java.dev.jna:jna:5.19.1@aar")
    implementation("org.rustls:rustls-platform-verifier:${cargoLockVersion("rustls-platform-verifier-android")}")

    androidTestImplementation(composeBom)
    androidTestImplementation("androidx.compose.ui:ui-test-junit4")
    androidTestImplementation("androidx.test.ext:junit:1.3.0")
    androidTestImplementation("androidx.test:runner:1.7.0")
    debugImplementation("androidx.compose.ui:ui-test-manifest")
}
