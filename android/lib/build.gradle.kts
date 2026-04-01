import groovy.json.JsonSlurper

plugins {
    alias(libs.plugins.android.library)
}

val repoRoot = rootProject.projectDir.parentFile
val generatedKotlinDir = layout.buildDirectory.dir("generated/uniffi/kotlin")
val generatedJniDir = layout.buildDirectory.dir("generated/jniLibs")
val releaseAarOutputDir = layout.buildDirectory.dir("outputs/aar")
val releaseBuild = providers.gradleProperty("kithara.release").map { it == "true" }.orElse(false)

fun cargoExecutable(): String {
    val configured = providers.environmentVariable("CARGO").orNull
    if (!configured.isNullOrBlank()) {
        return configured
    }

    val homeCargo = File(System.getProperty("user.home"), ".cargo/bin/cargo")
    if (homeCargo.isFile) {
        return homeCargo.absolutePath
    }

    return "cargo"
}

fun findRustlsPlatformVerifierAar(): File {
    val metadata = providers.exec {
        workingDir = repoRoot
        commandLine(
            cargoExecutable(),
            "metadata",
            "--format-version",
            "1",
            "--filter-platform",
            "aarch64-linux-android",
            "--manifest-path",
            repoRoot.resolve("crates/kithara-ffi/Cargo.toml").absolutePath,
        )
    }.standardOutput.asText.get()

    val packages = JsonSlurper().parseText(metadata) as Map<*, *>
    val manifestPath = (packages["packages"] as List<*>)
        .asSequence()
        .map { it as Map<*, *> }
        .first { pkg -> pkg["name"] == "rustls-platform-verifier-android" }["manifest_path"] as String

    return File(
        File(manifestPath).parentFile,
        "maven/rustls/rustls-platform-verifier/0.1.1/rustls-platform-verifier-0.1.1.aar",
    )
}

val rustlsPlatformVerifierAar = findRustlsPlatformVerifierAar()

val generateKitharaFfi by tasks.registering(Exec::class) {
    group = "build"
    description = "Build Rust Android libraries and generate Kotlin UniFFI bindings."
    workingDir = repoRoot
    inputs.dir(repoRoot.resolve("crates"))
    inputs.file(repoRoot.resolve("Cargo.toml"))
    inputs.file(repoRoot.resolve("Cargo.lock"))
    commandLine("cargo", "xtask", "android")
    if (releaseBuild.get()) {
        args("--profile", "release")
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

val exportReleaseAars by tasks.registering(Copy::class) {
    group = "distribution"
    description = "Copy release AARs with stable file names."
    dependsOn(tasks.named("assembleRelease"))

    from(layout.buildDirectory.file("outputs/aar/lib-release.aar")) {
        rename { "kithara.aar" }
    }
    from(rustlsPlatformVerifierAar) {
        rename { "rust-tls.aar" }
    }
    into(releaseAarOutputDir)
}

tasks.named("preBuild") {
    dependsOn(generateKitharaFfi)
}
