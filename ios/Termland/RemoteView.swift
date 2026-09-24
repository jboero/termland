import SwiftUI
import AVFoundation

/// Hosts the decoder's `AVSampleBufferDisplayLayer` and turns local input into
/// remote input. One implementation per platform; SwiftUI sees one view.
struct RemoteVideoView {
    let model: SessionModel
    /// iOS only: whether the soft keyboard is up.
    @Binding var keyboardVisible: Bool
}

// MARK: - macOS

#if os(macOS)
import AppKit

extension RemoteVideoView: NSViewRepresentable {
    func makeNSView(context: Context) -> RemoteNSView {
        RemoteNSView(model: model)
    }

    func updateNSView(_ view: RemoteNSView, context: Context) {}

    static func dismantleNSView(_ view: RemoteNSView, coordinator: ()) {
        view.model.router.releaseAll()
    }
}

/// Mouse and keyboard for a desktop session.
///
/// Key mapping is literal: Control→Ctrl, Option→Alt, Command→Super. The remote
/// is a Linux desktop, so its shortcuts are Ctrl-based; Command-Q and
/// Command-W still reach the local menu (quit, close window) so there is
/// always a way out.
final class RemoteNSView: NSView {
    let model: SessionModel
    private var router: InputRouter { model.router }
    private var trackingArea: NSTrackingArea?

    /// The server composites the cursor into the video (cursor-in-frame), so
    /// the local arrow is hidden over the view to avoid drawing two.
    private static let hiddenCursor = NSCursor(image: NSImage(size: NSSize(width: 1, height: 1)), hotSpot: .zero)

    init(model: SessionModel) {
        self.model = model
        super.init(frame: .zero)
        wantsLayer = true
        layer = CALayer()
        layer?.backgroundColor = NSColor.black.cgColor
        layer?.addSublayer(model.decoder.displayLayer)
        NotificationCenter.default.addObserver(
            self, selector: #selector(windowResignedKey), name: NSWindow.didResignKeyNotification, object: nil
        )
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    // Top-left origin, like the remote framebuffer.
    override var isFlipped: Bool { true }
    override var acceptsFirstResponder: Bool { true }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        window?.makeFirstResponder(self)
        window?.acceptsMouseMovedEvents = true
    }

    override func layout() {
        super.layout()
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        model.decoder.displayLayer.frame = bounds
        CATransaction.commit()
        model.viewSizeChanged(bounds.size)
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let trackingArea { removeTrackingArea(trackingArea) }
        let area = NSTrackingArea(
            rect: .zero,
            options: [.mouseMoved, .activeInKeyWindow, .inVisibleRect, .cursorUpdate],
            owner: self
        )
        addTrackingArea(area)
        trackingArea = area
    }

    override func cursorUpdate(with event: NSEvent) {
        if model.isLive { Self.hiddenCursor.set() } else { NSCursor.arrow.set() }
    }

    @objc private func windowResignedKey(_ note: Notification) {
        // Keys held while focus leaves never get their keyUp here.
        if (note.object as? NSWindow) === window { router.releaseAll() }
    }

    // MARK: Keyboard

    override func keyDown(with event: NSEvent) {
        // The remote compositor generates its own key repeat.
        guard !event.isARepeat, let scancode = KeyMap.scancode(macKeyCode: event.keyCode) else { return }
        router.key(scancode, pressed: true)
        if event.modifierFlags.contains(.command) {
            // AppKit never delivers keyUp for a key pressed while Command is
            // down; release now or it sticks on the remote.
            router.key(scancode, pressed: false)
        }
    }

    override func keyUp(with event: NSEvent) {
        guard let scancode = KeyMap.scancode(macKeyCode: event.keyCode) else { return }
        router.key(scancode, pressed: false)
    }

    /// Modifier keys arrive here, one event per press or release.
    override func flagsChanged(with event: NSEvent) {
        guard let scancode = KeyMap.scancode(macKeyCode: event.keyCode) else { return }
        if scancode == KeyMap.keyCapsLock {
            // Caps Lock reports its toggled state, not the key; the remote
            // keeps its own lock state, so each change is one press.
            router.tap(scancode)
            return
        }
        guard let flag = Self.flag(forScancode: scancode) else { return }
        // Pressed when the family flag is set and this particular key is not
        // already down (the other Shift may still be holding the flag).
        let pressed = event.modifierFlags.contains(flag) && !router.isHeld(scancode)
        router.key(scancode, pressed: pressed)
    }

