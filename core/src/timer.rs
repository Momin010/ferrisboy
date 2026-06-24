//! Divider and timer (DIV, TIMA, TMA, TAC).
//!
//! The hardware drives both DIV and TIMA from one free-running 16-bit counter
//! that ticks every T-cycle. DIV (FF04) is its upper 8 bits. TIMA (FF05) is
//! incremented on the **falling edge** of a selected counter bit (chosen by
//! TAC), so it counts at 4096/262144/65536/16384 Hz. When TIMA overflows it
//! reloads from TMA (FF06) and requests a timer interrupt.

use crate::interrupts;

pub struct Timer {
    /// Free-running 16-bit counter; DIV is `div_counter >> 8`.
    div_counter: u16,
    tima: u8,
    tma: u8,
    tac: u8,
}

impl Timer {
    pub fn new() -> Self {
        // Post-boot state: DIV reads ~0xAB shortly after handoff.
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
            0xFF07 => self.tac | 0xF8, // unused top bits read as 1
            _ => 0xFF,
        }
    }

    pub fn write_reg(&mut self, addr: u16, val: u8) {
        match addr {
            0xFF04 => self.div_counter = 0, // any write resets the whole counter
            0xFF05 => self.tima = val,
            0xFF06 => self.tma = val,
            0xFF07 => self.tac = val & 0x07,
            _ => {}
        }
    }

    /// Advance by `cycles` T-cycles, returning `interrupts::TIMER` if TIMA
    /// overflowed during the interval.
    pub fn step(&mut self, cycles: u32) -> u8 {
        let mut raised = 0;
        for _ in 0..cycles {
            let before = self.div_counter;
            self.div_counter = self.div_counter.wrapping_add(1);
            if self.tac & 0x04 != 0 && self.falling_edge(before, self.div_counter) {
                let (next, overflow) = self.tima.overflowing_add(1);
                if overflow {
                    self.tima = self.tma;
                    raised |= interrupts::TIMER;
                } else {
                    self.tima = next;
                }
            }
        }
        raised
    }

    /// True if the TAC-selected bit went 1 -> 0 between two counter values.
    fn falling_edge(&self, before: u16, after: u16) -> bool {
        let bit = match self.tac & 0x03 {
            0 => 9, // 4096 Hz
            1 => 3, // 262144 Hz
            2 => 5, // 65536 Hz
            _ => 7, // 16384 Hz
        };
        (before >> bit) & 1 == 1 && (after >> bit) & 1 == 0
    }
}
