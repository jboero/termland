@file:Suppress("unused", "UNUSED_PARAMETER")

package dev.termland.core

/*
 * ===========================================================================
 *  COMPILE-TIME STUB — not part of the shipped app.
 * ===========================================================================
 *
 * This file is a hand-written transcription of the FROZEN termland-mobile-core
 * UniFFI contract. It exists for exactly one reason: the Rust core and this
 * Android app are built concurrently, and Gradle needs *something* named
 * `dev.termland.core` on the classpath to type-check the Kotlin/Compose layer.
 *
 * app/build.gradle.kts adds this source directory to the main source set ONLY
 * when `crates/termland-mobile-core/Cargo.toml` is absent. As soon as the crate
 * lands, `uniffiBindgen` emits the real bindings into
 * build/generated/uniffi/kotlin and this directory drops out of the build. The
 * declarations below are signature-compatible with what uniffi-bindgen emits
 * (camelCase methods, UInt/ULong/UShort/UByte scalars, ByteArray for Vec<u8>,
 * nullable types for Option<T>), so the swap is a no-op for callers.
 *
 * Every entry point that would touch the network throws, so a stub build fails
 * loudly at connect time instead of pretending to work.
 */

private const val NO_CORE =
    "termland-mobile-core is not built into this APK. Build crates/termland-mobile-core " +
        "and re-run ./gradlew assembleDebug to link the real UniFFI bindings."

enum class MobileCodec { AV1, VP9, VP8, H265, H264 }

data class ServerProfile(
    var host: String,
    var port: UShort,
    var useTls: Boolean,
    var acceptInvalidCerts: Boolean,
    var username: String?,
    var password: String?,
)

data class SessionSummary(
    var sessionId: String,
    var mode: String,
    var width: UInt,
    var height: UInt,
    var ageSecs: ULong,
    var attached: Boolean,
)

data class SessionParams(
    var width: UInt,
    var height: UInt,
    var quality: UByte,
    var audio: Boolean,
    var desktopShell: String?,
    var appCommand: String?,
    var supportedCodecs: List<MobileCodec>,
)

data class SessionReadyInfo(
    var width: UInt,
    var height: UInt,
    var codec: MobileCodec,
    var sessionId: String,
)

data class VideoPacket(
    var data: ByteArray,
    var timestampUs: ULong,
    var keyframe: Boolean,
    var codec: MobileCodec,
    var width: UInt,
    var height: UInt,
)

interface SessionObserver {
    fun onSessionReady(info: SessionReadyInfo)
    fun onVideoPacket(packet: VideoPacket)
    fun onAudioPacket(data: ByteArray)
    fun onClipboard(mimeType: String, data: ByteArray)
    fun onDataRate(bytesPerSec: ULong)
    fun onDisconnected(reason: String)
    fun onError(message: String)
}

sealed class TermlandException(message: String) : Exception(message) {
    class Connect(val msg: String) : TermlandException(msg)
    class Tls(val msg: String) : TermlandException(msg)
    class Auth(val msg: String) : TermlandException(msg)
    class Protocol(val msg: String) : TermlandException(msg)
    class Io(val msg: String) : TermlandException(msg)
}

class TermlandClient {
    fun listSessions(profile: ServerProfile): List<SessionSummary> =
        throw TermlandException.Connect(NO_CORE)

    fun closeSession(profile: ServerProfile, sessionId: String): Unit =
        throw TermlandException.Connect(NO_CORE)

    fun connectNew(profile: ServerProfile, params: SessionParams, observer: SessionObserver): Unit =
        throw TermlandException.Connect(NO_CORE)

    fun attach(
        profile: ServerProfile,
        sessionId: String,
        params: SessionParams,
        observer: SessionObserver,
    ): Unit = throw TermlandException.Connect(NO_CORE)

    fun disconnect() {}
    fun isConnected(): Boolean = false

    fun sendKey(scancode: UInt, keysym: UInt, pressed: Boolean, modifiers: UInt) {}
    fun sendText(text: String) {}
    fun sendPointerMotion(x: Double, y: Double, absolute: Boolean) {}
    fun sendPointerButton(button: UInt, pressed: Boolean) {}
    fun sendScroll(dx: Double, dy: Double) {}
    fun sendClipboard(mimeType: String, data: ByteArray) {}
    fun resize(width: UInt, height: UInt) {}
    fun setCursorInFrame(inFrame: Boolean) {}
}
