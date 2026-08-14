//! Client-side UI overlays: the menubar, text rendering, and local cursor.
//!
//! Everything here draws directly into the softbuffer u32 framebuffer
//! (format: 0x00RRGGBB). No GPU, no toolkit - plain pixel blitting over
//! decoded video frames.
//!
//! See ROADMAP.md v0.2 blockers: this whole module is slated for
//! replacement when the client is ported to GTK4/Qt.

use font8x8::UnicodeFonts;

// ─── Colors (0x00RRGGBB) ──────────────────────────────────────────────────

const CURSOR_OUTLINE: u32 = 0x000000;
const CURSOR_FILL: u32    = 0xFFFFFF;

// ─── UI state (shared between menubar and F10 toggle in display.rs) ──────

/// Persistent per-option toggles. The old "dropdown menu" that this used
/// to drive has been removed; the same flags now control the menubar items.
pub struct MenuState {
    pub show_data_rate: bool,
    pub client_cursor: bool,
    /// Show the session's window list. Off by default: it covers part of the
    /// remote screen, so it is something you open to look at, not a permanent
    /// fixture.
    pub show_windows: bool,
}

impl MenuState {
    pub fn new() -> Self {
        Self {
            show_data_rate: true,
            client_cursor: true,
            show_windows: false,
        }
    }
}

// ─── Font rendering ───────────────────────────────────────────────────────

/// Draw one ASCII character into `buf` (row-major, width `stride`) at (x, y).
/// Each font glyph is 8x8 1-bit. We draw it 2x-scaled for legibility.
fn draw_char(buf: &mut [u32], stride: usize, x: usize, y: usize, ch: char, color: u32) {
    let Some(glyph) = font8x8::BASIC_FONTS.get(ch) else { return };

    let scale = 2usize;
    for row in 0..8 {
        let bits = glyph[row];
        for col in 0..8 {
            if bits & (1 << col) != 0 {
                // scale up: fill a scale×scale block
                for dy in 0..scale {
                    for dx in 0..scale {
                        let px = x + col * scale + dx;
                        let py = y + row * scale + dy;
                        let idx = py * stride + px;
                        if idx < buf.len() {
                            buf[idx] = color;
                        }
                    }
                }
            }
        }
    }
}

pub fn draw_text(buf: &mut [u32], stride: usize, x: usize, y: usize, text: &str, color: u32) {
    let char_w = 8 * 2;
    for (i, ch) in text.chars().enumerate() {
        draw_char(buf, stride, x + i * char_w, y, ch, color);
    }
}

// ─── Rectangle helpers ────────────────────────────────────────────────────

pub fn fill_rect(buf: &mut [u32], stride: usize, x: usize, y: usize, w: usize, h: usize, color: u32) {
    let h_total = buf.len() / stride.max(1);
    for row in y..(y + h).min(h_total) {
        let start = row * stride + x;
        let end = (start + w).min(row * stride + stride);
        if end > start && end <= buf.len() {
            for px in &mut buf[start..end] {
                *px = color;
            }
        }
    }
}

// ─── Local cursor sprite ──────────────────────────────────────────────────

// 16x16 classic arrow cursor. '#' = outline (black), 'X' = fill (white), '.' = transparent.
// Hotspot is at the top-left (0, 0).
const CURSOR_SPRITE: &[&str] = &[
    "#...............",
    "##..............",
    "#X#.............",
    "#XX#............",
    "#XXX#...........",
    "#XXXX#..........",
    "#XXXXX#.........",
    "#XXXXXX#........",
    "#XXXXXXX#.......",
    "#XXXXXXXX#......",
    "#XXXXX####......",
    "#XX#XX#.........",
    "#X#.#XX#........",
    "##..#XX#........",
    "#....#XX#.......",
    "......##........",
];

