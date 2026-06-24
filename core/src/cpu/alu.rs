//! ALU helper methods for the LR35902 CPU.
//!
//! These factor the flag-setting arithmetic/logic/rotate primitives shared
//! across the opcode tables so that the carry/half-carry bookkeeping lives in
//! exactly one place per operation (Blargg is unforgiving about flag bugs).

use super::{Cpu, FLAG_C, FLAG_H, FLAG_N, FLAG_Z};

impl Cpu {
    // ---- 8-bit arithmetic ----

    /// A = A + value. H from bit 3, C from bit 7. N=0.
    pub(super) fn add8(&mut self, value: u8) {
        let a = self.reg.a;
        let result = a.wrapping_add(value);
        self.reg.set_flag(FLAG_Z, result == 0);
        self.reg.set_flag(FLAG_N, false);
        self.reg.set_flag(FLAG_H, (a & 0x0F) + (value & 0x0F) > 0x0F);
        self.reg.set_flag(FLAG_C, (a as u16) + (value as u16) > 0xFF);
        self.reg.a = result;
    }

    /// A = A + value + carry-in.
    pub(super) fn adc8(&mut self, value: u8) {
        let a = self.reg.a;
        let carry = self.reg.flag(FLAG_C) as u8;
        let result = a.wrapping_add(value).wrapping_add(carry);
        self.reg.set_flag(FLAG_Z, result == 0);
        self.reg.set_flag(FLAG_N, false);
        self.reg
            .set_flag(FLAG_H, (a & 0x0F) + (value & 0x0F) + carry > 0x0F);
        self.reg
            .set_flag(FLAG_C, (a as u16) + (value as u16) + (carry as u16) > 0xFF);
        self.reg.a = result;
    }

    /// A = A - value. H = borrow from bit 4; C = borrow. N=1.
    pub(super) fn sub8(&mut self, value: u8) {
        let a = self.reg.a;
        let result = a.wrapping_sub(value);
        self.reg.set_flag(FLAG_Z, result == 0);
        self.reg.set_flag(FLAG_N, true);
        self.reg.set_flag(FLAG_H, (a & 0x0F) < (value & 0x0F));
        self.reg.set_flag(FLAG_C, (a as u16) < (value as u16));
        self.reg.a = result;
    }

    /// A = A - value - carry-in.
    pub(super) fn sbc8(&mut self, value: u8) {
        let a = self.reg.a;
        let carry = self.reg.flag(FLAG_C) as u8;
        let result = a.wrapping_sub(value).wrapping_sub(carry);
        self.reg.set_flag(FLAG_Z, result == 0);
        self.reg.set_flag(FLAG_N, true);
        self.reg
            .set_flag(FLAG_H, (a & 0x0F) < (value & 0x0F) + carry);
        self.reg
            .set_flag(FLAG_C, (a as u16) < (value as u16) + (carry as u16));
        self.reg.a = result;
    }

    /// Compare: like SUB but discards the result, keeping only flags.
    pub(super) fn cp8(&mut self, value: u8) {
        let a = self.reg.a;
        let result = a.wrapping_sub(value);
        self.reg.set_flag(FLAG_Z, result == 0);
        self.reg.set_flag(FLAG_N, true);
        self.reg.set_flag(FLAG_H, (a & 0x0F) < (value & 0x0F));
        self.reg.set_flag(FLAG_C, (a as u16) < (value as u16));
    }

    // ---- 8-bit logic ----

    pub(super) fn and8(&mut self, value: u8) {
        self.reg.a &= value;
        let z = self.reg.a == 0;
        self.reg.set_flag(FLAG_Z, z);
        self.reg.set_flag(FLAG_N, false);
        self.reg.set_flag(FLAG_H, true);
        self.reg.set_flag(FLAG_C, false);
    }

    pub(super) fn or8(&mut self, value: u8) {
        self.reg.a |= value;
        let z = self.reg.a == 0;
        self.reg.set_flag(FLAG_Z, z);
        self.reg.set_flag(FLAG_N, false);
        self.reg.set_flag(FLAG_H, false);
        self.reg.set_flag(FLAG_C, false);
    }

    pub(super) fn xor8(&mut self, value: u8) {
        self.reg.a ^= value;
        let z = self.reg.a == 0;
        self.reg.set_flag(FLAG_Z, z);
        self.reg.set_flag(FLAG_N, false);
        self.reg.set_flag(FLAG_H, false);
        self.reg.set_flag(FLAG_C, false);
    }

