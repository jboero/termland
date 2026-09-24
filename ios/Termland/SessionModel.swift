import Foundation
import Combine
import CoreGraphics

/// What the session screen opens: a new session, or an existing one by id.
struct SessionLaunch: Identifiable, Hashable, Codable {
    var id = UUID()
    var profileID: UUID
    /// `nil` creates a new session; otherwise attach (resume) this one.
    var sessionID: String?
}

/// One streaming session: connect/attach, the decoder, input, detach.
///
/// Each model owns its own `TermlandClient`. The core streams one session per
/// client, and a macOS user can have several session windows open at once;
/// the per-client cost is a two-thread tokio runtime.
@MainActor
final class SessionModel: ObservableObject {
    enum State: Equatable {
        /// Waiting for the view to report a size, which the session is created at.
        case idle
        case connecting
        case live(codec: String, width: UInt32, height: UInt32)
        /// The remote session is still running and can be resumed.
        case detached(String)
        case failed(String)
    }

    @Published private(set) var state: State = .idle
    @Published private(set) var hasFirstFrame = false
    @Published private(set) var bytesPerSecond: UInt64 = 0

    let profile: HostProfile
    let decoder = VideoDecoder()
    let router: InputRouter
    /// The session being shown; set from SessionReady, so after a "new session"
    /// connect it is what Resume attaches to.
    private(set) var sessionID: String?

    private let password: String
    private let client = TermlandClient()
    /// Bumped per connect so callbacks from a superseded connection (a late
    /// `onDisconnected` from the loop we just detached) are ignored.
    private var generation = 0
    private var viewSize: CGSize = .zero
    private var resizeTask: Task<Void, Never>?

    init(profile: HostProfile, password: String, sessionID: String?) {
        self.profile = profile
        self.password = password
        self.sessionID = sessionID
        self.router = InputRouter(client: client)
        decoder.onFirstFrame = { [weak self] in self?.hasFirstFrame = true }
        decoder.onFatal = { [weak self] message in self?.fail(message) }
    }

    deinit {
        // Detach, never close: the remote session outlives this screen.
        client.disconnect()
    }

    var isLive: Bool {
        if case .live = state { return true }
        return false
    }

    // MARK: Lifecycle

    /// Called whenever the render view's size changes. The first non-empty size
    /// starts the session; later ones become (debounced) remote resizes.
    func viewSizeChanged(_ size: CGSize) {
        guard size.width >= 1, size.height >= 1 else { return }
        viewSize = size
        router.viewBounds = CGRect(origin: .zero, size: size)
        if state == .idle {
            connect()
            return
        }
        scheduleResize()
    }

    /// Connect (first time) or resume after a detach/failure.
    func connect() {
        guard viewSize.width >= 1 else { return }
        switch state {
        case .connecting, .live: return
        default: break
        }
        generation += 1
        let generation = generation
        state = .connecting
        hasFirstFrame = false
        bytesPerSecond = 0
        decoder.reset()

        let size = Self.remoteSize(for: viewSize)
        let params = profile.sessionParams(width: size.width, height: size.height)
        let coreProfile = profile.coreProfile(password: password)
        let observer = Observer(model: self, decoder: decoder, generation: generation)
        let client = client
        let target = sessionID

        Task.detached {
            do {
                // Both block until SessionReady (or failure); never on main.
                if let target {
                    try client.attach(profile: coreProfile, sessionId: target, params: params, observer: observer)
                } else {
                    try client.connectNew(profile: coreProfile, params: params, observer: observer)
                }
                // The server defaults to a client-drawn cursor, but the mobile
                // core drops CursorUpdate messages; have it composite the cursor
                // into the video instead.
                client.setCursorInFrame(inFrame: true)
            } catch {
                await self.connectFailed(error, generation: generation)
            }
        }
    }

    /// Stop streaming and leave the remote session running (resumable).
    func detach(reason: String = "Disconnected") {
        switch state {
        case .connecting, .live: break
        default: return
        }
        generation += 1
        router.releaseAll()
        client.disconnect()
        resizeTask?.cancel()
        state = .detached(reason)
    }

    // MARK: Observer callbacks (main)

    fileprivate func sessionReady(_ info: SessionReadyInfo, generation: Int) {
        guard generation == self.generation else { return }
        sessionID = info.sessionId
        router.remoteSize = CGSize(width: Int(info.width), height: Int(info.height))
        state = .live(codec: Self.codecName(info.codec), width: info.width, height: info.height)
        // The view may have changed size while we were connecting.
        scheduleResize()
    }

    fileprivate func remoteSizeChanged(width: UInt32, height: UInt32, generation: Int) {
        guard generation == self.generation, width > 0, height > 0 else { return }
        router.remoteSize = CGSize(width: Int(width), height: Int(height))
        if case .live(let codec, _, _) = state { state = .live(codec: codec, width: width, height: height) }
    }

    fileprivate func dataRate(_ value: UInt64, generation: Int) {
        guard generation == self.generation else { return }
        bytesPerSecond = value
    }

    fileprivate func disconnected(_ reason: String, generation: Int) {
        guard generation == self.generation else { return }
        router.releaseAll()
        state = .detached(reason)
    }

    fileprivate func streamError(_ message: String, generation: Int) {
        guard generation == self.generation else { return }
        fail(message)
    }

