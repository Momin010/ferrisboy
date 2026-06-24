//! Sharp LR35902 CPU (a Game Boy-specific 8080/Z80 relative).
//!
//! The register file, flag helpers, fetch/stack helpers, and the
//! interrupt/HALT/EI control flow are implemented here. The two big opcode
//! decoders — [`Cpu::execute`] and [`Cpu::execute_cb`] — are the CPU task's
//! responsibility; they are the only `todo!`s in this module.

use crate::interrupts;
use crate::mmu::Mmu;

mod alu;
mod registers;
pub use registers::{Registers, FLAG_C, FLAG_H, FLAG_N, FLAG_Z};

pub struct Cpu {
    pub reg: Registers,
    pub pc: u16,
    pub sp: u16,
    /// Interrupt Master Enable.
    pub ime: bool,
    /// EI enables interrupts only *after the following instruction*.
    ime_pending: bool,
    /// HALT state — CPU paused until an interrupt is pending.
    pub halted: bool,
    /// Set when a HALT is executed with IME=0 and an interrupt already pending
    /// (the "HALT bug": the next byte is read without advancing PC).
    pub halt_bug: bool,
}

impl Cpu {
    /// Construct with the canonical DMG post-boot register state (boot ROM skipped).
    pub fn new() -> Self {
        Cpu {
            reg: Registers::post_boot_dmg(),
            pc: 0x0100,
            sp: 0xFFFE,
            ime: false,
            ime_pending: false,
            halted: false,
            halt_bug: false,
        }
    }

    /// Execute one step: service a pending interrupt, or run one instruction.
    /// Returns the elapsed T-cycles.
    pub fn step(&mut self, mmu: &mut Mmu) -> u32 {
        // 1. Interrupt servicing / HALT wake-up.
        if let Some(cycles) = self.service_interrupt(mmu) {
            return cycles;
        }
        if self.halted {
            return 4; // idle until an interrupt becomes pending
        }

        // 2. EI scheduled one instruction earlier takes effect now.
        let enabling_ime = self.ime_pending;

        // 3. Fetch + dispatch.
        let opcode = self.fetch_byte(mmu);
        let cycles = if opcode == 0xCB {
            let cb = self.fetch_byte(mmu);
            self.execute_cb(cb, mmu)
        } else {
            self.execute(opcode, mmu)
        };

        if enabling_ime {
            self.ime = true;
            self.ime_pending = false;
        }
        cycles
    }

    /// If an interrupt is pending and IME is set, dispatch it (20 T-cycles).
    /// Any pending interrupt also wakes the CPU from HALT regardless of IME.
    fn service_interrupt(&mut self, mmu: &mut Mmu) -> Option<u32> {
        let pending = mmu.pending_interrupts();
        if pending == 0 {
            return None;
        }
        self.halted = false;
        if !self.ime {
            return None;
        }
        let bit = pending.trailing_zeros() as usize; // lowest set bit = highest priority
        self.ime = false;
        self.ime_pending = false;
        mmu.clear_interrupt(1 << bit);
        let pc = self.pc;
        self.push_word(mmu, pc);
        self.pc = interrupts::VECTORS[bit];
        Some(20)
    }

    // ---- helpers used pervasively by the opcode tables ----

    pub fn fetch_byte(&mut self, mmu: &mut Mmu) -> u8 {
        let byte = mmu.read_byte(self.pc);
        if self.halt_bug {
            // HALT bug: PC fails to advance for exactly this one fetch.
            self.halt_bug = false;
        } else {
            self.pc = self.pc.wrapping_add(1);
        }
        byte
    }

    pub fn fetch_word(&mut self, mmu: &mut Mmu) -> u16 {
        let lo = self.fetch_byte(mmu) as u16;
        let hi = self.fetch_byte(mmu) as u16;
        (hi << 8) | lo
    }

    pub fn push_word(&mut self, mmu: &mut Mmu, value: u16) {
        self.sp = self.sp.wrapping_sub(1);
        mmu.write_byte(self.sp, (value >> 8) as u8);
        self.sp = self.sp.wrapping_sub(1);
        mmu.write_byte(self.sp, value as u8);
    }

