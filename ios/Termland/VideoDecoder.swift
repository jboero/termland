import AVFoundation
import CoreMedia
import VideoToolbox

/// Encoded packets in, frames on an `AVSampleBufferDisplayLayer` out.
///
/// Why decode with `VTDecompressionSession` instead of enqueueing compressed
/// samples on the display layer directly: the server's encoders are not all
/// B-frame free (libx264/libx265 at their default presets emit B-frames), and
/// the wire carries only a presentation timestamp. Temporal processing makes
/// VideoToolbox hand frames back in *presentation* order, and each decoded
/// frame is then shown immediately — no display-clock scheduling, which is the
/// buffering a remote desktop must not have. This mirrors what Android's
/// MediaCodec path does (reorder inside the decoder, render on output).
///
/// Threading: `submit` is called from a Rust worker thread that must never
/// block (the UniFFI contract on `SessionObserver`), so it only enqueues onto
/// a private serial queue. All decoder state below is confined to that queue.
final class VideoDecoder: @unchecked Sendable {
    let displayLayer = AVSampleBufferDisplayLayer()

    /// Main-thread callbacks.
    var onFirstFrame: (() -> Void)?
    var onFatal: ((String) -> Void)?
    /// Test seam: every displayed frame's presentation time, on VideoToolbox's
    /// output thread. Set before the first `submit`.
    var onDecodedFrame: ((CMTime) -> Void)?

    private let queue = DispatchQueue(label: "dev.termland.video-decode", qos: .userInteractive)

    // Backpressure, shared between the submitting thread and `queue`.
    private let pendingLock = NSLock()
    private var pending = 0
    private var overflowed = false
    /// Packets allowed to wait for the decoder before we start dropping. Past
    /// this, latency is growing without bound, and dropping until the next
    /// keyframe is the only way back to "live" (there is no keyframe request
    /// in the protocol, so we wait for the encoder's GOP).
    private let maxPending = 8

    // --- queue-confined ---
    private var session: VTDecompressionSession?
    private var formatDescription: CMVideoFormatDescription?
    private var parameterSets: [Data] = []
    private var codec: NALCodec?
    /// A decoder fed a delta frame first emits garbage (or errors) until the
    /// next IDR, so everything is dropped until a keyframe arrives. Re-armed on
    /// every reconfigure, decode error and overflow.
    private var awaitingKeyframe = true
    /// True right after resyncing at an HEVC CRA/BLA, until the first picture
    /// that is not a RASL leading picture.
    private var skippingLeadingPictures = false
    private var released = false
    private var reportedUnsupported = false

    // Set from VideoToolbox's output thread.
    private let firstFrameLock = NSLock()
    private var sawFirstFrame = false

    init() {
        displayLayer.videoGravity = .resizeAspect
        displayLayer.backgroundColor = CGColor(gray: 0, alpha: 1)
    }

    deinit { teardownSession() }

    /// Feed one packet. Safe from any thread; never blocks.
    func submit(_ packet: VideoPacket) {
        pendingLock.lock()
        if pending >= maxPending {
            overflowed = true
            pendingLock.unlock()
            return
        }
        pending += 1
        pendingLock.unlock()

        queue.async { [weak self] in
            guard let self else { return }
            self.pendingLock.lock()
            self.pending -= 1
            if self.overflowed {
                self.overflowed = false
                self.awaitingKeyframe = true
            }
            self.pendingLock.unlock()
            self.decode(packet)
        }
    }

    /// Prepare for a new connection: drop until its first keyframe, and report
    /// the next decoded frame as a first frame again. The session and last
    /// picture are kept, so a resume does not flash black.
    func reset() {
        firstFrameLock.lock()
        sawFirstFrame = false
        firstFrameLock.unlock()
        queue.async { [weak self] in self?.awaitingKeyframe = true }
    }

    /// Block until every submitted packet has been decoded and any frames held
    /// back for reordering have been emitted. For tests and end-of-stream only;
    /// a live stream never drains, it just keeps going.
    func drain() {
        queue.sync {
            guard let session else { return }
            VTDecompressionSessionFinishDelayedFrames(session)
            VTDecompressionSessionWaitForAsynchronousFrames(session)
        }
    }

    /// Stop decoding and blank the layer. Idempotent.
    func release() {
        queue.async { [weak self] in
            guard let self else { return }
            self.released = true
            self.teardownSession()
        }
        displayLayer.sampleBufferRenderer.flush(removingDisplayedImage: true, completionHandler: nil)
    }

    // MARK: - Decode (on `queue`)

