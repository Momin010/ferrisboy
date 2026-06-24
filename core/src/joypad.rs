//! Joypad (FF00) — an 8-button matrix read through two selectable nibbles.

/// A Game Boy button. `set_button` maps these onto the FF00 matrix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Right,
    Left,
    Up,
    Down,
    A,
    B,
    Select,
    Start,
}

pub struct Joypad {
    /// Which line is selected (bits 4–5 of FF00, active-low).
    select: u8,
    /// Current button states; bit set = released (hardware is active-low).
    /// Layout: bit0 Right/A, bit1 Left/B, bit2 Up/Select, bit3 Down/Start.
    dpad: u8,
    buttons: u8,
}

impl Joypad {
    pub fn new() -> Self {
        Joypad {
            select: 0x30,
            dpad: 0x0F,
            buttons: 0x0F,
        }
    }

    /// Read FF00. Implemented by the Joypad task (combines `select` with the
    /// selected nibble). Stub returns "nothing pressed".
    pub fn read(&self) -> u8 {
        0xFF
    }

    /// Write FF00 (only the line-select bits 4–5 are writable).
    pub fn write(&mut self, val: u8) {
        self.select = val & 0x30;
    }

    /// Update a button's state. Returns true if this transition should raise a
    /// joypad interrupt (a selected line going high→low, i.e. a fresh press).
    /// Implemented by the Joypad task.
    pub fn set_button(&mut self, _button: Button, _pressed: bool) -> bool {
        false
    }
}
