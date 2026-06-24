//! Cartridge: ROM, banked external RAM, and the memory-bank controller (MBC).
//!
//! Supports no-MBC, MBC1, MBC2, MBC3 (RTC registers accepted but not ticked),
//! and MBC5 — covering the overwhelming majority of commercial DMG games.

use std::fmt;

#[derive(Debug)]
pub enum CartridgeError {
    /// ROM is smaller than the 0x150-byte header.
    TooSmall,
    /// Cartridge type byte (0x147) names a mapper we don't implement.
    UnsupportedMapper(u8),
}

impl fmt::Display for CartridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CartridgeError::TooSmall => {
                write!(f, "ROM is too small to contain a valid 0x150-byte header")
            }
            CartridgeError::UnsupportedMapper(b) => {
                write!(f, "unsupported cartridge type byte {b:#04x}")
            }
        }
    }
}

impl std::error::Error for CartridgeError {}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    NoMbc,
    Mbc1,
    Mbc2,
    Mbc3,
    Mbc5,
}

pub struct Cartridge {
    rom: Vec<u8>,
    ram: Vec<u8>,
    kind: Kind,
    title: String,
    has_battery: bool,

    rom_banks: usize,
    ram_banks: usize,

    ram_enabled: bool,
    /// Lower bank-number register (MBC1: 5 bits, MBC5: low 8 bits).
    bank_lo: u16,
    /// Upper bank bits / RAM-bank select (MBC1: 2 bits, MBC5: bit 8).
    bank_hi: u16,
    /// MBC1 banking mode (0 = ROM banking, 1 = RAM/advanced).
    mode: u8,

    save_dirty: bool,
}

impl Cartridge {
    pub fn new(rom: Vec<u8>, save: Option<Vec<u8>>) -> Result<Self, CartridgeError> {
        if rom.len() < 0x150 {
            return Err(CartridgeError::TooSmall);
        }

        let title = parse_title(&rom);
        let type_byte = rom[0x147];
        let (kind, has_battery) = classify(type_byte)?;

        // ROM size: header 0x148 encodes 32 KiB << n; trust the actual length too.
        let header_rom_banks = 2usize << (rom[0x148] as usize).min(8);
        let rom_banks = header_rom_banks.max(rom.len().div_ceil(0x4000)).max(2);

        let ram_len = ram_size_bytes(rom[0x149], kind);
        let ram_banks = if ram_len == 0 { 0 } else { (ram_len / 0x2000).max(1) };
        let mut ram = vec![0u8; ram_len];
        if let Some(saved) = save {
            if saved.len() == ram.len() && !ram.is_empty() {
                ram.copy_from_slice(&saved);
            }
        }

        Ok(Cartridge {
            rom,
            ram,
            kind,
            title,
            has_battery,
            rom_banks,
            ram_banks,
            ram_enabled: false,
            bank_lo: 1,
            bank_hi: 0,
            mode: 0,
            save_dirty: false,
        })
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn save_data(&self) -> Option<Vec<u8>> {
        if self.has_battery && !self.ram.is_empty() {
            Some(self.ram.clone())
        } else {
            None
        }
    }

    pub fn save_is_dirty(&self) -> bool {
        self.save_dirty
    }

    pub fn mark_saved(&mut self) {
        self.save_dirty = false;
    }

    pub fn read(&self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x3FFF => {
                // MBC1 advanced mode can remap this region; the common case is bank 0.
                let bank = if self.kind == Kind::Mbc1 && self.mode == 1 {
                    (self.bank_hi << 5) as usize % self.rom_banks
                } else {
                    0
                };
                self.rom_at(bank, addr as usize)
            }
            0x4000..=0x7FFF => {
                let bank = self.current_rom_bank();
                self.rom_at(bank, addr as usize - 0x4000)
            }
            0xA000..=0xBFFF => self.read_ram(addr),
            _ => 0xFF,
        }
    }

    pub fn write(&mut self, addr: u16, val: u8) {
        match self.kind {
            Kind::NoMbc => {
                if (0xA000..=0xBFFF).contains(&addr) {
                    self.write_ram(addr, val);
                }
            }
            Kind::Mbc1 => self.write_mbc1(addr, val),
            Kind::Mbc2 => self.write_mbc2(addr, val),
            Kind::Mbc3 => self.write_mbc3(addr, val),
            Kind::Mbc5 => self.write_mbc5(addr, val),
        }
    }

    // ---- ROM/RAM access ----

    fn rom_at(&self, bank: usize, offset: usize) -> u8 {
        let idx = bank * 0x4000 + offset;
        self.rom.get(idx).copied().unwrap_or(0xFF)
    }

    fn current_rom_bank(&self) -> usize {
        let bank = match self.kind {
            Kind::Mbc1 => {
                let lo = if self.bank_lo == 0 { 1 } else { self.bank_lo };
                let hi = if self.mode == 0 { self.bank_hi } else { 0 };
                ((hi << 5) | (lo & 0x1F)) as usize
            }
            Kind::Mbc2 => (self.bank_lo as usize & 0x0F).max(1),
            Kind::Mbc3 => (self.bank_lo as usize & 0x7F).max(1),
            Kind::Mbc5 => ((self.bank_hi << 8) | self.bank_lo) as usize,
            Kind::NoMbc => 1,
        };
        bank % self.rom_banks.max(1)
    }

