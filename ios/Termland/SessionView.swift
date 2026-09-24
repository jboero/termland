import SwiftUI

/// Looks the profile up and owns the model for one session window/cover.
struct SessionContainer: View {
    @ObservedObject var home: HomeModel
    let launch: SessionLaunch
    var onClose: () -> Void = {}

    var body: some View {
        if let profile = home.profiles.first(where: { $0.id == launch.profileID }) {
            SessionScreen(
                profile: profile,
                password: home.password(for: profile),
                sessionID: launch.sessionID,
                onClose: onClose,
                onEnded: { home.refreshSessions(for: profile) }
            )
        } else {
            ContentUnavailableView("Server profile not found", systemImage: "questionmark.circle",
                                   description: Text("It may have been deleted."))
        }
    }
}

private struct SessionScreen: View {
    @StateObject private var model: SessionModel
    @State private var keyboardVisible = false
    @State private var controlsExpanded = true
    @Environment(\.scenePhase) private var scenePhase
    let onClose: () -> Void
    let onEnded: () -> Void

    init(profile: HostProfile, password: String, sessionID: String?, onClose: @escaping () -> Void, onEnded: @escaping () -> Void) {
        _model = StateObject(wrappedValue: SessionModel(profile: profile, password: password, sessionID: sessionID))
        self.onClose = onClose
        self.onEnded = onEnded
    }

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()
            RemoteVideoView(model: model, keyboardVisible: $keyboardVisible)
                .ignoresSafeArea(.container)
            statusOverlay
        }
        #if os(iOS)
        .overlay(alignment: .top) { controlBar.padding(.top, 4) }
        .statusBarHidden()
        .persistentSystemOverlays(.hidden)
        .onChange(of: scenePhase) { _, phase in
            // Detach, never close, when the app leaves the foreground: iOS
            // suspends the socket anyway, and Resume reattaches in one tap.
            if phase == .background { model.detach(reason: "The app went to the background.") }
        }
        #else
        .navigationTitle(title)
        .toolbar {
            ToolbarItemGroup(placement: .primaryAction) {
                if model.isLive {
                    Text(rateText).monospacedDigit().foregroundStyle(.secondary)
                    Button { model.detach() } label: { Label("Disconnect", systemImage: "eject") }
                        .help("Detach; the session keeps running on the server")
                }
            }
        }
        #endif
        .onDisappear {
            model.detach(reason: "Closed")
            model.decoder.release()
            onEnded()
        }
    }

    private var title: String {
        switch model.state {
        case .live(let codec, let width, let height):
            return "\(model.profile.displayName) — \(width)×\(height) \(codec)"
        default:
            return model.profile.displayName
        }
    }

    private var rateText: String {
        let kbps = Double(model.bytesPerSecond) * 8 / 1000
        return kbps >= 1000 ? String(format: "%.1f Mbps", kbps / 1000) : String(format: "%.0f kbps", kbps)
    }

    @ViewBuilder private var statusOverlay: some View {
        switch model.state {
        case .idle, .connecting:
            status {
                ProgressView().controlSize(.large)
                Text(model.sessionID == nil ? "Starting a new session…" : "Resuming session…")
            }
        case .live:
            if !model.hasFirstFrame {
                status {
                    ProgressView().controlSize(.large)
                    Text("Waiting for the first frame…")
                }
            }
        case .detached(let reason):
            status {
                Image(systemName: "pause.circle").font(.system(size: 44))
                Text("Detached").font(.title2.bold())
                Text(reason).foregroundStyle(.secondary).multilineTextAlignment(.center)
                Text("The session is still running on the server.").font(.footnote).foregroundStyle(.secondary)
                HStack {
                    Button("Close", action: onClose)
                    Button("Resume") { model.connect() }.buttonStyle(.borderedProminent)
                }
            }
        case .failed(let message):
            status {
                Image(systemName: "exclamationmark.triangle").font(.system(size: 44))
                Text("Couldn’t stream").font(.title2.bold())
                Text(message).foregroundStyle(.secondary).multilineTextAlignment(.center)
                HStack {
                    Button("Close", action: onClose)
                    Button("Retry") { model.connect() }.buttonStyle(.borderedProminent)
                }
            }
        }
    }

    private func status<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
        VStack(spacing: 12, content: content)
            .padding(24)
            .frame(maxWidth: 440)
            .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 16))
            .padding()
    }

    #if os(iOS)
    /// Compact, collapsible bar: the keys a touch keyboard lacks, plus
    /// keyboard toggle and detach.
    private var controlBar: some View {
        HStack(spacing: 6) {
            if controlsExpanded {
                barButton("chevron.left", "Disconnect") {
                    model.detach()
                    onClose()
                }
                if model.isLive {
                    barButton(keyboardVisible ? "keyboard.chevron.compact.down" : "keyboard", "Keyboard") {
                        keyboardVisible.toggle()
                    }
                    keyButton("Esc", KeyMap.keyEsc)
                    keyButton("Tab", KeyMap.keyTab)
                    StickyKeyButton(title: "Ctrl", scancode: KeyMap.keyLeftCtrl, router: model.router)
                    StickyKeyButton(title: "Alt", scancode: KeyMap.keyLeftAlt, router: model.router)
                    StickyKeyButton(title: "Super", scancode: KeyMap.keyLeftMeta, router: model.router)
                    keyButton("←", KeyMap.keyLeft)
                    keyButton("↑", KeyMap.keyUp)
                    keyButton("↓", KeyMap.keyDown)
                    keyButton("→", KeyMap.keyRight)
                    Text(rateText).font(.caption.monospacedDigit()).foregroundStyle(.secondary)
                }
            }
            barButton(controlsExpanded ? "chevron.up" : "chevron.down", controlsExpanded ? "Hide controls" : "Show controls") {
                withAnimation(.snappy) { controlsExpanded.toggle() }
            }
        }
        .padding(6)
        .background(.ultraThinMaterial, in: Capsule())
    }

    private func barButton(_ systemImage: String, _ label: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: systemImage).frame(width: 30, height: 30)
        }
        .accessibilityLabel(label)
        .buttonStyle(.plain)
    }

    private func keyButton(_ title: String, _ scancode: UInt32) -> some View {
        Button(title) { model.router.tap(scancode) }
            .font(.callout.monospaced())
            .frame(minWidth: 30, minHeight: 30)
            .buttonStyle(.plain)
    }
    #endif
}

#if os(iOS)
/// A latching modifier: tap to hold, released by the next ordinary key.
private struct StickyKeyButton: View {
    let title: String
    let scancode: UInt32
    @ObservedObject var router: InputRouter

    var body: some View {
        let latched = router.stickyModifiers & KeyMap.modifierBit(forScancode: scancode) != 0
        Button(title) { router.setSticky(scancode, latched: !latched) }
            .font(.callout.monospaced())
            .frame(minWidth: 36, minHeight: 30)
            .background(latched ? Color.accentColor.opacity(0.35) : .clear, in: RoundedRectangle(cornerRadius: 6))
            .buttonStyle(.plain)
    }
}
#endif
