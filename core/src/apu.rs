//! Audio Processing Unit (4 sound channels).
//!
//! ## Interface contract (implemented by the APU task)
//! - registers FF10..FF3F via `read_reg`/`write_reg`
//! - `step(cycles)` advances generation
//! - `take_samples()` drains interleaved stereo f32 at `crate::AUDIO_SAMPLE_RATE`
//!
//! This is a cycle-driven DMG (Game Boy) APU. Internally it keeps one struct
//! per channel plus a frame sequencer; the flat register file required by the
//! original interface is gone — `read_reg`/`write_reg` reconstruct register
//! values on demand and honour the per-register read masks the dmg_sound test
//! ROMs check.
//!
//! The fundamental clocks:
//! - CPU/APU master clock: 4_194_304 Hz (T-cycles).
//! - Frame sequencer: 512 Hz (every 8192 T-cycles), driving length counters
//!   (256 Hz), volume envelopes (64 Hz) and the sweep unit (128 Hz).
//! - Each channel has a frequency timer clocked off the master clock that steps
//!   its waveform generator.

use crate::{AUDIO_SAMPLE_RATE, CPU_CLOCK_HZ};

/// Square-wave duty patterns (NRx1 bits 6-7). Each is 8 entries; the channel's
/// frequency timer advances a position 0..8 through the selected row.
const DUTY_TABLE: [[u8; 8]; 4] = [
    [0, 0, 0, 0, 0, 0, 0, 1], // 12.5%
    [1, 0, 0, 0, 0, 0, 0, 1], // 25%
    [1, 0, 0, 0, 0, 1, 1, 1], // 50%
    [0, 1, 1, 1, 1, 1, 1, 0], // 75%
];

/// Divisor lookup for the noise channel (NR43 low 3 bits). Index 0 behaves as
/// 0.5, encoded here as 8 with the period formula `divisor << shift`.
const NOISE_DIVISORS: [u32; 8] = [8, 16, 32, 48, 64, 80, 96, 112];

/// Maximum buffered stereo samples (~1 second). Prevents unbounded growth if a
/// caller never drains `take_samples()`.
const MAX_BUFFERED_SAMPLES: usize = (AUDIO_SAMPLE_RATE as usize) * 2;

// ---------------------------------------------------------------------------
// Volume envelope (channels 1, 2, 4)
// ---------------------------------------------------------------------------

#[derive(Default, Clone)]
struct Envelope {
    /// Initial volume loaded on trigger (NRx2 bits 4-7).
    initial_volume: u8,
    /// Direction: true = increase, false = decrease (NRx2 bit 3).
    increase: bool,
    /// Envelope period (NRx2 bits 0-2). 0 disables the sweep.
    period: u8,
    /// Current output volume 0..15.
    volume: u8,
    /// Countdown timer for the envelope step.
    timer: u8,
}

impl Envelope {
    /// Load from an NRx2 write.
    fn write(&mut self, val: u8) {
        self.initial_volume = val >> 4;
        self.increase = val & 0x08 != 0;
        self.period = val & 0x07;
    }

    fn read(&self) -> u8 {
        (self.initial_volume << 4) | ((self.increase as u8) << 3) | self.period
    }

    /// DAC is enabled when the upper 5 bits of NRx2 are non-zero (volume != 0
    /// or increase direction set).
    fn dac_on(&self) -> bool {
        self.initial_volume != 0 || self.increase
    }

    fn trigger(&mut self) {
        self.volume = self.initial_volume;
        // A period of 0 reloads the timer to 8 internally on hardware.
        self.timer = if self.period == 0 { 8 } else { self.period };
    }

