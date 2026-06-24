//! The memory bus.
//!
//! Every CPU memory access goes through [`Mmu::read_byte`] / [`Mmu::write_byte`],
//! which decode the 16-bit address space and route to the right component. This
//! file is the **integration contract**: the method names and signatures it
//! calls on each peripheral are exactly what those peripherals must expose.
//!
//! ## Address map
//! | Range         | Target                                   |
//! |---------------|------------------------------------------|
//! | 0000–7FFF     | Cartridge ROM (banked)                   |
//! | 8000–9FFF     | PPU video RAM                            |
//! | A000–BFFF     | Cartridge external RAM (banked)          |
//! | C000–DFFF     | Work RAM                                 |
//! | E000–FDFF     | Echo of C000–DDFF                        |
//! | FE00–FE9F     | PPU sprite attribute table (OAM)         |
//! | FEA0–FEFF     | Prohibited                               |
//! | FF00          | Joypad                                   |
//! | FF01–FF02     | Serial                                   |
//! | FF04–FF07     | Timer                                    |
//! | FF0F          | IF — interrupt requests                  |
//! | FF10–FF3F     | APU (sound)                              |
//! | FF40–FF4B     | PPU registers (FF46 = OAM DMA, here)     |
//! | FF80–FFFE     | High RAM                                 |
//! | FFFF          | IE — interrupt enable                    |

use crate::apu::Apu;
use crate::cartridge::Cartridge;
use crate::joypad::Joypad;
use crate::ppu::Ppu;
use crate::serial::Serial;
use crate::timer::Timer;

pub struct Mmu {
    pub(crate) cartridge: Cartridge,
    pub(crate) ppu: Ppu,
    pub(crate) apu: Apu,
    pub(crate) timer: Timer,
    pub(crate) joypad: Joypad,
    pub(crate) serial: Serial,

    wram: [u8; 0x2000],
    hram: [u8; 0x7F],

    /// IF (0xFF0F): pending interrupt requests.
    iflags: u8,
    /// IE (0xFFFF): interrupt enable mask.
    ie: u8,
}

impl Mmu {
    pub fn new(cartridge: Cartridge) -> Self {
        Mmu {
            cartridge,
            ppu: Ppu::new(),
            apu: Apu::new(),
            timer: Timer::new(),
            joypad: Joypad::new(),
            serial: Serial::new(),
            wram: [0; 0x2000],
            hram: [0; 0x7F],
            iflags: 0xE1, // post-boot value
            ie: 0x00,
        }
    }

    /// Set interrupt request bit(s) in IF.
    pub fn request_interrupt(&mut self, mask: u8) {
        self.iflags |= mask;
    }

    /// Interrupts that are both requested and enabled (the set the CPU services).
    pub fn pending_interrupts(&self) -> u8 {
        self.iflags & self.ie & crate::interrupts::ALL
    }

    /// Acknowledge (clear) a serviced interrupt's request bit.
    pub fn clear_interrupt(&mut self, mask: u8) {
        self.iflags &= !mask;
    }

    /// Advance every peripheral by `cycles` T-cycles, folding any interrupts
    /// they raise into IF. Called once per CPU instruction.
    pub fn tick(&mut self, cycles: u32) {
        self.iflags |= self.timer.step(cycles);
        self.iflags |= self.ppu.step(cycles);
        self.iflags |= self.serial.step(cycles);
        self.apu.step(cycles);
    }

    pub fn read_byte(&self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x7FFF => self.cartridge.read(addr),
            0x8000..=0x9FFF => self.ppu.read_vram(addr),
            0xA000..=0xBFFF => self.cartridge.read(addr),
            0xC000..=0xDFFF => self.wram[(addr - 0xC000) as usize],
            0xE000..=0xFDFF => self.wram[(addr - 0xE000) as usize], // echo RAM
            0xFE00..=0xFE9F => self.ppu.read_oam(addr),
            0xFEA0..=0xFEFF => 0xFF, // prohibited
            0xFF00 => self.joypad.read(),
            0xFF01..=0xFF02 => self.serial.read_reg(addr),
            0xFF04..=0xFF07 => self.timer.read_reg(addr),
            0xFF0F => self.iflags | 0xE0, // top 3 bits read as 1
            0xFF10..=0xFF3F => self.apu.read_reg(addr),
            0xFF40..=0xFF4B => self.ppu.read_reg(addr),
            0xFF80..=0xFFFE => self.hram[(addr - 0xFF80) as usize],
            0xFFFF => self.ie,
            _ => 0xFF, // unmapped I/O
        }
    }

    pub fn write_byte(&mut self, addr: u16, val: u8) {
        match addr {
            0x0000..=0x7FFF => self.cartridge.write(addr, val),
            0x8000..=0x9FFF => self.ppu.write_vram(addr, val),
            0xA000..=0xBFFF => self.cartridge.write(addr, val),
            0xC000..=0xDFFF => self.wram[(addr - 0xC000) as usize] = val,
            0xE000..=0xFDFF => self.wram[(addr - 0xE000) as usize] = val,
            0xFE00..=0xFE9F => self.ppu.write_oam(addr, val),
            0xFEA0..=0xFEFF => {} // prohibited
            0xFF00 => self.joypad.write(val),
            0xFF01..=0xFF02 => self.serial.write_reg(addr, val),
            0xFF04..=0xFF07 => self.timer.write_reg(addr, val),
            0xFF0F => self.iflags = val & crate::interrupts::ALL,
            0xFF10..=0xFF3F => self.apu.write_reg(addr, val),
            0xFF46 => self.oam_dma(val), // must precede the FF40..=FF4B arm
            0xFF40..=0xFF4B => self.ppu.write_reg(addr, val),
            0xFF80..=0xFFFE => self.hram[(addr - 0xFF80) as usize] = val,
            0xFFFF => self.ie = val,
            _ => {} // unmapped I/O
        }
    }

    /// OAM DMA (FF46): copy 0xA0 bytes from `val << 8` into sprite memory.
    /// Done atomically here, which is accurate enough for the vast majority
    /// of games (they wait the conventional ~160 cycles in HRAM regardless).
    fn oam_dma(&mut self, val: u8) {
        let src = (val as u16) << 8;
        let mut buf = [0u8; 0xA0];
        for (i, b) in buf.iter_mut().enumerate() {
            *b = self.read_byte(src + i as u16);
        }
        for (i, &b) in buf.iter().enumerate() {
            self.ppu.write_oam(0xFE00 + i as u16, b);
        }
    }
}
