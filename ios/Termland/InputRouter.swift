import AVFoundation
import Combine
import CoreGraphics

/// Turns platform input into core calls. Main-thread only.
///
/// The `TermlandClient.send*` methods are fire-and-forget and never block, so
/// events are forwarded inline — a queue here would only add latency.
///
/// The router owns what the platform will not give us:
/// - the xkb modifier mask, tracked from the key stream (and the iOS on-screen
///   sticky modifiers, which no platform event knows about);
/// - which keys and buttons are down, so a detach can release them instead of
///   leaving Ctrl stuck on the remote desktop;
/// - the view → remote-framebuffer mapping, which has to account for the
///   letterboxing `resizeAspect` introduces.
@MainActor
final class InputRouter: ObservableObject {
    private let client: TermlandClient

    /// Remote framebuffer size in pixels (from SessionReady / packet headers).
    var remoteSize: CGSize = .zero
    /// Bounds of the view the video is drawn in, in view points.
    var viewBounds: CGRect = .zero

    private var heldModifiers: UInt32 = 0
    /// Published so the on-screen latch buttons follow it.
    @Published private(set) var stickyModifiers: UInt32 = 0
    private var heldKeys: Set<UInt32> = []
    private var heldButtons: Set<UInt32> = []

    init(client: TermlandClient) {
        self.client = client
    }

    var modifiers: UInt32 { heldModifiers | stickyModifiers }

    // MARK: Keyboard

    func key(_ scancode: UInt32, pressed: Bool) {
        // A release for a key we never pressed (it went down before this view
        // had focus, or AppKit already had us release it early) is dropped.
        if !pressed, !heldKeys.contains(scancode) { return }
        let bit = KeyMap.modifierBit(forScancode: scancode)
        // Include a modifier in its own press, exclude it from its own
        // release — the order xkb expects.
        if bit != 0, pressed { heldModifiers |= bit }
        if pressed { heldKeys.insert(scancode) } else { heldKeys.remove(scancode) }

        client.sendKey(scancode: scancode, keysym: 0, pressed: pressed, modifiers: modifiers)

        if bit != 0, !pressed { heldModifiers &= ~bit }
        // An ordinary key consumes latched on-screen modifiers, like sticky keys.
        if bit == 0, !pressed { releaseStickyModifiers() }
    }

    func tap(_ scancode: UInt32) {
        key(scancode, pressed: true)
        key(scancode, pressed: false)
    }

    /// Latch/unlatch an on-screen modifier. The real key press is sent now, so
    /// remote shortcut handling sees a genuinely held key.
    func setSticky(_ scancode: UInt32, latched: Bool) {
        let bit = KeyMap.modifierBit(forScancode: scancode)
        guard bit != 0 else { return }
        if latched {
            stickyModifiers |= bit
            client.sendKey(scancode: scancode, keysym: 0, pressed: true, modifiers: modifiers)
        } else {
            stickyModifiers &= ~bit
            client.sendKey(scancode: scancode, keysym: 0, pressed: false, modifiers: modifiers)
        }
    }

    func isHeld(_ scancode: UInt32) -> Bool { heldKeys.contains(scancode) }

    func isSticky(_ scancode: UInt32) -> Bool {
        stickyModifiers & KeyMap.modifierBit(forScancode: scancode) != 0
    }

    func releaseStickyModifiers() {
        guard stickyModifiers != 0 else { return }
        for scancode in [KeyMap.keyLeftCtrl, KeyMap.keyLeftAlt, KeyMap.keyLeftShift, KeyMap.keyLeftMeta]
        where isSticky(scancode) {
            setSticky(scancode, latched: false)
        }
    }

    /// Committed text from a soft keyboard / IME. Newline and tab are sent as
    /// real keys: terminals and forms treat them as keys, not characters.
    func text(_ string: String) {
        var buffer = ""
        func flush() {
            if !buffer.isEmpty { client.sendText(text: buffer); buffer = "" }
        }
        for character in string {
            switch character {
            case "\n", "\r": flush(); tap(KeyMap.keyEnter)
            case "\t": flush(); tap(KeyMap.keyTab)
            default: buffer.append(character)
            }
        }
        flush()
        releaseStickyModifiers()
    }

    // MARK: Pointer

    /// Where the video actually sits inside the view (aspect-fit letterbox).
    var videoRect: CGRect {
        guard remoteSize.width > 0, remoteSize.height > 0, !viewBounds.isEmpty else { return viewBounds }
        return AVMakeRect(aspectRatio: remoteSize, insideRect: viewBounds)
    }

    /// View point → remote pixel, clamped to the framebuffer.
    func remotePoint(_ point: CGPoint) -> CGPoint {
        let rect = videoRect
        guard rect.width > 0, rect.height > 0, remoteSize.width > 0, remoteSize.height > 0 else { return point }
        let x = (point.x - rect.minX) * remoteSize.width / rect.width
        let y = (point.y - rect.minY) * remoteSize.height / rect.height
        return CGPoint(
            x: min(max(x, 0), remoteSize.width - 1),
            y: min(max(y, 0), remoteSize.height - 1)
        )
    }

    func move(to point: CGPoint) {
        let remote = remotePoint(point)
        client.sendPointerMotion(x: remote.x, y: remote.y, absolute: true)
    }

    func button(_ button: UInt32, pressed: Bool) {
        if pressed { heldButtons.insert(button) } else { heldButtons.remove(button) }
        client.sendPointerButton(button: button, pressed: pressed)
    }

    func click(_ button: UInt32, at point: CGPoint) {
        move(to: point)
        self.button(button, pressed: true)
        self.button(button, pressed: false)
    }

    /// Scroll in remote pixels, Wayland convention (positive = down/right).
    func scroll(dx: Double, dy: Double) {
        guard dx != 0 || dy != 0 else { return }
        client.sendScroll(dx: dx, dy: dy)
    }

    /// Release everything still held. Call before detaching, and whenever the
    /// view loses focus: the remote otherwise sees keys held forever.
    func releaseAll() {
        releaseStickyModifiers()
        for button in heldButtons { client.sendPointerButton(button: button, pressed: false) }
        heldButtons.removeAll()
        for scancode in heldKeys { client.sendKey(scancode: scancode, keysym: 0, pressed: false, modifiers: 0) }
        heldKeys.removeAll()
        heldModifiers = 0
    }
}