    private static func flag(forScancode scancode: UInt32) -> NSEvent.ModifierFlags? {
        switch scancode {
        case KeyMap.keyLeftShift, KeyMap.keyRightShift: return .shift
        case KeyMap.keyLeftCtrl, KeyMap.keyRightCtrl: return .control
        case KeyMap.keyLeftAlt, KeyMap.keyRightAlt: return .option
        case KeyMap.keyLeftMeta, KeyMap.keyRightMeta: return .command
        default: return nil
        }
    }

    // MARK: Pointer

    private func location(_ event: NSEvent) -> CGPoint {
        convert(event.locationInWindow, from: nil)
    }

    override func mouseMoved(with event: NSEvent) { router.move(to: location(event)) }
    override func mouseDragged(with event: NSEvent) { router.move(to: location(event)) }
    override func rightMouseDragged(with event: NSEvent) { router.move(to: location(event)) }
    override func otherMouseDragged(with event: NSEvent) { router.move(to: location(event)) }

    override func mouseDown(with event: NSEvent) {
        window?.makeFirstResponder(self)
        press(KeyMap.buttonLeft, event, pressed: true)
    }
    override func mouseUp(with event: NSEvent) { press(KeyMap.buttonLeft, event, pressed: false) }
    override func rightMouseDown(with event: NSEvent) { press(KeyMap.buttonRight, event, pressed: true) }
    override func rightMouseUp(with event: NSEvent) { press(KeyMap.buttonRight, event, pressed: false) }
    override func otherMouseDown(with event: NSEvent) {
        if let button = Self.evdevButton(event.buttonNumber) { press(button, event, pressed: true) }
    }
    override func otherMouseUp(with event: NSEvent) {
        if let button = Self.evdevButton(event.buttonNumber) { press(button, event, pressed: false) }
    }

    private func press(_ button: UInt32, _ event: NSEvent, pressed: Bool) {
        router.move(to: location(event))
        router.button(button, pressed: pressed)
    }

    /// AppKit button numbers: 2 middle, 3 back, 4 forward.
    private static func evdevButton(_ number: Int) -> UInt32? {
        switch number {
        case 2: return KeyMap.buttonMiddle
        case 3: return 0x113 // BTN_SIDE
        case 4: return 0x114 // BTN_EXTRA
        default: return nil
        }
    }

    override func scrollWheel(with event: NSEvent) {
        // AppKit's deltas are "content moves by", already honouring the user's
        // natural-scrolling setting; Wayland's axis is positive = down. Hence
        // the negation, as the desktop client does for winit.
        let scale: CGFloat = event.hasPreciseScrollingDeltas ? 1 : 15
        router.scroll(dx: Double(-event.scrollingDeltaX * scale), dy: Double(-event.scrollingDeltaY * scale))
    }
}
#endif

// MARK: - iOS / iPadOS

#if os(iOS)
import UIKit

extension RemoteVideoView: UIViewRepresentable {
    func makeUIView(context: Context) -> RemoteUIView {
        let view = RemoteUIView(model: model)
        view.onKeyboardVisibilityChange = { visible in
            if keyboardVisible != visible { keyboardVisible = visible }
        }
        return view
    }

    func updateUIView(_ view: RemoteUIView, context: Context) {
        view.setKeyboardVisible(keyboardVisible)
    }

    static func dismantleUIView(_ view: RemoteUIView, coordinator: ()) {
        view.model.router.releaseAll()
    }
}

/// Touch, iPad pointer and hardware keyboard.
///
/// Direct touch (mirrors Android's TouchTranslator and the desktop client's
/// touchscreen handling):
///   tap → left click · long-press → right click · drag → left drag ·
///   two-finger drag → scroll.
/// iPad trackpad/mouse: hover moves the pointer, clicks and drags map to
/// buttons, secondary click is a right click, and two-finger trackpad scroll
/// scrolls.
final class RemoteUIView: UIView, UIGestureRecognizerDelegate, UIPointerInteractionDelegate {
    let model: SessionModel
    private var router: InputRouter { model.router }
    var onKeyboardVisibilityChange: ((Bool) -> Void)?

    /// Becomes first responder only while the soft keyboard is wanted, so a
    /// hardware-keyboard user does not get an on-screen keyboard on every tap.
    private lazy var keyboardProxy = KeyboardProxy(owner: self)
    private var lastScroll: CGPoint = .zero
    private var lastTrackpadScroll: CGPoint = .zero

