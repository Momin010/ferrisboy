//! Sharp LR35902 CPU (a Game Boy-specific 8080/Z80 relative).
//!
//! The register file, flag helpers, fetch/stack helpers, and the
//! interrupt/HALT/EI control flow are implemented here. The two big opcode
//! decoders — [`Cpu::execute`] and [`Cpu::execute_cb`] — are the CPU task's
//! responsibility; they are the only `todo!`s in this module.

use crate::interrupts;
use crate::mmu::Mmu;

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

    /// Execute one unprefixed opcode; return its T-cycle cost.
    fn execute(&mut self, opcode: u8, mmu: &mut Mmu) -> u32 {
        let _ = (opcode, mmu);
        todo!("unprefixed opcode table — see resources/Opcodes.json (unprefixed)")
    }

    /// Execute one CB-prefixed opcode; return its T-cycle cost.
    fn execute_cb(&mut self, opcode: u8, mmu: &mut Mmu) -> u32 {
        let _ = (opcode, mmu);
        todo!("CB-prefixed opcode table — see resources/Opcodes.json (cbprefixed)")
    }
}
