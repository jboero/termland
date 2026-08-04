package dev.termland.android.session

import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import android.media.MediaCodec
import android.media.MediaFormat
import android.os.Handler
import android.os.HandlerThread
import android.util.Log
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.ArrayDeque
import java.util.concurrent.atomic.AtomicLong

/**
 * Raw Opus packets in, sound out of the speaker. The audio-side twin of
 * [VideoDecoder]: same async-`MediaCodec` shape, same "never bundle a codec"
 * philosophy — Android's own Opus decoder does the decoding, termland never
 * links libopus (or any other software codec) into this app.
 *
 * Two things make Opus-in-MediaCodec fiddlier than the video path:
 *
 *  1. MediaCodec's Opus decoder was built for the Ogg/WebM-Opus container,
 *     where csd-0 is literally the container's identification-header packet.
 *     termland's server encodes raw Opus frames straight off
 *     `opus::Encoder::encode()` (crates/termland-codec/src/audio.rs) with no
 *     container at all, so there is no header to extract — [opusHeadBytes]
 *     synthesizes the mandatory 19 bytes by hand (see its doc for the exact
 *     layout). Malformed CSD is the classic way to make `configure()` throw
 *     or to get silence/garbage out the other end, so get this right.
 *  2. MediaCodec output for audio has no Surface to render into: decoded PCM
 *     comes back as ordinary `ByteBuffer`s that must be copied into an
 *     [AudioTrack]. What encoding those buffers use (16-bit PCM vs. float) is
 *     read off the decoder's actual output format rather than assumed, and
 *     the track is (re)built to match.
 */
