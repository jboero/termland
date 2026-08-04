package dev.termland.android.input

import android.view.KeyEvent
import android.view.MotionEvent
import dev.termland.core.TermlandClient
import java.util.concurrent.atomic.AtomicInteger

/**
 * Turns Android input events into core calls.
 *
 * All the send* methods on [TermlandClient] are fire-and-forget and documented
 * non-blocking, so everything here runs straight on the UI/input thread — adding
 * a queue would only add latency, which is the one thing a thin client cannot
 * afford.
 *
 * The router owns two pieces of state the platform will not give us:
 *  - the current xkb modifier mask, tracked from the key stream (Android's
 *    `event.metaState` does not include our sticky on-screen modifiers), and
 *  - the view-to-remote-framebuffer scale for absolute pointer coordinates.
 */
class InputRouter(private val client: TermlandClient) {

    /** Remote framebuffer size, from `onSessionReady` / resolution changes. */
    @Volatile var remoteWidth: Int = 0
    @Volatile var remoteHeight: Int = 0

    /** Size of the SurfaceView the video is rendered into, in view pixels. */
    @Volatile var viewWidth: Int = 0
    @Volatile var viewHeight: Int = 0

    /** Modifiers held on a physical keyboard. */
    private val hardwareMods = AtomicInteger(0)

    /** Modifiers latched by the on-screen bar; cleared after the next real key. */
    private val stickyMods = AtomicInteger(0)

    /** True while the pointer is captured (relative/trackpad mode). */
    @Volatile var pointerCaptured: Boolean = false

    /** Last absolute position we sent, in remote pixels; seeds relative mode. */
    @Volatile private var lastX: Double = 0.0
    @Volatile private var lastY: Double = 0.0

    val modifiers: Int get() = hardwareMods.get() or stickyMods.get()

    // -----------------------------------------------------------------------
    // Keyboard
    // -----------------------------------------------------------------------

    /**
     * @return true when the event was consumed (i.e. forwarded to the remote).
     */
    fun onKeyEvent(event: KeyEvent): Boolean {
        if (KeyMap.isDeviceReserved(event.keyCode)) return false

        val scancode = KeyMap.scancode(event.keyCode)
        if (scancode == 0) return false

        return when (event.action) {
            KeyEvent.ACTION_DOWN -> {
                // Key repeat: the remote compositor generates its own repeat from
                // the held key, so replaying Android's repeats would double up.
                if (event.repeatCount == 0) sendKey(scancode, KeyMap.keysym(event.keyCode), true)
                true
            }
            KeyEvent.ACTION_UP -> {
                sendKey(scancode, KeyMap.keysym(event.keyCode), false)
                true
            }
            // ACTION_MULTIPLE carries a string of characters (rare, legacy IMEs).
            KeyEvent.ACTION_MULTIPLE -> {
                event.characters?.takeIf { it.isNotEmpty() }?.let { client.sendText(it) }
                true
            }
            else -> false
        }
    }

    /** Send a key by evdev scancode, maintaining the modifier mask. */
    fun sendKey(scancode: Int, keysym: Int, pressed: Boolean) {
        val bit = KeyMap.modifierBitForScancode(scancode)
        if (bit != 0) {
            // Update the mask *before* sending a press and *after* sending a
            // release, so the modifier is included in its own press event the way
            // xkb expects.
            if (pressed) hardwareMods.updateAndGet { it or bit }
        }

        client.sendKey(scancode.toUInt(), keysym.toUInt(), pressed, modifiers.toUInt())

        if (bit != 0 && !pressed) hardwareMods.updateAndGet { it and bit.inv() }

        // A non-modifier key consumes any latched on-screen modifiers, matching
        // how a sticky-keys implementation behaves. Release them for real so the
        // remote does not see Ctrl stuck down.
        if (bit == 0 && !pressed) releaseStickyModifiers()
    }

    /** Press-and-release a key, for on-screen buttons. */
    fun tapKey(scancode: Int, keysym: Int) {
        sendKey(scancode, keysym, true)
        sendKey(scancode, keysym, false)
    }

    // -----------------------------------------------------------------------
    // On-screen modifier bar (sticky toggles)
    // -----------------------------------------------------------------------

    /**
     * Latch or unlatch a modifier from the on-screen bar. The real key press is
     * sent immediately so remote-side shortcut detection sees a genuine held key.
     */
    fun setStickyModifier(scancode: Int, latched: Boolean) {
        val bit = KeyMap.modifierBitForScancode(scancode)
        if (bit == 0) return
        if (latched) {
            stickyMods.updateAndGet { it or bit }
            client.sendKey(scancode.toUInt(), KeyMap.keysymForScancode(scancode).toUInt(), true, modifiers.toUInt())
        } else {
            stickyMods.updateAndGet { it and bit.inv() }
            client.sendKey(scancode.toUInt(), KeyMap.keysymForScancode(scancode).toUInt(), false, modifiers.toUInt())
        }
    }

    fun isStickyLatched(scancode: Int): Boolean =
        stickyMods.get() and KeyMap.modifierBitForScancode(scancode) != 0

    /** Release every latched on-screen modifier; returns the ones released. */
    fun releaseStickyModifiers(): Int {
        val latched = stickyMods.getAndSet(0)
        if (latched == 0) return 0
        STICKY_KEYS.forEach { sc ->
            if (latched and KeyMap.modifierBitForScancode(sc) != 0) {
                client.sendKey(sc.toUInt(), KeyMap.keysymForScancode(sc).toUInt(), false, modifiers.toUInt())
            }
        }
        return latched
    }

