package dev.termland.android.input

import android.view.KeyEvent
import android.view.View
import android.view.inputmethod.BaseInputConnection
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputMethodManager

/**
 * Soft-keyboard bridge.
 *
 * A remote framebuffer has no local text buffer, so we cannot implement a real
 * editable target. Instead this is a write-only sink: whatever the IME *commits*
 * is forwarded as Unicode text (`sendText`), and editing keys are forwarded as
 * evdev key events. That is exactly the MVP split docs/mobile-clients.md
 * prescribes — two channels, committed text and real keys.
 *
 * Intermediate composition (`setComposingText`) is intentionally dropped: showing
 * it would need a local overlay and a way to retract it remotely, which is the K3
 * refinement, not M2. Autocorrect still works because Android commits the
 * corrected word.
 */
class RemoteInputConnection(
    view: View,
    private val router: InputRouter,
) : BaseInputConnection(view, /* fullEditor = */ false) {

    override fun commitText(text: CharSequence?, newCursorPosition: Int): Boolean {
        text?.let { router.sendText(it) }
        return true
    }

    override fun setComposingText(text: CharSequence?, newCursorPosition: Int): Boolean {
        // Swallowed on purpose — see the class doc. Returning true keeps the IME
        // from falling back to raw key events for every keystroke.
        return true
    }

    override fun finishComposingText(): Boolean = true

    override fun deleteSurroundingText(beforeLength: Int, afterLength: Int): Boolean {
        repeat(beforeLength) { router.sendBackspace() }
        repeat(afterLength) { router.tapKey(/* KEY_DELETE */ 111, 0xFFFF) }
        return true
    }

    override fun deleteSurroundingTextInCodePoints(before: Int, after: Int): Boolean =
        deleteSurroundingText(before, after)

    override fun sendKeyEvent(event: KeyEvent?): Boolean {
        // Some IMEs deliver Backspace/Enter/arrows this way rather than through
        // the editing methods.
        event ?: return false
        return router.onKeyEvent(event)
    }

    override fun performEditorAction(actionCode: Int): Boolean {
        if (actionCode == EditorInfo.IME_ACTION_DONE || actionCode == EditorInfo.IME_ACTION_GO ||
            actionCode == EditorInfo.IME_ACTION_SEND || actionCode == EditorInfo.IME_ACTION_UNSPECIFIED
        ) {
            router.tapKey(KeyMap.KEY_ENTER, 0xFF0D)
            return true
        }
        return false
    }

    companion object {
        /**
         * Editor config for the hidden view. TYPE_NULL + IME_FLAG_NO_EXTRACT_UI
         * keeps the IME in "dumb keyboard" mode: no suggestion strip anchored to a
         * text field we do not have, no fullscreen extract editor in landscape.
         */
        fun configure(outAttrs: EditorInfo) {
            outAttrs.inputType = EditorInfo.TYPE_NULL
            outAttrs.imeOptions = EditorInfo.IME_ACTION_NONE or
                EditorInfo.IME_FLAG_NO_EXTRACT_UI or
                EditorInfo.IME_FLAG_NO_FULLSCREEN
            outAttrs.initialSelStart = -1
            outAttrs.initialSelEnd = -1
        }

        fun show(view: View) {
            view.requestFocus()
            view.context.getSystemService(InputMethodManager::class.java)
                ?.showSoftInput(view, InputMethodManager.SHOW_IMPLICIT)
        }

        fun hide(view: View) {
            view.context.getSystemService(InputMethodManager::class.java)
                ?.hideSoftInputFromWindow(view.windowToken, 0)
        }
    }
}
