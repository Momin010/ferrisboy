//! Serial link port (FF01 SB / FF02 SC).
//!
//! With no link cable connected, a transfer started against the internal clock
//! shifts in 0xFF and "completes" immediately. Crucially for this project, the
//! byte written to SB is captured: Blargg's test ROMs stream their pass/fail
//! text out the serial port, which is how the headless harness reads results.
//!
//! This component is simple enough that it is fully implemented here.

/// Cap on the captured-output buffer (~64 KiB). A ROM that endlessly triggers
/// serial transfers can't grow this without bound; the oldest half is dropped
/// when the cap is hit. Far larger than any test ROM's real output.
const MAX_OUTPUT: usize = 1 << 16;

pub struct Serial {
    sb: u8,
    sc: u8,
    output: Vec<u8>,
}

impl Serial {
    pub fn new() -> Self {
        Serial {
            sb: 0x00,
            sc: 0x7E,
            output: Vec::new(),
        }
    }

    pub fn read_reg(&self, addr: u16) -> u8 {
        match addr {
            0xFF01 => self.sb,
            0xFF02 => self.sc | 0x7E, // bits 1–6 unused, read as 1
            _ => 0xFF,
        }
    }

    pub fn write_reg(&mut self, addr: u16, val: u8) {
        match addr {
            0xFF01 => self.sb = val,
            0xFF02 => {
                self.sc = val;
                // Bit 7 = start, bit 0 = use internal clock. With no peer, the
                // transfer completes at once; capture the byte for test output.
                if val & 0x81 == 0x81 {
                    if self.output.len() >= MAX_OUTPUT {
                        self.output.drain(0..MAX_OUTPUT / 2);
                    }
                    self.output.push(self.sb);
                    self.sb = 0xFF;
                    self.sc &= 0x7F; // clear the start bit
                }
            }
            _ => {}
        }
    }

    /// Advance the serial clock. The instant-complete model above raises no
    /// interrupt; returns 0. (A future cycle-accurate model would return
    /// `interrupts::SERIAL` here.)
    pub fn step(&mut self, _cycles: u32) -> u8 {
        0
    }

    /// Drain captured serial bytes (used by the headless test harness).
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.output)
    }
}