/// The server's actual compositor cursor bitmap, as last received via
/// `Message::CursorUpdate`. Image-only: no position. The server does report
/// a position on the wire (the protocol message carries x/y), but display.rs
/// deliberately ignores it and keeps rendering at the locally-tracked mouse
/// position instead - see the comment where this is consumed for why
/// (round-tripping position through the server would add exactly the input
/// latency client-side-cursor mode exists to avoid).
#[derive(Clone, Default)]
pub struct RemoteCursor {
    pub hotspot_x: i32,
    pub hotspot_y: i32,
    pub width: u32,
    pub height: u32,
    pub visible: bool,
    /// RGBA8 pixels, `width * height * 4` bytes, with a real per-pixel alpha
    /// channel (unlike the video frame path, cursor bitmaps are mostly
    /// transparent around a small opaque glyph).
    pub rgba: Vec<u8>,
}

impl RemoteCursor {
    /// A cursor update carries no usable image yet (e.g. right after
    /// connect, or the server couldn't capture one this round).
    pub fn is_empty(&self) -> bool {
        self.rgba.is_empty() || self.width == 0 || self.height == 0
    }
}

/// Draw the server's real cursor bitmap at the locally-tracked pointer
/// position `(x, y)` (window pixels), offset by its hotspot so the same
/// point of the image sits under the pointer as it would on the remote.
/// Alpha-blends since cursor bitmaps have real transparency, unlike the
/// opaque video frame.
pub fn draw_remote_cursor(buf: &mut [u32], fb_width: u32, fb_height: u32, x: f64, y: f64, cursor: &RemoteCursor) {
    if !cursor.visible || cursor.is_empty() {
        return;
    }
    let stride = fb_width as usize;
    let top_left_x = x as i32 - cursor.hotspot_x;
    let top_left_y = y as i32 - cursor.hotspot_y;

    for row in 0..cursor.height as usize {
        let py = top_left_y + row as i32;
        if py < 0 || py >= fb_height as i32 {
            continue;
        }
        for col in 0..cursor.width as usize {
            let px = top_left_x + col as i32;
            if px < 0 || px >= fb_width as i32 {
                continue;
            }
            let src_off = (row * cursor.width as usize + col) * 4;
            let Some(chunk) = cursor.rgba.get(src_off..src_off + 4) else {
                continue;
            };
            let (r, g, b, a) = (chunk[0], chunk[1], chunk[2], chunk[3]);
            if a == 0 {
                continue;
            }
            let idx = py as usize * stride + px as usize;
            if idx >= buf.len() {
                continue;
            }
            let src = (r as u32) << 16 | (g as u32) << 8 | b as u32;
            buf[idx] = if a == 255 { src } else { blend(buf[idx], src, a as u32) };
        }
    }
}

pub fn draw_local_cursor(buf: &mut [u32], fb_width: u32, fb_height: u32, x: f64, y: f64) {
    let stride = fb_width as usize;
    let cx = x as i32;
    let cy = y as i32;

    for (row, line) in CURSOR_SPRITE.iter().enumerate() {
        for (col, ch) in line.chars().enumerate() {
            let color = match ch {
                '#' => CURSOR_OUTLINE,
                'X' => CURSOR_FILL,
                _ => continue,
            };
            let px = cx + col as i32;
            let py = cy + row as i32;
            if px < 0 || py < 0 || px >= fb_width as i32 || py >= fb_height as i32 {
                continue;
            }
            let idx = py as usize * stride + px as usize;
            if idx < buf.len() {
                buf[idx] = color;
            }
        }
    }
}

// ─── Data rate formatting ─────────────────────────────────────────────────

pub fn format_rate(bytes_per_sec: u64) -> String {
    let bps = bytes_per_sec as f64;
    if bps >= 1_048_576.0 {
        format!("{:.1} MB/s", bps / 1_048_576.0)
    } else if bps >= 1024.0 {
        format!("{:.1} KB/s", bps / 1024.0)
    } else {
        format!("{bps:.0} B/s")
    }
}

// ─── Alpha blending (framebuffer has no alpha channel; blend manually) ────

