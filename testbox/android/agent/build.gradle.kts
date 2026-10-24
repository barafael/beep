// AGP 9 compiles Kotlin itself (its built-in Kotlin), as Signal-Android's app module does.
// Dependencies are checked against gradle/verification-metadata.xml (Gradle's dependency
// verification), written with `--write-verification-metadata sha256` and kept with the sources.
plugins {
  alias(libs.plugins.android.application)
  alias(libs.plugins.kotlinx.serialization)
}

android {
  namespace = "org.b33p.testbox.agent"
  compileSdk {
    version = release(libs.versions.compileSdk.get().toInt())
  }
  defaultConfig {
    applicationId = "org.b33p.testbox.agent"
    // The emulator's system image (testbox/android/Containerfile).
    minSdk = 35
    targetSdk = libs.versions.targetSdk.get().toInt()
  }
  compileOptions {
    sourceCompatibility = JavaVersion.toVersion(libs.versions.javaVersion.get())
    targetCompatibility = JavaVersion.toVersion(libs.versions.javaVersion.get())
  }
}

dependencies {
  implementation(libs.kotlinx.serialization.json)
}
