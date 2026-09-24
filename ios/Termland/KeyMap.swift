import Foundation

/// Platform key codes → Linux evdev scancodes.
///
/// The server injects keys through `zwp_virtual_keyboard_v1` with a fixed US
/// xkb keymap (crates/termland-compositor/src/input.rs), so the *scancode* is
/// what produces a character remotely; like the desktop client we send
/// `keysym: 0`. Scancodes are the KEY_* values from
/// linux/input-event-codes.h and must agree with the Android `KeyMap.kt` and
/// the desktop client's `keycode_to_evdev`.
///
/// Two sources feed this:
/// - iOS / iPadOS hardware keyboards report USB HID usages (`UIKey.keyCode`).
/// - AppKit reports Carbon virtual key codes (`NSEvent.keyCode`, `kVK_*`).
enum KeyMap {
    // evdev scancodes used by name elsewhere.
    static let keyEsc: UInt32 = 1
    static let keyBackspace: UInt32 = 14
    static let keyTab: UInt32 = 15
    static let keyEnter: UInt32 = 28
    static let keyLeftCtrl: UInt32 = 29
    static let keyLeftShift: UInt32 = 42
    static let keyRightShift: UInt32 = 54
    static let keyLeftAlt: UInt32 = 56
    static let keyCapsLock: UInt32 = 58
    static let keyNumLock: UInt32 = 69
    static let keyRightCtrl: UInt32 = 97
    static let keyRightAlt: UInt32 = 100
    static let keyUp: UInt32 = 103
    static let keyLeft: UInt32 = 105
    static let keyRight: UInt32 = 106
    static let keyDown: UInt32 = 108
    static let keyLeftMeta: UInt32 = 125
    static let keyRightMeta: UInt32 = 126

    // xkb modifier mask, matching the compositor's mods_depressed bits.
    static let modShift: UInt32 = 0x01
    static let modCaps: UInt32 = 0x02
    static let modCtrl: UInt32 = 0x04
    static let modAlt: UInt32 = 0x08
    static let modNum: UInt32 = 0x10
    static let modSuper: UInt32 = 0x40

    // evdev pointer buttons.
    static let buttonLeft: UInt32 = 0x110
    static let buttonRight: UInt32 = 0x111
    static let buttonMiddle: UInt32 = 0x112

    /// The xkb modifier bit a scancode holds down, or 0 for ordinary keys.
    static func modifierBit(forScancode scancode: UInt32) -> UInt32 {
        switch scancode {
        case keyLeftShift, keyRightShift: return modShift
        case keyLeftCtrl, keyRightCtrl: return modCtrl
        case keyLeftAlt, keyRightAlt: return modAlt
        case keyLeftMeta, keyRightMeta: return modSuper
        default: return 0
        }
    }

    /// USB HID keyboard-page usage → evdev, or `nil` if it has no PC-keyboard
    /// equivalent and should not be forwarded. Mirrors the kernel's
    /// `hid_keyboard[]` table (drivers/hid/hid-input.c).
    static func scancode(hidUsage usage: Int) -> UInt32? {
        switch usage {
        case 0x04...0x1D: return letters[usage - 0x04]
        case 0x1E...0x27: return UInt32(usage - 0x1E + 2) // 1...9, 0 → 2...11
        case 0x28: return keyEnter
        case 0x29: return keyEsc
        case 0x2A: return keyBackspace
        case 0x2B: return keyTab
        case 0x2C: return 57 // space
        case 0x2D: return 12 // -
        case 0x2E: return 13 // =
        case 0x2F: return 26 // [
        case 0x30: return 27 // ]
        case 0x31, 0x32: return 43 // \ and non-US #
        case 0x33: return 39 // ;
        case 0x34: return 40 // '
        case 0x35: return 41 // `
        case 0x36: return 51 // ,
        case 0x37: return 52 // .
        case 0x38: return 53 // /
        case 0x39: return keyCapsLock
        case 0x3A...0x43: return UInt32(usage - 0x3A + 59) // F1...F10 → 59...68
        case 0x44: return 87 // F11 (not 69: classic evdev gotcha)
        case 0x45: return 88 // F12
        case 0x46: return 99 // PrintScreen
        case 0x47: return 70 // ScrollLock
        case 0x48: return 119 // Pause
        case 0x49: return 110 // Insert
        case 0x4A: return 102 // Home
        case 0x4B: return 104 // PageUp
        case 0x4C: return 111 // Delete (forward)
        case 0x4D: return 107 // End
        case 0x4E: return 109 // PageDown
        case 0x4F: return keyRight
        case 0x50: return keyLeft
        case 0x51: return keyDown
        case 0x52: return keyUp
        case 0x53: return keyNumLock
        case 0x54: return 98 // KP /
        case 0x55: return 55 // KP *
        case 0x56: return 74 // KP -
        case 0x57: return 78 // KP +
        case 0x58: return 96 // KP Enter
        case 0x59: return 79 // KP 1
        case 0x5A: return 80
        case 0x5B: return 81
        case 0x5C: return 75
        case 0x5D: return 76
        case 0x5E: return 77
        case 0x5F: return 71
        case 0x60: return 72
        case 0x61: return 73 // KP 9
        case 0x62: return 82 // KP 0
        case 0x63: return 83 // KP .
        case 0x64: return 86 // non-US \ (KEY_102ND)
        case 0x65: return 127 // Application → KEY_COMPOSE (xkb "Menu")
        case 0x67: return 117 // KP =
        case 0x68...0x73: return UInt32(usage - 0x68 + 183) // F13...F24
        case 0x85: return 121 // KP ,
        case 0xE0: return keyLeftCtrl
        case 0xE1: return keyLeftShift
        case 0xE2: return keyLeftAlt
        case 0xE3: return keyLeftMeta
        case 0xE4: return keyRightCtrl
        case 0xE5: return keyRightShift
        case 0xE6: return keyRightAlt
        case 0xE7: return keyRightMeta
        default: return nil
        }
    }

