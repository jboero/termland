package dev.termland.android.input

import android.view.KeyEvent

/**
 * Android [KeyEvent] keycode -> Linux evdev scancode + X11 keysym.
 *
 * The server injects through `zwp_virtual_keyboard_v1` on a fixed xkb keymap
 * (crates/termland-compositor/src/input.rs), so the *scancode* is what actually
 * produces a character on the remote side — the desktop client sends `keysym: 0`
 * for that reason. We still fill the keysym in because the protocol carries it
 * and the server's text-injection path (docs/mobile-clients.md, "Server-side:
 * injecting Unicode into Wayland") can use it for keys that have no scancode on
 * the remote keymap.
 *
 * Scancodes are the KEY_* constants from linux/input-event-codes.h; keysyms are
 * the X11 keysymdef.h values. Both tables must stay in sync with
 * `keycode_to_evdev` in crates/termland-client/src/display.rs.
 */
object KeyMap {

    // --- evdev scancodes (linux/input-event-codes.h) ---
    const val KEY_ESC = 1
    const val KEY_BACKSPACE = 14
    const val KEY_TAB = 15
    const val KEY_ENTER = 28
    const val KEY_LEFTCTRL = 29
    const val KEY_LEFTSHIFT = 42
    const val KEY_RIGHTSHIFT = 54
    const val KEY_LEFTALT = 56
    const val KEY_RIGHTALT = 100
    const val KEY_RIGHTCTRL = 97
    const val KEY_LEFTMETA = 125
    const val KEY_RIGHTMETA = 126
    const val KEY_UP = 103
    const val KEY_LEFT = 105
    const val KEY_RIGHT = 106
    const val KEY_DOWN = 108

    // --- xkb modifier mask, matching the compositor's mods_depressed bits ---
    // (crates/termland-compositor/src/input.rs: Shift=0x1 Lock=0x2 Ctrl=0x4
    //  Mod1/Alt=0x8 Mod2/NumLock=0x10 Mod4/Super=0x40)
    const val MOD_SHIFT = 0x01
    const val MOD_CAPS = 0x02
    const val MOD_CTRL = 0x04
    const val MOD_ALT = 0x08
    const val MOD_NUM = 0x10
    const val MOD_SUPER = 0x40

    /** evdev BTN_* codes, as in the desktop client's `mouse_button_to_linux`. */
    const val BTN_LEFT = 0x110
    const val BTN_RIGHT = 0x111
    const val BTN_MIDDLE = 0x112
    const val BTN_SIDE = 0x113
    const val BTN_EXTRA = 0x114

    /** Which modifier bit (if any) a scancode toggles. */
    fun modifierBitForScancode(scancode: Int): Int = when (scancode) {
        KEY_LEFTSHIFT, KEY_RIGHTSHIFT -> MOD_SHIFT
        KEY_LEFTCTRL, KEY_RIGHTCTRL -> MOD_CTRL
        KEY_LEFTALT, KEY_RIGHTALT -> MOD_ALT
        KEY_LEFTMETA, KEY_RIGHTMETA -> MOD_SUPER
        58 -> MOD_CAPS
        69 -> MOD_NUM
        else -> 0
    }

