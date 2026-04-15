use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyState {
    Pressed,
    Released,
    Repeat,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyEvent {
    pub scancode: u32,
    pub keysym: u32,
    pub state: KeyState,
    pub modifiers: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MouseMove {
    pub x: f64,
    pub y: f64,
    pub absolute: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ButtonState {
    Pressed,
    Released,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MouseButton {
    pub button: u32,
    pub state: ButtonState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MouseScroll {
    pub dx: f64,
    pub dy: f64,
}
