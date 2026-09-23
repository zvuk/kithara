plugins {
    alias(libs.plugins.android.library)
    alias(libs.plugins.dokka)
}

val repoRoot = rootProject.projectDir.parentFile
val generatedKotlinDir = layout.buildDirectory.dir("generated/uniffi/kotlin")
val generatedJniDir = layout.buildDirectory.dir("generated/jniLibs")
val generatedTestAssetsDir = layout.buildDirectory.dir("generated/testAssets")
val releaseAarOutputDir = layout.buildDirectory.dir("outputs/aar")
val apiDocsOutputDir = repoRoot.resolve("docs-build/kithara-android")
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

val generateKitharaFfi by tasks.registering(Exec::class) {
    group = "build"
    description = "Build Rust Android libraries and generate Kotlin UniFFI bindings."
    workingDir = repoRoot
    inputs.dir(repoRoot.resolve("crates"))
    inputs.file(repoRoot.resolve("Cargo.toml"))
    inputs.file(repoRoot.resolve("Cargo.lock"))
    commandLine(cargoExecutable(), "xtask", "android", "build")
    if (releaseBuild.get()) {
        args("--profile", "release")
    }
    outputs.dir(generatedKotlinDir)
    outputs.dir(generatedJniDir)
}

// The instrumented offline capture reads an MPEG file out of its own APK. The
// body is generated rather than committed, and the store it lands in is
// content-addressed under a build fingerprint, so Gradle asks the generator for
// a copy at a path it chooses itself.
val exportTestFixtures by tasks.registering(Exec::class) {
    group = "build"
    description = "Export generated audio fixtures the instrumented tests read."
    workingDir = repoRoot
    inputs.dir(repoRoot.resolve("crates/kithara-test-fixtures/src"))
    inputs.file(repoRoot.resolve("crates/kithara-test-fixtures/build.rs"))
    outputs.dir(generatedTestAssetsDir)
    commandLine(
        cargoExecutable(),
        "run",
        "--quiet",
        "--package",
        "kithara-test-fixtures",
        "--features",
        "all",
        "--bin",
        "kithara-fixture-export",
        "--",
        "signal_mp3_track_sine440_187s",
        generatedTestAssetsDir.get().asFile.resolve("test.mp3").absolutePath,
    )
}

android {
    testOptions {
        providers.gradleProperty("kithara.testResultsDir").orNull?.let { resultsDir = it }
        providers.gradleProperty("kithara.testReportDir").orNull?.let { reportDir = it }
    }

    namespace = "com.kithara"
    compileSdk = libs.versions.compileSdk.get().toInt()
    ndkVersion = rootProject.extra["kitharaNdkVersion"] as String

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

    sourceSets.named("androidTest") {
        assets.directories.add(generatedTestAssetsDir.get().asFile.path)
    }
}

dependencies {
    implementation(libs.androidx.annotation)
    implementation("net.java.dev.jna:jna:${libs.versions.jna.get()}@aar")
    implementation(libs.kotlinx.coroutines.core)

    testImplementation(libs.junit4)
    androidTestImplementation(project(":okhttp"))
    androidTestImplementation(libs.junit4)
    androidTestImplementation(libs.androidx.test.core)
    androidTestImplementation(libs.androidx.test.ext.junit)
    androidTestImplementation(libs.androidx.test.runner)
}

val exportReleaseAars by tasks.registering(Copy::class) {
    group = "distribution"
    description = "Copy the release AARs under stable file names."
    dependsOn(tasks.named("assembleRelease"), ":okhttp:assembleRelease")

    from(layout.buildDirectory.file("outputs/aar/lib-release.aar")) {
        rename { "kithara.aar" }
    }
    from(project(":okhttp").layout.buildDirectory.file("outputs/aar/okhttp-release.aar")) {
        rename { "kithara-okhttp.aar" }
    }
    into(releaseAarOutputDir)
}

tasks.named("preBuild") {
    dependsOn(generateKitharaFfi)
}

// The variant tasks AGP creates for the instrumentation APK do not exist yet
// while this script is evaluated, so the export is attached to the assets merge
// by name as it appears.
tasks.matching { it.name.endsWith("AndroidTestAssets") }.configureEach {
    dependsOn(exportTestFixtures)
}

dokka {
    moduleName.set("kithara-android")

    dokkaSourceSets.configureEach {
        // The UniFFI bindings are generated from the Rust FFI surface and are
        // not part of the documented facade.
        perPackageOption {
            matchingRegex.set("com\\.kithara\\.ffi.*")
            suppress.set(true)
        }
    }

    dokkaPublications.html {
        outputDirectory.set(apiDocsOutputDir)
    }
}

tasks.named("dokkaGenerate") {
    dependsOn(generateKitharaFfi)
}
