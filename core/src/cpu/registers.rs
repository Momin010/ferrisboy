//! CPU register file and flag helpers.
//!
//! Eight 8-bit registers (A,B,C,D,E,F,H,L) that pair into four 16-bit views
//! (AF,BC,DE,HL). F holds the flags in its upper nibble; its lower nibble is
//! always zero on this CPU, which the setters enforce.

pub const FLAG_Z: u8 = 0b1000_0000; // zero
pub const FLAG_N: u8 = 0b0100_0000; // subtract (BCD)
pub const FLAG_H: u8 = 0b0010_0000; // half-carry (BCD)
pub const FLAG_C: u8 = 0b0001_0000; // carry

#[derive(Default, Clone, Copy)]
pub struct Registers {
    pub a: u8,
    pub b: u8,
    pub c: u8,
    pub d: u8,
    pub e: u8,
    pub f: u8,
    pub h: u8,
    pub l: u8,
}

impl Registers {
    /// Canonical DMG register state immediately after the boot ROM hands off.
    pub fn post_boot_dmg() -> Self {
        Registers {
            a: 0x01,
            f: 0xB0,
            b: 0x00,
            c: 0x13,
            d: 0x00,
            e: 0xD8,
            h: 0x01,
            l: 0x4D,
        }
    }

    #[inline]
    pub fn af(&self) -> u16 {
        (self.a as u16) << 8 | self.f as u16
    }
    #[inline]
    pub fn bc(&self) -> u16 {
        (self.b as u16) << 8 | self.c as u16
    }
    #[inline]
    pub fn de(&self) -> u16 {
        (self.d as u16) << 8 | self.e as u16
    }
    #[inline]
    pub fn hl(&self) -> u16 {
        (self.h as u16) << 8 | self.l as u16
    }

    #[inline]
    pub fn set_af(&mut self, v: u16) {
        self.a = (v >> 8) as u8;
        self.f = (v as u8) & 0xF0; // low nibble of F is always 0
    }
    #[inline]
    pub fn set_bc(&mut self, v: u16) {
        self.b = (v >> 8) as u8;
        self.c = v as u8;
    }
    #[inline]
    pub fn set_de(&mut self, v: u16) {
        self.d = (v >> 8) as u8;
        self.e = v as u8;
    }
    #[inline]
    pub fn set_hl(&mut self, v: u16) {
        self.h = (v >> 8) as u8;
        self.l = v as u8;
    }

    /// Read a single flag.
    #[inline]
    pub fn flag(&self, mask: u8) -> bool {
        self.f & mask != 0
    }

    /// Set or clear a single flag, keeping F's low nibble zero.
    #[inline]
    pub fn set_flag(&mut self, mask: u8, on: bool) {
        if on {
            self.f |= mask;
        } else {
            self.f &= !mask;
        }
        self.f &= 0xF0;
    }
}