    init(model: SessionModel) {
        self.model = model
        super.init(frame: .zero)
        backgroundColor = .black
        isMultipleTouchEnabled = true
        layer.addSublayer(model.decoder.displayLayer)
        addSubview(keyboardProxy)
        installGestures()
        addInteraction(UIPointerInteraction(delegate: self))
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    override var canBecomeFirstResponder: Bool { true }

    override func didMoveToWindow() {
        super.didMoveToWindow()
        // First responder for hardware-keyboard presses.
        if window != nil { becomeFirstResponder() }
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        model.decoder.displayLayer.frame = bounds
        CATransaction.commit()
        model.viewSizeChanged(bounds.size)
    }

    func setKeyboardVisible(_ visible: Bool) {
        if visible, !keyboardProxy.isFirstResponder {
            keyboardProxy.becomeFirstResponder()
        } else if !visible, keyboardProxy.isFirstResponder {
            _ = keyboardProxy.resignFirstResponder()
            becomeFirstResponder()
        }
    }

    fileprivate func keyboardProxyResigned() {
        onKeyboardVisibilityChange?(false)
    }

    // MARK: Hardware keyboard

    override func pressesBegan(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        if !handle(presses, pressed: true) { super.pressesBegan(presses, with: event) }
    }

    override func pressesEnded(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        if !handle(presses, pressed: false) { super.pressesEnded(presses, with: event) }
    }

    override func pressesCancelled(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        if !handle(presses, pressed: false) { super.pressesCancelled(presses, with: event) }
    }

    /// Returns false when nothing in `presses` maps to a PC key, so UIKit's
    /// own handling (system shortcuts) still runs.
    @discardableResult
    fileprivate func handle(_ presses: Set<UIPress>, pressed: Bool) -> Bool {
        var handled = false
        for press in presses {
            guard let key = press.key, let scancode = KeyMap.scancode(hidUsage: key.keyCode.rawValue) else { continue }
            router.key(scancode, pressed: pressed)
            handled = true
        }
        return handled
    }

    // MARK: Touch and pointer

    private func installGestures() {
        let direct = NSNumber(value: UITouch.TouchType.direct.rawValue)
        let pointer = NSNumber(value: UITouch.TouchType.indirectPointer.rawValue)

        let tap = UITapGestureRecognizer(target: self, action: #selector(tapped))
        tap.allowedTouchTypes = [direct, pointer]
        addGestureRecognizer(tap)

        let secondaryClick = UITapGestureRecognizer(target: self, action: #selector(secondaryClicked))
        secondaryClick.buttonMaskRequired = .secondary
        secondaryClick.allowedTouchTypes = [pointer]
        addGestureRecognizer(secondaryClick)

        let longPress = UILongPressGestureRecognizer(target: self, action: #selector(longPressed))
        longPress.minimumPressDuration = 0.45
        longPress.allowedTouchTypes = [direct]
        addGestureRecognizer(longPress)

        let drag = UIPanGestureRecognizer(target: self, action: #selector(dragged))
        drag.maximumNumberOfTouches = 1
        drag.allowedTouchTypes = [direct, pointer]
        drag.allowedScrollTypesMask = []
        addGestureRecognizer(drag)

        let twoFingerScroll = UIPanGestureRecognizer(target: self, action: #selector(twoFingerScrolled))
        twoFingerScroll.minimumNumberOfTouches = 2
        twoFingerScroll.allowedTouchTypes = [direct]
        twoFingerScroll.allowedScrollTypesMask = []
        addGestureRecognizer(twoFingerScroll)

        // Trackpad/wheel scroll: a pan that only receives scroll events.
        let trackpadScroll = UIPanGestureRecognizer(target: self, action: #selector(trackpadScrolled))
        trackpadScroll.allowedTouchTypes = []
        trackpadScroll.allowedScrollTypesMask = .all
        addGestureRecognizer(trackpadScroll)

        let hover = UIHoverGestureRecognizer(target: self, action: #selector(hovered))
        addGestureRecognizer(hover)

        tap.require(toFail: longPress)
        for recognizer in [tap, secondaryClick, longPress, drag, twoFingerScroll, trackpadScroll, hover] {
            recognizer.delegate = self
        }
    }

    func gestureRecognizer(_ gestureRecognizer: UIGestureRecognizer, shouldRecognizeSimultaneouslyWith other: UIGestureRecognizer) -> Bool {
        // Hover runs alongside everything; the rest are exclusive.
        gestureRecognizer is UIHoverGestureRecognizer || other is UIHoverGestureRecognizer
    }

    @objc private func tapped(_ recognizer: UITapGestureRecognizer) {
        router.click(KeyMap.buttonLeft, at: recognizer.location(in: self))
    }

    @objc private func secondaryClicked(_ recognizer: UITapGestureRecognizer) {
        router.click(KeyMap.buttonRight, at: recognizer.location(in: self))
    }

    @objc private func longPressed(_ recognizer: UILongPressGestureRecognizer) {
        guard recognizer.state == .began else { return }
        UIImpactFeedbackGenerator(style: .light).impactOccurred()
        router.click(KeyMap.buttonRight, at: recognizer.location(in: self))
    }

    @objc private func dragged(_ recognizer: UIPanGestureRecognizer) {
        let point = recognizer.location(in: self)
        switch recognizer.state {
        case .began:
            // Press where the finger landed, so the remote sees the grab there.
            let start = CGPoint(
                x: point.x - recognizer.translation(in: self).x,
                y: point.y - recognizer.translation(in: self).y
            )
            router.move(to: start)
            router.button(KeyMap.buttonLeft, pressed: true)
            router.move(to: point)
        case .changed:
            router.move(to: point)
        case .ended, .cancelled, .failed:
            router.move(to: point)
            router.button(KeyMap.buttonLeft, pressed: false)
        default:
            break
        }
    }

    @objc private func twoFingerScrolled(_ recognizer: UIPanGestureRecognizer) {
        scroll(recognizer, last: &lastScroll)
    }

    @objc private func trackpadScrolled(_ recognizer: UIPanGestureRecognizer) {
        scroll(recognizer, last: &lastTrackpadScroll)
    }

    private func scroll(_ recognizer: UIPanGestureRecognizer, last: inout CGPoint) {
        let translation = recognizer.translation(in: self)
        if recognizer.state == .began { last = .zero }
        let dx = translation.x - last.x
        let dy = translation.y - last.y
        last = translation
        // Content follows the fingers: dragging up scrolls down.
        router.scroll(dx: Double(-dx), dy: Double(-dy))
    }

    @objc private func hovered(_ recognizer: UIHoverGestureRecognizer) {
        switch recognizer.state {
        case .began, .changed: router.move(to: recognizer.location(in: self))
        default: break
        }
    }

    // The server draws the cursor into the video; hide the iPad pointer over it.
    func pointerInteraction(_ interaction: UIPointerInteraction, styleFor region: UIPointerRegion) -> UIPointerStyle? {
        model.isLive ? .hidden() : nil
    }
}

/// Soft-keyboard target. Committed text goes to `TextInput` (so autocorrect,
/// emoji and non-Latin scripts work); Backspace is a real key.
final class KeyboardProxy: UIView, UIKeyInput {
    private weak var owner: RemoteUIView?

    init(owner: RemoteUIView) {
        self.owner = owner
        super.init(frame: .zero)
        isUserInteractionEnabled = false
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    override var canBecomeFirstResponder: Bool { true }

    override func resignFirstResponder() -> Bool {
        let resigned = super.resignFirstResponder()
        if resigned { owner?.keyboardProxyResigned() }
        return resigned
    }

    // Remote apps do their own text handling; local autocorrect would rewrite
    // what was already sent.
    var autocorrectionType: UITextAutocorrectionType = .no
    var autocapitalizationType: UITextAutocapitalizationType = .none
    var spellCheckingType: UITextSpellCheckingType = .no
    var smartQuotesType: UITextSmartQuotesType = .no
    var smartDashesType: UITextSmartDashesType = .no
    var smartInsertDeleteType: UITextSmartInsertDeleteType = .no
    var keyboardType: UIKeyboardType = .asciiCapable

    var hasText: Bool { true }

    func insertText(_ text: String) {
        MainActor.assumeIsolated { owner?.model.router.text(text) }
    }

    func deleteBackward() {
        MainActor.assumeIsolated { owner?.model.router.tap(KeyMap.keyBackspace) }
    }

    // A hardware keyboard stays mapped by scancode even while the soft
    // keyboard is up; without this, UIKit would turn presses into insertText.
    override func pressesBegan(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        if owner?.handle(presses, pressed: true) != true { super.pressesBegan(presses, with: event) }
    }

    override func pressesEnded(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        if owner?.handle(presses, pressed: false) != true { super.pressesEnded(presses, with: event) }
    }

    override func pressesCancelled(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        if owner?.handle(presses, pressed: false) != true { super.pressesCancelled(presses, with: event) }
    }
}
#endif
