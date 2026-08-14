import org.jetbrains.kotlin.gradle.tasks.KotlinCompile

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.android)
    alias(libs.plugins.kotlin.compose)
    alias(libs.plugins.kotlin.serialization)
}

// ---------------------------------------------------------------------------
// Native core (termland-mobile-core) wiring
//
// The whole protocol lives in Rust. Gradle's job is to (1) cross-compile the
// cdylib for each Android ABI with cargo-ndk and drop the .so where the APK
// packager finds it, and (2) run uniffi-bindgen over one of those .so files to
// produce the `dev.termland.core` Kotlin bindings. Both outputs land under
// build/ so nothing generated is ever committed.
// ---------------------------------------------------------------------------

val rustRoot: File = rootProject.file(providers.gradleProperty("termland.rustRoot").get())
val rustPackage: String = providers.gradleProperty("termland.rustPackage").get()
val rustRelease: Boolean = providers.gradleProperty("termland.rustRelease").get().toBoolean()
val abis: List<String> = providers.gradleProperty("termland.abis").get()
    .split(',').map { it.trim() }.filter { it.isNotEmpty() }

val crateDir: File = File(rustRoot, "crates/$rustPackage")
// The Rust core is developed concurrently with this app. When the crate is not
// present yet we fall back to a hand-written stub of the frozen UniFFI contract
// (src/stub/java) so the Kotlin/Compose side still compiles and links. The stub
// is signature-identical to what uniffi-bindgen emits; swapping is a no-op.
val nativeCoreAvailable: Boolean = File(crateDir, "Cargo.toml").isFile

val generatedBindingsDir: File = layout.buildDirectory.dir("generated/uniffi/kotlin").get().asFile
val generatedJniLibsDir: File = layout.buildDirectory.dir("generated/jniLibs").get().asFile

/**
 * Private CARGO_TARGET_DIR for the Android build.
 *
 * The repo's own target/ is populated by whatever rustc the distro ships, while
 * rustup's nightly is what's first on PATH here; sharing the directory makes
 * cargo trip over "found crate X compiled by an incompatible version of rustc"
 * on every host build-script dependency. A separate target dir also keeps
 * `cargo build` for the desktop client from being invalidated by an APK build.
 */
val cargoTargetDir: File = layout.buildDirectory.dir("cargo-target").get().asFile

/** ABI -> Rust target triple, matching cargo-ndk's own table. */
fun abiToTriple(abi: String): String = when (abi) {
    "arm64-v8a" -> "aarch64-linux-android"
    "armeabi-v7a" -> "armv7-linux-androideabi"
    "x86_64" -> "x86_64-linux-android"
    "x86" -> "i686-linux-android"
    else -> error("unknown ABI $abi")
}

/** cargo/cargo-ndk are installed per-user by rustup, which is not on Gradle's PATH. */
fun cargoEnv(): Map<String, String> {
    val home = System.getProperty("user.home")
    val sdk = System.getenv("ANDROID_HOME")
        ?: System.getenv("ANDROID_SDK_ROOT")
        ?: File(home, "Android/Sdk").takeIf { it.isDirectory }?.absolutePath
    val ndk = System.getenv("ANDROID_NDK_HOME")
        ?: sdk?.let { s ->
            File(s, "ndk").listFiles()
                ?.filter { it.isDirectory }
                ?.maxByOrNull { it.name }   // newest installed NDK
                ?.absolutePath
        }
    return buildMap {
        put("PATH", "$home/.cargo/bin:" + System.getenv("PATH"))
        put("CARGO_TARGET_DIR", cargoTargetDir.absolutePath)
        // Pin explicitly: once the workspace's `rust-version = "1.85"` is
        // satisfied by more than one installed toolchain (both rustup's
        // default nightly and, once anything auto-installs it, a matching
        // stable release), toolchain selection has been observed to differ
        // between separate `cargo`/`cargo ndk` invocations sharing this same
        // CARGO_TARGET_DIR — producing "compiled by an incompatible version
        // of rustc" (E0514) from artifacts left by the other toolchain. There
        // is exactly one toolchain here with all three Android std targets
        // verified installed (nightly); pin to it so builds are
        // reproducible regardless of what else rustup has lying around.
        put("RUSTUP_TOOLCHAIN", "nightly")
        if (sdk != null) put("ANDROID_HOME", sdk)
        if (ndk != null) put("ANDROID_NDK_HOME", ndk)
    }
}

