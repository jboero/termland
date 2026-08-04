package dev.termland.android.session

import android.app.Application
import android.content.ClipData
import android.content.ClipboardManager
import android.util.Log
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import dev.termland.android.data.HostProfile
import dev.termland.android.data.toServerProfile
import dev.termland.android.data.toSessionParams
import dev.termland.android.input.InputRouter
import dev.termland.android.net.CodecSupport
import dev.termland.android.net.Core
import dev.termland.core.SessionObserver
import dev.termland.core.SessionReadyInfo
import dev.termland.core.VideoPacket
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

/** Where a session is in its lifecycle, as far as the UI is concerned. */
sealed interface SessionState {
    data object Idle : SessionState
    data object Connecting : SessionState
    data class Live(val sessionId: String, val width: Int, val height: Int, val codec: String) : SessionState

    /**
     * The flagship persistence state: we are no longer streaming, but the remote
     * session is still alive and can be re-attached with one tap. Reached by
     * backgrounding the app or by losing the link.
     */
    data class Detached(val sessionId: String?, val reason: String) : SessionState
    data class Failed(val message: String) : SessionState
}

/**
 * Owns the streaming session: the core observer, the decoder, and the input router.
 *
 * A ViewModel rather than activity fields so an accidental recreation (the viewer
 * declares configChanges for rotation, but not every path is covered) does not
 * tear down a live session.
 */
class SessionViewModel(app: Application) : AndroidViewModel(app) {

    val router = InputRouter(Core.client)

    private val _state = MutableStateFlow<SessionState>(SessionState.Idle)
    val state: StateFlow<SessionState> = _state.asStateFlow()

    private val _dataRate = MutableStateFlow(0L)
    val dataRate: StateFlow<Long> = _dataRate.asStateFlow()

    private val _firstFrame = MutableStateFlow(false)
    val firstFrame: StateFlow<Boolean> = _firstFrame.asStateFlow()

    private var decoder: VideoDecoder? = null
    private var audioPlayer: AudioPlayer? = null
    private var profile: HostProfile? = null

    /** Session we last streamed, so a resume knows what to attach to. */
    private var lastSessionId: String? = null

    /** Password typed at connect time when the profile does not persist one. */
    private var passwordOverride: String? = null

    private val appContext = app.applicationContext
    private val clipboard: ClipboardManager? =
        app.getSystemService(ClipboardManager::class.java)

    fun bind(profile: HostProfile, sessionId: String?, password: String?) {
        this.profile = profile
        this.lastSessionId = sessionId
        this.passwordOverride = password
    }

    /** Attach the decoder to a live Surface. Must happen before connecting. */
    fun attachDecoder(view: RemoteSurfaceView) {
        decoder?.release()
        decoder = VideoDecoder(
            surface = view.holder.surface,
            onFatal = { msg -> _state.value = SessionState.Failed(msg) },
            onFirstFrame = { _firstFrame.value = true },
        )
    }

    fun releaseDecoder() {
        decoder?.release()
        decoder = null
        _firstFrame.value = false
    }

    /**
     * Symmetric with [releaseDecoder]/[attachDecoder] except it needs no
     * Surface, so it is (re)built directly in [connect] rather than waiting
     * on a view-lifecycle callback. Only started when the profile actually
     * asked for audio (SessionParams.audio) — no point spinning up a decoder
     * and AudioTrack that will never receive a packet.
     */
    private fun releaseAudioPlayer() {
        audioPlayer?.release()
        audioPlayer = null
    }

    // -----------------------------------------------------------------------
    // Connect / attach / detach
    // -----------------------------------------------------------------------

    fun connect(width: Int, height: Int, resume: Boolean) {
        val p = profile ?: return
        if (_state.value is SessionState.Connecting) return
        _state.value = SessionState.Connecting
        _firstFrame.value = false

        releaseAudioPlayer()
        if (p.audio) {
            audioPlayer = AudioPlayer(onFatal = { msg -> Log.w(TAG, "audio: $msg") })
        }

        viewModelScope.launch(Dispatchers.IO) {
            // connect_new/attach are documented blocking (they return once
            // SessionReady arrives), hence the IO dispatcher.
            try {
                val server = p.toServerProfile(passwordOverride)
                val params = p.toSessionParams(width, height, CodecSupport.supportedCodecs)
                val target = lastSessionId
                if (resume && target != null) {
                    Core.client.attach(server, target, params, observer)
                } else {
                    Core.client.connectNew(server, params, observer)
                }
                sendLocalClipboard()
            } catch (e: Exception) {
                Log.w(TAG, "connect failed", e)
                _state.value = SessionState.Failed(Core.describe(e))
            }
        }
    }