    pub fn pop_word(&mut self, mmu: &mut Mmu) -> u16 {
        let lo = mmu.read_byte(self.sp) as u16;
        self.sp = self.sp.wrapping_add(1);
        let hi = mmu.read_byte(self.sp) as u16;
        self.sp = self.sp.wrapping_add(1);
        (hi << 8) | lo
    }

    /// EI: schedule IME to turn on after the next instruction.
    pub fn schedule_ei(&mut self) {
        self.ime_pending = true;
    }

    /// DI: clear IME immediately.
    pub fn disable_interrupts(&mut self) {
        self.ime = false;
        self.ime_pending = false;
    }

    /// HALT: enter low-power state, accounting for the HALT bug.
    pub fn halt(&mut self, mmu: &Mmu) {
        if self.ime {
            self.halted = true;
        } else if mmu.pending_interrupts() != 0 {
            // IME=0 with an interrupt already pending → HALT bug, no halt.
            self.halt_bug = true;
        } else {
            self.halted = true;
        }
    }

    // ---- the instruction decoders: implemented by the CPU task ----

    // ---- small read/write helpers for the (HL) memory operand ----

    fn read_hl(&self, mmu: &Mmu) -> u8 {
        mmu.read_byte(self.reg.hl())
    }
    fn write_hl(&mut self, mmu: &mut Mmu, val: u8) {
        mmu.write_byte(self.reg.hl(), val);
    }

    /// Conditional relative jump. Returns the T-cycle cost (12 taken, 8 not).
    fn jr_cond(&mut self, mmu: &mut Mmu, cond: bool) -> u32 {
        let offset = self.fetch_byte(mmu) as i8 as i16; // read operand regardless
        if cond {
            self.pc = (self.pc as i16).wrapping_add(offset) as u16;
            12
        } else {
            8
        }
    }

    /// Conditional absolute jump. Returns 16 taken, 12 not.
    fn jp_cond(&mut self, mmu: &mut Mmu, cond: bool) -> u32 {
        let addr = self.fetch_word(mmu); // read operand regardless
        if cond {
            self.pc = addr;
            16
        } else {
            12
        }
    }

    /// Conditional call. Returns 24 taken, 12 not.
    fn call_cond(&mut self, mmu: &mut Mmu, cond: bool) -> u32 {
        let addr = self.fetch_word(mmu); // read operand regardless
        if cond {
            let pc = self.pc;
            self.push_word(mmu, pc);
            self.pc = addr;
            24
        } else {
            12
        }
    }

    /// Conditional return. Returns 20 taken, 8 not.
    fn ret_cond(&mut self, mmu: &mut Mmu, cond: bool) -> u32 {
        if cond {
            self.pc = self.pop_word(mmu);
            20
        } else {
            8
        }
    }

    fn rst(&mut self, mmu: &mut Mmu, addr: u16) -> u32 {
        let pc = self.pc;
        self.push_word(mmu, pc);
        self.pc = addr;
        16
    }

