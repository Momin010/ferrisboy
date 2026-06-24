//! Audio Processing Unit (4 sound channels).
//!
//! ## Interface contract (implemented by the APU task)
//! - registers FF10..FF3F via `read_reg`/`write_reg`
//! - `step(cycles)` advances generation
//! - `take_samples()` drains interleaved stereo f32 at `crate::AUDIO_SAMPLE_RATE`

pub struct Apu {
    /// Raw register file for FF10..FF3F (0x30 bytes). Implementations may keep
    /// structured channel state instead and use this only as a fallback.
    regs: [u8; 0x30],
    /// Generated stereo samples, interleaved L,R,L,R…, drained by the frontend.
    output: Vec<f32>,
}

impl Apu {
    pub fn new() -> Self {
        Apu {
            regs: [0; 0x30],
            output: Vec::new(),
        }
    }

    pub fn read_reg(&self, addr: u16) -> u8 {
        self.regs[(addr - 0xFF10) as usize]
    }

    pub fn write_reg(&mut self, addr: u16, val: u8) {
        self.regs[(addr - 0xFF10) as usize] = val;
    }

    /// Advance sound generation by `cycles` T-cycles, pushing resampled stereo
    /// frames into the output buffer. Implemented by the APU task.
    pub fn step(&mut self, _cycles: u32) {}

    /// Drain accumulated stereo samples.
    pub fn take_samples(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.output)
    }
}
