import XCTest
import CoreMedia
@testable import Termland

final class TermlandTests: XCTestCase {
    @MainActor
    func testClientStartsDisconnected() {
        let connected = HomeModel().clientIsConnectedForTesting
        XCTAssertFalse(connected)
    }
}

// MARK: - Annex B → AVCC/HVCC

final class AnnexBTests: XCTestCase {
    private func bytes(_ values: [UInt8]) -> Data { Data(values) }

    func testSplitHandlesThreeAndFourByteStartCodes() {
        // 4-byte start code, then a 3-byte one: both are legal in one stream.
        let stream = bytes([0, 0, 0, 1, 0x67, 0xAA, 0, 0, 1, 0x68, 0xBB])
        XCTAssertEqual(AnnexB.split(stream), [bytes([0x67, 0xAA]), bytes([0x68, 0xBB])])
    }

    func testSplitStripsTrailingZeroPadding() {
        // The zero before a 4-byte start code belongs to the start code, not
        // to the preceding NAL; trailing_zero_8bits padding is dropped too.
        let stream = bytes([0, 0, 1, 0x65, 0x11, 0, 0, 0, 0, 1, 0x41, 0x22, 0, 0])
        XCTAssertEqual(AnnexB.split(stream), [bytes([0x65, 0x11]), bytes([0x41, 0x22])])
    }

    func testSplitRejectsDataWithoutStartCodes() {
        XCTAssertTrue(AnnexB.split(bytes([0x65, 0x11, 0x22])).isEmpty)
        XCTAssertNil(AnnexB.convert(bytes([0x65, 0x11, 0x22]), codec: .h264))
    }

    func testH264KeyframeMovesParameterSetsOutOfBandAndLengthPrefixesTheRest() throws {
        let stream = bytes([
            0, 0, 0, 1, 0x09, 0xF0,       // AUD: dropped
            0, 0, 0, 1, 0x67, 0x42, 0x1E, // SPS (a NAL never ends in 0x00)
            0, 0, 0, 1, 0x68, 0xCE,       // PPS
            0, 0, 1, 0x06, 0x05,          // SEI: kept as sample data
            0, 0, 1, 0x65, 0x88, 0x84,    // IDR slice
        ])
        let unit = try XCTUnwrap(AnnexB.convert(stream, codec: .h264))
        XCTAssertEqual(unit.parameterSets, [bytes([0x67, 0x42, 0x1E]), bytes([0x68, 0xCE])])
        XCTAssertEqual(unit.sampleData, bytes([0, 0, 0, 2, 0x06, 0x05, 0, 0, 0, 3, 0x65, 0x88, 0x84]))
        XCTAssertTrue(AnnexB.isComplete(unit.parameterSets, codec: .h264))
    }

    func testHEVCOrdersParameterSetsVPSThenSPSThenPPS() throws {
        // Deliberately out of order on the wire; CoreMedia wants VPS, SPS, PPS.
        let stream = bytes([
            0, 0, 1, 0x44, 0x01, 0xC1, // PPS  (type 34)
            0, 0, 1, 0x40, 0x01, 0x0C, // VPS  (type 32)
            0, 0, 1, 0x42, 0x01, 0x01, // SPS  (type 33)
            0, 0, 1, 0x46, 0x01, 0x50, // AUD  (type 35): dropped
            0, 0, 1, 0x26, 0x01, 0xAF, // IDR_W_RADL (type 19)
        ])
        let unit = try XCTUnwrap(AnnexB.convert(stream, codec: .hevc))
        XCTAssertEqual(unit.parameterSets, [
            bytes([0x40, 0x01, 0x0C]), bytes([0x42, 0x01, 0x01]), bytes([0x44, 0x01, 0xC1]),
        ])
        XCTAssertEqual(unit.sampleData, bytes([0, 0, 0, 3, 0x26, 0x01, 0xAF]))
        XCTAssertTrue(AnnexB.isComplete(unit.parameterSets, codec: .hevc))
        XCTAssertFalse(AnnexB.isComplete(Array(unit.parameterSets.dropFirst()), codec: .hevc), "HEVC needs a VPS")
    }
}

// MARK: - Key maps

final class KeyMapTests: XCTestCase {
    func testHIDUsagesMatchEvdev() {
        XCTAssertEqual(KeyMap.scancode(hidUsage: 0x04), 30) // a → KEY_A
        XCTAssertEqual(KeyMap.scancode(hidUsage: 0x1D), 44) // z → KEY_Z
        XCTAssertEqual(KeyMap.scancode(hidUsage: 0x1E), 2)  // 1 → KEY_1
        XCTAssertEqual(KeyMap.scancode(hidUsage: 0x27), 11) // 0 → KEY_0
        XCTAssertEqual(KeyMap.scancode(hidUsage: 0x43), 68) // F10
        XCTAssertEqual(KeyMap.scancode(hidUsage: 0x44), 87) // F11 is 87, not 69
        XCTAssertEqual(KeyMap.scancode(hidUsage: 0x45), 88) // F12
        XCTAssertEqual(KeyMap.scancode(hidUsage: 0xE3), 125) // Left GUI → KEY_LEFTMETA
        XCTAssertNil(KeyMap.scancode(hidUsage: 0x00))
    }