    /// Execute one unprefixed opcode; return its T-cycle cost.
    fn execute(&mut self, opcode: u8, mmu: &mut Mmu) -> u32 {
        match opcode {
            // ---- 0x0_ ----
            0x00 => 4, // NOP
            0x01 => {
                let v = self.fetch_word(mmu);
                self.reg.set_bc(v);
                12
            }
            0x02 => {
                mmu.write_byte(self.reg.bc(), self.reg.a);
                8
            }
            0x03 => {
                self.reg.set_bc(self.reg.bc().wrapping_add(1));
                8
            }
            0x04 => {
                self.reg.b = self.inc8(self.reg.b);
                4
            }
            0x05 => {
                self.reg.b = self.dec8(self.reg.b);
                4
            }
            0x06 => {
                self.reg.b = self.fetch_byte(mmu);
                8
            }
            0x07 => {
                self.rlca();
                4
            }
            0x08 => {
                let addr = self.fetch_word(mmu);
                mmu.write_byte(addr, self.sp as u8);
                mmu.write_byte(addr.wrapping_add(1), (self.sp >> 8) as u8);
                20
            }
            0x09 => {
                self.add16_hl(self.reg.bc());
                8
            }
            0x0A => {
                self.reg.a = mmu.read_byte(self.reg.bc());
                8
            }
            0x0B => {
                self.reg.set_bc(self.reg.bc().wrapping_sub(1));
                8
            }
            0x0C => {
                self.reg.c = self.inc8(self.reg.c);
                4
            }
            0x0D => {
                self.reg.c = self.dec8(self.reg.c);
                4
            }
            0x0E => {
                self.reg.c = self.fetch_byte(mmu);
                8
            }
            0x0F => {
                self.rrca();
                4
            }

            // ---- 0x1_ ----
            0x10 => {
                // STOP — consume the following byte; 4-cycle no-op on DMG.
                self.fetch_byte(mmu);
                4
            }
            0x11 => {
                let v = self.fetch_word(mmu);
                self.reg.set_de(v);
                12
            }
            0x12 => {
                mmu.write_byte(self.reg.de(), self.reg.a);
                8
            }
            0x13 => {
                self.reg.set_de(self.reg.de().wrapping_add(1));
                8
            }
            0x14 => {
                self.reg.d = self.inc8(self.reg.d);
                4
            }
            0x15 => {
                self.reg.d = self.dec8(self.reg.d);
                4
            }
            0x16 => {
                self.reg.d = self.fetch_byte(mmu);
                8
            }
            0x17 => {
                self.rla();
                4
            }
            0x18 => self.jr_cond(mmu, true),
            0x19 => {
                self.add16_hl(self.reg.de());
                8
            }
            0x1A => {
                self.reg.a = mmu.read_byte(self.reg.de());
                8
            }
            0x1B => {
                self.reg.set_de(self.reg.de().wrapping_sub(1));
                8
            }
            0x1C => {
                self.reg.e = self.inc8(self.reg.e);
                4
            }
            0x1D => {
                self.reg.e = self.dec8(self.reg.e);
                4
            }
            0x1E => {
                self.reg.e = self.fetch_byte(mmu);
                8
            }
            0x1F => {
                self.rra();
                4
            }

            // ---- 0x2_ ----
            0x20 => {
                let cond = !self.reg.flag(FLAG_Z);
                self.jr_cond(mmu, cond)
            }
            0x21 => {
                let v = self.fetch_word(mmu);
                self.reg.set_hl(v);
                12
            }
            0x22 => {
                // LD (HL+),A
                let hl = self.reg.hl();
                mmu.write_byte(hl, self.reg.a);
                self.reg.set_hl(hl.wrapping_add(1));
                8
            }
            0x23 => {
                self.reg.set_hl(self.reg.hl().wrapping_add(1));
                8
            }
            0x24 => {
                self.reg.h = self.inc8(self.reg.h);
                4
            }
            0x25 => {
                self.reg.h = self.dec8(self.reg.h);
                4
            }
            0x26 => {
                self.reg.h = self.fetch_byte(mmu);
                8
            }
            0x27 => {
                self.daa();
                4
            }
            0x28 => {
                let cond = self.reg.flag(FLAG_Z);
                self.jr_cond(mmu, cond)
            }
            0x29 => {
                self.add16_hl(self.reg.hl());
                8
            }
            0x2A => {
                // LD A,(HL+)
                let hl = self.reg.hl();
                self.reg.a = mmu.read_byte(hl);
                self.reg.set_hl(hl.wrapping_add(1));
                8
            }
            0x2B => {
                self.reg.set_hl(self.reg.hl().wrapping_sub(1));
                8
            }
            0x2C => {
                self.reg.l = self.inc8(self.reg.l);
                4
            }
            0x2D => {
                self.reg.l = self.dec8(self.reg.l);
                4
            }
            0x2E => {
                self.reg.l = self.fetch_byte(mmu);
                8
            }
            0x2F => {
                self.cpl();
                4
            }

            // ---- 0x3_ ----
            0x30 => {
                let cond = !self.reg.flag(FLAG_C);
                self.jr_cond(mmu, cond)
            }
            0x31 => {
                self.sp = self.fetch_word(mmu);
                12
            }
            0x32 => {
                // LD (HL-),A
                let hl = self.reg.hl();
                mmu.write_byte(hl, self.reg.a);
                self.reg.set_hl(hl.wrapping_sub(1));
                8
            }
            0x33 => {
                self.sp = self.sp.wrapping_add(1);
                8
            }
            0x34 => {
                let v = self.read_hl(mmu);
                let r = self.inc8(v);
                self.write_hl(mmu, r);
                12
            }
            0x35 => {
                let v = self.read_hl(mmu);
                let r = self.dec8(v);
                self.write_hl(mmu, r);
                12
            }
            0x36 => {
                let v = self.fetch_byte(mmu);
                self.write_hl(mmu, v);
                12
            }
            0x37 => {
                self.scf();
                4
            }
            0x38 => {
                let cond = self.reg.flag(FLAG_C);
                self.jr_cond(mmu, cond)
            }
            0x39 => {
                self.add16_hl(self.sp);
                8
            }
            0x3A => {
                // LD A,(HL-)
                let hl = self.reg.hl();
                self.reg.a = mmu.read_byte(hl);
                self.reg.set_hl(hl.wrapping_sub(1));
                8
            }
            0x3B => {
                self.sp = self.sp.wrapping_sub(1);
                8
            }
            0x3C => {
                self.reg.a = self.inc8(self.reg.a);
                4
            }
            0x3D => {
                self.reg.a = self.dec8(self.reg.a);
                4
            }
            0x3E => {
                self.reg.a = self.fetch_byte(mmu);
                8
            }
            0x3F => {
                self.ccf();
                4
            }

            // ---- 0x4_ .. 0x7_ : the LD r,r' block (and HALT at 0x76) ----
            0x40..=0x7F => self.execute_ld_block(opcode, mmu),

            // ---- 0x8_ : ADD / ADC A,r ----
            0x80 => {
                self.add8(self.reg.b);
                4
            }
            0x81 => {
                self.add8(self.reg.c);
                4
            }
            0x82 => {
                self.add8(self.reg.d);
                4
            }
            0x83 => {
                self.add8(self.reg.e);
                4
            }
            0x84 => {
                self.add8(self.reg.h);
                4
            }
            0x85 => {
                self.add8(self.reg.l);
                4
            }
            0x86 => {
                let v = self.read_hl(mmu);
                self.add8(v);
                8
            }
            0x87 => {
                self.add8(self.reg.a);
                4
            }
            0x88 => {
                self.adc8(self.reg.b);
                4
            }
            0x89 => {
                self.adc8(self.reg.c);
                4
            }
            0x8A => {
                self.adc8(self.reg.d);
                4
            }
            0x8B => {
                self.adc8(self.reg.e);
                4
            }
            0x8C => {
                self.adc8(self.reg.h);
                4
            }
            0x8D => {
                self.adc8(self.reg.l);
                4
            }
            0x8E => {
                let v = self.read_hl(mmu);
                self.adc8(v);
                8
            }
            0x8F => {
                self.adc8(self.reg.a);
                4
            }

            // ---- 0x9_ : SUB / SBC A,r ----
            0x90 => {
                self.sub8(self.reg.b);
                4
            }
            0x91 => {
                self.sub8(self.reg.c);
                4
            }
            0x92 => {
                self.sub8(self.reg.d);
                4
            }
            0x93 => {
                self.sub8(self.reg.e);
                4
            }
            0x94 => {
                self.sub8(self.reg.h);
                4
            }
            0x95 => {
                self.sub8(self.reg.l);
                4
            }
            0x96 => {
                let v = self.read_hl(mmu);
                self.sub8(v);
                8
            }
            0x97 => {
                self.sub8(self.reg.a);
                4
            }
            0x98 => {
                self.sbc8(self.reg.b);
                4
            }
            0x99 => {
                self.sbc8(self.reg.c);
                4
            }
            0x9A => {
                self.sbc8(self.reg.d);
                4
            }
            0x9B => {
                self.sbc8(self.reg.e);
                4
            }
            0x9C => {
                self.sbc8(self.reg.h);
                4
            }
            0x9D => {
                self.sbc8(self.reg.l);
                4
            }
            0x9E => {
                let v = self.read_hl(mmu);
                self.sbc8(v);
                8
            }
            0x9F => {
                self.sbc8(self.reg.a);
                4
            }

            // ---- 0xA_ : AND / XOR A,r ----
            0xA0 => {
                self.and8(self.reg.b);
                4
            }
            0xA1 => {
                self.and8(self.reg.c);
                4
            }
            0xA2 => {
                self.and8(self.reg.d);
                4
            }
            0xA3 => {
                self.and8(self.reg.e);
                4
            }
            0xA4 => {
                self.and8(self.reg.h);
                4
            }
            0xA5 => {
                self.and8(self.reg.l);
                4
            }
            0xA6 => {
                let v = self.read_hl(mmu);
                self.and8(v);
                8
            }
            0xA7 => {
                self.and8(self.reg.a);
                4
            }
            0xA8 => {
                self.xor8(self.reg.b);
                4
            }
            0xA9 => {
                self.xor8(self.reg.c);
                4
            }
            0xAA => {
                self.xor8(self.reg.d);
                4
            }
            0xAB => {
                self.xor8(self.reg.e);
                4
            }
            0xAC => {
                self.xor8(self.reg.h);
                4
            }
            0xAD => {
                self.xor8(self.reg.l);
                4
            }
            0xAE => {
                let v = self.read_hl(mmu);
                self.xor8(v);
                8
            }
            0xAF => {
                self.xor8(self.reg.a);
                4
            }

            // ---- 0xB_ : OR / CP A,r ----
            0xB0 => {
                self.or8(self.reg.b);
                4
            }
            0xB1 => {
                self.or8(self.reg.c);
                4
            }
            0xB2 => {
                self.or8(self.reg.d);
                4
            }
            0xB3 => {
                self.or8(self.reg.e);
                4
            }
            0xB4 => {
                self.or8(self.reg.h);
                4
            }
            0xB5 => {
                self.or8(self.reg.l);
                4
            }
            0xB6 => {
                let v = self.read_hl(mmu);
                self.or8(v);
                8
            }
            0xB7 => {
                self.or8(self.reg.a);
                4
            }
            0xB8 => {
                self.cp8(self.reg.b);
                4
            }
            0xB9 => {
                self.cp8(self.reg.c);
                4
            }
            0xBA => {
                self.cp8(self.reg.d);
                4
            }
            0xBB => {
                self.cp8(self.reg.e);
                4
            }
            0xBC => {
                self.cp8(self.reg.h);
                4
            }
            0xBD => {
                self.cp8(self.reg.l);
                4
            }
            0xBE => {
                let v = self.read_hl(mmu);
                self.cp8(v);
                8
            }
            0xBF => {
                self.cp8(self.reg.a);
                4
            }

            // ---- 0xC_ ----
            0xC0 => {
                let cond = !self.reg.flag(FLAG_Z);
                self.ret_cond(mmu, cond)
            }
            0xC1 => {
                let v = self.pop_word(mmu);
                self.reg.set_bc(v);
                12
            }
            0xC2 => {
                let cond = !self.reg.flag(FLAG_Z);
                self.jp_cond(mmu, cond)
            }
            0xC3 => self.jp_cond(mmu, true),
            0xC4 => {
                let cond = !self.reg.flag(FLAG_Z);
                self.call_cond(mmu, cond)
            }
            0xC5 => {
                let v = self.reg.bc();
                self.push_word(mmu, v);
                16
            }
            0xC6 => {
                let v = self.fetch_byte(mmu);
                self.add8(v);
                8
            }
            0xC7 => self.rst(mmu, 0x00),
            0xC8 => {
                let cond = self.reg.flag(FLAG_Z);
                self.ret_cond(mmu, cond)
            }
            0xC9 => {
                // RET
                self.pc = self.pop_word(mmu);
                16
            }
            0xCA => {
                let cond = self.reg.flag(FLAG_Z);
                self.jp_cond(mmu, cond)
            }
            0xCB => {
                // Should be intercepted in step(); handle defensively.
                let cb = self.fetch_byte(mmu);
                self.execute_cb(cb, mmu)
            }
            0xCC => {
                let cond = self.reg.flag(FLAG_Z);
                self.call_cond(mmu, cond)
            }
            0xCD => self.call_cond(mmu, true),
            0xCE => {
                let v = self.fetch_byte(mmu);
                self.adc8(v);
                8
            }
            0xCF => self.rst(mmu, 0x08),

            // ---- 0xD_ ----
            0xD0 => {
                let cond = !self.reg.flag(FLAG_C);
                self.ret_cond(mmu, cond)
            }
            0xD1 => {
                let v = self.pop_word(mmu);
                self.reg.set_de(v);
                12
            }
            0xD2 => {
                let cond = !self.reg.flag(FLAG_C);
                self.jp_cond(mmu, cond)
            }
            0xD3 => 4, // undefined opcode — no-op
            0xD4 => {
                let cond = !self.reg.flag(FLAG_C);
                self.call_cond(mmu, cond)
            }
            0xD5 => {
                let v = self.reg.de();
                self.push_word(mmu, v);
                16
            }
            0xD6 => {
                let v = self.fetch_byte(mmu);
                self.sub8(v);
                8
            }
            0xD7 => self.rst(mmu, 0x10),
            0xD8 => {
                let cond = self.reg.flag(FLAG_C);
                self.ret_cond(mmu, cond)
            }
            0xD9 => {
                // RETI
                self.pc = self.pop_word(mmu);
                self.ime = true;
                16
            }
            0xDA => {
                let cond = self.reg.flag(FLAG_C);
                self.jp_cond(mmu, cond)
            }
            0xDB => 4, // undefined
            0xDC => {
                let cond = self.reg.flag(FLAG_C);
                self.call_cond(mmu, cond)
            }
            0xDD => 4, // undefined
            0xDE => {
                let v = self.fetch_byte(mmu);
                self.sbc8(v);
                8
            }
            0xDF => self.rst(mmu, 0x18),

            // ---- 0xE_ ----
            0xE0 => {
                // LDH (a8),A
                let off = self.fetch_byte(mmu) as u16;
                mmu.write_byte(0xFF00 + off, self.reg.a);
                12
            }
            0xE1 => {
                let v = self.pop_word(mmu);
                self.reg.set_hl(v);
                12
            }
            0xE2 => {
                // LD (C),A
                mmu.write_byte(0xFF00 + self.reg.c as u16, self.reg.a);
                8
            }
            0xE3 => 4, // undefined
            0xE4 => 4, // undefined
            0xE5 => {
                let v = self.reg.hl();
                self.push_word(mmu, v);
                16
            }
            0xE6 => {
                let v = self.fetch_byte(mmu);
                self.and8(v);
                8
            }
            0xE7 => self.rst(mmu, 0x20),
            0xE8 => {
                // ADD SP,e8
                let e8 = self.fetch_byte(mmu);
                self.sp = self.add_sp_e8(e8);
                16
            }
            0xE9 => {
                // JP HL
                self.pc = self.reg.hl();
                4
            }
            0xEA => {
                let addr = self.fetch_word(mmu);
                mmu.write_byte(addr, self.reg.a);
                16
            }
            0xEB => 4, // undefined
            0xEC => 4, // undefined
            0xED => 4, // undefined
            0xEE => {
                let v = self.fetch_byte(mmu);
                self.xor8(v);
                8
            }
            0xEF => self.rst(mmu, 0x28),

            // ---- 0xF_ ----
            0xF0 => {
                // LDH A,(a8)
                let off = self.fetch_byte(mmu) as u16;
                self.reg.a = mmu.read_byte(0xFF00 + off);
                12
            }
            0xF1 => {
                let v = self.pop_word(mmu);
                self.reg.set_af(v); // set_af masks low nibble of F
                12
            }
            0xF2 => {
                // LD A,(C)
                self.reg.a = mmu.read_byte(0xFF00 + self.reg.c as u16);
                8
            }
            0xF3 => {
                self.disable_interrupts();
                4
            }
            0xF4 => 4, // undefined
            0xF5 => {
                let v = self.reg.af();
                self.push_word(mmu, v);
                16
            }
            0xF6 => {
                let v = self.fetch_byte(mmu);
                self.or8(v);
                8
            }
            0xF7 => self.rst(mmu, 0x30),
            0xF8 => {
                // LD HL,SP+e8
                let e8 = self.fetch_byte(mmu);
                let v = self.add_sp_e8(e8);
                self.reg.set_hl(v);
                12
            }
            0xF9 => {
                // LD SP,HL
                self.sp = self.reg.hl();
                8
            }
            0xFA => {
                let addr = self.fetch_word(mmu);
                self.reg.a = mmu.read_byte(addr);
                16
            }
            0xFB => {
                self.schedule_ei();
                4
            }
            0xFC => 4, // undefined
            0xFD => 4, // undefined
            0xFE => {
                let v = self.fetch_byte(mmu);
                self.cp8(v);
                8
            }
            0xFF => self.rst(mmu, 0x38),
        }
    }