    // -----------------------------------------------------------------------
    // Text (IME)
    // -----------------------------------------------------------------------

    fun sendText(text: CharSequence) {
        if (text.isEmpty()) return
        client.sendText(text.toString())
    }

    fun sendBackspace() = tapKey(KeyMap.KEY_BACKSPACE, 0xFF08)

    // -----------------------------------------------------------------------
    // Pointer
    // -----------------------------------------------------------------------

    /** Scale view coordinates to remote framebuffer pixels. */
    fun toRemote(x: Float, y: Float): Pair<Double, Double> {
        val vw = viewWidth
        val vh = viewHeight
        val rw = remoteWidth
        val rh = remoteHeight
        if (vw <= 0 || vh <= 0 || rw <= 0 || rh <= 0) return x.toDouble() to y.toDouble()
        return (x.toDouble() * rw / vw).coerceIn(0.0, (rw - 1).toDouble()) to
            (y.toDouble() * rh / vh).coerceIn(0.0, (rh - 1).toDouble())
    }

    fun moveAbsolute(x: Float, y: Float) {
        val (rx, ry) = toRemote(x, y)
        lastX = rx
        lastY = ry
        client.sendPointerMotion(rx, ry, true)
    }

    /**
     * Relative motion, used by pointer capture and trackpad mode. Sent as
     * `absolute = false` so the server applies it as a delta.
     */
    fun moveRelative(dx: Float, dy: Float) {
        if (dx == 0f && dy == 0f) return
        // Deltas arrive in view pixels; scale to remote pixels so the pointer
        // travels the same visual distance whatever the scaling factor.
        val sx = if (viewWidth > 0 && remoteWidth > 0) remoteWidth.toDouble() / viewWidth else 1.0
        val sy = if (viewHeight > 0 && remoteHeight > 0) remoteHeight.toDouble() / viewHeight else 1.0
        client.sendPointerMotion(dx * sx, dy * sy, false)
    }

    fun button(button: Int, pressed: Boolean) =
        client.sendPointerButton(button.toUInt(), pressed)

    /**
     * Wheel scroll. Android's AXIS_VSCROLL is positive for "away from the user"
     * (content scrolls up); the Wayland axis convention the server injects uses
     * positive = down, so the vertical delta is negated — the same correction the
     * desktop client applies to winit's MouseWheel deltas.
     */
    fun scroll(hScroll: Float, vScroll: Float) {
        if (hScroll == 0f && vScroll == 0f) return
        client.sendScroll((hScroll * SCROLL_STEP).toDouble(), (-vScroll * SCROLL_STEP).toDouble())
    }

    /** Raw scroll in already-correct sign/pixels (touch two-finger path). */
    fun scrollRaw(dx: Double, dy: Double) {
        if (dx == 0.0 && dy == 0.0) return
        client.sendScroll(dx, dy)
    }

    fun cursorInFrame(inFrame: Boolean) = client.setCursorInFrame(inFrame)

    /**
     * Map [MotionEvent.getButtonState] bits to evdev BTN_* codes.
     * Android reports stylus primary/secondary as the same bits as mouse
     * left/right, which is what we want.
     */
    fun evdevButtonsFrom(buttonState: Int): List<Int> = buildList {
        if (buttonState and MotionEvent.BUTTON_PRIMARY != 0) add(KeyMap.BTN_LEFT)
        if (buttonState and MotionEvent.BUTTON_SECONDARY != 0) add(KeyMap.BTN_RIGHT)
        if (buttonState and MotionEvent.BUTTON_TERTIARY != 0) add(KeyMap.BTN_MIDDLE)
        if (buttonState and MotionEvent.BUTTON_BACK != 0) add(KeyMap.BTN_SIDE)
        if (buttonState and MotionEvent.BUTTON_FORWARD != 0) add(KeyMap.BTN_EXTRA)
    }

    /** Release everything we might be holding, on detach/background. */
    fun releaseAll() {
        releaseStickyModifiers()
        listOf(KeyMap.BTN_LEFT, KeyMap.BTN_RIGHT, KeyMap.BTN_MIDDLE).forEach { button(it, false) }
        // Any physical modifier still held would be stuck down on the remote.
        val held = hardwareMods.getAndSet(0)
        if (held != 0) {
            STICKY_KEYS.forEach { sc ->
                if (held and KeyMap.modifierBitForScancode(sc) != 0) {
                    client.sendKey(sc.toUInt(), 0u, false, 0u)
                }
            }
        }
    }

    private companion object {
        /**
         * One scancode per modifier we can latch. Left-hand keys only: the remote
         * keymap treats left/right as equivalent for shortcut purposes.
         */
        val STICKY_KEYS = intArrayOf(
            KeyMap.KEY_LEFTCTRL, KeyMap.KEY_LEFTALT, KeyMap.KEY_LEFTSHIFT, KeyMap.KEY_LEFTMETA,
        )

        /**
         * Pixels of remote scroll per wheel notch. Android reports 1.0 per detent
         * for a real wheel; the desktop client uses 15 px per line, so match it.
         */
        const val SCROLL_STEP = 15.0f
    }
}
