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

    /// Read FF00. Bits 4–5 select which nibble is exposed (active-low); the low
    /// nibble reports the selected buttons (0 = pressed). Unused top bits read 1.
    pub fn read(&self) -> u8 {
        let mut low = 0x0F;
        if self.select & 0x10 == 0 {
            low &= self.dpad;
        }
        if self.select & 0x20 == 0 {
            low &= self.buttons;
        }
        0xC0 | self.select | low
    }

    /// Write FF00 (only the line-select bits 4–5 are writable).
    pub fn write(&mut self, val: u8) {
        self.select = val & 0x30;
    }

    /// Update a button's state (active-low internally). Returns true if this is
    /// a fresh press on a currently-selected line, which raises a joypad interrupt.
    pub fn set_button(&mut self, button: Button, pressed: bool) -> bool {
        let (nibble_is_dpad, bit) = match button {
            Button::Right => (true, 0),
            Button::Left => (true, 1),
            Button::Up => (true, 2),
            Button::Down => (true, 3),
            Button::A => (false, 0),
            Button::B => (false, 1),
            Button::Select => (false, 2),
            Button::Start => (false, 3),
        };
        let mask = 1u8 << bit;
        let nibble = if nibble_is_dpad {
            &mut self.dpad
        } else {
            &mut self.buttons
        };
        let was_pressed = *nibble & mask == 0;
        if pressed {
            *nibble &= !mask; // 0 = pressed
        } else {
            *nibble |= mask; // 1 = released
        }
        // Interrupt on a fresh press while this button's line is selected.
        let line_selected = if nibble_is_dpad {
            self.select & 0x10 == 0
        } else {
            self.select & 0x20 == 0
        };
        pressed && !was_pressed && line_selected
    }
}
