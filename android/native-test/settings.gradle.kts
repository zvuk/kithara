pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}
dependencyResolutionManagement {
    versionCatalogs {
        create("libs") { from(files("../gradle/libs.versions.toml")) }
    }
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}
rootProject.name = "kithara-native-test"

// The transport the test process installs comes from the library build.
includeBuild("..") {
    dependencySubstitution {
        substitute(module("com.kithara:okhttp")).using(project(":okhttp"))
    }
}
