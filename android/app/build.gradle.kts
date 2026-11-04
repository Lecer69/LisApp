plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
}

val rustLibs = layout.buildDirectory.dir("generated/rust/jniLibs")
val rustKotlin = layout.buildDirectory.dir("generated/rust/kotlin")

android {
    namespace = "com.lecer.lisapp"
    compileSdk = 37

    defaultConfig {
        applicationId = "com.lecer.lisapp"
        minSdk = 33
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"
        ndk {
            abiFilters += "arm64-v8a"
        }
    }

    buildFeatures {
        compose = true
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    sourceSets {
        getByName("main") {
            jniLibs.directories.add(rustLibs.get().asFile.path)
            kotlin.directories.add(rustKotlin.get().asFile.path)
        }
    }
}

val buildRust = tasks.register<Exec>("buildRust") {
    workingDir = rootDir
    commandLine("./build-rust.sh", rustLibs.get().asFile.path, rustKotlin.get().asFile.path)
}

tasks.named("preBuild") {
    dependsOn(buildRust)
}

dependencies {
    implementation(platform("androidx.compose:compose-bom:2026.09.00"))
    implementation("androidx.compose.foundation:foundation")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.ui:ui")
    implementation("androidx.activity:activity-compose:1.13.0")
    implementation("androidx.core:core-ktx:1.19.1")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.11.0")
    implementation("net.java.dev.jna:jna:5.19.1@aar")
}
