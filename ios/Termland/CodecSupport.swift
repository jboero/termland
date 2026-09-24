import CoreMedia
import VideoToolbox

/// What this device will ask the server to encode.
///
/// The server picks the best encoder *from this list*, so it must be honest:
/// advertising a codec that ends up on a software path costs battery and
/// latency for the whole session (docs/mobile-clients.md, "hardware decode is
/// mandatory").
///
/// - HEVC is advertised first, and only when VideoToolbox reports a hardware
///   decoder for it (every Apple-Silicon Mac and A9-or-later iOS device).
/// - H.264 is always advertised as the universal fallback.
/// - AV1 is deliberately not advertised yet: hardware AV1 is only on
///   A17 Pro / M3 and later, and VideoToolbox has no software AV1 path to fall
///   back on. VP8/VP9 are never decodable by VideoToolbox.
enum CodecSupport {
    static let supportedCodecs: [MobileCodec] = {
        var codecs: [MobileCodec] = []
        if VTIsHardwareDecodeSupported(kCMVideoCodecType_HEVC) { codecs.append(.h265) }
        codecs.append(.h264)
        return codecs
    }()

    static func nalCodec(for codec: MobileCodec) -> NALCodec? {
        switch codec {
        case .h264: return .h264
        case .h265: return .hevc
        case .av1, .vp8, .vp9: return nil
        }
    }
}

extension HostProfile {
    /// Session parameters for a remote surface of `width` × `height` pixels.
    ///
    /// Desktop mode (no app command) with a mid quality. Audio stays off until
    /// the Opus → AVAudioEngine path exists (M3e); asking for it now would only
    /// spend bandwidth on packets nothing plays.
    func sessionParams(width: Int, height: Int) -> SessionParams {
        SessionParams(
            width: UInt32(clamping: width),
            height: UInt32(clamping: height),
            quality: 75,
            audio: false,
            desktopShell: nil,
            appCommand: nil,
            supportedCodecs: CodecSupport.supportedCodecs
        )
    }
}