    private func decode(_ packet: VideoPacket) {
        guard !released else { return }
        guard let nalCodec = CodecSupport.nalCodec(for: packet.codec) else {
            // Only reachable if the server ignored our codec list.
            if !reportedUnsupported {
                reportedUnsupported = true
                fail("Server sent \(packet.codec) video, which this device cannot decode.")
            }
            return
        }
        guard let unit = AnnexB.convert(packet.data, codec: nalCodec) else { return }

        if nalCodec != codec {
            // Mid-stream encoder switch (e.g. after a resize): start over.
            teardownSession()
            codec = nalCodec
            parameterSets = []
            awaitingKeyframe = true
        }

        if !unit.parameterSets.isEmpty,
           AnnexB.isComplete(unit.parameterSets, codec: nalCodec),
           unit.parameterSets != parameterSets {
            configure(parameterSets: unit.parameterSets, codec: nalCodec)
        }

        // A packet of parameter sets alone configures the decoder but is not a
        // picture: it must not count as "the keyframe arrived", or the next
        // delta frame would be decoded without its reference.
        guard !unit.sampleData.isEmpty else { return }

        if awaitingKeyframe {
            guard packet.keyframe, session != nil else { return }
            awaitingKeyframe = false
            // Starting at an HEVC CRA (libx265's default open-GOP keyframe):
            // the RASL pictures right after it need the previous GOP.
            skippingLeadingPictures = AnnexB.isCleanRandomAccess(unit.pictureType, codec: nalCodec)
        } else if skippingLeadingPictures {
            if unit.isSkippableLeadingPicture { return }
            skippingLeadingPictures = false
        }

        guard let session, let formatDescription,
              let sample = makeSampleBuffer(unit.sampleData, format: formatDescription, timestampUs: packet.timestampUs)
        else { return }

        let flags: VTDecodeFrameFlags = [._EnableAsynchronousDecompression, ._EnableTemporalProcessing]
        let status = VTDecompressionSessionDecodeFrame(
            session, sampleBuffer: sample, flags: flags, infoFlagsOut: nil
        ) { [weak self] status, infoFlags, imageBuffer, presentationTime, _ in
            self?.didDecode(status: status, infoFlags: infoFlags, imageBuffer: imageBuffer, pts: presentationTime)
        }
        if status != noErr {
            if status == kVTInvalidSessionErr {
                // iOS invalidates hardware sessions when the app is backgrounded.
                // Rebuild from the next keyframe's parameter sets.
                teardownSession()
                parameterSets = []
            }
            awaitingKeyframe = true
        }
    }

    private func configure(parameterSets sets: [Data], codec: NALCodec) {
        guard let description = Self.makeFormatDescription(sets, codec: codec) else {
            awaitingKeyframe = true
            return
        }
        parameterSets = sets
        // Same stream shape (e.g. only the PPS changed): keep the session.
        if let session, VTDecompressionSessionCanAcceptFormatDescription(session, formatDescription: description) {
            formatDescription = description
            return
        }
        teardownSession()
        formatDescription = description
        awaitingKeyframe = true

        var specification: [CFString: Any] = [:]
        #if os(macOS)
        specification[kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder] = true
        #endif
        // IOSurface-backed output is what AVSampleBufferDisplayLayer composites
        // without a copy.
        let attributes: [CFString: Any] = [kCVPixelBufferIOSurfacePropertiesKey: [CFString: Any]()]

        var created: VTDecompressionSession?
        let status = VTDecompressionSessionCreate(
            allocator: kCFAllocatorDefault,
            formatDescription: description,
            decoderSpecification: specification as CFDictionary,
            imageBufferAttributes: attributes as CFDictionary,
            outputCallback: nil,
            decompressionSessionOut: &created
        )
        guard status == noErr, let created else {
            fail("Could not start the \(codec == .hevc ? "HEVC" : "H.264") decoder (VideoToolbox error \(status)).")
            return
        }
        VTSessionSetProperty(created, key: kVTDecompressionPropertyKey_RealTime, value: kCFBooleanTrue)
        session = created
    }

    private func teardownSession() {
        if let session {
            VTDecompressionSessionInvalidate(session)
        }
        session = nil
        formatDescription = nil
    }

    // MARK: - Output (VideoToolbox's thread)

    private func didDecode(status: OSStatus, infoFlags: VTDecodeInfoFlags, imageBuffer: CVImageBuffer?, pts: CMTime) {
        guard status == noErr, let imageBuffer, !infoFlags.contains(.frameDropped) else {
            if status != noErr {
                // Corrupt reference chain: drop until the next keyframe.
                queue.async { [weak self] in self?.awaitingKeyframe = true }
            }
            return
        }
        guard let sample = Self.makeDisplaySample(imageBuffer, pts: pts) else { return }

        let renderer = displayLayer.sampleBufferRenderer
        if renderer.status == .failed { renderer.flush() }
        renderer.enqueue(sample)
        onDecodedFrame?(pts)

        firstFrameLock.lock()
        let first = !sawFirstFrame
        sawFirstFrame = true
        firstFrameLock.unlock()
        if first {
            DispatchQueue.main.async { [weak self] in self?.onFirstFrame?() }
        }
    }

