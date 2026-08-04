package dev.termland.android.net

import android.media.MediaCodecInfo
import android.media.MediaCodecList
import android.os.Build
import android.util.Log
import dev.termland.core.MobileCodec

/**
 * What this device can actually decode, and with which decoder.
 *
 * This probe is the entire reason the server-side codec negotiation exists.
 * Android decoder support is a patchwork (docs/mobile-clients.md, "Video decode
 * matrix"): AV1 is hardware only on recent SoCs, VP8/VP9 vary, HEVC is common
 * but not universal. We advertise exactly the decodable set in
 * [dev.termland.core.SessionParams.supportedCodecs] and let the server pick, so
 * no guessing and no per-device special-casing on either side.
 */
object CodecSupport {

    private const val TAG = "TermlandCodec"

    /** MobileCodec -> MediaFormat MIME type. */
    val MIME: Map<MobileCodec, String> = mapOf(
        MobileCodec.AV1 to "video/av01",
        MobileCodec.VP9 to "video/x-vnd.on2.vp9",
        MobileCodec.VP8 to "video/x-vnd.on2.vp8",
        MobileCodec.H265 to "video/hevc",
        MobileCodec.H264 to "video/avc",
    )

    /**
     * Preference order used only to make logs and the advertised list readable —
     * the server makes the final choice. Best quality-per-bit first, with H.264
     * last as the universal fallback.
     */
    private val PREFERENCE = listOf(
        MobileCodec.AV1, MobileCodec.H265, MobileCodec.VP9, MobileCodec.VP8, MobileCodec.H264,
    )

    /**
     * Software AV1 decode is real on Android (libgav1 ships in most builds) but at
     * desktop resolutions it burns battery and adds tens of milliseconds — worse
     * than HEVC in every way that matters for a thin client. Only advertise AV1
     * when a hardware decoder is present.
     */
    private const val ALLOW_SOFTWARE_AV1 = false

    data class Decoder(
        val codec: MobileCodec,
        val mime: String,
        /** Concrete decoder to instantiate, e.g. "c2.qti.avc.decoder". */
        val name: String,
        val hardware: Boolean,
        val maxWidth: Int,
        val maxHeight: Int,
    )

    /** Probed once per process; MediaCodecList enumeration is not cheap. */
    val decoders: List<Decoder> by lazy { probe() }

    /** Exactly the set to advertise to the server. */
    val supportedCodecs: List<MobileCodec> by lazy {
        PREFERENCE.filter { codec ->
            val d = decoders.filter { it.codec == codec }
            when {
                d.isEmpty() -> false
                codec == MobileCodec.AV1 && !ALLOW_SOFTWARE_AV1 -> d.any { it.hardware }
                else -> true
            }
        }
    }

    /** Best decoder for a codec: hardware wins, then larger max resolution. */
    fun best(codec: MobileCodec): Decoder? = decoders
        .filter { it.codec == codec }
        .maxWithOrNull(compareBy({ it.hardware }, { it.maxWidth.toLong() * it.maxHeight }))

    private fun probe(): List<Decoder> {
        // REGULAR_CODECS excludes the "for size only"/deprecated entries, i.e. it
        // reports what an app may actually instantiate.
        val list = MediaCodecList(MediaCodecList.REGULAR_CODECS)
        val found = mutableListOf<Decoder>()

        for (info in list.codecInfos) {
            if (info.isEncoder) continue
            for ((codec, mime) in MIME) {
                if (info.supportedTypes.none { it.equals(mime, ignoreCase = true) }) continue
                val caps = runCatching { info.getCapabilitiesForType(mime) }.getOrNull() ?: continue
                val video = caps.videoCapabilities ?: continue
                found += Decoder(
                    codec = codec,
                    mime = mime,
                    name = info.name,
                    hardware = isHardware(info),
                    maxWidth = video.supportedWidths.upper,
                    maxHeight = video.supportedHeights.upper,
                )
            }
        }

        Log.i(TAG, "probed ${found.size} decoders: " + found.joinToString {
            "${it.codec}/${it.name}${if (it.hardware) " [hw]" else " [sw]"}"
        })
        return found
    }

    private fun isHardware(info: MediaCodecInfo): Boolean =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            info.isHardwareAccelerated
        } else {
            // Pre-29 has no API for this. Google's own software decoders are the
            // ones prefixed OMX.google. / c2.android. — everything else is a
            // vendor codec and effectively always hardware.
            !info.name.startsWith("OMX.google.", ignoreCase = true) &&
                !info.name.startsWith("c2.android.", ignoreCase = true)
        }
}
