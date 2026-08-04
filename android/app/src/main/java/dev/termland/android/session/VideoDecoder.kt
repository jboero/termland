package dev.termland.android.session

import android.media.MediaCodec
import android.media.MediaFormat
import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import android.util.Log
import android.view.Surface
import dev.termland.android.net.CodecSupport
import dev.termland.core.MobileCodec
import java.nio.ByteBuffer
import java.util.ArrayDeque
import java.util.concurrent.atomic.AtomicLong

/**
 * Encoded packets in, pixels on the screen out. Nothing in between touches Java.
 *
 * Two decisions dominate this class, both for latency:
 *
 *  1. `configure(format, surface, null, 0)` — the decoder renders **directly into
 *     the SurfaceView's Surface**. Output buffers are never mapped into the JVM,
 *     so a decoded 4K frame is a buffer handle, not 12 MB of copying. This is why
 *     we never see the pixels and never want to.
 *  2. Asynchronous mode (`setCallback`) on a dedicated [HandlerThread]. Polling
 *     `dequeueInputBuffer` with a timeout adds jitter and a thread that either
 *     spins or sleeps too long; the callback fires the instant a buffer frees up.
 *
 * `onVideoPacket` is called from a Rust worker thread that must not block
 * (see the UniFFI contract), so packets are pushed onto a bounded queue and the
 * codec thread drains it.
 */