/// Blend `src` (0x00RRGGBB) over `dst` at opacity `alpha` (0..=255).
fn blend(dst: u32, src: u32, alpha: u32) -> u32 {
    let ia = 255 - alpha;
    let dr = (dst >> 16) & 0xff;
    let dg = (dst >> 8) & 0xff;
    let db = dst & 0xff;
    let sr = (src >> 16) & 0xff;
    let sg = (src >> 8) & 0xff;
    let sb = src & 0xff;
    let r = (sr * alpha + dr * ia) / 255;
    let g = (sg * alpha + dg * ia) / 255;
    let b = (sb * alpha + db * ia) / 255;
    (r << 16) | (g << 8) | b
}

fn fill_rect_alpha(buf: &mut [u32], stride: usize, x: usize, y: usize, w: usize, h: usize, color: u32, alpha: u32) {
    let h_total = buf.len() / stride.max(1);
    for row in y..(y + h).min(h_total) {
        let base = row * stride;
        for px in x..(x + w).min(stride) {
            let idx = base + px;
            if idx < buf.len() {
                buf[idx] = blend(buf[idx], color, alpha);
            }
        }
    }
}

fn draw_rect_outline(buf: &mut [u32], stride: usize, x: usize, y: usize, w: usize, h: usize, color: u32) {
    fill_rect(buf, stride, x, y, w, 1, color);
    fill_rect(buf, stride, x, y + h - 1, w, 1, color);
    fill_rect(buf, stride, x, y, 1, h, color);
    fill_rect(buf, stride, x + w - 1, y, 1, h, color);
}

// ─── Translucent keyboard button (touch: pop up an on-screen keyboard) ────

pub const KBD_BTN_SIZE: u32 = 46;
const KBD_BTN_MARGIN: u32 = 18;

/// Bottom-right rect (x, y, w, h) of the keyboard button in framebuffer pixels.
pub fn keyboard_button_rect(fb_width: u32, fb_height: u32) -> (u32, u32, u32, u32) {
    let s = KBD_BTN_SIZE;
    let x = fb_width.saturating_sub(s + KBD_BTN_MARGIN);
    let y = fb_height.saturating_sub(s + KBD_BTN_MARGIN);
    (x, y, s, s)
}

/// Draw a translucent rounded keyboard button in the bottom-right corner.
/// `active` = the on-screen keyboard is currently shown (highlights the button).
pub fn draw_keyboard_button(buf: &mut [u32], fb_width: u32, fb_height: u32, active: bool) {
    let (x, y, w, h) = keyboard_button_rect(fb_width, fb_height);
    let (x, y, w, h) = (x as usize, y as usize, w as usize, h as usize);
    let stride = fb_width as usize;

    // Translucent panel + subtle border.
    let bg_alpha = if active { 210 } else { 140 };
    fill_rect_alpha(buf, stride, x, y, w, h, 0x1E1E2E, bg_alpha);
    let border = if active { BAR_ON_FG } else { 0x585B70 };
    draw_rect_outline(buf, stride, x, y, w, h, border);

    // Keyboard glyph: an outlined body with a grid of keys and a spacebar.
    let fg = if active { BAR_ON_FG } else { 0xCDD6F4 };
    let pad = 10;
    let (bx, by) = (x + pad, y + pad);
    let (bw, bh) = (w - 2 * pad, h - 2 * pad);
    draw_rect_outline(buf, stride, bx, by, bw, bh, fg);

    // Two rows of little keys.
    let key = 3usize;
    let gap = 3usize;
    let cols = 4;
    let start_x = bx + 4;
    for r in 0..2 {
        let ky = by + 4 + r * (key + gap);
        for c in 0..cols {
            let kx = start_x + c * (key + gap);
            fill_rect(buf, stride, kx, ky, key, key, fg);
        }
    }
    // Spacebar.
    let sy = by + bh - 6;
    fill_rect(buf, stride, bx + 4, sy, bw - 8, 2, fg);
}

