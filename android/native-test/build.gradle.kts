plugins {
    alias(libs.plugins.android.application)
}
android {
    namespace = "com.kithara.nativetest"
    ndkPath = providers.gradleProperty("kithara.nativeTestNdk").get()
    compileSdk = libs.versions.compileSdk.get().toInt()
    defaultConfig {
        applicationId = "com.kithara.nativetest"
        minSdk = libs.versions.minSdk.get().toInt()
        targetSdk = libs.versions.targetSdk.get().toInt()
    }
    buildTypes {
        debug {
            isDebuggable = true
        }
    }
    sourceSets.named("main") {
        jniLibs.directories.add(providers.gradleProperty("kithara.nativeTestLibraries").get())
    }
    packaging {
        // Size only: the test process loads its test image alone, never the FFI library or JNA.
        jniLibs.excludes += listOf("**/libkithara_ffi.so", "**/libjnidispatch.so")
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}
layout.buildDirectory.set(file(providers.gradleProperty("kithara.nativeTestBuild").get()))

dependencies {
    implementation("com.kithara:okhttp")
}
