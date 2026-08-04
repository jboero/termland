package dev.termland.android

import android.app.Application
import android.util.Log
import dev.termland.android.net.CodecSupport

class TermlandApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        // Probe decoders at startup, off the critical path of the first connect:
        // the result is what we advertise in SessionParams.supportedCodecs, and
        // MediaCodecList enumeration takes tens of milliseconds.
        Log.i("Termland", "decodable codecs: ${CodecSupport.supportedCodecs}")
    }
}