    /// Clocked at 64 Hz by the frame sequencer.
    fn clock(&mut self) {
        if self.period == 0 {
            return;
        }
        if self.timer > 0 {
            self.timer -= 1;
        }
        if self.timer == 0 {
            self.timer = self.period;
            if self.increase && self.volume < 15 {
                self.volume += 1;
            } else if !self.increase && self.volume > 0 {
                self.volume -= 1;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Length counter (all channels)
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct LengthCounter {
    /// Remaining length ticks.
    counter: u16,
    /// Maximum length value (64 for square/noise, 256 for wave).
    max: u16,
    /// NRx4 bit 6 — when set, the length counter disables the channel at 0.
    enabled: bool,
}

impl LengthCounter {
    fn new(max: u16) -> Self {
        LengthCounter {
            counter: 0,
            max,
            enabled: false,
        }
    }

    /// Load the length value. The stored counter is `max - load`.
    fn set_length(&mut self, load: u16) {
        self.counter = self.max - load;
    }

    /// Clocked at 256 Hz. Returns true if the channel should be disabled this
    /// tick (counter just reached 0 while enabled).
    fn clock(&mut self) -> bool {
        if self.enabled && self.counter > 0 {
            self.counter -= 1;
            if self.counter == 0 {
                return true;
            }
        }
        false
    }
}

// ---------------------------------------------------------------------------
// Channel 1: square + frequency sweep
// ---------------------------------------------------------------------------

struct SquareChannel {
    enabled: bool,
    dac_on: bool,

    // Frequency: 11-bit value split across NRx3 (low 8) / NRx4 (high 3).
    frequency: u16,
    timer: i32,
    duty: u8,
    duty_pos: usize,

    envelope: Envelope,
    length: LengthCounter,

    // Sweep (channel 1 only; unused on channel 2).
    has_sweep: bool,
    sweep_period: u8,
    sweep_negate: bool,
    sweep_shift: u8,
    sweep_enabled: bool,
    sweep_timer: u8,
    sweep_shadow: u16,
    /// Set once a negate-mode calculation has happened since the last trigger;
    /// clearing negate afterwards disables the channel (a documented quirk the
    /// 04/06 sweep tests check).
    sweep_negate_calculated: bool,

    /// NRx1 raw duty/length byte cache for the readback path.
    nrx1_raw: u8,
}

impl SquareChannel {
    fn new(has_sweep: bool) -> Self {
        SquareChannel {
            enabled: false,
            dac_on: false,
            frequency: 0,
            timer: 0,
            duty: 0,
            duty_pos: 0,
            envelope: Envelope::default(),
            length: LengthCounter::new(64),
            has_sweep,
            sweep_period: 0,
            sweep_negate: false,
            sweep_shift: 0,
            sweep_enabled: false,
            sweep_timer: 0,
            sweep_shadow: 0,
            sweep_negate_calculated: false,
            nrx1_raw: 0,
        }
    }

    /// Advance the frequency timer by `cycles` T-cycles, stepping the duty
    /// position when it underflows. The square timer period is
    /// `(2048 - frequency) * 4`.
    fn step(&mut self, cycles: i32) {
        self.timer -= cycles;
        while self.timer <= 0 {
            let period = (2048 - self.frequency as i32) * 4;
            self.timer += period.max(1);
            self.duty_pos = (self.duty_pos + 1) & 7;
        }
    }

    /// Current DAC input: 0 or `volume` depending on the duty waveform.
    fn output(&self) -> u8 {
        if !self.enabled || !self.dac_on {
            return 0;
        }
        if DUTY_TABLE[self.duty as usize][self.duty_pos] == 1 {
            self.envelope.volume
        } else {
            0
        }
    }

    // --- Sweep helpers (channel 1) ---

    /// Compute the next sweep frequency. May disable the channel on overflow.
    fn sweep_calculate(&mut self) -> u16 {
        let delta = self.sweep_shadow >> self.sweep_shift;
        let new_freq = if self.sweep_negate {
            self.sweep_negate_calculated = true;
            self.sweep_shadow.wrapping_sub(delta)
        } else {
            self.sweep_shadow + delta
        };
        if new_freq > 2047 {
            self.enabled = false;
        }
        new_freq
    }

    /// Clocked at 128 Hz.
    fn sweep_clock(&mut self) {
        if !self.has_sweep {
            return;
        }
        if self.sweep_timer > 0 {
            self.sweep_timer -= 1;
        }
        if self.sweep_timer == 0 {
            self.sweep_timer = if self.sweep_period == 0 {
                8
            } else {
                self.sweep_period
            };
            if self.sweep_enabled && self.sweep_period != 0 {
                let new_freq = self.sweep_calculate();
                if new_freq <= 2047 && self.sweep_shift != 0 {
                    self.sweep_shadow = new_freq;
                    self.frequency = new_freq;
                    // A second overflow check is performed (and can disable).
                    self.sweep_calculate();
                }
            }
        }
    }

    fn trigger(&mut self) {
        self.enabled = true;
        if self.length.counter == 0 {
            self.length.counter = self.length.max;
        }
        self.timer = ((2048 - self.frequency as i32) * 4).max(1);
        self.envelope.trigger();

        if self.has_sweep {
            self.sweep_shadow = self.frequency;
            self.sweep_timer = if self.sweep_period == 0 {
                8
            } else {
                self.sweep_period
            };
            self.sweep_enabled = self.sweep_period != 0 || self.sweep_shift != 0;
            self.sweep_negate_calculated = false;
            // The initial calculation runs immediately if shift != 0 and can
            // disable the channel on overflow.
            if self.sweep_shift != 0 {
                self.sweep_calculate();
            }
        }

        if !self.dac_on {
            self.enabled = false;
        }
    }
}

// ---------------------------------------------------------------------------
// Channel 3: wave
// ---------------------------------------------------------------------------

struct WaveChannel {
    enabled: bool,
    /// NR30 bit 7 — wave DAC power.
    dac_on: bool,
    frequency: u16,
    timer: i32,
    /// Position 0..32 into the wave RAM nibbles.
    position: usize,
    /// NR32 bits 5-6 — output level shift code.
    volume_code: u8,
    length: LengthCounter,
    /// 16 bytes = 32 4-bit samples.
    wave_ram: [u8; 16],
    /// Last nibble read out; exposed via the wave-RAM read quirk while on.
    sample_buffer: u8,
}

impl WaveChannel {
    fn new() -> Self {
        WaveChannel {
            enabled: false,
            dac_on: false,
            frequency: 0,
            timer: 0,
            position: 0,
            volume_code: 0,
            length: LengthCounter::new(256),
            wave_ram: [0; 16],
            sample_buffer: 0,
        }
    }

    /// Wave timer period is `(2048 - frequency) * 2`.
    fn step(&mut self, cycles: i32) {
        self.timer -= cycles;
        while self.timer <= 0 {
            let period = (2048 - self.frequency as i32) * 2;
            self.timer += period.max(1);
            self.position = (self.position + 1) & 31;
            let byte = self.wave_ram[self.position / 2];
            self.sample_buffer = if self.position & 1 == 0 {
                byte >> 4
            } else {
                byte & 0x0F
            };
        }
    }

    fn output(&self) -> u8 {
        if !self.enabled || !self.dac_on {
            return 0;
        }
        match self.volume_code {
            0 => 0,
            1 => self.sample_buffer,
            2 => self.sample_buffer >> 1,
            _ => self.sample_buffer >> 2,
        }
    }

    fn trigger(&mut self) {
        self.enabled = true;
        if self.length.counter == 0 {
            self.length.counter = self.length.max;
        }
        self.timer = ((2048 - self.frequency as i32) * 2).max(1);
        self.position = 0;
        if !self.dac_on {
            self.enabled = false;
        }
    }
}

// ---------------------------------------------------------------------------
// Channel 4: noise (LFSR)
// ---------------------------------------------------------------------------

struct NoiseChannel {
    enabled: bool,
    dac_on: bool,
    timer: i32,
    /// 15-bit linear-feedback shift register; starts all-ones on trigger.
    lfsr: u16,
    /// NR43 bits 4-7 — clock shift.
    clock_shift: u8,
    /// NR43 bit 3 — width mode (true = 7-bit).
    width_7bit: bool,
    /// NR43 bits 0-2 — divisor code.
    divisor_code: u8,
    envelope: Envelope,
    length: LengthCounter,
}

impl NoiseChannel {
    fn new() -> Self {
        NoiseChannel {
            enabled: false,
            dac_on: false,
            timer: 0,
            lfsr: 0x7FFF,
            clock_shift: 0,
            width_7bit: false,
            divisor_code: 0,
            envelope: Envelope::default(),
            length: LengthCounter::new(64),
        }
    }

    fn period(&self) -> i32 {
        (NOISE_DIVISORS[self.divisor_code as usize] << self.clock_shift) as i32
    }

    fn step(&mut self, cycles: i32) {
        self.timer -= cycles;
        while self.timer <= 0 {
            self.timer += self.period().max(1);
            // XOR the two low bits, feed back into bit 14 (and bit 6 in 7-bit
            // mode).
            let bit = (self.lfsr & 1) ^ ((self.lfsr >> 1) & 1);
            self.lfsr >>= 1;
            self.lfsr |= bit << 14;
            if self.width_7bit {
                self.lfsr &= !(1 << 6);
                self.lfsr |= bit << 6;
            }
        }
    }

    fn output(&self) -> u8 {
        if !self.enabled || !self.dac_on {
            return 0;
        }
        // Output is high when the low bit is 0.
        if self.lfsr & 1 == 0 {
            self.envelope.volume
        } else {
            0
        }
    }

    fn trigger(&mut self) {
        self.enabled = true;
        if self.length.counter == 0 {
            self.length.counter = self.length.max;
        }
        self.timer = self.period().max(1);
        self.lfsr = 0x7FFF;
        self.envelope.trigger();
        if !self.dac_on {
            self.enabled = false;
        }
    }
}

// ---------------------------------------------------------------------------
// The APU
// ---------------------------------------------------------------------------

pub struct Apu {
    /// Master power (NR52 bit 7).
    powered: bool,

    ch1: SquareChannel,
    ch2: SquareChannel,
    ch3: WaveChannel,
    ch4: NoiseChannel,

    // Control registers.
    /// NR50: bit 7 VIN->L, bits 4-6 left volume, bit 3 VIN->R, bits 0-2 right.
    nr50: u8,
    /// NR51: per-channel L/R routing.
    nr51: u8,

    /// Frame sequencer step 0..8, advanced every 8192 T-cycles.
    fs_step: u8,
    /// T-cycle countdown to the next frame-sequencer tick.
    fs_timer: u32,

    /// Fractional accumulator for resampling to AUDIO_SAMPLE_RATE.
    sample_accumulator: f32,
    /// Cycles per output sample (≈ 95.11).
    cycles_per_sample: f32,

    /// Generated stereo samples, interleaved L,R,L,R…, drained by the frontend.
    output: Vec<f32>,
}

impl Apu {
    pub fn new() -> Self {
        Apu {
            powered: false,
            ch1: SquareChannel::new(true),
            ch2: SquareChannel::new(false),
            ch3: WaveChannel::new(),
            ch4: NoiseChannel::new(),
            nr50: 0,
            nr51: 0,
            fs_step: 0,
            fs_timer: 8192,
            sample_accumulator: 0.0,
            cycles_per_sample: CPU_CLOCK_HZ as f32 / AUDIO_SAMPLE_RATE as f32,
            output: Vec::new(),
        }
    }

    // -- Register read -----------------------------------------------------

    pub fn read_reg(&self, addr: u16) -> u8 {
        // Wave RAM is always readable (FF30..FF3F).
        if (0xFF30..=0xFF3F).contains(&addr) {
            return self.read_wave_ram(addr);
        }

        // Each register ORs in the bits that read back as 1.
        match addr {
            0xFF10 => self.read_nr10(),       // NR10
            0xFF11 => self.ch1.nrx1_raw | 0x3F, // NR11: only duty readable
            0xFF12 => self.ch1.envelope.read(), // NR12
            0xFF13 => 0xFF,                   // NR13 write-only
            0xFF14 => self.read_nrx4(&self.ch1.length), // NR14
            0xFF15 => 0xFF,                   // unused
            0xFF16 => self.ch2.nrx1_raw | 0x3F, // NR21
            0xFF17 => self.ch2.envelope.read(), // NR22
            0xFF18 => 0xFF,                   // NR23 write-only
            0xFF19 => self.read_nrx4(&self.ch2.length), // NR24
            0xFF1A => ((self.ch3.dac_on as u8) << 7) | 0x7F, // NR30
            0xFF1B => 0xFF,                   // NR31 write-only
            0xFF1C => (self.ch3.volume_code << 5) | 0x9F, // NR32
            0xFF1D => 0xFF,                   // NR33 write-only
            0xFF1E => self.read_nrx4(&self.ch3.length), // NR34
            0xFF1F => 0xFF,                   // unused
            0xFF20 => 0xFF,                   // NR41 write-only (length)
            0xFF21 => self.ch4.envelope.read(), // NR42
            0xFF22 => self.read_nr43(),       // NR43
            0xFF23 => self.read_nrx4(&self.ch4.length), // NR44
            0xFF24 => self.nr50,              // NR50 (all readable)
            0xFF25 => self.nr51,              // NR51 (all readable)
            0xFF26 => self.read_nr52(),       // NR52
            // FF27..FF2F unused.
            0xFF27..=0xFF2F => 0xFF,
            _ => 0xFF,
        }
    }

    fn read_nr10(&self) -> u8 {
        0x80 | (self.ch1.sweep_period << 4)
            | ((self.ch1.sweep_negate as u8) << 3)
            | self.ch1.sweep_shift
    }

    /// NRx4 readback: only bit 6 (length enable) is meaningful; everything else
    /// reads as 1.
    fn read_nrx4(&self, len: &LengthCounter) -> u8 {
        0xBF | ((len.enabled as u8) << 6)
    }

    fn read_nr43(&self) -> u8 {
        (self.ch4.clock_shift << 4)
            | ((self.ch4.width_7bit as u8) << 3)
            | self.ch4.divisor_code
    }

    fn read_nr52(&self) -> u8 {
        let mut v = 0x70; // bits 4-6 read as 1
        if self.powered {
            v |= 0x80;
        }
        if self.ch1.enabled {
            v |= 0x01;
        }
        if self.ch2.enabled {
            v |= 0x02;
        }
        if self.ch3.enabled {
            v |= 0x04;
        }
        if self.ch4.enabled {
            v |= 0x08;
        }
        v
    }

    fn read_wave_ram(&self, addr: u16) -> u8 {
        let idx = (addr - 0xFF30) as usize;
        // While channel 3 is playing, the CPU sees the byte currently being
        // read (DMG returns 0xFF except in the brief window after a read; we
        // approximate the common-case behaviour of returning the active byte).
        if self.ch3.enabled && self.ch3.dac_on {
            self.ch3.wave_ram[self.ch3.position / 2]
        } else {
            self.ch3.wave_ram[idx]
        }
    }

    // -- Register write ----------------------------------------------------

    pub fn write_reg(&mut self, addr: u16, val: u8) {
        // Wave RAM is writable regardless of power.
        if (0xFF30..=0xFF3F).contains(&addr) {
            self.write_wave_ram(addr, val);
            return;
        }

        // NR52 is always writable (it controls power).
        if addr == 0xFF26 {
            self.write_nr52(val);
            return;
        }

        // While powered off, all other register writes are ignored on DMG.
        // (Length-load registers are writable while off on CGB, but DMG
        // dmg_sound test 08 expects them ignored.)
        if !self.powered {
            return;
        }

        match addr {
            0xFF10 => self.write_nr10(val),
            0xFF11 => self.write_square_nrx1(false, val),
            0xFF12 => self.write_square_nrx2(false, val),
            0xFF13 => self.write_square_nrx3(false, val),
            0xFF14 => self.write_square_nrx4(false, val),
            0xFF16 => self.write_square_nrx1(true, val),
            0xFF17 => self.write_square_nrx2(true, val),
            0xFF18 => self.write_square_nrx3(true, val),
            0xFF19 => self.write_square_nrx4(true, val),
            0xFF1A => self.write_nr30(val),
            0xFF1B => self.write_nr31(val),
            0xFF1C => self.write_nr32(val),
            0xFF1D => self.write_nr33(val),
            0xFF1E => self.write_nr34(val),
            0xFF20 => self.write_nr41(val),
            0xFF21 => self.write_nr42(val),
            0xFF22 => self.write_nr43(val),
            0xFF23 => self.write_nr44(val),
            0xFF24 => self.nr50 = val,
            0xFF25 => self.nr51 = val,
            _ => {}
        }
    }

    fn write_wave_ram(&mut self, addr: u16, val: u8) {
        let idx = (addr - 0xFF30) as usize;
        if self.ch3.enabled && self.ch3.dac_on {
            // On DMG, a write while the channel is on lands on the byte being
            // read. We approximate by writing the active byte.
            let i = self.ch3.position / 2;
            self.ch3.wave_ram[i] = val;
        } else {
            self.ch3.wave_ram[idx] = val;
        }
    }

    fn write_nr52(&mut self, val: u8) {
        let new_power = val & 0x80 != 0;
        if !new_power && self.powered {
            // Powering off clears every register and silences all channels.
            self.power_off();
        } else if new_power && !self.powered {
            // Powering on resets the frame sequencer step.
            self.powered = true;
            self.fs_step = 0;
            self.fs_timer = 8192;
        }
    }

    fn power_off(&mut self) {
        let wave_ram = self.ch3.wave_ram; // wave RAM survives power-off
        self.ch1 = SquareChannel::new(true);
        self.ch2 = SquareChannel::new(false);
        self.ch3 = WaveChannel::new();
        self.ch3.wave_ram = wave_ram;
        self.ch4 = NoiseChannel::new();
        self.nr50 = 0;
        self.nr51 = 0;
        self.powered = false;
    }

    // --- Square channel writes (shared by ch1/ch2) ---

    fn square(&mut self, ch2: bool) -> &mut SquareChannel {
        if ch2 {
            &mut self.ch2
        } else {
            &mut self.ch1
        }
    }

    fn write_nr10(&mut self, val: u8) {
        let negate_now = val & 0x08 != 0;
        // Clearing negate after a negate-mode calculation disables the channel.
        if self.ch1.sweep_negate && !negate_now && self.ch1.sweep_negate_calculated {
            self.ch1.enabled = false;
        }
        self.ch1.sweep_period = (val >> 4) & 0x07;
        self.ch1.sweep_negate = negate_now;
        self.ch1.sweep_shift = val & 0x07;
    }

    fn write_square_nrx1(&mut self, ch2: bool, val: u8) {
        let ch = self.square(ch2);
        ch.nrx1_raw = val;
        ch.duty = val >> 6;
        ch.length.set_length((val & 0x3F) as u16);
    }

    fn write_square_nrx2(&mut self, ch2: bool, val: u8) {
        let ch = self.square(ch2);
        ch.envelope.write(val);
        ch.dac_on = ch.envelope.dac_on();
        if !ch.dac_on {
            ch.enabled = false;
        }
    }

    fn write_square_nrx3(&mut self, ch2: bool, val: u8) {
        let ch = self.square(ch2);
        ch.frequency = (ch.frequency & 0x0700) | val as u16;
    }

    fn write_square_nrx4(&mut self, ch2: bool, val: u8) {
        let trigger = val & 0x80 != 0;
        let length_enable = val & 0x40 != 0;
        let fs_step = self.fs_step;
        let ch = self.square(ch2);
        ch.frequency = (ch.frequency & 0x00FF) | (((val & 0x07) as u16) << 8);

        if Self::update_length_enable(&mut ch.length, length_enable, fs_step) {
            ch.enabled = false;
        }

        if trigger {
            ch.trigger();
            // On trigger with length enabled and the counter just reloaded to
            // max during the first half of a length period, an extra clock
            // occurs (the "trigger length" quirk).
            Self::trigger_length_quirk(&mut ch.length, fs_step);
        }
    }

    // --- Wave channel writes ---

    fn write_nr30(&mut self, val: u8) {
        self.ch3.dac_on = val & 0x80 != 0;
        if !self.ch3.dac_on {
            self.ch3.enabled = false;
        }
    }

    fn write_nr31(&mut self, val: u8) {
        self.ch3.length.set_length(val as u16);
    }

    fn write_nr32(&mut self, val: u8) {
        self.ch3.volume_code = (val >> 5) & 0x03;
    }

    fn write_nr33(&mut self, val: u8) {
        self.ch3.frequency = (self.ch3.frequency & 0x0700) | val as u16;
    }

    fn write_nr34(&mut self, val: u8) {
        let trigger = val & 0x80 != 0;
        let length_enable = val & 0x40 != 0;
        let fs_step = self.fs_step;
        self.ch3.frequency = (self.ch3.frequency & 0x00FF) | (((val & 0x07) as u16) << 8);
        if Self::update_length_enable(&mut self.ch3.length, length_enable, fs_step) {
            self.ch3.enabled = false;
        }
        if trigger {
            self.ch3.trigger();
            Self::trigger_length_quirk(&mut self.ch3.length, fs_step);
        }
    }

    // --- Noise channel writes ---

    fn write_nr41(&mut self, val: u8) {
        self.ch4.length.set_length((val & 0x3F) as u16);
    }

    fn write_nr42(&mut self, val: u8) {
        self.ch4.envelope.write(val);
        self.ch4.dac_on = self.ch4.envelope.dac_on();
        if !self.ch4.dac_on {
            self.ch4.enabled = false;
        }
    }

    fn write_nr43(&mut self, val: u8) {
        self.ch4.clock_shift = val >> 4;
        self.ch4.width_7bit = val & 0x08 != 0;
        self.ch4.divisor_code = val & 0x07;
    }

    fn write_nr44(&mut self, val: u8) {
        let trigger = val & 0x80 != 0;
        let length_enable = val & 0x40 != 0;
        let fs_step = self.fs_step;
        if Self::update_length_enable(&mut self.ch4.length, length_enable, fs_step) {
            self.ch4.enabled = false;
        }
        if trigger {
            self.ch4.trigger();
            Self::trigger_length_quirk(&mut self.ch4.length, fs_step);
        }
    }

    /// "Extra length clocking" quirk: enabling the length counter during the
    /// first half of a length period (a frame-sequencer step that does not
    /// itself clock length) causes one extra immediate length clock. Returns
    /// true if that extra clock brought the counter to 0, meaning the caller
    /// must disable the channel.
    #[must_use]
    fn update_length_enable(len: &mut LengthCounter, enable: bool, fs_step: u8) -> bool {
        let was_enabled = len.enabled;
        // Length is clocked on FS steps 0, 2, 4, 6. The extra clock happens
        // when the *next* FS length tick is not imminent, i.e. the current step
        // is an odd one.
        let length_clock_next = fs_step % 2 == 0;
        let mut hit_zero = false;
        if !was_enabled && enable && !length_clock_next && len.counter > 0 {
            len.counter -= 1;
            hit_zero = len.counter == 0;
        }
        len.enabled = enable;
        hit_zero
    }

    /// On trigger with length enabled and counter reloaded to max during the
    /// first half, an extra clock occurs.
    fn trigger_length_quirk(len: &mut LengthCounter, fs_step: u8) {
        let length_clock_next = fs_step % 2 == 0;
        if len.enabled && len.counter == len.max && !length_clock_next {
            len.counter -= 1;
        }
    }

    // -- Stepping ----------------------------------------------------------

    pub fn step(&mut self, cycles: u32) {
        // We process in reasonably small chunks so the frequency timers stay
        // accurate even across large `cycles` values; in practice `cycles` is
        // a single instruction's T-cycle count (<= ~24).
        let mut remaining = cycles;
        while remaining > 0 {
            // Step up to the next frame-sequencer boundary so length/envelope/
            // sweep events land at the right cycle.
            let chunk = remaining.min(self.fs_timer);
            self.run_channels(chunk as i32);
            self.advance_resampler(chunk);

            self.fs_timer -= chunk;
            if self.fs_timer == 0 {
                self.fs_timer = 8192;
                self.frame_sequencer_tick();
            }
            remaining -= chunk;
        }
    }

    fn run_channels(&mut self, cycles: i32) {
        if !self.powered {
            return;
        }
        if self.ch1.dac_on {
            self.ch1.step(cycles);
        }
        if self.ch2.dac_on {
            self.ch2.step(cycles);
        }
        if self.ch3.dac_on {
            self.ch3.step(cycles);
        }
        if self.ch4.dac_on {
            self.ch4.step(cycles);
        }
    }

    fn frame_sequencer_tick(&mut self) {
        if !self.powered {
            return;
        }
        // Step pattern (512 Hz):
        //   0: length        2: length+sweep   4: length        6: length+sweep
        //   7: envelope      (1,3,5: nothing)
        match self.fs_step {
            0 | 4 => self.clock_length(),
            2 | 6 => {
                self.clock_length();
                self.ch1.sweep_clock();
            }
            7 => self.clock_envelopes(),
            _ => {}
        }
        self.fs_step = (self.fs_step + 1) & 7;
    }

    fn clock_length(&mut self) {
        if self.ch1.length.clock() {
            self.ch1.enabled = false;
        }
        if self.ch2.length.clock() {
            self.ch2.enabled = false;
        }
        if self.ch3.length.clock() {
            self.ch3.enabled = false;
        }
        if self.ch4.length.clock() {
            self.ch4.enabled = false;
        }
    }

    fn clock_envelopes(&mut self) {
        self.ch1.envelope.clock();
        self.ch2.envelope.clock();
        self.ch4.envelope.clock();
    }

    fn advance_resampler(&mut self, cycles: u32) {
        self.sample_accumulator += cycles as f32;
        while self.sample_accumulator >= self.cycles_per_sample {
            self.sample_accumulator -= self.cycles_per_sample;
            self.emit_sample();
        }
    }

    /// Mix the four channels into one stereo pair and push L,R.
    fn emit_sample(&mut self) {
        let (mut left, mut right) = (0.0f32, 0.0f32);

        if self.powered {
            // Per-channel DAC output mapped from [0,15] to [-1,1].
            let c1 = dac(self.ch1.output());
            let c2 = dac(self.ch2.output());
            let c3 = dac(self.ch3.output());
            let c4 = dac(self.ch4.output());

            // NR51 routing: bits 0-3 = right, bits 4-7 = left.
            let mix = |sample: f32, right_bit: u8, left_bit: u8, l: &mut f32, r: &mut f32| {
                if self.nr51 & left_bit != 0 {
                    *l += sample;
                }
                if self.nr51 & right_bit != 0 {
                    *r += sample;
                }
            };
            mix(c1, 0x01, 0x10, &mut left, &mut right);
            mix(c2, 0x02, 0x20, &mut left, &mut right);
            mix(c3, 0x04, 0x40, &mut left, &mut right);
            mix(c4, 0x08, 0x80, &mut left, &mut right);

            // NR50 master volume: 0..7 per side, +1 so it never fully mutes the
            // mixer scaling. Average over 4 channels keeps the result in range.
            let left_vol = ((self.nr50 >> 4) & 0x07) as f32 + 1.0;
            let right_vol = (self.nr50 & 0x07) as f32 + 1.0;
            left = left / 4.0 * (left_vol / 8.0);
            right = right / 4.0 * (right_vol / 8.0);
        }

        if self.output.len() >= MAX_BUFFERED_SAMPLES {
            // Drop the oldest stereo pair so a non-draining caller can't OOM.
            self.output.drain(0..2);
        }
        self.output.push(left);
        self.output.push(right);
    }

    // -- Drain -------------------------------------------------------------

    pub fn take_samples(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.output)
    }
}

/// Map a 4-bit DAC level (0..15) to roughly [-1.0, 1.0]. A channel with DAC on
/// but level 0 sits at -1.0 (the analogue rest point); silenced channels are
/// fed 0 here and also map to -1.0, which is fine because NR51/volume scaling
/// keeps the summed mix bounded.
fn dac(level: u8) -> f32 {
    (level as f32 / 7.5) - 1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Roughly one second of CPU cycles.
    const ONE_SECOND: u32 = CPU_CLOCK_HZ;

    fn power_on(apu: &mut Apu) {
        apu.write_reg(0xFF26, 0x80); // NR52: power on
        apu.write_reg(0xFF24, 0x77); // NR50: full volume both sides
        apu.write_reg(0xFF25, 0xFF); // NR51: route everything L and R
    }

    /// Step `cycles` T-cycles in small instruction-sized chunks, like the MMU.
    fn step_for(apu: &mut Apu, cycles: u32) {
        let mut left = cycles;
        while left > 0 {
            let c = left.min(20);
            apu.step(c);
            left -= c;
        }
    }

    fn assert_buffer_ok(samples: &[f32]) {
        let pairs = samples.len() / 2;
        // 44_100 stereo pairs expected, within a few percent.
        assert!(
            (pairs as i64 - 44_100).abs() < 2_000,
            "expected ~44100 stereo pairs, got {}",
            pairs
        );
        assert_eq!(samples.len() % 2, 0, "samples must be interleaved pairs");
        assert!(
            samples.iter().all(|s| s.is_finite()),
            "all samples must be finite"
        );
        assert!(
            samples.iter().all(|s| *s >= -1.1 && *s <= 1.1),
            "all samples within [-1.1, 1.1]"
        );
        assert!(
            samples.iter().any(|s| *s != 0.0),
            "buffer must not be all zero"
        );
    }

    #[test]
    fn channel2_produces_audio() {
        let mut apu = Apu::new();
        power_on(&mut apu);
        // Channel 2: duty 50%, envelope volume 15 decreasing slowly, mid freq.
        apu.write_reg(0xFF16, 0x80); // NR21: duty=2, length irrelevant
        apu.write_reg(0xFF17, 0xF0); // NR22: volume 15, no envelope sweep
        apu.write_reg(0xFF18, 0x00); // NR23: freq low
        apu.write_reg(0xFF19, 0x87); // NR24: freq high=7, trigger, no length

        step_for(&mut apu, ONE_SECOND);
        let samples = apu.take_samples();
        assert_buffer_ok(&samples);
    }

    #[test]
    fn channel1_with_sweep_produces_audio() {
        let mut apu = Apu::new();
        power_on(&mut apu);
        apu.write_reg(0xFF10, 0x00); // NR10: no sweep
        apu.write_reg(0xFF11, 0x80); // NR11: duty 2
        apu.write_reg(0xFF12, 0xF0); // NR12: volume 15
        apu.write_reg(0xFF13, 0x00); // NR13
        apu.write_reg(0xFF14, 0x86); // NR14: trigger

        step_for(&mut apu, ONE_SECOND);
        let samples = apu.take_samples();
        assert_buffer_ok(&samples);
    }

    #[test]
    fn wave_channel_produces_audio() {
        let mut apu = Apu::new();
        power_on(&mut apu);
        // Fill wave RAM with a ramp 0,1,2,...,F repeated.
        for i in 0..16u16 {
            apu.write_reg(0xFF30 + i, ((i as u8 % 16) << 4) | ((i as u8 + 1) % 16));
        }
        apu.write_reg(0xFF1A, 0x80); // NR30: DAC on
        apu.write_reg(0xFF1B, 0x00); // NR31: length
        apu.write_reg(0xFF1C, 0x20); // NR32: volume code 1 (full)
        apu.write_reg(0xFF1D, 0x00); // NR33: freq low
        apu.write_reg(0xFF1E, 0x87); // NR34: freq high, trigger

        step_for(&mut apu, ONE_SECOND);
        let samples = apu.take_samples();
        assert_buffer_ok(&samples);
    }

    #[test]
    fn noise_channel_produces_audio() {
        let mut apu = Apu::new();
        power_on(&mut apu);
        apu.write_reg(0xFF20, 0x00); // NR41: length
        apu.write_reg(0xFF21, 0xF0); // NR42: volume 15
        apu.write_reg(0xFF22, 0x33); // NR43: some clock/divisor
        apu.write_reg(0xFF23, 0x80); // NR44: trigger

        step_for(&mut apu, ONE_SECOND);
        let samples = apu.take_samples();
        assert_buffer_ok(&samples);
    }

    #[test]
    fn read_masks() {
        let mut apu = Apu::new();
        power_on(&mut apu);

        // NR52 with all channels off: power bit + 0x70.
        assert_eq!(apu.read_reg(0xFF26) & 0x70, 0x70);
        assert_eq!(apu.read_reg(0xFF26) & 0x80, 0x80);

        // Write-only registers read back as 0xFF.
        assert_eq!(apu.read_reg(0xFF13), 0xFF); // NR13
        assert_eq!(apu.read_reg(0xFF18), 0xFF); // NR23
        assert_eq!(apu.read_reg(0xFF1B), 0xFF); // NR31
        assert_eq!(apu.read_reg(0xFF1D), 0xFF); // NR33

        // NR11/NR21: only duty bits (6-7) readable, low 6 bits read as 1.
        apu.write_reg(0xFF11, 0xC5);
        assert_eq!(apu.read_reg(0xFF11), 0xFF); // 0xC0 duty | 0x3F mask
        apu.write_reg(0xFF11, 0x00);
        assert_eq!(apu.read_reg(0xFF11), 0x3F);

        // NR10 bit 7 always reads 1.
        assert_eq!(apu.read_reg(0xFF10) & 0x80, 0x80);

        // NR30 bit 7 reflects DAC; low 7 bits read as 1.
        apu.write_reg(0xFF1A, 0x80);
        assert_eq!(apu.read_reg(0xFF1A), 0xFF);
        apu.write_reg(0xFF1A, 0x00);
        assert_eq!(apu.read_reg(0xFF1A), 0x7F);

        // NR32: bits 5-6 readable, rest 1 -> mask 0x9F.
        apu.write_reg(0xFF1C, 0x40);
        assert_eq!(apu.read_reg(0xFF1C), 0x40 | 0x9F);

        // NRx4: only length-enable (bit 6) readable -> mask 0xBF.
        apu.write_reg(0xFF14, 0x40); // length enable, no trigger
        assert_eq!(apu.read_reg(0xFF14), 0xBF | 0x40);

        // Unused regions read as 0xFF.
        assert_eq!(apu.read_reg(0xFF15), 0xFF);
        assert_eq!(apu.read_reg(0xFF1F), 0xFF);
        assert_eq!(apu.read_reg(0xFF27), 0xFF);
    }

    #[test]
    fn power_off_clears_registers_and_ignores_writes() {
        let mut apu = Apu::new();
        power_on(&mut apu);
        apu.write_reg(0xFF12, 0xF0); // NR12 envelope
        apu.write_reg(0xFF26, 0x00); // power off

        // NR52 reads back with power bit clear.
        assert_eq!(apu.read_reg(0xFF26) & 0x80, 0x00);
        // NR50/NR51 cleared.
        assert_eq!(apu.read_reg(0xFF24), 0x00);
        assert_eq!(apu.read_reg(0xFF25), 0x00);

        // Writes to channel regs are ignored while off.
        apu.write_reg(0xFF12, 0xF0);
        assert_eq!(apu.read_reg(0xFF12), 0x00);

        // Wave RAM is still writable while powered off.
        apu.write_reg(0xFF30, 0xAB);
        assert_eq!(apu.read_reg(0xFF30), 0xAB);
    }

    #[test]
    fn length_counter_silences_channel() {
        let mut apu = Apu::new();
        power_on(&mut apu);
        // Channel 2 with a very short length and length-enable; should go quiet.
        apu.write_reg(0xFF16, 0xBF); // NR21: duty + length load 63 -> 1 tick
        apu.write_reg(0xFF17, 0xF0); // NR22: volume 15
        apu.write_reg(0xFF18, 0x00);
        apu.write_reg(0xFF19, 0xC7); // NR24: trigger + length enable

        // Run enough for the length counter (1 tick @256Hz ~ 16384 cycles).
        step_for(&mut apu, 50_000);
        // Channel should now be disabled.
        assert!(!apu.ch2.enabled, "channel 2 should be silenced by length");
    }
}