    // ---- INC / DEC (8-bit; do NOT touch C) ----

    pub(super) fn inc8(&mut self, value: u8) -> u8 {
        let result = value.wrapping_add(1);
        self.reg.set_flag(FLAG_Z, result == 0);
        self.reg.set_flag(FLAG_N, false);
        self.reg.set_flag(FLAG_H, (value & 0x0F) == 0x0F);
        result
    }

    pub(super) fn dec8(&mut self, value: u8) -> u8 {
        let result = value.wrapping_sub(1);
        self.reg.set_flag(FLAG_Z, result == 0);
        self.reg.set_flag(FLAG_N, true);
        self.reg.set_flag(FLAG_H, (value & 0x0F) == 0x00);
        result
    }

    // ---- 16-bit arithmetic ----

    /// HL = HL + value. N=0; H from bit 11; C from bit 15; Z unchanged.
    pub(super) fn add16_hl(&mut self, value: u16) {
        let hl = self.reg.hl();
        let result = hl.wrapping_add(value);
        self.reg.set_flag(FLAG_N, false);
        self.reg
            .set_flag(FLAG_H, (hl & 0x0FFF) + (value & 0x0FFF) > 0x0FFF);
        self.reg
            .set_flag(FLAG_C, (hl as u32) + (value as u32) > 0xFFFF);
        self.reg.set_hl(result);
    }

    /// SP + signed e8 (used by ADD SP,e8 and LD HL,SP+e8).
    /// Z=0, N=0; H from bit 3, C from bit 7 of the unsigned low-byte add.
    pub(super) fn add_sp_e8(&mut self, e8: u8) -> u16 {
        let sp = self.sp;
        let offset = e8 as i8 as i16 as u16;
        let result = sp.wrapping_add(offset);
        self.reg.set_flag(FLAG_Z, false);
        self.reg.set_flag(FLAG_N, false);
        self.reg
            .set_flag(FLAG_H, (sp & 0x0F) + (e8 as u16 & 0x0F) > 0x0F);
        self.reg
            .set_flag(FLAG_C, (sp & 0xFF) + (e8 as u16 & 0xFF) > 0xFF);
        result
    }

    // ---- rotates: "A" variants (Z always 0) ----

    pub(super) fn rlca(&mut self) {
        let a = self.reg.a;
        let carry = a >> 7;
        self.reg.a = (a << 1) | carry;
        self.reg.set_flag(FLAG_Z, false);
        self.reg.set_flag(FLAG_N, false);
        self.reg.set_flag(FLAG_H, false);
        self.reg.set_flag(FLAG_C, carry != 0);
    }

    pub(super) fn rrca(&mut self) {
        let a = self.reg.a;
        let carry = a & 1;
        self.reg.a = (a >> 1) | (carry << 7);
        self.reg.set_flag(FLAG_Z, false);
        self.reg.set_flag(FLAG_N, false);
        self.reg.set_flag(FLAG_H, false);
        self.reg.set_flag(FLAG_C, carry != 0);
    }

    pub(super) fn rla(&mut self) {
        let a = self.reg.a;
        let old_carry = self.reg.flag(FLAG_C) as u8;
        let carry = a >> 7;
        self.reg.a = (a << 1) | old_carry;
        self.reg.set_flag(FLAG_Z, false);
        self.reg.set_flag(FLAG_N, false);
        self.reg.set_flag(FLAG_H, false);
        self.reg.set_flag(FLAG_C, carry != 0);
    }

    pub(super) fn rra(&mut self) {
        let a = self.reg.a;
        let old_carry = self.reg.flag(FLAG_C) as u8;
        let carry = a & 1;
        self.reg.a = (a >> 1) | (old_carry << 7);
        self.reg.set_flag(FLAG_Z, false);
        self.reg.set_flag(FLAG_N, false);
        self.reg.set_flag(FLAG_H, false);
        self.reg.set_flag(FLAG_C, carry != 0);
    }

    // ---- CB rotates/shifts (Z from result, N=0, H=0, C = shifted-out bit) ----

    pub(super) fn rlc(&mut self, value: u8) -> u8 {
        let carry = value >> 7;
        let result = (value << 1) | carry;
        self.set_shift_flags(result, carry);
        result
    }

    pub(super) fn rrc(&mut self, value: u8) -> u8 {
        let carry = value & 1;
        let result = (value >> 1) | (carry << 7);
        self.set_shift_flags(result, carry);
        result
    }

