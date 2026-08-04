package dev.termland.android.session

import android.content.Context
import android.view.InputDevice
import android.view.MotionEvent
import android.view.SurfaceView
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import dev.termland.android.input.InputRouter
import dev.termland.android.input.RemoteInputConnection
import dev.termland.android.input.TouchTranslator

/**
 * The video surface, and the single owner of pointer + IME input.
 *
 * **SurfaceView, not TextureView.** A SurfaceView gets its own dedicated
 * SurfaceFlinger layer, so MediaCodec can write decoded frames straight into it
 * and the compositor scans them out without a GPU copy. A TextureView routes
 * every frame through the app's GL context and the View hierarchy's draw pass,
 * costing a full-frame copy and typically one extra frame of latency. For a
 * remote desktop that latency *is* the product, so TextureView is not an option
 * — the only thing we give up is animating/rotating the video view, which we
 * never do.
 *
 * The view is also the IME target (a write-only [RemoteInputConnection]) so the
 * soft keyboard has something to attach to without a real text field.
 */
class RemoteSurfaceView(context: Context, private val router: InputRouter) : SurfaceView(context) {

    private val touch = TouchTranslator(context, router) {
        performHapticFeedback(android.view.HapticFeedbackConstants.LONG_PRESS)
    }

    /** Mouse buttons we believe are currently down, as an evdev-code set. */
    private var heldButtons = emptySet<Int>()

    init {
        isFocusable = true
        isFocusableInTouchMode = true
        // No focus ring over remote pixels.
        defaultFocusHighlightEnabled = false
        keepScreenOn = true
    }

    override fun onSizeChanged(w: Int, h: Int, oldw: Int, oldh: Int) {
        super.onSizeChanged(w, h, oldw, oldh)
        router.viewWidth = w
        router.viewHeight = h
    }

    // -----------------------------------------------------------------------
    // IME
    // -----------------------------------------------------------------------

    override fun onCheckIsTextEditor(): Boolean = true

    override fun onCreateInputConnection(outAttrs: EditorInfo): InputConnection {
        RemoteInputConnection.configure(outAttrs)
        return RemoteInputConnection(this, router)
    }

    // -----------------------------------------------------------------------
    // Hardware mouse / trackpad
    // -----------------------------------------------------------------------

    /**
     * Real pointing devices deliver motion here, not through onTouchEvent:
     * ACTION_HOVER_MOVE while no button is down, ACTION_MOVE while dragging, and
     * ACTION_SCROLL for the wheel.
     */
    override fun onGenericMotionEvent(event: MotionEvent): Boolean {
        if (!event.isFromPointingDevice()) return super.onGenericMotionEvent(event)

        when (event.actionMasked) {
            MotionEvent.ACTION_SCROLL -> {
                router.scroll(
                    event.getAxisValue(MotionEvent.AXIS_HSCROLL),
                    event.getAxisValue(MotionEvent.AXIS_VSCROLL),
                )
                return true
            }

            MotionEvent.ACTION_HOVER_MOVE,
            MotionEvent.ACTION_HOVER_ENTER,
            -> {
                router.cursorInFrame(true)
                router.moveAbsolute(event.x, event.y)
                syncButtons(event.buttonState)
                return true
            }

            MotionEvent.ACTION_HOVER_EXIT -> {
                // Tell the server the local cursor left, so it can draw its own.
                router.cursorInFrame(false)
                return true
            }

            MotionEvent.ACTION_BUTTON_PRESS,
            MotionEvent.ACTION_BUTTON_RELEASE,
            -> {
                router.moveAbsolute(event.x, event.y)
                syncButtons(event.buttonState)
                return true
            }

            MotionEvent.ACTION_MOVE -> {
                router.moveAbsolute(event.x, event.y)
                syncButtons(event.buttonState)
                return true
            }
        }
        return super.onGenericMotionEvent(event)
    }

    /**
     * A mouse also generates DOWN/MOVE/UP through the touch pipeline. Route those
     * by *source* rather than by action so a real mouse never goes through the
     * tap/long-press heuristics — a right-click must be a right-click, not a
     * 450 ms hold.
     */
    override fun onTouchEvent(event: MotionEvent): Boolean {
        if (event.isFromPointingDevice()) {
            router.cursorInFrame(true)
            router.moveAbsolute(event.x, event.y)
            when (event.actionMasked) {
                MotionEvent.ACTION_DOWN, MotionEvent.ACTION_MOVE -> syncButtons(event.buttonState)
                MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> syncButtons(0)
            }
            return true
        }
        if (event.actionMasked == MotionEvent.ACTION_DOWN) requestFocus()
        return touch.onTouchEvent(event)
    }

    // -----------------------------------------------------------------------
    // Pointer capture (relative / trackpad mode)
    // -----------------------------------------------------------------------

    /**
     * With the pointer captured, Android stops drawing its own cursor and delivers
     * raw relative deltas. That is what a desktop target actually wants: the
     * remote compositor draws the cursor and there is no local/remote cursor
     * mismatch, and edge-of-screen motion is not clamped.
     */
    override fun onCapturedPointerEvent(event: MotionEvent): Boolean {
        when (event.actionMasked) {
            MotionEvent.ACTION_SCROLL -> router.scroll(
                event.getAxisValue(MotionEvent.AXIS_HSCROLL),
                event.getAxisValue(MotionEvent.AXIS_VSCROLL),
            )
            MotionEvent.ACTION_MOVE, MotionEvent.ACTION_HOVER_MOVE -> {
                // In captured mode x/y ARE the deltas.
                router.moveRelative(event.x, event.y)
            }
            MotionEvent.ACTION_BUTTON_PRESS, MotionEvent.ACTION_BUTTON_RELEASE,
            MotionEvent.ACTION_DOWN, MotionEvent.ACTION_UP,
            -> syncButtons(event.buttonState)
        }
        return true
    }

    override fun onPointerCaptureChange(hasCapture: Boolean) {
        super.onPointerCaptureChange(hasCapture)
        router.pointerCaptured = hasCapture
        router.cursorInFrame(hasCapture)
    }

    fun togglePointerCapture(): Boolean {
        if (router.pointerCaptured) {
            releasePointerCapture()
        } else {
            requestFocus()
            requestPointerCapture()
        }
        return !router.pointerCaptured
    }

    // -----------------------------------------------------------------------

    /** Diff the reported button state against what we think is held and emit edges. */
    private fun syncButtons(buttonState: Int) {
        val now = router.evdevButtonsFrom(buttonState).toSet()
        (now - heldButtons).forEach { router.button(it, true) }
        (heldButtons - now).forEach { router.button(it, false) }
        heldButtons = now
    }

    fun releaseHeldButtons() {
        heldButtons.forEach { router.button(it, false) }
        heldButtons = emptySet()
    }

    /**
     * SOURCE_TOUCHPAD is deliberately absent: an external trackpad (including the
     * one on a tablet keyboard case) is surfaced as SOURCE_MOUSE with cursor
     * coordinates, and raw SOURCE_TOUCHPAD events carry *device* coordinates that
     * would be nonsense to scale into the remote framebuffer.
     */
    private fun MotionEvent.isFromPointingDevice(): Boolean =
        isFromSource(InputDevice.SOURCE_MOUSE) ||
            isFromSource(InputDevice.SOURCE_MOUSE_RELATIVE) ||
            isFromSource(InputDevice.SOURCE_STYLUS)
}
