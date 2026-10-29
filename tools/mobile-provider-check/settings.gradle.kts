pluginManagement {
    repositories { google(); mavenCentral(); gradlePluginPortal() }
    plugins {
        id("com.android.library") version "8.10.1"
        id("org.jetbrains.kotlin.android") version "2.1.10"
    }
}
dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories { google(); mavenCentral() }
}
rootProject.name = "mobile-provider-check"
include(":tauri-android", ":mobile-provider")
project(":tauri-android").projectDir = file(System.getenv("TAURI_ANDROID_DIR"))
project(":mobile-provider").projectDir = file("../../crates/tauri-plugin-mobile-provider/android")
