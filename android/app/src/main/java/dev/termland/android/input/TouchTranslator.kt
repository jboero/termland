package dev.termland.android.input

import android.content.Context
import android.os.Handler
import android.os.Looper
import android.view.MotionEvent
import android.view.ViewConfiguration
import kotlin.math.abs

/**
 * Touch fallback, mirroring the desktop client's touchscreen behaviour
 * (crates/termland-client/src/display.rs, `WindowEvent::Touch`) and extending it
 * with the gestures docs/mobile-clients.md specifies:
 *
 *   tap             -> left click at the point
 *   long-press      -> right click at the point
 *   drag            -> press, move, release (left button held)
 *   two-finger drag -> wheel scroll
 *
 * Touch is deliberately secondary here: the primary use case is a tablet with a
 * real keyboard and mouse. This exists so the app is usable when they are not
 * attached, not to be a great touch experience.
 */
class TouchTranslator(
    context: Context,
    private val router: InputRouter,
    private val onHapticTick: () -> Unit = {},
) {
    private val handler = Handler(Looper.getMainLooper())
    private val slop: Int = ViewConfiguration.get(context).scaledTouchSlop

    private var downX = 0f
    private var downY = 0f
    private var moved = false
    /** True once we have committed to a left-button drag. */
    private var dragging = false
    /** True once a long-press fired; the up event must not become a left click. */
    private var longPressFired = false
    private var scrolling = false
    private var lastScrollY = 0f
    private var lastScrollX = 0f

    private val longPress = Runnable {
        if (moved || scrolling) return@Runnable
        longPressFired = true
        onHapticTick()
        router.moveAbsolute(downX, downY)
        router.button(KeyMap.BTN_RIGHT, true)
        router.button(KeyMap.BTN_RIGHT, false)
    }

    fun onTouchEvent(event: MotionEvent): Boolean {
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                downX = event.x
                downY = event.y
                moved = false
                dragging = false
                longPressFired = false
                scrolling = false
                router.cursorInFrame(true)
                handler.postDelayed(longPress, LONG_PRESS_MS)
            }

            MotionEvent.ACTION_POINTER_DOWN -> {
                // Second finger down: this is a scroll, not a click. Undo any drag.
                handler.removeCallbacks(longPress)
                if (dragging) {
                    router.button(KeyMap.BTN_LEFT, false)
                    dragging = false
                }
                scrolling = true
                lastScrollX = centroidX(event)
                lastScrollY = centroidY(event)
            }

            MotionEvent.ACTION_MOVE -> {
                if (scrolling && event.pointerCount >= 2) {
                    val cx = centroidX(event)
                    val cy = centroidY(event)
                    val dx = cx - lastScrollX
                    val dy = cy - lastScrollY
                    lastScrollX = cx
                    lastScrollY = cy
                    // Content follows the fingers: dragging up scrolls down, hence
                    // the sign flip relative to finger movement.
                    router.scrollRaw(-dx.toDouble() * TOUCH_SCROLL_GAIN, -dy.toDouble() * TOUCH_SCROLL_GAIN)
                    return true
                }

                if (!moved && (abs(event.x - downX) > slop || abs(event.y - downY) > slop)) {
                    moved = true
                    handler.removeCallbacks(longPress)
                }
                if (moved && !dragging && !longPressFired) {
                    // Commit to a drag: press at the original point so the remote
                    // sees a grab where the finger landed, then track.
                    router.moveAbsolute(downX, downY)
                    router.button(KeyMap.BTN_LEFT, true)
                    dragging = true
                }
                if (dragging) router.moveAbsolute(event.x, event.y)
            }

            MotionEvent.ACTION_POINTER_UP -> {
                if (event.pointerCount <= 2) scrolling = false
            }

            MotionEvent.ACTION_UP -> {
                handler.removeCallbacks(longPress)
                when {
                    dragging -> {
                        router.moveAbsolute(event.x, event.y)
                        router.button(KeyMap.BTN_LEFT, false)
                    }
                    longPressFired || scrolling -> Unit
                    else -> {
                        router.moveAbsolute(event.x, event.y)
                        router.button(KeyMap.BTN_LEFT, true)
                        router.button(KeyMap.BTN_LEFT, false)
                    }
                }
                reset()
            }

            MotionEvent.ACTION_CANCEL -> {
                handler.removeCallbacks(longPress)
                if (dragging) router.button(KeyMap.BTN_LEFT, false)
                reset()
            }
        }
        return true
    }

    private fun reset() {
        dragging = false
        moved = false
        longPressFired = false
        scrolling = false
    }

    private fun centroidX(e: MotionEvent): Float {
        var sum = 0f
        for (i in 0 until e.pointerCount) sum += e.getX(i)
        return sum / e.pointerCount
    }

    private fun centroidY(e: MotionEvent): Float {
        var sum = 0f
        for (i in 0 until e.pointerCount) sum += e.getY(i)
        return sum / e.pointerCount
    }

    private companion object {
        const val LONG_PRESS_MS = 450L
        /** Touch scroll needs more travel per pixel than a wheel notch. */
        const val TOUCH_SCROLL_GAIN = 1.0
    }
}