/// Is (x, y) (framebuffer pixels) inside the keyboard button?
pub fn hit_test_keyboard_button(fb_width: u32, fb_height: u32, x: f64, y: f64) -> bool {
    let (bx, by, bw, bh) = keyboard_button_rect(fb_width, fb_height);
    let (xi, yi) = (x as i32, y as i32);
    xi >= bx as i32 && xi < (bx + bw) as i32 && yi >= by as i32 && yi < (by + bh) as i32
}

// ─── Reconnect banner ──────────────────────────────────────────────────────
// Drawn by display.rs while connection.rs's background task is retrying a
// dropped connection. Deliberately reuses draw_text/fill_rect_alpha/
// draw_rect_outline rather than adding a new UI subsystem, following the
// same "small overlay drawn straight into the framebuffer" style as the
// menubar and keyboard button above.

const RECONNECT_BG: u32 = 0x181825; // matches BAR_BG
const RECONNECT_FG: u32 = 0xF9E2AF; // amber - distinct from the menubar's green "on" color, reads as "caution"

/// Draw a small status banner near the top-center of the window while a
/// dropped connection is being retried. Callers should draw this over
/// whatever's already in `buf` (the last frame received before the drop)
/// rather than clearing first - a reconnect should never look like a crash
/// to black.
pub fn draw_reconnect_banner(buf: &mut [u32], fb_width: u32, fb_height: u32, attempt: u32) {
    let text = format!(" Reconnecting... (attempt {attempt}) ");
    let char_w = 8 * 2;
    let text_w = text.chars().count() * char_w;
    let pad = 8usize;
    let h = 28usize;
    let w = text_w + pad * 2;
    let x = (fb_width as usize).saturating_sub(w) / 2;
    // Sit just below the menubar strip (which may or may not be visible)
    // and clear of the corner keyboard button.
    let y = (MENUBAR_HEIGHT as usize) + 12;
    let stride = fb_width as usize;

    fill_rect_alpha(buf, stride, x, y, w, h, RECONNECT_BG, 220);
    draw_rect_outline(buf, stride, x, y, w, h, RECONNECT_FG);
    draw_text(buf, stride, x + pad, y + (h - 16) / 2, &text, RECONNECT_FG);

    let _ = fb_height;
}

// ─── Menubar (persistent, always visible unless fullscreen) ───────────────

pub const MENUBAR_HEIGHT: u32 = 24;

const BAR_BG: u32       = 0x181825;
const BAR_BORDER: u32   = 0x313244;
const BAR_TEXT: u32     = 0xCDD6F4;
const BAR_HOVER_BG: u32 = 0x313244;
const BAR_ON_FG: u32    = 0xA6E3A1; // green for enabled toggles

/// An entry in the menubar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarItem {
    DataRate,
    ClientCursor,
    Windows,
    Fullscreen,
    Quit,
}

/// All bar items in display order (left to right).
pub const BAR_ITEMS: &[BarItem] = &[
    BarItem::DataRate,
    BarItem::ClientCursor,
    BarItem::Windows,
    BarItem::Fullscreen,
    BarItem::Quit,
];

impl BarItem {
    fn label(&self, show_data_rate: bool, client_cursor: bool, show_windows: bool, fullscreen: bool, rate: u64, window_count: usize) -> String {
        match self {
            Self::DataRate => {
                if show_data_rate {
                    format!(" {} ", format_rate(rate))
                } else {
                    " Data rate ".to_string()
                }
            }
            Self::ClientCursor => {
                if client_cursor {
                    " [x] Local cursor ".to_string()
                } else {
                    " [ ] Local cursor ".to_string()
                }
            }
            Self::Windows => {
                let mark = if show_windows { "x" } else { " " };
                format!(" [{mark}] Windows ({window_count}) ")
            }
            Self::Fullscreen => {
                if fullscreen {
                    " Windowed ".to_string()
                } else {
                    " Fullscreen ".to_string()
                }
            }
            Self::Quit => " Quit ".to_string(),
        }
    }
}