    /**
     * @return evdev scancode, or 0 when this device key has no equivalent on a
     *         PC keyboard (media keys, camera, etc.) and should not be forwarded.
     */
    fun scancode(keyCode: Int): Int = when (keyCode) {
        KeyEvent.KEYCODE_ESCAPE -> 1

        // Digit row
        KeyEvent.KEYCODE_1 -> 2
        KeyEvent.KEYCODE_2 -> 3
        KeyEvent.KEYCODE_3 -> 4
        KeyEvent.KEYCODE_4 -> 5
        KeyEvent.KEYCODE_5 -> 6
        KeyEvent.KEYCODE_6 -> 7
        KeyEvent.KEYCODE_7 -> 8
        KeyEvent.KEYCODE_8 -> 9
        KeyEvent.KEYCODE_9 -> 10
        KeyEvent.KEYCODE_0 -> 11
        KeyEvent.KEYCODE_MINUS -> 12
        KeyEvent.KEYCODE_EQUALS -> 13
        KeyEvent.KEYCODE_DEL -> 14 // Android DEL == Backspace
        KeyEvent.KEYCODE_TAB -> 15

        // QWERTY row
        KeyEvent.KEYCODE_Q -> 16
        KeyEvent.KEYCODE_W -> 17
        KeyEvent.KEYCODE_E -> 18
        KeyEvent.KEYCODE_R -> 19
        KeyEvent.KEYCODE_T -> 20
        KeyEvent.KEYCODE_Y -> 21
        KeyEvent.KEYCODE_U -> 22
        KeyEvent.KEYCODE_I -> 23
        KeyEvent.KEYCODE_O -> 24
        KeyEvent.KEYCODE_P -> 25
        KeyEvent.KEYCODE_LEFT_BRACKET -> 26
        KeyEvent.KEYCODE_RIGHT_BRACKET -> 27
        KeyEvent.KEYCODE_ENTER -> 28
        KeyEvent.KEYCODE_CTRL_LEFT -> 29

        // ASDF row
        KeyEvent.KEYCODE_A -> 30
        KeyEvent.KEYCODE_S -> 31
        KeyEvent.KEYCODE_D -> 32
        KeyEvent.KEYCODE_F -> 33
        KeyEvent.KEYCODE_G -> 34
        KeyEvent.KEYCODE_H -> 35
        KeyEvent.KEYCODE_J -> 36
        KeyEvent.KEYCODE_K -> 37
        KeyEvent.KEYCODE_L -> 38
        KeyEvent.KEYCODE_SEMICOLON -> 39
        KeyEvent.KEYCODE_APOSTROPHE -> 40
        KeyEvent.KEYCODE_GRAVE -> 41
        KeyEvent.KEYCODE_SHIFT_LEFT -> 42
        KeyEvent.KEYCODE_BACKSLASH -> 43

        // ZXCV row
        KeyEvent.KEYCODE_Z -> 44
        KeyEvent.KEYCODE_X -> 45
        KeyEvent.KEYCODE_C -> 46
        KeyEvent.KEYCODE_V -> 47
        KeyEvent.KEYCODE_B -> 48
        KeyEvent.KEYCODE_N -> 49
        KeyEvent.KEYCODE_M -> 50
        KeyEvent.KEYCODE_COMMA -> 51
        KeyEvent.KEYCODE_PERIOD -> 52
        KeyEvent.KEYCODE_SLASH -> 53
        KeyEvent.KEYCODE_SHIFT_RIGHT -> 54
        KeyEvent.KEYCODE_NUMPAD_MULTIPLY -> 55
        KeyEvent.KEYCODE_ALT_LEFT -> 56
        KeyEvent.KEYCODE_SPACE -> 57
        KeyEvent.KEYCODE_CAPS_LOCK -> 58

        // Function keys. F11/F12 are 87/88, NOT 69/70 — a classic evdev gotcha.
        KeyEvent.KEYCODE_F1 -> 59
        KeyEvent.KEYCODE_F2 -> 60
        KeyEvent.KEYCODE_F3 -> 61
        KeyEvent.KEYCODE_F4 -> 62
        KeyEvent.KEYCODE_F5 -> 63
        KeyEvent.KEYCODE_F6 -> 64
        KeyEvent.KEYCODE_F7 -> 65
        KeyEvent.KEYCODE_F8 -> 66
        KeyEvent.KEYCODE_F9 -> 67
        KeyEvent.KEYCODE_F10 -> 68
        KeyEvent.KEYCODE_F11 -> 87
        KeyEvent.KEYCODE_F12 -> 88

        KeyEvent.KEYCODE_NUM_LOCK -> 69
        KeyEvent.KEYCODE_SCROLL_LOCK -> 70

        // Numeric keypad
        KeyEvent.KEYCODE_NUMPAD_7 -> 71
        KeyEvent.KEYCODE_NUMPAD_8 -> 72
        KeyEvent.KEYCODE_NUMPAD_9 -> 73
        KeyEvent.KEYCODE_NUMPAD_SUBTRACT -> 74
        KeyEvent.KEYCODE_NUMPAD_4 -> 75
        KeyEvent.KEYCODE_NUMPAD_5 -> 76
        KeyEvent.KEYCODE_NUMPAD_6 -> 77
        KeyEvent.KEYCODE_NUMPAD_ADD -> 78
        KeyEvent.KEYCODE_NUMPAD_1 -> 79
        KeyEvent.KEYCODE_NUMPAD_2 -> 80
        KeyEvent.KEYCODE_NUMPAD_3 -> 81
        KeyEvent.KEYCODE_NUMPAD_0 -> 82
        KeyEvent.KEYCODE_NUMPAD_DOT -> 83
        KeyEvent.KEYCODE_NUMPAD_ENTER -> 96
        KeyEvent.KEYCODE_NUMPAD_DIVIDE -> 98
        KeyEvent.KEYCODE_NUMPAD_EQUALS -> 117
        KeyEvent.KEYCODE_NUMPAD_COMMA -> 121

        KeyEvent.KEYCODE_CTRL_RIGHT -> 97
        KeyEvent.KEYCODE_SYSRQ -> 99 // PrintScreen
        KeyEvent.KEYCODE_ALT_RIGHT -> 100

        // Navigation cluster
        KeyEvent.KEYCODE_MOVE_HOME -> 102
        KeyEvent.KEYCODE_DPAD_UP -> 103
        KeyEvent.KEYCODE_PAGE_UP -> 104
        KeyEvent.KEYCODE_DPAD_LEFT -> 105
        KeyEvent.KEYCODE_DPAD_RIGHT -> 106
        KeyEvent.KEYCODE_MOVE_END -> 107
        KeyEvent.KEYCODE_DPAD_DOWN -> 108
        KeyEvent.KEYCODE_PAGE_DOWN -> 109
        KeyEvent.KEYCODE_INSERT -> 110
        KeyEvent.KEYCODE_FORWARD_DEL -> 111 // Android FORWARD_DEL == Delete

        KeyEvent.KEYCODE_VOLUME_MUTE -> 113
        KeyEvent.KEYCODE_VOLUME_DOWN -> 114
        KeyEvent.KEYCODE_VOLUME_UP -> 115
        KeyEvent.KEYCODE_BREAK -> 119 // Pause/Break

        KeyEvent.KEYCODE_META_LEFT -> 125
        KeyEvent.KEYCODE_META_RIGHT -> 126
        KeyEvent.KEYCODE_MENU -> 127 // KEY_COMPOSE, what xkb calls Menu

        // Punctuation Android names differently from evdev
        KeyEvent.KEYCODE_AT -> 3 // '@' lives on Shift+2 on a US layout
        KeyEvent.KEYCODE_POUND -> 4
        KeyEvent.KEYCODE_STAR -> 9
        KeyEvent.KEYCODE_PLUS -> 13

        else -> 0
    }