    func testMacVirtualKeysMatchEvdev() {
        XCTAssertEqual(KeyMap.scancode(macKeyCode: 0x00), 30)  // kVK_ANSI_A
        XCTAssertEqual(KeyMap.scancode(macKeyCode: 0x24), 28)  // kVK_Return
        XCTAssertEqual(KeyMap.scancode(macKeyCode: 0x33), 14)  // kVK_Delete is Backspace
        XCTAssertEqual(KeyMap.scancode(macKeyCode: 0x75), 111) // kVK_ForwardDelete
        XCTAssertEqual(KeyMap.scancode(macKeyCode: 0x37), 125) // kVK_Command → Super
        XCTAssertEqual(KeyMap.scancode(macKeyCode: 0x3A), 56)  // kVK_Option → Alt
        XCTAssertNil(KeyMap.scancode(macKeyCode: 0x3F))        // kVK_Function: not forwarded
    }

    /// Both platform tables must agree wherever they describe the same key.
    func testHIDAndMacTablesAgreeOnLetters() {
        let macLetters: [UInt16] = [0x00, 0x0B, 0x08, 0x02, 0x0E, 0x03, 0x05, 0x04, 0x22, 0x26, 0x28, 0x25, 0x2E,
                                    0x2D, 0x1F, 0x23, 0x0C, 0x0F, 0x01, 0x11, 0x20, 0x09, 0x0D, 0x07, 0x10, 0x06]
        for (index, macCode) in macLetters.enumerated() {
            XCTAssertEqual(KeyMap.scancode(macKeyCode: macCode), KeyMap.scancode(hidUsage: 0x04 + index),
                           "letter \(Character(UnicodeScalar(UInt8(97 + index))))")
        }
    }
}

// MARK: - Pointer mapping and sizing

@MainActor
final class InputMappingTests: XCTestCase {
    func testPointerMappingAccountsForLetterbox() {
        let router = InputRouter(client: TermlandClient())
        // 1920×1080 video in a 1000×1000 view: aspect-fit leaves 218.75 pt bars
        // above and below.
        router.remoteSize = CGSize(width: 1920, height: 1080)
        router.viewBounds = CGRect(x: 0, y: 0, width: 1000, height: 1000)
        XCTAssertEqual(router.remotePoint(CGPoint(x: 500, y: 500)).x, 960, accuracy: 0.001)
        XCTAssertEqual(router.remotePoint(CGPoint(x: 500, y: 500)).y, 540, accuracy: 0.001)
        XCTAssertEqual(router.remotePoint(CGPoint(x: 0, y: 218.75)).y, 0, accuracy: 0.001)
        // Points in the bars clamp to the framebuffer edge.
        XCTAssertEqual(router.remotePoint(CGPoint(x: 1000, y: 1000)), CGPoint(x: 1919, y: 1079))
    }

    func testRemoteSizeIsEvenAndClamped() {
        XCTAssertTrue(SessionModel.remoteSize(for: CGSize(width: 1281, height: 801)) == (1280, 800))
        XCTAssertTrue(SessionModel.remoteSize(for: CGSize(width: 100, height: 100)) == (320, 320))
        XCTAssertTrue(SessionModel.remoteSize(for: CGSize(width: 9000, height: 5000)) == (3840, 3840))
    }
}

// MARK: - VideoToolbox, end to end

/// Feeds real encoder output — libx264/libx265 at their default-ish presets
/// with B-frames, the same shape the server's software encoders produce — one
/// access unit per packet, as the core delivers them.
///
/// Fixtures (ios/TermlandTests/Fixtures) were made with:
///   ffmpeg -f lavfi -i testsrc=size=320x240:rate=30 -frames:v 30 -pix_fmt yuv420p \
///     -c:v libx264 -preset fast -bf 2 -g 15 -x264-params aud=1 out.mkv
///   (libx265: -x265-params aud=1:bframes=2:keyint=15)
///   ffmpeg -i out.mkv -c copy -bsf:v h264_mp4toannexb -f h264 bframes.h264
///   ffprobe -show_entries packet=pts,flags -of csv=p=0 out.mkv > bframes.h264.packets
/// The `.packets` sidecar gives each packet's PTS (ms) and keyframe flag in
/// decode order, which a raw elementary stream does not carry.
final class VideoDecoderTests: XCTestCase {
    func testH264WithBFramesDecodesEveryFrameInPresentationOrder() throws {
        try assertDecodes(fixture: "bframes.h264", codec: .h264)
    }

    func testHEVCWithBFramesDecodesEveryFrameInPresentationOrder() throws {
        try assertDecodes(fixture: "bframes.hevc", codec: .h265)
    }

    func testDeltaFramesBeforeTheFirstKeyframeAreDropped() throws {
        let packets = try loadPackets("bframes.h264", codec: .h264)
        let decoder = VideoDecoder()
        let frames = FrameLog()
        decoder.onDecodedFrame = { frames.append($0) }
        // Join mid-GOP: skip the leading keyframe, as after a lossy reconnect.
        for packet in packets.dropFirst() { decoder.submit(packet) }
        decoder.drain()
        let secondKeyframe = try XCTUnwrap(packets.dropFirst().firstIndex(where: \.keyframe))
        let decodable = packets.count - secondKeyframe
        XCTAssertEqual(frames.count, decodable, "only the GOP starting at the next keyframe is decodable")
    }

