import Foundation

/// The two codecs VideoToolbox decodes here. AV1/VP8/VP9 are never advertised
/// (see `CodecSupport`), so the converter does not need to know about them.
enum NALCodec {
    case h264
    case hevc
}

/// One server video packet, reshaped for VideoToolbox.
///
/// The server sends FFmpeg's elementary stream: Annex B, i.e. NAL units
/// separated by `00 00 01` / `00 00 00 01` start codes, with the parameter
/// sets (SPS/PPS, plus VPS for HEVC) repeated in-band on every keyframe.
/// VideoToolbox wants the ISO BMFF shape instead: parameter sets out-of-band
/// in a `CMVideoFormatDescription`, and every remaining NAL prefixed with its
/// 4-byte big-endian length (AVCC / HVCC sample data).
struct AccessUnit: Equatable {
    /// In the order the format-description constructors expect: SPS, PPS for
    /// H.264; VPS, SPS, PPS for HEVC. Empty on delta frames.
    var parameterSets: [Data]
    /// Length-prefixed slice (and SEI) NAL units, start codes removed.
    var sampleData: Data
    /// `nal_unit_type` of the first coded slice (VCL NAL), if the packet holds
    /// a picture at all. Needed to recognise HEVC leading pictures.
    var pictureType: UInt8? = nil

    /// HEVC RASL pictures (types 8/9) reference pictures from *before* the
    /// CRA they follow. When decoding starts at that CRA they cannot be
    /// decoded and must be skipped (H.265 §8.1.3, "NoRaslOutputFlag").
    var isSkippableLeadingPicture: Bool {
        guard let pictureType else { return false }
        return pictureType == 8 || pictureType == 9
    }
}

enum AnnexB {
    /// Split an Annex B byte stream into NAL units without their start codes.
    ///
    /// Trailing zero bytes are stripped from each unit: a 4-byte start code is
    /// a 3-byte one preceded by a zero, and encoders may also pad with
    /// `trailing_zero_8bits`. A NAL unit can never legitimately end in 0x00
    /// (it ends with the RBSP stop bit), so this is lossless.
    static func split(_ data: Data) -> [Data] {
        let bytes = [UInt8](data)
        var units: [Data] = []
        var start: Int? = nil
        var i = 0
        while i + 2 < bytes.count {
            if bytes[i] == 0, bytes[i + 1] == 0, bytes[i + 2] == 1 {
                if let s = start { units.append(trimmed(bytes, s, i)) }
                i += 3
                start = i
            } else {
                i += 1
            }
        }
        if let s = start, s < bytes.count { units.append(trimmed(bytes, s, bytes.count)) }
        return units.filter { !$0.isEmpty }
    }

    /// Classify and reshape one packet. Returns `nil` when the packet holds no
    /// NAL units at all (not Annex B, or empty) so the caller can drop it.
    static func convert(_ data: Data, codec: NALCodec) -> AccessUnit? {
        let units = split(data)
        guard !units.isEmpty else { return nil }

        var vps: [Data] = [], sps: [Data] = [], pps: [Data] = []
        var sample = Data()
        var pictureType: UInt8?
        for unit in units {
            if pictureType == nil, let type = sliceType(of: unit, codec: codec) { pictureType = type }
            switch kind(of: unit, codec: codec) {
            case .vps: vps.append(unit)
            case .sps: sps.append(unit)
            case .pps: pps.append(unit)
            // Access-unit delimiters mean nothing once each packet is already
            // one access unit, and some decoders reject them in AVCC samples.
            case .delimiter: continue
            case .other:
                var length = UInt32(unit.count).bigEndian
                withUnsafeBytes(of: &length) { sample.append(contentsOf: $0) }
                sample.append(unit)
            }
        }
        let parameterSets = codec == .hevc ? vps + sps + pps : sps + pps
        return AccessUnit(parameterSets: parameterSets, sampleData: sample, pictureType: pictureType)
    }

    /// `nal_unit_type` if `unit` is a coded slice (VCL NAL), else `nil`.
    /// H.264 slices are types 1...5; HEVC VCL types are 0...31.
    static func sliceType(of unit: Data, codec: NALCodec) -> UInt8? {
        guard let header = unit.first else { return nil }
        switch codec {
        case .h264:
            let type = header & 0x1F
            return (1...5).contains(type) ? type : nil
        case .hevc:
            let type = (header >> 1) & 0x3F
            return type <= 31 ? type : nil
        }
    }

    /// HEVC random-access points that may be followed by RASL pictures.
    static func isCleanRandomAccess(_ type: UInt8?, codec: NALCodec) -> Bool {
        guard codec == .hevc, let type else { return false }
        return (16...18).contains(type) || type == 21 // BLA_W_LP...BLA_N_LP, CRA_NUT
    }

    /// True once every parameter set VideoToolbox needs to build a format
    /// description is present.
    static func isComplete(_ parameterSets: [Data], codec: NALCodec) -> Bool {
        let kinds = parameterSets.map { kind(of: $0, codec: codec) }
        let hasCore = kinds.contains(.sps) && kinds.contains(.pps)
        return codec == .hevc ? hasCore && kinds.contains(.vps) : hasCore
    }

    enum Kind: Equatable { case vps, sps, pps, delimiter, other }

    static func kind(of unit: Data, codec: NALCodec) -> Kind {
        guard let header = unit.first else { return .other }
        switch codec {
        case .h264:
            // nal_unit_type is the low 5 bits (ITU-T H.264 table 7-1).
            switch header & 0x1F {
            case 7: return .sps
            case 8: return .pps
            case 9: return .delimiter
            default: return .other
            }
        case .hevc:
            // nal_unit_type is bits 1...6 of the first header byte (H.265 table 7-1).
            switch (header >> 1) & 0x3F {
            case 32: return .vps
            case 33: return .sps
            case 34: return .pps
            case 35: return .delimiter
            default: return .other
            }
        }
    }

    private static func trimmed(_ bytes: [UInt8], _ from: Int, _ to: Int) -> Data {
        var end = to
        while end > from, bytes[end - 1] == 0 { end -= 1 }
        return Data(bytes[from..<end])
    }
}
