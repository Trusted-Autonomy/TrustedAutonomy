import org.jetbrains.intellij.platform.gradle.IntelliJPlatformType

plugins {
    id("java")
    kotlin("jvm") version "1.9.22"
    id("org.jetbrains.intellij.platform") version "2.3.0"
}

group = "com.trusted-autonomy"
version = providers.gradleProperty("pluginVersion").get()

kotlin {
    jvmToolchain(17)
}

repositories {
    mavenCentral()
    intellijPlatform {
        defaultRepositories()
    }
}

dependencies {
    // Gson is bundled with IntelliJ Platform — declare compileOnly so it's not re-bundled in the plugin JAR
    compileOnly("com.google.code.gson:gson:2.10.1")

    intellijPlatform {
        create("IC", providers.gradleProperty("platformVersion").get())
        pluginVerifier()
        zipSigner()
    }
}

intellijPlatform {
    pluginConfiguration {
        id = providers.gradleProperty("pluginId").get()
        name = providers.gradleProperty("pluginName").get()
        version = providers.gradleProperty("pluginVersion").get()

        ideaVersion {
            sinceBuild = providers.gradleProperty("pluginSinceBuild").get()
            untilBuild = provider { null }
        }
    }

    signing {
        certificateChain = providers.environmentVariable("JETBRAINS_CERTIFICATE_CHAIN")
        privateKey = providers.environmentVariable("JETBRAINS_PRIVATE_KEY")
        password = providers.environmentVariable("JETBRAINS_PRIVATE_KEY_PASSWORD")
    }

    publishing {
        token = providers.environmentVariable("JETBRAINS_MARKETPLACE_TOKEN")
    }

    pluginVerification {
        ides {
            ide(IntelliJPlatformType.IntellijIdeaCommunity, providers.gradleProperty("platformVersion").get())
        }
    }
}
