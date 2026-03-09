import groovy.json.JsonSlurper

pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}
plugins {
    id("org.gradle.toolchains.foojay-resolver-convention") version "1.0.0"
}

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

fun findRustlsPlatformVerifierMavenRepo(): File {
    val metadata = providers.exec {
        workingDir = rootDir.parentFile
        commandLine(
            cargoExecutable(),
            "metadata",
            "--format-version",
            "1",
            "--filter-platform",
            "aarch64-linux-android",
            "--manifest-path",
            rootDir.parentFile.resolve("crates/kithara-ffi/Cargo.toml").absolutePath,
        )
    }.standardOutput.asText.get()

    val packages = JsonSlurper().parseText(metadata) as Map<*, *>
    val manifestPath = (packages["packages"] as List<*>)
        .asSequence()
        .map { it as Map<*, *> }
        .first { pkg -> pkg["name"] == "rustls-platform-verifier-android" }["manifest_path"] as String

    return File(File(manifestPath).parentFile, "maven")
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
        maven {
            url = uri(findRustlsPlatformVerifierMavenRepo())
            metadataSources {
                mavenPom()
                artifact()
            }
        }
    }
}

rootProject.name = "kithara-android"

include(":lib")
include(":example")