    pub(super) fn rl(&mut self, value: u8) -> u8 {
        let old_carry = self.reg.flag(FLAG_C) as u8;
        let carry = value >> 7;
        let result = (value << 1) | old_carry;
        self.set_shift_flags(result, carry);
        result
    }

    pub(super) fn rr(&mut self, value: u8) -> u8 {
        let old_carry = self.reg.flag(FLAG_C) as u8;
        let carry = value & 1;
        let result = (value >> 1) | (old_carry << 7);
        self.set_shift_flags(result, carry);
        result
    }

    pub(super) fn sla(&mut self, value: u8) -> u8 {
        let carry = value >> 7;
        let result = value << 1;
        self.set_shift_flags(result, carry);
        result
    }

    pub(super) fn sra(&mut self, value: u8) -> u8 {
        let carry = value & 1;
        // arithmetic shift: preserve the sign bit (bit 7).
        let result = (value >> 1) | (value & 0x80);
        self.set_shift_flags(result, carry);
        result
    }

    pub(super) fn srl(&mut self, value: u8) -> u8 {
        let carry = value & 1;
        let result = value >> 1;
        self.set_shift_flags(result, carry);
        result
    }

    pub(super) fn swap(&mut self, value: u8) -> u8 {
        let result = (value >> 4) | (value << 4);
        self.reg.set_flag(FLAG_Z, result == 0);
        self.reg.set_flag(FLAG_N, false);
        self.reg.set_flag(FLAG_H, false);
        self.reg.set_flag(FLAG_C, false);
        result
    }

    /// Common flag update for CB rotate/shift ops.
    fn set_shift_flags(&mut self, result: u8, carry: u8) {
        self.reg.set_flag(FLAG_Z, result == 0);
        self.reg.set_flag(FLAG_N, false);
        self.reg.set_flag(FLAG_H, false);
        self.reg.set_flag(FLAG_C, carry != 0);
    }

    // ---- bit ops ----

    /// BIT b,r: Z = NOT(bit b); N=0; H=1; C unchanged.
    pub(super) fn bit(&mut self, bit: u8, value: u8) {
        let set = value & (1 << bit) != 0;
        self.reg.set_flag(FLAG_Z, !set);
        self.reg.set_flag(FLAG_N, false);
        self.reg.set_flag(FLAG_H, true);
    }

    pub(super) fn res(&self, bit: u8, value: u8) -> u8 {
        value & !(1 << bit)
    }

    pub(super) fn set(&self, bit: u8, value: u8) -> u8 {
        value | (1 << bit)
    }

    // ---- DAA / CPL / SCF / CCF ----

    /// Decimal-adjust A after a BCD add/sub using the N/H/C flags.
    pub(super) fn daa(&mut self) {
        let mut a = self.reg.a;
        let mut adjust = 0u8;
        let mut carry = false;
        if self.reg.flag(FLAG_N) {
            // After a subtraction.
            if self.reg.flag(FLAG_H) {
                adjust |= 0x06;
            }
            if self.reg.flag(FLAG_C) {
                adjust |= 0x60;
                carry = true;
            }
            a = a.wrapping_sub(adjust);
        } else {
            // After an addition.
            if self.reg.flag(FLAG_H) || (a & 0x0F) > 0x09 {
                adjust |= 0x06;
            }
            if self.reg.flag(FLAG_C) || a > 0x99 {
                adjust |= 0x60;
                carry = true;
            }
            a = a.wrapping_add(adjust);
        }
        self.reg.a = a;
        self.reg.set_flag(FLAG_Z, a == 0);
        self.reg.set_flag(FLAG_H, false);
        self.reg.set_flag(FLAG_C, carry);
    }

    pub(super) fn cpl(&mut self) {
        self.reg.a = !self.reg.a;
        self.reg.set_flag(FLAG_N, true);
        self.reg.set_flag(FLAG_H, true);
    }

    pub(super) fn scf(&mut self) {
        self.reg.set_flag(FLAG_N, false);
        self.reg.set_flag(FLAG_H, false);
        self.reg.set_flag(FLAG_C, true);
    }

    pub(super) fn ccf(&mut self) {
        let c = self.reg.flag(FLAG_C);
        self.reg.set_flag(FLAG_N, false);
        self.reg.set_flag(FLAG_H, false);
        self.reg.set_flag(FLAG_C, !c);
    }
}