val cargoNdkBuild = tasks.register<Exec>("cargoNdkBuild") {
    group = "termland"
    description = "Cross-compile $rustPackage for ${abis.joinToString()} via cargo-ndk"
    onlyIf { nativeCoreAvailable }

    workingDir = rustRoot
    environment(cargoEnv())

    val args = mutableListOf("cargo", "ndk")
    abis.forEach { args += listOf("-t", it) }
    args += listOf("-o", generatedJniLibsDir.absolutePath)
    // -P is cargo-ndk's API level (NOT cargo's -p/--package); keep it equal to
    // defaultConfig.minSdk or the .so will refuse to load on older devices.
    args += listOf("-P", "26")
    args += listOf("build", "-p", rustPackage)
    if (rustRelease) args += "--release"
    commandLine(args)

    // Coarse but correct: any change under the crate re-runs cargo, and cargo
    // itself is the real incremental engine.
    if (nativeCoreAvailable) {
        inputs.dir(crateDir).withPropertyName("crate")
            .withPathSensitivity(PathSensitivity.RELATIVE)
        outputs.dir(generatedJniLibsDir)
    }

    doFirst { generatedJniLibsDir.mkdirs() }
}

val uniffiBindgen = tasks.register<Exec>("uniffiBindgen") {
    group = "termland"
    description = "Generate dev.termland.core Kotlin bindings from the built .so"
    onlyIf { nativeCoreAvailable }
    dependsOn(cargoNdkBuild)

    workingDir = rustRoot
    environment(cargoEnv())

    // Library mode: bindgen reads the UniFFI metadata straight out of the ELF, so
    // a cross-compiled Android .so is a perfectly good input (it is never dlopened).
    val soName = "lib" + rustPackage.replace('-', '_') + ".so"
    val libArg = File(File(generatedJniLibsDir, abis.first()), soName)

    commandLine(
        "cargo", "run", "--bin", "uniffi-bindgen", "--",
        "generate", "--library", libArg.absolutePath,
        "--language", "kotlin",
        "--out-dir", generatedBindingsDir.absolutePath,
    )

    if (nativeCoreAvailable) {
        inputs.dir(generatedJniLibsDir)
        outputs.dir(generatedBindingsDir)
    }
    doFirst { generatedBindingsDir.mkdirs() }
}

val reportNativeCore = tasks.register("reportNativeCore") {
    group = "termland"
    doLast {
        if (nativeCoreAvailable) {
            logger.lifecycle("termland: building native core from ${crateDir.absolutePath}")
        } else {
            logger.warn(
                "termland: crates/$rustPackage not found — compiling against the " +
                    "hand-written UniFFI contract stub in app/src/stub/java. " +
                    "The APK will build but cannot connect to a server."
            )
        }
    }
}

android {
    namespace = "dev.termland.android"
    compileSdk = 35

    defaultConfig {
        applicationId = "dev.termland.android"
        // 26: the oldest API where MediaCodec async mode, VP9/HEVC decode and
        // requestPointerCapture() are all reliably present.
        minSdk = 26
        targetSdk = 35
        versionCode = 2
        versionName = "0.7.0"
        ndk { abiFilters += abis }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
        }
        debug {
            isMinifyEnabled = false
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }
    buildFeatures {
        compose = true
        buildConfig = true
    }
    packaging {
        // Keep the core unstripped-but-uncompressed; MediaCodec/JNA both want
        // page-aligned loadable segments.
        jniLibs.useLegacyPackaging = false
        resources.excludes += setOf("META-INF/*.kotlin_module")
    }

    sourceSets {
        getByName("main") {
            jniLibs.srcDir(generatedJniLibsDir)
            if (nativeCoreAvailable) {
                java.srcDir(generatedBindingsDir)
            } else {
                java.srcDir("src/stub/java")
            }
        }
    }

    lint {
        // A fresh checkout has no generated bindings; don't fail CI on that.
        abortOnError = false
    }
}

dependencies {
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.activity.compose)
    implementation(libs.androidx.lifecycle.runtime.ktx)
    implementation(libs.androidx.lifecycle.runtime.compose)
    implementation(libs.androidx.lifecycle.viewmodel.compose)
    implementation(libs.androidx.lifecycle.viewmodel.ktx)
    implementation(libs.androidx.datastore.preferences)
    implementation(libs.kotlinx.serialization.json)

    implementation(platform(libs.androidx.compose.bom))
    implementation(libs.androidx.compose.ui)
    implementation(libs.androidx.compose.ui.graphics)
    implementation(libs.androidx.compose.ui.tooling.preview)
    implementation(libs.androidx.compose.material3)
    implementation(libs.androidx.compose.material.icons.extended)
    debugImplementation(libs.androidx.compose.ui.tooling)

    // UniFFI's generated Kotlin talks to the cdylib through JNA. The @aar variant
    // is required on Android — it ships libjnidispatch.so for every ABI.
    implementation("net.java.dev.jna:jna:${libs.versions.jna.get()}@aar")
}

// Make the whole pipeline implicit in `./gradlew assembleDebug`.
tasks.named("preBuild") {
    dependsOn(reportNativeCore)
    if (nativeCoreAvailable) dependsOn(uniffiBindgen)
}
// preBuild ordering is not always enough for Kotlin compilation of generated
// sources, so state the dependency directly too.
tasks.withType<KotlinCompile>().configureEach {
    if (nativeCoreAvailable) dependsOn(uniffiBindgen)
}
