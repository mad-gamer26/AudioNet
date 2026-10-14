// AudioNet for Android: a native app (Kotlin, Jetpack Compose) on the same
// Rust engine as the other apps. Build the engine first:
//   powershell -ExecutionPolicy Bypass -File build-core.ps1
// then: gradlew assembleDebug
pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}
dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
        // The Android part of rustls-platform-verifier (the system's
        // certificate checks for the engine's TLS connections).
        maven { url = uri("https://github.com/rustls/rustls-platform-verifier/raw/maven-archive/android-release-support/maven/") }
    }
}
rootProject.name = "AudioNet"
include(":app")
