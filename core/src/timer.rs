//! Divider and timer (DIV, TIMA, TMA, TAC).
//!
//! ## Interface contract (implemented by the Timer task)
//! - registers FF04..FF07 via `read_reg`/`write_reg`
//! - `step(cycles)` advances counters and returns `interrupts::TIMER` on overflow

pub struct Timer {
    /// Internal 16-bit divider; DIV (FF04) is its upper 8 bits.
    div_counter: u16,
    tima: u8,
    tma: u8,
    tac: u8,
}

impl Timer {
    pub fn new() -> Self {
        // Post-boot state.
        Timer {
            div_counter: 0xAB00,
            tima: 0x00,
            tma: 0x00,
            tac: 0xF8,
        }
    }

    pub fn read_reg(&self, addr: u16) -> u8 {
        match addr {
            0xFF04 => (self.div_counter >> 8) as u8,
            0xFF05 => self.tima,
            0xFF06 => self.tma,
            0xFF07 => self.tac | 0xF8,
            _ => 0xFF,
        }
    }

    pub fn write_reg(&mut self, addr: u16, val: u8) {
        match addr {
            0xFF04 => self.div_counter = 0, // any write resets DIV
            0xFF05 => self.tima = val,
            0xFF06 => self.tma = val,
            0xFF07 => self.tac = val & 0x07,
            _ => {}
        }
    }

    /// Advance by `cycles` T-cycles. Returns `interrupts::TIMER` when TIMA
    /// overflows. Implemented by the Timer task.
    pub fn step(&mut self, _cycles: u32) -> u8 {
        0
    }
}