pub struct BarLayout {
    pub item_rects: Vec<(BarItem, u32, u32)>, // (item, x, width) in fb pixels
}

/// Draw the menubar at the top of the framebuffer and return each item's
/// pixel-space bounds so callers can hit-test mouse clicks.
#[allow(clippy::too_many_arguments)]
pub fn draw_menubar(
    buf: &mut [u32],
    fb_width: u32,
    fb_height: u32,
    show_data_rate: bool,
    client_cursor: bool,
    show_windows: bool,
    fullscreen: bool,
    data_rate: u64,
    window_count: usize,
    hovered: Option<BarItem>,
) -> BarLayout {
    let stride = fb_width as usize;
    let h = MENUBAR_HEIGHT as usize;

    // Background strip + bottom border
    fill_rect(buf, stride, 0, 0, fb_width as usize, h, BAR_BG);
    fill_rect(buf, stride, 0, h - 1, fb_width as usize, 1, BAR_BORDER);

    // Title on the far left
    let char_w = 8 * 2;
    let title = " Termland ";
    draw_text(buf, stride, 8, 4, title, 0x89B4FA);
    let mut x = 8 + title.chars().count() * char_w + 12;

    let mut item_rects: Vec<(BarItem, u32, u32)> = Vec::new();

    for item in BAR_ITEMS {
        let label = item.label(show_data_rate, client_cursor, show_windows, fullscreen, data_rate, window_count);
        let text_w = label.chars().count() * char_w;
        let item_w = text_w;

        // Hover highlight
        if hovered == Some(*item) {
            fill_rect(buf, stride, x, 0, item_w, h - 1, BAR_HOVER_BG);
        }

        // "On" indicator color
        let color = match item {
            BarItem::DataRate if show_data_rate => BAR_ON_FG,
            BarItem::ClientCursor if client_cursor => BAR_ON_FG,
            BarItem::Windows if show_windows => BAR_ON_FG,
            _ => BAR_TEXT,
        };
        draw_text(buf, stride, x, 4, &label, color);

        item_rects.push((*item, x as u32, item_w as u32));
        x += item_w + 4;
    }

    // Keep fb_height reference to silence unused warning (caller may need it later)
    let _ = fb_height;

    BarLayout { item_rects }
}

/// Given a mouse position (compositor-space pixel coords), return which
/// menubar item is under it, or None if the mouse is not in the menubar.
pub fn hit_test_menubar(layout: &BarLayout, x: f64, y: f64) -> Option<BarItem> {
    if y < 0.0 || y >= MENUBAR_HEIGHT as f64 {
        return None;
    }
    let xi = x as i32;
    for (item, ix, iw) in &layout.item_rects {
        if xi >= *ix as i32 && xi < (*ix + *iw) as i32 {
            return Some(*item);
        }
    }
    None
}

// ─── Window list ──────────────────────────────────────────────────────────

