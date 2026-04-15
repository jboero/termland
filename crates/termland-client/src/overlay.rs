//! Client-side UI overlays: the options menu, text rendering, and local cursor.
//!
//! Everything here draws directly into the softbuffer u32 framebuffer
//! (format: 0x00RRGGBB). No GPU, no toolkit - a few hundred lines of
//! plain blitting over decoded video frames.

use font8x8::UnicodeFonts;

// ─── Colors (0x00RRGGBB) ──────────────────────────────────────────────────

const MENU_BG: u32       = 0x1E1E2E; // dark backing
const MENU_BORDER: u32   = 0x89B4FA; // light blue border
const MENU_TITLE: u32    = 0xF5E0DC; // warm white
const MENU_TEXT: u32     = 0xCDD6F4; // light gray
const MENU_SELECTED: u32 = 0x585B70; // highlight for selected row
const MENU_HINT: u32     = 0x6C7086; // faint footer

const CURSOR_OUTLINE: u32 = 0x000000;
const CURSOR_FILL: u32    = 0xFFFFFF;

// ─── Menu items ───────────────────────────────────────────────────────────

/// Which item is currently selected in the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    ShowDataRate,
    ClientCursor,
    Quit,
}

pub const MENU_ITEMS: &[MenuAction] = &[
    MenuAction::ShowDataRate,
    MenuAction::ClientCursor,
    MenuAction::Quit,
];

impl MenuAction {
    pub fn label(&self, show_data_rate: bool, client_cursor: bool) -> String {
        match self {
            Self::ShowDataRate => format!("[{}] Show data rate in title",
                if show_data_rate { "x" } else { " " }),
            Self::ClientCursor => format!("[{}] Client-side cursor (less lag)",
                if client_cursor { "x" } else { " " }),
            Self::Quit => "    Quit (clean disconnect)".to_string(),
        }
    }
}

// ─── Menu state ───────────────────────────────────────────────────────────

pub struct MenuState {
    pub visible: bool,
    pub selected: usize,
    pub show_data_rate: bool,
    pub client_cursor: bool,
}

impl MenuState {
    pub fn new() -> Self {
        Self {
            visible: false,
            selected: 0,
            show_data_rate: false,
            client_cursor: false,
        }
    }

    pub fn toggle(&mut self) {
        self.visible = !self.visible;
    }

    pub fn up(&mut self) {
        if self.selected == 0 {
            self.selected = MENU_ITEMS.len() - 1;
        } else {
            self.selected -= 1;
        }
    }

    pub fn down(&mut self) {
        self.selected = (self.selected + 1) % MENU_ITEMS.len();
    }

    /// Activate the currently selected item. Returns the action taken.
    pub fn activate(&mut self) -> MenuAction {
        let action = MENU_ITEMS[self.selected];
        match action {
            MenuAction::ShowDataRate => self.show_data_rate = !self.show_data_rate,
            MenuAction::ClientCursor => self.client_cursor = !self.client_cursor,
            MenuAction::Quit => {}
        }
        action
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

pub fn text_width_px(text: &str) -> usize {
    text.chars().count() * 8 * 2
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

pub fn stroke_rect(buf: &mut [u32], stride: usize, x: usize, y: usize, w: usize, h: usize, color: u32) {
    fill_rect(buf, stride, x, y, w, 1, color);            // top
    fill_rect(buf, stride, x, y + h - 1, w, 1, color);    // bottom
    fill_rect(buf, stride, x, y, 1, h, color);            // left
    fill_rect(buf, stride, x + w - 1, y, 1, h, color);    // right
}

// ─── Menu rendering ───────────────────────────────────────────────────────

pub fn draw_menu(
    buf: &mut [u32],
    fb_width: u32,
    fb_height: u32,
    menu: &MenuState,
) {
    if !menu.visible {
        return;
    }

    let stride = fb_width as usize;
    let row_h = 24;
    let pad = 16;
    let title_text = "Termland Options";
    let hint_text = "F10 close  ^v nav  Enter toggle  Esc cancel";

    // Menu width: widest label determines it
    let mut content_w = text_width_px(title_text);
    for item in MENU_ITEMS {
        let w = text_width_px(&item.label(menu.show_data_rate, menu.client_cursor));
        if w > content_w { content_w = w; }
    }
    let w = content_w + pad * 2;
    let h = row_h * (MENU_ITEMS.len() + 2) + pad;

    // Center in the framebuffer
    let x = (fb_width as usize).saturating_sub(w) / 2;
    let y = (fb_height as usize).saturating_sub(h) / 2;

    // Background + border
    fill_rect(buf, stride, x, y, w, h, MENU_BG);
    stroke_rect(buf, stride, x, y, w, h, MENU_BORDER);
    stroke_rect(buf, stride, x + 1, y + 1, w - 2, h - 2, MENU_BORDER);

    // Title
    draw_text(buf, stride, x + pad, y + pad, title_text, MENU_TITLE);

    // Items
    let items_y = y + pad + row_h;
    for (i, item) in MENU_ITEMS.iter().enumerate() {
        let row_y = items_y + i * row_h;
        if i == menu.selected {
            fill_rect(buf, stride, x + 4, row_y - 2, w - 8, row_h, MENU_SELECTED);
        }
        let label = item.label(menu.show_data_rate, menu.client_cursor);
        draw_text(buf, stride, x + pad, row_y, &label, MENU_TEXT);
    }

    // Footer hint
    let hint_y = y + h - row_h;
    draw_text(buf, stride, x + pad, hint_y, hint_text, MENU_HINT);
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
    Fullscreen,
    Quit,
}

/// All bar items in display order (left to right).
pub const BAR_ITEMS: &[BarItem] = &[
    BarItem::DataRate,
    BarItem::ClientCursor,
    BarItem::Fullscreen,
    BarItem::Quit,
];

impl BarItem {
    fn label(&self, show_data_rate: bool, client_cursor: bool, fullscreen: bool, rate: u64) -> String {
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
    fullscreen: bool,
    data_rate: u64,
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
        let label = item.label(show_data_rate, client_cursor, fullscreen, data_rate);
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