class VideoDecoder(
    private val surface: Surface,
    private val onFatal: (String) -> Unit,
    private val onFirstFrame: () -> Unit = {},
) {
    private val thread = HandlerThread("termland-decode").apply { start() }
    private val handler = Handler(thread.looper)

    /** Guards [codec], [pendingInputs] and the packet queue. */
    private val lock = Any()

    private var codec: MediaCodec? = null
    private var currentCodec: MobileCodec? = null
    private var currentWidth = 0
    private var currentHeight = 0

    /** Input buffer indices the codec has handed us but we had no data for. */
    private val pendingInputs = ArrayDeque<Int>()

    /** Encoded packets waiting for an input buffer. */
    private val queue = ArrayDeque<Packet>()

    /**
     * A decoder fed a delta frame first will either error out or emit garbage
     * until the next keyframe, so drop everything until one arrives. Set again on
     * every reconfigure.
     */
    private var awaitingKeyframe = true

    private var released = false
    private var sawOutput = false
    private val droppedPackets = AtomicLong(0)

    private class Packet(val data: ByteArray, val ptsUs: Long, val keyframe: Boolean)

    /**
     * Feed one packet. Safe to call from any thread; never blocks.
     *
     * Resolution or codec changes are handled here because that is where we have
     * the packet header: the stream is reconfigured on the next keyframe.
     */
    fun submit(
        data: ByteArray,
        ptsUs: Long,
        keyframe: Boolean,
        codecId: MobileCodec,
        width: Int,
        height: Int,
    ) {
        synchronized(lock) {
            if (released) return

            val needsConfigure = codec == null ||
                codecId != currentCodec ||
                (width > 0 && height > 0 && (width != currentWidth || height != currentHeight))

            if (needsConfigure) {
                // Only a keyframe can start (or restart) a stream.
                if (!keyframe) return
                if (!configure(codecId, width, height)) return
            }

            if (awaitingKeyframe) {
                if (!keyframe) {
                    droppedPackets.incrementAndGet()
                    return
                }
                awaitingKeyframe = false
            }

            if (queue.size >= MAX_QUEUE) {
                // Better to lose the oldest frame than to grow unbounded latency;
                // the encoder will send another keyframe soon enough.
                queue.pollFirst()
                droppedPackets.incrementAndGet()
                awaitingKeyframe = true
            }
            queue.addLast(Packet(data, ptsUs, keyframe))
            drainLocked()
        }
    }

    /** Called on rotation: the Surface is the same, only its size changed. */
    fun onSurfaceSizeChanged() {
        // Nothing to do — a SurfaceView scales the decoder's output to the layout
        // bounds in the compositor. Kept as a hook so the caller reads honestly.
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
        }
        thread.quitSafely()
    }

    val droppedFrameCount: Long get() = droppedPackets.get()

    // -----------------------------------------------------------------------

    private fun configure(codecId: MobileCodec, width: Int, height: Int): Boolean {
        runCatching { codec?.stop() }
        runCatching { codec?.release() }
        codec = null
        pendingInputs.clear()
        queue.clear()

        val mime = CodecSupport.MIME[codecId] ?: run {
            onFatal("no MIME mapping for $codecId")
            return false
        }
        val choice = CodecSupport.best(codecId)

        return try {
            // createByCodecName pins the hardware decoder the probe found;
            // createDecoderByType would let the framework pick, and it sometimes
            // picks the software one.
            val mc = if (choice != null) {
                MediaCodec.createByCodecName(choice.name)
            } else {
                MediaCodec.createDecoderByType(mime)
            }

            val format = MediaFormat.createVideoFormat(mime, width.coerceAtLeast(2), height.coerceAtLeast(2))
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                // Tells the decoder not to buffer frames for reordering. Cuts
                // multiple frames of latency on hardware decoders that honour it.
                format.setInteger(MediaFormat.KEY_LOW_LATENCY, 1)
            }
            // Never let the framework hold frames back waiting for a display
            // deadline; we always want the newest frame on screen.
            format.setInteger(MediaFormat.KEY_PRIORITY, 0) // 0 = realtime
            format.setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, MAX_INPUT_SIZE)

            mc.setCallback(callback, handler)
            mc.configure(format, surface, null, 0)
            mc.start()

            codec = mc
            currentCodec = codecId
            currentWidth = width
            currentHeight = height
            awaitingKeyframe = true
            Log.i(
                TAG,
                "decoder=${choice?.name ?: mime} hw=${choice?.hardware} ${width}x$height low-latency=" +
                    (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R),
            )
            true
        } catch (e: Exception) {
            onFatal("cannot start $codecId decoder: ${e.message}")
            false
        }
    }

    /** Must hold [lock]. */
    private fun drainLocked() {
        val mc = codec ?: return
        while (queue.isNotEmpty() && pendingInputs.isNotEmpty()) {
            val index = pendingInputs.pollFirst()
            val packet = queue.pollFirst() ?: break
            if (!feed(mc, index, packet)) break
        }
    }

    private fun feed(mc: MediaCodec, index: Int, packet: Packet): Boolean {
        return try {
            val buf: ByteBuffer = mc.getInputBuffer(index) ?: return false
            if (buf.capacity() < packet.data.size) {
                Log.w(TAG, "input buffer too small (${buf.capacity()} < ${packet.data.size}), dropping")
                droppedPackets.incrementAndGet()
                mc.queueInputBuffer(index, 0, 0, packet.ptsUs, 0)
                return true
            }
            buf.clear()
            buf.put(packet.data)
            val flags = if (packet.keyframe) MediaCodec.BUFFER_FLAG_KEY_FRAME else 0
            mc.queueInputBuffer(index, 0, packet.data.size, packet.ptsUs, flags)
            true
        } catch (e: IllegalStateException) {
            // Codec was reset/released underneath us.
            Log.w(TAG, "queueInputBuffer failed: ${e.message}")
            false
        }
    }

    private val callback = object : MediaCodec.Callback() {
        override fun onInputBufferAvailable(mc: MediaCodec, index: Int) {
            synchronized(lock) {
                if (released || mc !== codec) return
                val packet = queue.pollFirst()
                if (packet == null) {
                    pendingInputs.addLast(index)
                } else {
                    feed(mc, index, packet)
                }
            }
        }

        override fun onOutputBufferAvailable(mc: MediaCodec, index: Int, info: MediaCodec.BufferInfo) {
            // render = true hands the buffer to SurfaceFlinger. No timestamp
            // variant: releaseOutputBuffer(index, nanoTime) would schedule the
            // frame for a future vsync, which is exactly the buffering we are
            // trying to avoid — show it now.
            try {
                mc.releaseOutputBuffer(index, true)
            } catch (e: IllegalStateException) {
                Log.w(TAG, "releaseOutputBuffer failed: ${e.message}")
                return
            }
            if (!sawOutput) {
                sawOutput = true
                onFirstFrame()
            }
        }

        override fun onOutputFormatChanged(mc: MediaCodec, format: MediaFormat) {
            // The decoder's real output geometry, which can differ from the format
            // we configured (crop, alignment) and changes on a mid-stream
            // resolution switch. The SurfaceView scales for us, so this is
            // informational — but log it, because a mismatch here explains most
            // "why is it stretched" reports.
            val w = format.getIntegerOrNull(MediaFormat.KEY_WIDTH) ?: 0
            val h = format.getIntegerOrNull(MediaFormat.KEY_HEIGHT) ?: 0
            val cropW = format.getIntegerOrNull("crop-right")?.let { r ->
                format.getIntegerOrNull("crop-left")?.let { l -> r - l + 1 }
            }
            val cropH = format.getIntegerOrNull("crop-bottom")?.let { b ->
                format.getIntegerOrNull("crop-top")?.let { t -> b - t + 1 }
            }
            Log.i(TAG, "output format ${w}x$h crop=${cropW}x$cropH")
        }

        override fun onError(mc: MediaCodec, e: MediaCodec.CodecException) {
            Log.e(TAG, "codec error transient=${e.isTransient} recoverable=${e.isRecoverable}", e)
            if (e.isTransient) return
            synchronized(lock) {
                if (released) return
                if (e.isRecoverable) {
                    // Documented recovery path: stop, restart, wait for a keyframe.
                    runCatching { mc.stop(); mc.start() }
                        .onFailure { onFatal("codec restart failed: ${it.message}") }
                    awaitingKeyframe = true
                    pendingInputs.clear()
                    queue.clear()
                } else {
                    onFatal("decoder failed: ${e.message}")
                }
            }
        }
    }

    private fun MediaFormat.getIntegerOrNull(key: String): Int? =
        if (containsKey(key)) getInteger(key) else null

    private companion object {
        const val TAG = "TermlandDecode"

        /**
         * At 60 fps this is ~130 ms of slack. Deeper queues only convert dropped
         * frames into input lag, which is the wrong trade for a remote desktop.
         */
        const val MAX_QUEUE = 8

        /** 4 MB comfortably holds a 4K keyframe at the bitrates the server uses. */
        const val MAX_INPUT_SIZE = 4 * 1024 * 1024
    }
}