/// Draw the session's window list as a panel under the menubar.
///
/// This exists because the *in-session* taskbar cannot show it. KDE's task
/// manager speaks only `org_kde_plasma_window_management`, a KWin protocol, so
/// on labwc it lists nothing no matter what the session is running (issue #1).
/// The server reads the standard `zwlr_foreign_toplevel_management_v1` list
/// instead and sends it over, so the client can render it locally — which also
/// means it works for `App` (kiosk) sessions that have no panel at all.
pub fn draw_window_list(
    buf: &mut [u32],
    fb_width: u32,
    fb_height: u32,
    windows: &[termland_protocol::WindowInfo],
    top_offset: u32,
) {
    let stride = fb_width as usize;
    let char_w = 8 * 2usize;
    let line_h = 20usize;

    let title = if windows.is_empty() {
        "No windows".to_string()
    } else {
        format!("Windows ({})", windows.len())
    };

    // Width tracks the longest line so long titles are not clipped, but stays
    // within a third of the screen — this overlays the remote desktop, and a
    // panel wide enough to hide what you are working on defeats the point.
    let max_chars = windows
        .iter()
        .map(|w| display_line(w).chars().count())
        .chain(std::iter::once(title.chars().count()))
        .max()
        .unwrap_or(10);
    let max_w = (fb_width as usize / 3).max(200);
    let panel_w = ((max_chars * char_w) + 24).min(max_w);
    let panel_h = 12 + line_h + (windows.len().max(1) * line_h) + 8;

    let x = 8usize;
    let y = top_offset as usize + 8;
    if y + panel_h >= fb_height as usize || panel_w >= fb_width as usize {
        return; // Not enough room; drawing a clipped panel is worse than none.
    }

    fill_rect_alpha(buf, stride, x, y, panel_w, panel_h, BAR_BG, 220);
    draw_rect_outline(buf, stride, x, y, panel_w, panel_h, BAR_BORDER);
    draw_text(buf, stride, x + 10, y + 8, &title, 0x89B4FA);

    let usable_chars = (panel_w.saturating_sub(24)) / char_w;
    for (i, w) in windows.iter().enumerate() {
        let row_y = y + 10 + line_h + (i * line_h);
        // The focused window is the one a user is looking for, so mark it
        // rather than relying on colour alone.
        let color = if w.activated { BAR_ON_FG } else { BAR_TEXT };
        let line = truncate(&display_line(w), usable_chars);
        draw_text(buf, stride, x + 10, row_y, &line, color);
    }
}

/// One window's row: focus marker, state, then title (falling back to app id).
fn display_line(w: &termland_protocol::WindowInfo) -> String {
    let marker = if w.activated { ">" } else { " " };
    let state = if w.minimized {
        "_ "
    } else if w.fullscreen {
        "[] "
    } else if w.maximized {
        "^ "
    } else {
        ""
    };
    // A window that has mapped but not yet set a title is normal during
    // startup; showing the app id beats showing an empty row.
    let name = if !w.title.is_empty() {
        w.title.as_str()
    } else if !w.app_id.is_empty() {
        w.app_id.as_str()
    } else {
        "(untitled)"
    };
    format!("{marker} {state}{name}")
}

/// Truncate to `max` characters, marking that it was cut.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max || max < 2 {
        return s.to_string();
    }
    let kept: String = s.chars().take(max - 1).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod window_list_tests {
    use super::*;
    use termland_protocol::WindowInfo;

    fn win(title: &str, app_id: &str, activated: bool) -> WindowInfo {
        WindowInfo {
            id: 1,
            title: title.into(),
            app_id: app_id.into(),
            minimized: false,
            maximized: false,
            fullscreen: false,
            activated,
        }
    }

    /// A window that has mapped but not yet titled itself must still produce a
    /// readable row — an empty line looks like a rendering bug.
    #[test]
    fn untitled_window_falls_back_to_app_id() {
        let line = display_line(&win("", "org.kde.konsole", false));
        assert!(line.contains("org.kde.konsole"), "got {line:?}");
    }

    #[test]
    fn window_with_neither_title_nor_app_id_still_renders() {
        let line = display_line(&win("", "", false));
        assert!(line.contains("(untitled)"), "got {line:?}");
    }

    #[test]
    fn focused_window_is_marked() {
        assert!(display_line(&win("Konsole", "x", true)).starts_with('>'));
        assert!(!display_line(&win("Konsole", "x", false)).starts_with('>'));
    }

    #[test]
    fn truncation_marks_that_it_cut() {
        let long = "a".repeat(100);
        let out = truncate(&long, 10);
        assert_eq!(out.chars().count(), 10);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn short_lines_are_left_alone() {
        assert_eq!(truncate("konsole", 40), "konsole");
    }

    /// Multi-byte titles are common (CJK, emoji in window titles). Truncation
    /// is by character, so it must not split a codepoint.
    #[test]
    fn truncation_is_character_wise_not_byte_wise() {
        let s = "日本語のウィンドウタイトル";
        let out = truncate(s, 5);
        assert_eq!(out.chars().count(), 5);
    }
}