    /// LD r,r' block (0x40..=0x7F), including LD r,(HL), LD (HL),r and HALT.
    fn execute_ld_block(&mut self, opcode: u8, mmu: &mut Mmu) -> u32 {
        if opcode == 0x76 {
            self.halt(mmu);
            return 4;
        }
        // Source = low 3 bits, destination = bits 3..5.
        let src = opcode & 0x07;
        let dst = (opcode >> 3) & 0x07;
        let (value, src_is_mem) = self.read_reg_operand(src, mmu);
        let dst_is_mem = dst == 6;
        match dst {
            0 => self.reg.b = value,
            1 => self.reg.c = value,
            2 => self.reg.d = value,
            3 => self.reg.e = value,
            4 => self.reg.h = value,
            5 => self.reg.l = value,
            6 => self.write_hl(mmu, value),
            7 => self.reg.a = value,
            _ => unreachable!(),
        }
        if src_is_mem || dst_is_mem {
            8
        } else {
            4
        }
    }

    /// Read one of the 8 register operands (0=B,1=C,2=D,3=E,4=H,5=L,6=(HL),7=A).
    /// Returns the value and whether the source was memory (for cycle counting).
    fn read_reg_operand(&self, idx: u8, mmu: &Mmu) -> (u8, bool) {
        match idx {
            0 => (self.reg.b, false),
            1 => (self.reg.c, false),
            2 => (self.reg.d, false),
            3 => (self.reg.e, false),
            4 => (self.reg.h, false),
            5 => (self.reg.l, false),
            6 => (self.read_hl(mmu), true),
            7 => (self.reg.a, false),
            _ => unreachable!(),
        }
    }