    fn current_ram_bank(&self) -> usize {
        match self.kind {
            Kind::Mbc1 if self.mode == 1 => self.bank_hi as usize,
            Kind::Mbc3 | Kind::Mbc5 => self.bank_hi as usize,
            _ => 0,
        }
    }

    fn read_ram(&self, addr: u16) -> u8 {
        if !self.ram_enabled || self.ram.is_empty() {
            return 0xFF;
        }
        if self.kind == Kind::Mbc2 {
            // 512 × 4-bit; upper nibble reads as 1.
            return 0xF0 | (self.ram[(addr as usize - 0xA000) & 0x1FF] & 0x0F);
        }
        let bank = self.current_ram_bank() % self.ram_banks.max(1);
        let idx = bank * 0x2000 + (addr as usize - 0xA000);
        self.ram.get(idx).copied().unwrap_or(0xFF)
    }

    fn write_ram(&mut self, addr: u16, val: u8) {
        if !self.ram_enabled || self.ram.is_empty() {
            return;
        }
        if self.kind == Kind::Mbc2 {
            let idx = (addr as usize - 0xA000) & 0x1FF;
            self.ram[idx] = val & 0x0F;
            self.save_dirty = true;
            return;
        }
        let bank = self.current_ram_bank() % self.ram_banks.max(1);
        let idx = bank * 0x2000 + (addr as usize - 0xA000);
        if let Some(cell) = self.ram.get_mut(idx) {
            *cell = val;
            self.save_dirty = true;
        }
    }

    // ---- per-mapper register writes ----

    fn write_mbc1(&mut self, addr: u16, val: u8) {
        match addr {
            0x0000..=0x1FFF => self.ram_enabled = val & 0x0F == 0x0A,
            0x2000..=0x3FFF => self.bank_lo = (val & 0x1F) as u16,
            0x4000..=0x5FFF => self.bank_hi = (val & 0x03) as u16,
            0x6000..=0x7FFF => self.mode = val & 0x01,
            0xA000..=0xBFFF => self.write_ram(addr, val),
            _ => {}
        }
    }

    fn write_mbc2(&mut self, addr: u16, val: u8) {
        match addr {
            0x0000..=0x3FFF => {
                // Bit 8 of the address selects RAM-enable vs ROM-bank.
                if addr & 0x0100 == 0 {
                    self.ram_enabled = val & 0x0F == 0x0A;
                } else {
                    self.bank_lo = (val & 0x0F) as u16;
                }
            }
            0xA000..=0xBFFF => self.write_ram(addr, val),
            _ => {}
        }
    }

    fn write_mbc3(&mut self, addr: u16, val: u8) {
        match addr {
            0x0000..=0x1FFF => self.ram_enabled = val & 0x0F == 0x0A,
            0x2000..=0x3FFF => self.bank_lo = (val & 0x7F) as u16,
            0x4000..=0x5FFF => self.bank_hi = val as u16, // 0–3 RAM bank, 8–C RTC reg
            0x6000..=0x7FFF => { /* RTC latch — RTC not yet ticked */ }
            0xA000..=0xBFFF => {
                if self.bank_hi <= 0x03 {
                    self.write_ram(addr, val);
                }
            }
            _ => {}
        }
    }

    fn write_mbc5(&mut self, addr: u16, val: u8) {
        match addr {
            0x0000..=0x1FFF => self.ram_enabled = val & 0x0F == 0x0A,
            0x2000..=0x2FFF => self.bank_lo = (self.bank_lo & 0x100) | val as u16,
            0x3000..=0x3FFF => self.bank_lo = (self.bank_lo & 0x0FF) | ((val as u16 & 1) << 8),
            0x4000..=0x5FFF => self.bank_hi = (val & 0x0F) as u16,
            0xA000..=0xBFFF => self.write_ram(addr, val),
            _ => {}
        }
    }
}

fn parse_title(rom: &[u8]) -> String {
    let bytes = &rom[0x134..0x144];
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end])
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .collect()
}

fn classify(type_byte: u8) -> Result<(Kind, bool), CartridgeError> {
    Ok(match type_byte {
        0x00 => (Kind::NoMbc, false),
        0x01 => (Kind::Mbc1, false),
        0x02 => (Kind::Mbc1, false),
        0x03 => (Kind::Mbc1, true),
        0x05 => (Kind::Mbc2, false),
        0x06 => (Kind::Mbc2, true),
        0x08 => (Kind::NoMbc, false),
        0x09 => (Kind::NoMbc, true),
        0x0F | 0x10 | 0x13 => (Kind::Mbc3, true),
        0x11 | 0x12 => (Kind::Mbc3, false),
        0x19 | 0x1A | 0x1C | 0x1D => (Kind::Mbc5, false),
        0x1B | 0x1E => (Kind::Mbc5, true),
        other => return Err(CartridgeError::UnsupportedMapper(other)),
    })
}

fn ram_size_bytes(code: u8, kind: Kind) -> usize {
    if kind == Kind::Mbc2 {
        return 512; // built-in 512 × 4-bit
    }
    match code {
        0x00 => 0,
        0x01 => 2 * 1024,
        0x02 => 8 * 1024,
        0x03 => 32 * 1024,
        0x04 => 128 * 1024,
        0x05 => 64 * 1024,
        _ => 0,
    }
}
