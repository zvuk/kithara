plugins {
    alias(libs.plugins.android.application) apply false
    alias(libs.plugins.android.library) apply false
    alias(libs.plugins.kotlin.compose) apply false
    alias(libs.plugins.dokka) apply false
}

val pinsFile = rootProject.projectDir.parentFile.resolve(".config/ci-pins.toml")
extra["kitharaNdkVersion"] = pinsFile.readLines()
    .firstOrNull { it.startsWith("android_ndk_version") }
    ?.substringAfter('"')
    ?.substringBefore('"')
    ?: error("android_ndk_version is missing from $pinsFile")