    /// Execute one CB-prefixed opcode; return its T-cycle cost.
    fn execute_cb(&mut self, opcode: u8, mmu: &mut Mmu) -> u32 {
        let reg = opcode & 0x07; // operand register index
        let op = opcode >> 3; // operation index (0..31)
        let is_mem = reg == 6;

        // Read the operand value.
        let value = match reg {
            0 => self.reg.b,
            1 => self.reg.c,
            2 => self.reg.d,
            3 => self.reg.e,
            4 => self.reg.h,
            5 => self.reg.l,
            6 => self.read_hl(mmu),
            7 => self.reg.a,
            _ => unreachable!(),
        };

        // Compute the result + decide whether it must be written back.
        // op layout: 0..7 = rotate/shift/swap group; 8..15 = BIT; 16..23 = RES;
        // 24..31 = SET.
        let (result, writeback) = match op {
            0 => (self.rlc(value), true),
            1 => (self.rrc(value), true),
            2 => (self.rl(value), true),
            3 => (self.rr(value), true),
            4 => (self.sla(value), true),
            5 => (self.sra(value), true),
            6 => (self.swap(value), true),
            7 => (self.srl(value), true),
            8..=15 => {
                self.bit(op - 8, value);
                (value, false)
            }
            16..=23 => (self.res(op - 16, value), true),
            24..=31 => (self.set(op - 24, value), true),
            _ => unreachable!(),
        };

        if writeback {
            match reg {
                0 => self.reg.b = result,
                1 => self.reg.c = result,
                2 => self.reg.d = result,
                3 => self.reg.e = result,
                4 => self.reg.h = result,
                5 => self.reg.l = result,
                6 => self.write_hl(mmu, result),
                7 => self.reg.a = result,
                _ => unreachable!(),
            }
        }

        // Cycle cost: BIT b,(HL) is 12; other (HL) ops are 16; register ops 8.
        if is_mem {
            if (8..=15).contains(&op) {
                12 // BIT b,(HL)
            } else {
                16
            }
        } else {
            8
        }
    }
}
