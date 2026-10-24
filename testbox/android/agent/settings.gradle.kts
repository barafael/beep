// The on-device agent of testbox/tools `android`, built with Signal-Android's Gradle wrapper and
// its version catalog, so that the two builds share plugins and a cache
// (testbox/android/Containerfile, stage `agent`).
pluginManagement {
  repositories {
    google()
    mavenCentral()
    gradlePluginPortal()
  }
}
dependencyResolutionManagement {
  // Repositories here only, none per project.
  repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
  repositories {
    google()
    mavenCentral()
  }
  versionCatalogs {
    create("libs") {
      from(files("/opt/signal-android/gradle/libs.versions.toml"))
    }
  }
}
rootProject.name = "testbox-agent"