    /** X11 keysym for a keycode, or 0 when unknown. */
    fun keysym(keyCode: Int): Int = when (keyCode) {
        in KeyEvent.KEYCODE_A..KeyEvent.KEYCODE_Z -> 0x61 + (keyCode - KeyEvent.KEYCODE_A) // 'a'..'z'
        in KeyEvent.KEYCODE_0..KeyEvent.KEYCODE_9 -> 0x30 + (keyCode - KeyEvent.KEYCODE_0) // '0'..'9'
        in KeyEvent.KEYCODE_F1..KeyEvent.KEYCODE_F12 -> 0xFFBE + (keyCode - KeyEvent.KEYCODE_F1)
        in KeyEvent.KEYCODE_NUMPAD_0..KeyEvent.KEYCODE_NUMPAD_9 ->
            0xFFB0 + (keyCode - KeyEvent.KEYCODE_NUMPAD_0)

        KeyEvent.KEYCODE_SPACE -> 0x0020
        KeyEvent.KEYCODE_APOSTROPHE -> 0x0027
        KeyEvent.KEYCODE_COMMA -> 0x002C
        KeyEvent.KEYCODE_MINUS -> 0x002D
        KeyEvent.KEYCODE_PERIOD -> 0x002E
        KeyEvent.KEYCODE_SLASH -> 0x002F
        KeyEvent.KEYCODE_SEMICOLON -> 0x003B
        KeyEvent.KEYCODE_EQUALS -> 0x003D
        KeyEvent.KEYCODE_AT -> 0x0040
        KeyEvent.KEYCODE_LEFT_BRACKET -> 0x005B
        KeyEvent.KEYCODE_BACKSLASH -> 0x005C
        KeyEvent.KEYCODE_RIGHT_BRACKET -> 0x005D
        KeyEvent.KEYCODE_GRAVE -> 0x0060
        KeyEvent.KEYCODE_STAR -> 0x002A
        KeyEvent.KEYCODE_PLUS -> 0x002B
        KeyEvent.KEYCODE_POUND -> 0x0023

        KeyEvent.KEYCODE_ESCAPE -> 0xFF1B
        KeyEvent.KEYCODE_DEL -> 0xFF08 // BackSpace
        KeyEvent.KEYCODE_TAB -> 0xFF09
        KeyEvent.KEYCODE_ENTER -> 0xFF0D // Return
        KeyEvent.KEYCODE_NUMPAD_ENTER -> 0xFF8D // KP_Enter
        KeyEvent.KEYCODE_CLEAR -> 0xFF0B
        KeyEvent.KEYCODE_BREAK -> 0xFF13 // Pause
        KeyEvent.KEYCODE_SCROLL_LOCK -> 0xFF14
        KeyEvent.KEYCODE_SYSRQ -> 0xFF61 // Print
        KeyEvent.KEYCODE_INSERT -> 0xFF63
        KeyEvent.KEYCODE_FORWARD_DEL -> 0xFFFF // Delete
        KeyEvent.KEYCODE_MENU -> 0xFF67

        KeyEvent.KEYCODE_MOVE_HOME -> 0xFF50
        KeyEvent.KEYCODE_DPAD_LEFT -> 0xFF51
        KeyEvent.KEYCODE_DPAD_UP -> 0xFF52
        KeyEvent.KEYCODE_DPAD_RIGHT -> 0xFF53
        KeyEvent.KEYCODE_DPAD_DOWN -> 0xFF54
        KeyEvent.KEYCODE_PAGE_UP -> 0xFF55
        KeyEvent.KEYCODE_PAGE_DOWN -> 0xFF56
        KeyEvent.KEYCODE_MOVE_END -> 0xFF57

        KeyEvent.KEYCODE_NUM_LOCK -> 0xFF7F
        KeyEvent.KEYCODE_NUMPAD_MULTIPLY -> 0xFFAA
        KeyEvent.KEYCODE_NUMPAD_ADD -> 0xFFAB
        KeyEvent.KEYCODE_NUMPAD_COMMA -> 0xFFAC
        KeyEvent.KEYCODE_NUMPAD_SUBTRACT -> 0xFFAD
        KeyEvent.KEYCODE_NUMPAD_DOT -> 0xFFAE
        KeyEvent.KEYCODE_NUMPAD_DIVIDE -> 0xFFAF
        KeyEvent.KEYCODE_NUMPAD_EQUALS -> 0xFFBD

        KeyEvent.KEYCODE_SHIFT_LEFT -> 0xFFE1
        KeyEvent.KEYCODE_SHIFT_RIGHT -> 0xFFE2
        KeyEvent.KEYCODE_CTRL_LEFT -> 0xFFE3
        KeyEvent.KEYCODE_CTRL_RIGHT -> 0xFFE4
        KeyEvent.KEYCODE_CAPS_LOCK -> 0xFFE5
        KeyEvent.KEYCODE_ALT_LEFT -> 0xFFE9
        KeyEvent.KEYCODE_ALT_RIGHT -> 0xFFEA
        KeyEvent.KEYCODE_META_LEFT -> 0xFFEB // Super_L
        KeyEvent.KEYCODE_META_RIGHT -> 0xFFEC // Super_R

        else -> 0
    }

