plugins {
    alias(libs.plugins.android.library)
}

android {
    namespace = "com.kithara.okhttp"
    compileSdk = libs.versions.compileSdk.get().toInt()

    defaultConfig {
        minSdk = libs.versions.minSdk.get().toInt()
    }

    buildFeatures {
        buildConfig = false
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

dependencies {
    api(project(":lib"))
    // Only API that OkHttp 4.12 and 5.x share; a host resolves its own version.
    api(libs.okhttp)

    testImplementation(libs.junit4)
}
