//! # ferrisboy-core
//!
//! A Game Boy (DMG) emulator core, written in safe, dependency-free Rust.
//!
//! The core is pure logic: it owns no windows, audio devices, or files. A
//! frontend drives it by calling [`GameBoy::run_frame`] ~60 times a second,
//! blitting [`GameBoy::framebuffer`] to a screen, pumping [`GameBoy::take_audio`]
//! to a speaker, and forwarding input through [`GameBoy::set_button`].
//!
//! ## Architecture
//!
//! ```text
//!   GameBoy
//!   ├── Cpu          Sharp LR35902 — 256 + 256 (CB) opcodes, interrupts, HALT
//!   └── Mmu          the bus: routes every address to the right component
//!       ├── Cartridge    ROM + banked RAM (no-MBC / MBC1 / MBC3 / MBC5)
//!       ├── Ppu          LCD: background, window, sprites -> framebuffer
//!       ├── Apu          4 sound channels -> resampled f32 stream
//!       ├── Timer        DIV / TIMA
//!       ├── Joypad       button matrix
//!       └── Serial       link port (also the Blargg test-ROM output channel)
//! ```
//!
//! Components communicate only through the [`mmu`] bus and a shared interrupt
//! register, so each can be implemented and tested independently.

mod apu;
mod cartridge;
mod cpu;
pub mod interrupts;
mod joypad;
mod mmu;
mod ppu;
mod serial;
mod timer;

use cpu::Cpu;
use mmu::Mmu;

pub use cartridge::CartridgeError;
pub use joypad::Button;
pub use ppu::{SCREEN_HEIGHT, SCREEN_WIDTH};

/// The DMG CPU clock: 2^22 Hz.
pub const CPU_CLOCK_HZ: u32 = 4_194_304;
/// Audio sample rate produced by [`GameBoy::take_audio`].
pub const AUDIO_SAMPLE_RATE: u32 = 44_100;
/// Number of RGBA bytes in a frame produced by [`GameBoy::framebuffer_rgba`].
pub const FRAMEBUFFER_RGBA_LEN: usize = SCREEN_WIDTH * SCREEN_HEIGHT * 4;

/// A fully assembled Game Boy: CPU + bus + all peripherals.
pub struct GameBoy {
    cpu: Cpu,
    mmu: Mmu,
}

impl GameBoy {
    /// Build a Game Boy from cartridge ROM bytes.
    pub fn new(rom: Vec<u8>) -> Result<Self, CartridgeError> {
        Self::with_save(rom, None)
    }

    /// Build a Game Boy, restoring previously saved battery RAM if provided.
    pub fn with_save(rom: Vec<u8>, save: Option<Vec<u8>>) -> Result<Self, CartridgeError> {
        let cartridge = cartridge::Cartridge::new(rom, save)?;
        Ok(Self {
            cpu: Cpu::new(),
            mmu: Mmu::new(cartridge),
        })
    }

    /// Run the system until the PPU finishes one full frame (~70224 T-cycles).
    pub fn run_frame(&mut self) {
        loop {
            let cycles = self.cpu.step(&mut self.mmu);
            self.mmu.tick(cycles);
            if self.mmu.ppu.take_frame_ready() {
                break;
            }
        }
    }

    /// Execute a single CPU instruction and advance peripherals by its duration.
    /// Returns the elapsed T-cycles. Primarily for tests and benchmarks.
    pub fn step(&mut self) -> u32 {
        let cycles = self.cpu.step(&mut self.mmu);
        self.mmu.tick(cycles);
        cycles
    }

    /// The current frame as 160×144 2-bit shade indices (0 = lightest .. 3 = darkest).
    pub fn framebuffer(&self) -> &[u8] {
        self.mmu.ppu.framebuffer()
    }

    /// Write the current frame into `out` as RGBA8888 using the classic DMG
    /// green palette. `out` must be at least [`FRAMEBUFFER_RGBA_LEN`] bytes.
    pub fn framebuffer_rgba(&self, out: &mut [u8]) {
        ppu::shades_to_rgba(self.mmu.ppu.framebuffer(), out);
    }

    /// Press or release a button. Raises a joypad interrupt on a fresh press.
    pub fn set_button(&mut self, button: Button, pressed: bool) {
        if self.mmu.joypad.set_button(button, pressed) {
            self.mmu.request_interrupt(interrupts::JOYPAD);
        }
    }

    /// Drain accumulated audio: interleaved stereo `f32` at [`AUDIO_SAMPLE_RATE`].
    pub fn take_audio(&mut self) -> Vec<f32> {
        self.mmu.apu.take_samples()
    }

    /// Battery-backed cartridge RAM to persist, if the cartridge has a battery.
    pub fn save_data(&self) -> Option<Vec<u8>> {
        self.mmu.cartridge.save_data()
    }

    /// Whether the cartridge RAM has changed since the last [`Self::mark_saved`].
    pub fn save_is_dirty(&self) -> bool {
        self.mmu.cartridge.save_is_dirty()
    }

    /// Acknowledge that the current save RAM has been persisted.
    pub fn mark_saved(&mut self) {
        self.mmu.cartridge.mark_saved();
    }

    /// The cartridge's internal title string (header bytes 0x134..0x143).
    pub fn title(&self) -> &str {
        self.mmu.cartridge.title()
    }

    /// Bytes the ROM has written to the serial port. Blargg test ROMs report
    /// their results here, which is how the headless test harness reads them.
    pub fn take_serial(&mut self) -> Vec<u8> {
        self.mmu.serial.take_output()
    }
}