    /// libx265's second GOP starts at a CRA followed by RASL pictures that
    /// reference the first GOP. Joining there (reconnect, overflow resync)
    /// must skip exactly those, and decode everything else.
    func testJoiningHEVCAtACRASkipsItsRASLPictures() throws {
        let packets = try loadPackets("bframes.hevc", codec: .h265)
        let units = packets.map { AnnexB.convert($0.data, codec: .hevc)! }
        let craIndex = try XCTUnwrap(units.indices.dropFirst().first { units[$0].pictureType == 21 },
                                     "fixture's second GOP must start with a CRA")
        // The server takes the keyframe flag from FFmpeg's packet flags, as
        // ffprobe does here; a CRA is a keyframe there.
        XCTAssertTrue(packets[craIndex].keyframe, "CRA must be flagged as a keyframe")
        let joined = Array(packets[craIndex...])
        let rasl = units[craIndex...].filter(\.isSkippableLeadingPicture).count
        XCTAssertGreaterThan(rasl, 0, "fixture must have RASL pictures after the CRA")

        let decoder = VideoDecoder()
        let frames = FrameLog()
        decoder.onDecodedFrame = { frames.append($0) }
        for packet in joined { decoder.submit(packet) }
        decoder.drain()

        let shown = frames.times.map { UInt64($0.convertScale(1_000_000, method: .default).value) }
        XCTAssertEqual(shown.count, joined.count - rasl)
        XCTAssertEqual(shown, shown.sorted(), "still in presentation order")
    }

    private func assertDecodes(fixture: String, codec: MobileCodec) throws {
        let packets = try loadPackets(fixture, codec: codec)
        let inputOrder = packets.map(\.timestampUs)
        XCTAssertNotEqual(inputOrder, inputOrder.sorted(), "fixture must contain B-frames to be meaningful")

        let decoder = VideoDecoder()
        let frames = FrameLog()
        decoder.onDecodedFrame = { frames.append($0) }
        for packet in packets { decoder.submit(packet) }
        decoder.drain()

        let shown = frames.times.map { UInt64($0.convertScale(1_000_000, method: .default).value) }
        XCTAssertEqual(shown.count, packets.count, "every frame decodes")
        XCTAssertEqual(shown, inputOrder.sorted(), "frames come out in presentation order")
    }

    private func loadPackets(_ name: String, codec: MobileCodec) throws -> [VideoPacket] {
        let directory = URL(fileURLWithPath: #filePath).deletingLastPathComponent().appendingPathComponent("Fixtures")
        let stream = try Data(contentsOf: directory.appendingPathComponent(name))
        let sidecar = try String(contentsOf: directory.appendingPathComponent(name + ".packets"), encoding: .utf8)
        let meta = sidecar.split(separator: "\n").map { line -> (UInt64, Bool) in
            let fields = line.split(separator: ",")
            return (UInt64(fields[0])! * 1_000, fields[1].hasPrefix("K"))
        }

        // Every access unit starts with an AUD (encoded with aud=1): cut there.
        let nalCodec = try XCTUnwrap(CodecSupport.nalCodec(for: codec))
        let units = Self.accessUnits(stream, codec: nalCodec)
        XCTAssertEqual(units.count, meta.count, "one access unit per ffprobe packet")
        return zip(units, meta).map { data, info in
            VideoPacket(data: data, timestampUs: info.0, keyframe: info.1, codec: codec, width: 320, height: 240)
        }
    }

    /// Re-joins NAL units into per-access-unit Annex B packets, split at AUDs.
    ///
    /// Only an AUD that follows a picture starts a new unit: the
    /// `*_mp4toannexb` filters put the keyframe's parameter sets *before* its
    /// AUD, and those belong to the keyframe, not to a packet of their own.
    private static func accessUnits(_ stream: Data, codec: NALCodec) -> [Data] {
        var units: [Data] = []
        var current = Data()
        var currentHasPicture = false
        for nal in AnnexB.split(stream) {
            let kind = AnnexB.kind(of: nal, codec: codec)
            if kind == .delimiter, currentHasPicture {
                units.append(current)
                current = Data()
                currentHasPicture = false
            }
            if AnnexB.sliceType(of: nal, codec: codec) != nil { currentHasPicture = true }
            current.append(contentsOf: [0, 0, 0, 1])
            current.append(nal)
        }
        if !current.isEmpty { units.append(current) }
        return units
    }
}

/// Thread-safe collector for VideoToolbox's output thread.
private final class FrameLog: @unchecked Sendable {
    private let lock = NSLock()
    private var storage: [CMTime] = []
    func append(_ time: CMTime) { lock.lock(); storage.append(time); lock.unlock() }
    var times: [CMTime] { lock.lock(); defer { lock.unlock() }; return storage }
    var count: Int { times.count }
}