    /**
     * Detach, not close: `disconnect()` stops streaming and leaves the remote
     * session running so it can be resumed. This is what makes backgrounding the
     * app on a phone or losing Wi-Fi a non-event.
     */
    fun detach(reason: String) {
        if (_state.value is SessionState.Detached) return
        router.releaseAll()
        viewModelScope.launch(Dispatchers.IO) {
            runCatching { Core.client.disconnect() }
        }
        releaseDecoder()
        releaseAudioPlayer()
        _state.value = SessionState.Detached(lastSessionId, reason)
    }

    fun resize(width: Int, height: Int) {
        if (_state.value !is SessionState.Live) return
        if (width <= 0 || height <= 0) return
        Core.client.resize(width.toUInt(), height.toUInt())
    }

    // -----------------------------------------------------------------------
    // Clipboard
    // -----------------------------------------------------------------------

    /** Push the device clipboard up, on connect and on regaining focus. */
    fun sendLocalClipboard() {
        val text = clipboard?.primaryClip
            ?.takeIf { it.itemCount > 0 }
            ?.getItemAt(0)?.coerceToText(appContext)
            ?.toString()
            ?.takeIf { it.isNotEmpty() }
            ?: return
        runCatching { Core.client.sendClipboard("text/plain", text.toByteArray()) }
    }

    // -----------------------------------------------------------------------

    private val observer = object : SessionObserver {
        override fun onSessionReady(info: SessionReadyInfo) {
            lastSessionId = info.sessionId
            router.remoteWidth = info.width.toInt()
            router.remoteHeight = info.height.toInt()
            _state.value = SessionState.Live(
                sessionId = info.sessionId,
                width = info.width.toInt(),
                height = info.height.toInt(),
                codec = info.codec.name,
            )
            Log.i(TAG, "session ${info.sessionId} ready ${info.width}x${info.height} ${info.codec}")
        }

        override fun onVideoPacket(packet: VideoPacket) {
            // Called on a Rust worker thread that must not block: submit() only
            // enqueues, the codec thread does the work.
            val w = packet.width.toInt()
            val h = packet.height.toInt()
            if (w > 0 && h > 0 && (w != router.remoteWidth || h != router.remoteHeight)) {
                router.remoteWidth = w
                router.remoteHeight = h
            }
            decoder?.submit(
                data = packet.data,
                ptsUs = packet.timestampUs.toLong(),
                keyframe = packet.keyframe,
                codecId = packet.codec,
                width = w,
                height = h,
            )
        }

        override fun onAudioPacket(data: ByteArray) {
            // Called on a Rust worker thread that must not block: feed() only
            // enqueues, the audio codec thread does the decode + AudioTrack write.
            // audioPlayer is null whenever the profile didn't ask for audio, so
            // this is a no-op drop in that case (the core still only sends these
            // when SessionParams.audio was set, but a stale/racing packet costs
            // nothing to ignore).
            audioPlayer?.feed(data)
        }

        override fun onClipboard(mimeType: String, data: ByteArray) {
            if (!mimeType.startsWith("text/")) return
            val text = runCatching { String(data) }.getOrNull() ?: return
            // ClipboardManager is main-thread-only on some OEM builds.
            viewModelScope.launch {
                runCatching { clipboard?.setPrimaryClip(ClipData.newPlainText("termland", text)) }
            }
        }

        override fun onDataRate(bytesPerSec: ULong) {
            _dataRate.value = bytesPerSec.toLong()
        }

        override fun onDisconnected(reason: String) {
            // The remote session survives; offer a resume rather than an error.
            _state.value = SessionState.Detached(lastSessionId, reason)
            releaseDecoder()
            releaseAudioPlayer()
        }

        override fun onError(message: String) {
            _state.value = SessionState.Failed(message)
        }
    }

    override fun onCleared() {
        releaseDecoder()
        releaseAudioPlayer()
        // viewModelScope is already cancelled here, so use a bare thread. Still a
        // detach, not a close: the session outlives the app.
        Thread { runCatching { Core.client.disconnect() } }.start()
        super.onCleared()
    }

    private companion object {
        const val TAG = "TermlandSession"
    }
}