    private func connectFailed(_ error: Error, generation: Int) {
        guard generation == self.generation else { return }
        fail(TermlandErrorText.describe(error, profile: profile))
    }

    private func fail(_ message: String) {
        generation += 1
        router.releaseAll()
        client.disconnect()
        state = .failed(message)
    }

    // MARK: Resize

    private func scheduleResize() {
        resizeTask?.cancel()
        resizeTask = Task { [weak self] in
            // Window drags and rotations produce a burst of sizes; only the
            // final one is worth a server-side encoder rebuild.
            try? await Task.sleep(for: .milliseconds(500))
            guard !Task.isCancelled, let self, self.isLive else { return }
            let size = Self.remoteSize(for: self.viewSize)
            // Compare against the session's actual size too, not just our last
            // request: an attached session keeps whatever size its previous
            // client gave it (a Mac window, say), so resuming it on a phone
            // must resize even though the view itself never changed.
            let remote = (width: Int(self.router.remoteSize.width), height: Int(self.router.remoteSize.height))
            if remote == size { return }
            self.client.resize(width: UInt32(size.width), height: UInt32(size.height))
        }
    }

    /// Remote framebuffer size for a view of `size` points.
    ///
    /// Points, not pixels: the remote compositor has no HiDPI scaling, so a
    /// Retina-pixel desktop would render its UI at half size and cost four
    /// times the bandwidth. Even dimensions because 4:2:0 encoders require
    /// them; clamped to what the encoders accept.
    static func remoteSize(for size: CGSize) -> (width: Int, height: Int) {
        func even(_ value: CGFloat) -> Int {
            let clamped = min(max(Int(value.rounded(.down)), 320), 3840)
            return clamped & ~1
        }
        return (even(size.width), even(size.height))
    }

    static func codecName(_ codec: MobileCodec) -> String {
        switch codec {
        case .h264: return "H.264"
        case .h265: return "HEVC"
        case .av1: return "AV1"
        case .vp8: return "VP8"
        case .vp9: return "VP9"
        }
    }
}

/// UniFFI callback object. Called on a Rust worker thread that must not
/// block: video goes straight to the decoder's queue; everything else hops to
/// the main actor.
private final class Observer: SessionObserver, @unchecked Sendable {
    private weak var model: SessionModel?
    private let decoder: VideoDecoder
    private let generation: Int
    private let lock = NSLock()
    private var lastWidth: UInt32 = 0
    private var lastHeight: UInt32 = 0

    init(model: SessionModel, decoder: VideoDecoder, generation: Int) {
        self.model = model
        self.decoder = decoder
        self.generation = generation
    }

    func onSessionReady(info: SessionReadyInfo) {
        lock.lock(); lastWidth = info.width; lastHeight = info.height; lock.unlock()
        let generation = generation
        Task { @MainActor [weak model] in model?.sessionReady(info, generation: generation) }
    }

    func onVideoPacket(packet: VideoPacket) {
        decoder.submit(packet)
        lock.lock()
        let changed = packet.width > 0 && packet.height > 0 && (packet.width != lastWidth || packet.height != lastHeight)
        if changed { lastWidth = packet.width; lastHeight = packet.height }
        lock.unlock()
        if changed {
            let (width, height, generation) = (packet.width, packet.height, generation)
            Task { @MainActor [weak model] in model?.remoteSizeChanged(width: width, height: height, generation: generation) }
        }
    }

    // Audio is M3e (libopus → AVAudioEngine); the session never asks for it.
    func onAudioPacket(data: Data) {}

    // Clipboard sync is M3e.
    func onClipboard(mimeType: String, data: Data) {}

    func onDataRate(bytesPerSec: UInt64) {
        let generation = generation
        Task { @MainActor [weak model] in model?.dataRate(bytesPerSec, generation: generation) }
    }

    func onDisconnected(reason: String) {
        let generation = generation
        Task { @MainActor [weak model] in model?.disconnected(reason, generation: generation) }
    }

    func onError(message: String) {
        let generation = generation
        Task { @MainActor [weak model] in model?.streamError(message, generation: generation) }
    }
}

/// Human-readable core errors, with a hint for the setup mistakes that are
/// easy to make and hard to diagnose from the raw message.
enum TermlandErrorText {
    static func describe(_ error: Error, profile: HostProfile) -> String {
        guard let error = error as? TermlandError else { return error.localizedDescription }
        switch error {
        case .Connect(let msg): return "Couldn’t connect: \(msg)"
        case .Auth(let msg): return "Authentication failed: \(msg)"
        case .Protocol(let msg): return "Protocol error: \(msg)"
        case .Io(let msg): return "Connection error: \(msg)"
        case .Tls(let msg):
            var text = "TLS error: \(msg)"
            if msg.contains("UnknownIssuer") || msg.contains("invalid peer certificate") {
                text += "\n\nThe server’s certificate isn’t trusted. Termland servers use a self-signed certificate by default; enable “Accept invalid certificate” for a server you trust."
            } else if msg.contains("handshake eof") || msg.contains("close_notify") {
                text += "\n\nThe server closed the connection during the TLS handshake. It may be running without --tls; turn off “Use TLS” for this profile."
            }
            return text
        }
    }
}
