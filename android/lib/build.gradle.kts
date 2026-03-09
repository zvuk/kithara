plugins {
    alias(libs.plugins.android.library)
}

val repoRoot = rootProject.projectDir.parentFile
val generatedKotlinDir = layout.buildDirectory.dir("generated/uniffi/kotlin")
val generatedJniDir = layout.buildDirectory.dir("generated/jniLibs")
val releaseBuild = providers.gradleProperty("kithara.release").map { it == "true" }.orElse(false)

val generateKitharaFfi by tasks.registering(Exec::class) {
    group = "build"
    description = "Build Rust Android libraries and generate Kotlin UniFFI bindings."
    workingDir = repoRoot
    inputs.dir(repoRoot.resolve("crates"))
    inputs.file(repoRoot.resolve("Cargo.toml"))
    inputs.file(repoRoot.resolve("Cargo.lock"))
    inputs.file(repoRoot.resolve("scripts/build-android-bindings.sh"))
    commandLine(
        "bash",
        repoRoot.resolve("scripts/build-android-bindings.sh").absolutePath,
    )
    if (releaseBuild.get()) {
        args("--release")
    }
    outputs.dir(generatedKotlinDir)
    outputs.dir(generatedJniDir)
}

android {
    namespace = "com.kithara"
    compileSdk = libs.versions.compileSdk.get().toInt()

    defaultConfig {
        minSdk = libs.versions.minSdk.get().toInt()
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        consumerProguardFiles("consumer-rules.pro")
    }

    buildFeatures {
        buildConfig = false
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    sourceSets.named("main") {
        kotlin.directories.addAll(listOf("src/main/kotlin", generatedKotlinDir.get().asFile.path))
        jniLibs.directories.add(generatedJniDir.get().asFile.path)
    }
}

dependencies {
    implementation(libs.androidx.annotation)
    implementation("net.java.dev.jna:jna:${libs.versions.jna.get()}@aar")
    implementation(libs.kotlinx.coroutines.core)
    implementation("rustls:rustls-platform-verifier:0.1.1")

    androidTestImplementation(libs.junit4)
    androidTestImplementation(libs.androidx.test.core)
    androidTestImplementation(libs.androidx.test.ext.junit)
    androidTestImplementation(libs.androidx.test.runner)
}

tasks.named("preBuild") {
    dependsOn(generateKitharaFfi)
}