class AudioPlayer(
    private val onFatal: (String) -> Unit = {},
) {
    private val thread = HandlerThread("termland-audio-decode").apply { start() }
    private val handler = Handler(thread.looper)

    /** Guards [codec], [track], the packet queue and the track's current format. */
    private val lock = Any()

    private var codec: MediaCodec? = null
    private var track: AudioTrack? = null
    private var trackSampleRate = 0
    private var trackChannelMask = 0
    private var trackEncoding = 0

    /** Input buffer indices the codec has handed us but we had no data for. */
    private val pendingInputs = ArrayDeque<Int>()

    /** Encoded Opus packets waiting for an input buffer. */
    private val queue = ArrayDeque<ByteArray>()

    private var released = false
    private val droppedPackets = AtomicLong(0)

    init {
        configure()
    }

    /**
     * Feed one raw Opus packet (one 20ms frame — FRAME_SIZE=960 @ 48kHz in
     * crates/termland-codec/src/audio.rs). Safe to call from any thread
     * (`SessionObserver.onAudioPacket` runs on a Rust worker thread that must
     * not block); never blocks.
     */
    fun feed(data: ByteArray) {
        synchronized(lock) {
            if (released) return
            if (queue.size >= MAX_QUEUE) {
                // Better to drop the oldest frame than let latency grow
                // unbounded; a lost 20ms Opus frame is inaudible.
                queue.pollFirst()
                droppedPackets.incrementAndGet()
            }
            queue.addLast(data)
            drainLocked()
        }
    }

    fun release() {
        synchronized(lock) {
            if (released) return
            released = true
            queue.clear()
            pendingInputs.clear()
            runCatching { codec?.stop() }
            runCatching { codec?.release() }
            codec = null
            runCatching { track?.stop() }
            runCatching { track?.release() }
            track = null
        }
        thread.quitSafely()
    }

    val droppedPacketCount: Long get() = droppedPackets.get()

    // -----------------------------------------------------------------------

    private fun configure() {
        try {
            val format = MediaFormat.createAudioFormat(MIME, SAMPLE_RATE, CHANNEL_COUNT)
            format.setByteBuffer("csd-0", ByteBuffer.wrap(opusHeadBytes()))
            format.setByteBuffer("csd-1", ByteBuffer.wrap(nanosLE(0L)))
            format.setByteBuffer("csd-2", ByteBuffer.wrap(nanosLE(SEEK_PREROLL_NS)))

            val mc = MediaCodec.createDecoderByType(MIME)
            mc.setCallback(callback, handler)
            mc.configure(format, null, null, 0)
            mc.start()
            codec = mc
            Log.i(TAG, "opus decoder started sr=$SAMPLE_RATE ch=$CHANNEL_COUNT")
        } catch (e: Exception) {
            onFatal("cannot start opus decoder: ${e.message}")
        }
    }

    /** Must hold [lock]. */
    private fun drainLocked() {
        val mc = codec ?: return
        while (queue.isNotEmpty() && pendingInputs.isNotEmpty()) {
            val index = pendingInputs.pollFirst()
            val data = queue.pollFirst() ?: break
            if (!feedInput(mc, index, data)) break
        }
    }

    private fun feedInput(mc: MediaCodec, index: Int, data: ByteArray): Boolean {
        return try {
            val buf: ByteBuffer = mc.getInputBuffer(index) ?: return false
            if (buf.capacity() < data.size) {
                Log.w(TAG, "audio input buffer too small (${buf.capacity()} < ${data.size}), dropping")
                droppedPackets.incrementAndGet()
                mc.queueInputBuffer(index, 0, 0, 0, 0)
                return true
            }
            buf.clear()
            buf.put(data)
            // No meaningful per-packet timestamp reaches us: onAudioPacket
            // only carries the raw bytes (session.rs discards AudioChunk's
            // timestamp_us before the FFI boundary), and this is a live
            // stream with nothing to render against, so 0 is fine.
            mc.queueInputBuffer(index, 0, data.size, 0, 0)
            true
        } catch (e: IllegalStateException) {
            // Codec was reset/released underneath us.
            Log.w(TAG, "audio queueInputBuffer failed: ${e.message}")
            false
        }
    }

    /**
     * (Re)build the [AudioTrack] to match what the decoder is actually
     * producing. Must hold [lock].
     */
    private fun configureTrack(format: MediaFormat) {
        val sampleRate = format.getIntegerOrNull(MediaFormat.KEY_SAMPLE_RATE) ?: SAMPLE_RATE
        val channelCount = format.getIntegerOrNull(MediaFormat.KEY_CHANNEL_COUNT) ?: CHANNEL_COUNT
        // KEY_PCM_ENCODING (API 24+) is what the decoder actually emits.
        // Android's Opus decoder outputs 16-bit PCM in practice, but the key
        // is cheap to read and future/vendor decoders are free to emit
        // float — match reality instead of assuming. ENCODING_PCM_16BIT is
        // the documented default when the key is absent.
        val encoding = format.getIntegerOrNull(MediaFormat.KEY_PCM_ENCODING) ?: AudioFormat.ENCODING_PCM_16BIT
        val channelMask = if (channelCount == 1) AudioFormat.CHANNEL_OUT_MONO else AudioFormat.CHANNEL_OUT_STEREO

        if (track != null && sampleRate == trackSampleRate && channelMask == trackChannelMask && encoding == trackEncoding) {
            return // Nothing actually changed; keep the running track.
        }

        val minBuf = AudioTrack.getMinBufferSize(sampleRate, channelMask, encoding)
        if (minBuf <= 0) {
            onFatal("AudioTrack.getMinBufferSize failed for sr=$sampleRate ch=$channelCount enc=$encoding")
            return
        }

        val newTrack = try {
            AudioTrack.Builder()
                .setAudioAttributes(
                    AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_MEDIA)
                        .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                        .build(),
                )
                .setAudioFormat(
                    AudioFormat.Builder()
                        .setEncoding(encoding)
                        .setSampleRate(sampleRate)
                        .setChannelMask(channelMask)
                        .build(),
                )
                // Headroom over the platform minimum to absorb network jitter
                // (packets arrive in Rust-worker-thread bursts, not a steady
                // 20ms tick) without underrunning; still small enough that it
                // doesn't add noticeable latency.
                .setBufferSizeInBytes(minBuf * BUFFER_MULTIPLIER)
                .setTransferMode(AudioTrack.MODE_STREAM)
                .build()
        } catch (e: Exception) {
            onFatal("cannot start AudioTrack: ${e.message}")
            return
        }
        newTrack.play()

        track?.let { old -> runCatching { old.stop() }; runCatching { old.release() } }
        track = newTrack
        trackSampleRate = sampleRate
        trackChannelMask = channelMask
        trackEncoding = encoding
        Log.i(TAG, "audio track sr=$sampleRate ch=$channelCount enc=$encoding buffer=${minBuf * BUFFER_MULTIPLIER}")
    }

    private val callback = object : MediaCodec.Callback() {
        override fun onInputBufferAvailable(mc: MediaCodec, index: Int) {
            synchronized(lock) {
                if (released || mc !== codec) return
                val data = queue.pollFirst()
                if (data == null) {
                    pendingInputs.addLast(index)
                } else {
                    feedInput(mc, index, data)
                }
            }
        }

        override fun onOutputBufferAvailable(mc: MediaCodec, index: Int, info: MediaCodec.BufferInfo) {
            val t = synchronized(lock) { if (released || mc !== codec) null else track }
            if (t == null) {
                // onOutputFormatChanged always precedes the first output
                // buffer in practice, but if it hasn't fired yet there is
                // nowhere to put the PCM — drop this buffer rather than
                // crash the session.
                try {
                    mc.releaseOutputBuffer(index, false)
                } catch (_: IllegalStateException) {
                }
                return
            }
            try {
                val buf = mc.getOutputBuffer(index)
                if (buf != null && info.size > 0) {
                    buf.position(info.offset)
                    buf.limit(info.offset + info.size)
                    t.write(buf, info.size, AudioTrack.WRITE_BLOCKING)
                }
                mc.releaseOutputBuffer(index, false)
            } catch (e: IllegalStateException) {
                Log.w(TAG, "audio releaseOutputBuffer failed: ${e.message}")
            }
        }

        override fun onOutputFormatChanged(mc: MediaCodec, format: MediaFormat) {
            synchronized(lock) {
                if (released || mc !== codec) return
                configureTrack(format)
            }
        }

        override fun onError(mc: MediaCodec, e: MediaCodec.CodecException) {
            Log.e(TAG, "audio codec error transient=${e.isTransient} recoverable=${e.isRecoverable}", e)
            if (e.isTransient) return
            synchronized(lock) {
                if (released) return
                if (e.isRecoverable) {
                    // Documented recovery path: stop, restart, keep going —
                    // Opus has no keyframe concept, so no need to wait for one.
                    runCatching { mc.stop(); mc.start() }
                        .onFailure { onFatal("audio codec restart failed: ${it.message}") }
                    pendingInputs.clear()
                    queue.clear()
                } else {
                    onFatal("audio decoder failed: ${e.message}")
                }
            }
        }
    }

    private fun MediaFormat.getIntegerOrNull(key: String): Int? =
        if (containsKey(key)) getInteger(key) else null

    // -----------------------------------------------------------------------
    // Opus-in-MediaCodec codec-specific data
    // -----------------------------------------------------------------------

    /**
     * Synthesizes a minimal Opus identification header ("OpusHead", RFC 7845
     * §5.1) to satisfy MediaCodec's csd-0 requirement. There is no real
     * header to extract — termland's stream is headerless raw Opus packets —
     * so every field below is either a fixed constant from the spec or one
     * of the server's two fixed stream parameters (48kHz stereo; see
     * crates/termland-codec/src/audio.rs's SAMPLE_RATE/CHANNELS, which are
     * never negotiated).
     *
     * Byte layout (19 bytes total, multi-byte fields little-endian):
     * ```
     *   0..7   "OpusHead" magic signature (8 ASCII bytes, no NUL terminator)
     *   8      version = 1
     *   9      channel count = 2 (server is always stereo)
     *   10..11 pre-skip, u16 = 0 (see note below)
     *   12..15 original input sample rate, u32 = 48000 (informational only —
     *          the decoder always outputs at its own native rate)
     *   16..17 output gain, s16 = 0 (no adjustment)
     *   18     channel mapping family = 0 (plain L/R stereo, no mapping table)
     * ```
     *
     * Pre-skip is normally the encoder's algorithmic lookahead, so a decoder
     * knows how many samples to trim from the very start of playback.
     * termland streams live PCM straight through `opus_encode` with no
     * container/seek layer to carry that number to us, so it is set to 0:
     * worst case a few milliseconds of encoder priming are audible once at
     * connect time, which beats guessing a value and trimming real audio.
     */
    private fun opusHeadBytes(): ByteArray {
        val buf = ByteBuffer.allocate(19).order(ByteOrder.LITTLE_ENDIAN)
        buf.put("OpusHead".toByteArray(Charsets.US_ASCII)) // 0..7
        buf.put(1.toByte())                                // 8: version
        buf.put(CHANNEL_COUNT.toByte())                     // 9: channel count
        buf.putShort(0.toShort())                          // 10..11: pre-skip
        buf.putInt(SAMPLE_RATE)                             // 12..15: input sample rate
        buf.putShort(0.toShort())                          // 16..17: output gain
        buf.put(0.toByte())                                 // 18: channel mapping family
        return buf.array()
    }

    /**
     * csd-1 and csd-2 for Opus are not part of RFC 7845 — they are an
     * Android/MediaCodec-specific extension (the same convention ExoPlayer's
     * OpusUtil uses when it hands a decoder container-free CSD): each is an
     * 8-byte little-endian signed integer, in **nanoseconds**. csd-1 is the
     * codec delay, matching the pre-skip above (so 0); csd-2 is the seek
     * pre-roll, meaningless for a live stream, so it uses the conventional
     * default of 80ms — 3840 samples at 48kHz — the same constant most
     * headerless-Opus-on-Android integrations default to.
     */
    private fun nanosLE(value: Long): ByteArray =
        ByteBuffer.allocate(8).order(ByteOrder.LITTLE_ENDIAN).putLong(value).array()

    private companion object {
        const val TAG = "TermlandAudio"

        const val MIME = MediaFormat.MIMETYPE_AUDIO_OPUS

        /** termland_codec::audio::SAMPLE_RATE — hardcoded server-side, never negotiated. */
        const val SAMPLE_RATE = 48000

        /** termland_codec::audio::CHANNELS — the server always captures/encodes stereo. */
        const val CHANNEL_COUNT = 2

        /** 3840 samples @ 48kHz = 80ms, the conventional Opus seek-preroll default. */
        const val SEEK_PREROLL_NS = 80_000_000L

        /** ~640ms of 20ms Opus frames — audio needs far less slack than video's frame queue. */
        const val MAX_QUEUE = 32

        /** Headroom multiplier over AudioTrack's platform-minimum buffer size. */
        const val BUFFER_MULTIPLIER = 4
    }
}