    /// HID a...z in usage order (0x04...0x1D) → evdev.
    private static let letters: [UInt32] = [
        30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, // a...m
        49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45, 21, 44, // n...z
    ]

    /// Carbon virtual key code (`kVK_*`, HIToolbox/Events.h) → evdev, or `nil`
    /// for keys that should not be forwarded (Fn, media keys).
    static func scancode(macKeyCode code: UInt16) -> UInt32? {
        macKeys[code]
    }

    private static let macKeys: [UInt16: UInt32] = [
        0x00: 30, 0x01: 31, 0x02: 32, 0x03: 33, 0x04: 35, 0x05: 34, // A S D F H G
        0x06: 44, 0x07: 45, 0x08: 46, 0x09: 47, 0x0A: 86, 0x0B: 48, // Z X C V § B
        0x0C: 16, 0x0D: 17, 0x0E: 18, 0x0F: 19, 0x10: 21, 0x11: 20, // Q W E R Y T
        0x12: 2, 0x13: 3, 0x14: 4, 0x15: 5, 0x16: 7, 0x17: 6,       // 1 2 3 4 6 5
        0x18: 13, 0x19: 10, 0x1A: 8, 0x1B: 12, 0x1C: 9, 0x1D: 11,   // = 9 7 - 8 0
        0x1E: 27, 0x1F: 24, 0x20: 22, 0x21: 26, 0x22: 23, 0x23: 25, // ] O U [ I P
        0x24: 28, 0x25: 38, 0x26: 36, 0x27: 40, 0x28: 37, 0x29: 39, // Return L J ' K ;
        0x2A: 43, 0x2B: 51, 0x2C: 53, 0x2D: 49, 0x2E: 50, 0x2F: 52, // \ , / N M .
        0x30: 15, 0x31: 57, 0x32: 41, 0x33: 14, 0x35: 1,            // Tab Space ` Backspace Esc
        0x36: 126, 0x37: 125, 0x38: 42, 0x39: 58, 0x3A: 56,          // RCmd Cmd Shift Caps Option
        0x3B: 29, 0x3C: 54, 0x3D: 100, 0x3E: 97,                     // Ctrl RShift ROption RCtrl
        0x40: 187, 0x41: 83, 0x43: 55, 0x45: 78, 0x47: 69,           // F17 KP. KP* KP+ Clear→NumLock
        0x4B: 98, 0x4C: 96, 0x4E: 74, 0x4F: 188, 0x50: 189,          // KP/ KPEnter KP- F18 F19
        0x51: 117, 0x52: 82, 0x53: 79, 0x54: 80, 0x55: 81,           // KP= KP0 KP1 KP2 KP3
        0x56: 75, 0x57: 76, 0x58: 77, 0x59: 71, 0x5A: 190,           // KP4 KP5 KP6 KP7 F20
        0x5B: 72, 0x5C: 73,                                          // KP8 KP9
        0x60: 63, 0x61: 64, 0x62: 65, 0x63: 61, 0x64: 66, 0x65: 67,  // F5 F6 F7 F3 F8 F9
        0x67: 87, 0x69: 183, 0x6A: 186, 0x6B: 184, 0x6D: 68,         // F11 F13 F16 F14 F10
        0x6E: 127, 0x6F: 88, 0x71: 185, 0x72: 110,                   // Menu F12 F15 Help→Insert
        0x73: 102, 0x74: 104, 0x75: 111, 0x76: 62, 0x77: 107,        // Home PgUp FwdDel F4 End
        0x78: 60, 0x79: 109, 0x7A: 59,                               // F2 PgDn F1
        0x7B: 105, 0x7C: 106, 0x7D: 108, 0x7E: 103,                  // ← → ↓ ↑
    ]
}