    private func fail(_ message: String) {
        DispatchQueue.main.async { [weak self] in self?.onFatal?(message) }
    }

    // MARK: - CoreMedia plumbing

    static func makeFormatDescription(_ sets: [Data], codec: NALCodec) -> CMVideoFormatDescription? {
        // The parameter-set pointers must stay valid for the duration of the
        // call; copy each into its own allocation rather than juggling nested
        // withUnsafeBytes closures for a variable count.
        let copies = sets.map { set -> UnsafeMutablePointer<UInt8> in
            let pointer = UnsafeMutablePointer<UInt8>.allocate(capacity: set.count)
            set.copyBytes(to: pointer, count: set.count)
            return pointer
        }
        defer { copies.forEach { $0.deallocate() } }
        let pointers = copies.map { UnsafePointer($0) }
        let sizes = sets.map(\.count)

        var description: CMFormatDescription?
        let status: OSStatus
        switch codec {
        case .h264:
            status = CMVideoFormatDescriptionCreateFromH264ParameterSets(
                allocator: kCFAllocatorDefault,
                parameterSetCount: sets.count,
                parameterSetPointers: pointers,
                parameterSetSizes: sizes,
                nalUnitHeaderLength: 4,
                formatDescriptionOut: &description
            )
        case .hevc:
            status = CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                allocator: kCFAllocatorDefault,
                parameterSetCount: sets.count,
                parameterSetPointers: pointers,
                parameterSetSizes: sizes,
                nalUnitHeaderLength: 4,
                extensions: nil,
                formatDescriptionOut: &description
            )
        }
        return status == noErr ? description : nil
    }

    private func makeSampleBuffer(_ data: Data, format: CMVideoFormatDescription, timestampUs: UInt64) -> CMSampleBuffer? {
        var block: CMBlockBuffer?
        guard CMBlockBufferCreateWithMemoryBlock(
            allocator: kCFAllocatorDefault,
            memoryBlock: nil,
            blockLength: data.count,
            blockAllocator: kCFAllocatorDefault,
            customBlockSource: nil,
            offsetToData: 0,
            dataLength: data.count,
            flags: kCMBlockBufferAssureMemoryNowFlag,
            blockBufferOut: &block
        ) == kCMBlockBufferNoErr, let block else { return nil }

        let copied = data.withUnsafeBytes { raw in
            CMBlockBufferReplaceDataBytes(with: raw.baseAddress!, blockBuffer: block, offsetIntoDestination: 0, dataLength: data.count)
        }
        guard copied == kCMBlockBufferNoErr else { return nil }

        var timing = CMSampleTimingInfo(
            duration: .invalid,
            presentationTimeStamp: CMTime(value: CMTimeValue(timestampUs), timescale: 1_000_000),
            decodeTimeStamp: .invalid
        )
        var size = data.count
        var sample: CMSampleBuffer?
        guard CMSampleBufferCreateReady(
            allocator: kCFAllocatorDefault,
            dataBuffer: block,
            formatDescription: format,
            sampleCount: 1,
            sampleTimingEntryCount: 1,
            sampleTimingArray: &timing,
            sampleSizeEntryCount: 1,
            sampleSizeArray: &size,
            sampleBufferOut: &sample
        ) == noErr else { return nil }
        return sample
    }

    private static func makeDisplaySample(_ imageBuffer: CVImageBuffer, pts: CMTime) -> CMSampleBuffer? {
        var format: CMVideoFormatDescription?
        guard CMVideoFormatDescriptionCreateForImageBuffer(
            allocator: kCFAllocatorDefault, imageBuffer: imageBuffer, formatDescriptionOut: &format
        ) == noErr, let format else { return nil }

        var timing = CMSampleTimingInfo(duration: .invalid, presentationTimeStamp: pts, decodeTimeStamp: .invalid)
        var sample: CMSampleBuffer?
        guard CMSampleBufferCreateReadyWithImageBuffer(
            allocator: kCFAllocatorDefault,
            imageBuffer: imageBuffer,
            formatDescription: format,
            sampleTiming: &timing,
            sampleBufferOut: &sample
        ) == noErr, let sample else { return nil }

        // Show it now: the layer has no control timebase, and a remote desktop
        // wants the newest frame on screen, not one scheduled against a clock.
        if let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: true),
           CFArrayGetCount(attachments) > 0 {
            let dictionary = unsafeBitCast(CFArrayGetValueAtIndex(attachments, 0), to: CFMutableDictionary.self)
            CFDictionarySetValue(
                dictionary,
                Unmanaged.passUnretained(kCMSampleAttachmentKey_DisplayImmediately).toOpaque(),
                Unmanaged.passUnretained(kCFBooleanTrue).toOpaque()
            )
        }
        return sample
    }
}
