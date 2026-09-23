plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

// Версия приложения — та же, что у программы на ПК (workspace.package в Cargo.toml).
val appVersion: String = Regex("""\[workspace\.package\]\s*version\s*=\s*"([^"]+)"""")
    .find(rootProject.file("../Cargo.toml").readText())?.groupValues?.get(1) ?: "0.0.0"
val appVersionCode: Int = appVersion.split(".").map { it.toIntOrNull() ?: 0 }
    .let { (major, minor, patch) -> major * 10000 + minor * 100 + patch }

android {
    namespace = "dev.escanor"
    compileSdk = 35
    ndkVersion = "28.2.13676358"

    defaultConfig {
        applicationId = "dev.escanor"
        minSdk = 30
        targetSdk = 34
        versionCode = appVersionCode
        versionName = appVersion
        ndk {
            abiFilters += listOf("arm64-v8a", "armeabi-v7a")
        }
    }

    externalNativeBuild {
        cmake {
            path = file("src/main/cpp/CMakeLists.txt")
            version = "3.22.1"
        }
    }

    // Один ключ для всех сборок — на этом Mac, у других разработчиков и в CI. Иначе Android
    // отказался бы ставить обновление поверх приложения, подписанного другим ключом.
    // Приложение ставится только через adb с ПК пользователя, поэтому ключ хранится в репозитории.
    signingConfigs {
        create("escanor") {
            storeFile = rootProject.file("escanor.keystore")
            storePassword = "escanor"
            keyAlias = "escanor"
            keyPassword = "escanor"
        }
    }

    buildTypes {
        debug {
            signingConfig = signingConfigs.getByName("escanor")
        }
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.getByName("escanor")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

kotlin {
    compilerOptions {
        jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17)
    }
}
