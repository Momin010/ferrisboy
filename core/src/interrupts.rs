//! Interrupt bit masks and handler vectors.
//!
//! The same five bits are shared by the IF register (0xFF0F, "requested") and
//! the IE register (0xFFFF, "enabled"). An interrupt fires when its bit is set
//! in both and the CPU's IME flag is on. Lower bit = higher priority.

/// Bit 0 — vertical blank (PPU finished a frame).
pub const VBLANK: u8 = 1 << 0;
/// Bit 1 — LCD STAT (PPU mode/LYC coincidence conditions).
pub const LCD_STAT: u8 = 1 << 1;
/// Bit 2 — timer (TIMA overflowed).
pub const TIMER: u8 = 1 << 2;
/// Bit 3 — serial transfer complete.
pub const SERIAL: u8 = 1 << 3;
/// Bit 4 — joypad (a selected button line went low).
pub const JOYPAD: u8 = 1 << 4;

/// Mask of all real interrupt bits.
pub const ALL: u8 = 0x1F;

/// Handler entry addresses, indexed by bit position (VBlank = bit 0 = 0x40).
pub const VECTORS: [u16; 5] = [0x0040, 0x0048, 0x0050, 0x0058, 0x0060];