    /** Keysym for a modifier scancode; the sticky-key path works in scancodes. */
    fun keysymForScancode(scancode: Int): Int = when (scancode) {
        KEY_LEFTSHIFT -> 0xFFE1
        KEY_RIGHTSHIFT -> 0xFFE2
        KEY_LEFTCTRL -> 0xFFE3
        KEY_RIGHTCTRL -> 0xFFE4
        KEY_LEFTALT -> 0xFFE9
        KEY_RIGHTALT -> 0xFFEA
        KEY_LEFTMETA -> 0xFFEB
        KEY_RIGHTMETA -> 0xFFEC
        else -> 0
    }

    /**
     * Keys we must never forward, because Android owns them and the user needs
     * them to escape the fullscreen viewer / control the device.
     */
    fun isDeviceReserved(keyCode: Int): Boolean = when (keyCode) {
        KeyEvent.KEYCODE_VOLUME_UP,
        KeyEvent.KEYCODE_VOLUME_DOWN,
        KeyEvent.KEYCODE_VOLUME_MUTE,
        KeyEvent.KEYCODE_POWER,
        KeyEvent.KEYCODE_HOME,
        KeyEvent.KEYCODE_APP_SWITCH,
        KeyEvent.KEYCODE_BACK,
        -> true
        else -> false
    }
}
