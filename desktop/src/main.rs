//! ferrisboy-desktop — a native player for the ferrisboy Game Boy core.
//!
//! Load a ROM (from `argv[1]` or a file dialog) and it plays: window, keyboard,
//! sound, and battery saves. The core does the emulation; this binary just wires
//! it to the platform — minifb for the window/keyboard, cpal for audio, rfd for
//! the open dialog.

use std::collections::VecDeque;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ferrisboy_core::{
    Button, GameBoy, AUDIO_SAMPLE_RATE, SCREEN_HEIGHT, SCREEN_WIDTH,
};
use minifb::{Key, Scale, ScaleMode, Window, WindowOptions};

/// Integer pixel scale for the window (160x144 -> 640x576).
const SCALE: usize = 4;
/// One emulated frame of wall-clock time (~59.7 fps).
const FRAME_TIME: Duration = Duration::from_nanos(16_742_706);
/// How often to flush a dirty battery save to disk.
const SAVE_INTERVAL: Duration = Duration::from_secs(1);

/// Classic DMG green palette, indexed by the core's 0..3 shade indices, packed
/// as 0x00RRGGBB for minifb's u32 framebuffer.
const PALETTE: [u32; 4] = [0x9B_BC_0F, 0x8B_AC_0F, 0x30_62_30, 0x0F_38_0F];

/// Keyboard -> Game Boy button mapping, applied fresh every frame.
const KEY_MAP: [(Key, Button); 8] = [
    (Key::Right, Button::Right),
    (Key::Left, Button::Left),
    (Key::Up, Button::Up),
    (Key::Down, Button::Down),
    (Key::Z, Button::A),
    (Key::X, Button::B),
    (Key::Enter, Button::Start),
    (Key::RightShift, Button::Select),
];

fn main() {
    if let Err(e) = run() {
        eprintln!("ferrisboy: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    // 1. Find a ROM: command-line argument, otherwise a native file dialog.
    let rom_path = match std::env::args_os().nth(1) {
        Some(arg) => PathBuf::from(arg),
        None => pick_rom().ok_or("no ROM selected")?,
    };

    let rom = std::fs::read(&rom_path)
        .map_err(|e| format!("could not read ROM '{}': {e}", rom_path.display()))?;

    // 2. Look for a battery save sitting next to the ROM.
    let save_path = rom_path.with_extension("sav");
    let save = std::fs::read(&save_path).ok();
    if save.is_some() {
        println!("Loaded save: {}", save_path.display());
    }

    let mut gb = GameBoy::with_save(rom, save)
        .map_err(|e| format!("unsupported cartridge: {e}"))?;

    let title = gb.title().trim().to_string();
    print_banner(&title, &rom_path);

    // 3. Window.
    let mut window = create_window(&title)?;

    // 4. Audio. Keep the stream alive for the lifetime of the loop; on failure we
    // simply play silently rather than refusing to start the game.
    let audio = AudioOutput::start();
    let ring = match &audio {
        Some(a) => Some(a.ring.clone()),
        None => {
            eprintln!("Audio unavailable; continuing without sound.");
            None
        }
    };

    // 5. Main loop: pace emulation to wall-clock, render, forward input, save.
    let mut next_frame = Instant::now();
    let mut last_save = Instant::now();
    let mut pixels = vec![0u32; SCREEN_WIDTH * SCREEN_HEIGHT];

    while window.is_open() && !window.is_key_down(Key::Escape) {
        forward_input(&window, &mut gb);

        gb.run_frame();

        if let Some(ring) = &ring {
            push_audio(ring, gb.take_audio());
        }

        render(&gb, &mut pixels, &mut window)?;

        // Periodic battery save so progress survives a crash / force-quit.
        if last_save.elapsed() >= SAVE_INTERVAL {
            persist_save(&mut gb, &save_path);
            last_save = Instant::now();
        }

        // Pace to ~59.7 fps without busy-waiting.
        next_frame += FRAME_TIME;
        let now = Instant::now();
        if next_frame > now {
            std::thread::sleep(next_frame - now);
        } else {
            // We fell behind (e.g. window drag); resync rather than spiral.
            next_frame = now;
        }
    }

    // Final flush on a clean exit.
    persist_save(&mut gb, &save_path);
    println!("Goodbye.");
    Ok(())
}

/// Open a native file-open dialog filtered to Game Boy ROMs.
fn pick_rom() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .add_filter("Game Boy ROM", &["gb", "gbc"])
        .set_title("Open a Game Boy ROM")
        .pick_file()
}

fn print_banner(title: &str, rom_path: &Path) {
    let shown = if title.is_empty() { "(untitled)" } else { title };
    println!("ferrisboy — now playing: {shown}");
    println!("  ROM: {}", rom_path.display());
    println!("Controls:");
    println!("  D-pad ........ Arrow keys");
    println!("  A ............ Z");
    println!("  B ............ X");
    println!("  Start ........ Enter");
    println!("  Select ....... Right Shift / Backspace");
    println!("  Quit ......... Esc");
}

fn create_window(title: &str) -> Result<Window, Box<dyn Error>> {
    let name = if title.is_empty() {
        "ferrisboy".to_string()
    } else {
        format!("ferrisboy — {title}")
    };
    let mut window = Window::new(
        &name,
        SCREEN_WIDTH * SCALE,
        SCREEN_HEIGHT * SCALE,
        WindowOptions {
            scale: Scale::X1,
            scale_mode: ScaleMode::Stretch, // nearest-neighbor upscale of our buffer
            resize: true,
            ..WindowOptions::default()
        },
    )
    .map_err(|e| format!("could not open window: {e}"))?;

    // Let minifb cap presentation near 60 fps; emulation is paced separately.
    window.set_target_fps(60);
    Ok(window)
}

/// Set every button's pressed/released state from the current key state.
fn forward_input(window: &Window, gb: &mut GameBoy) {
    for (key, button) in KEY_MAP {
        gb.set_button(button, window.is_key_down(key));
    }
    // Backspace is an alternate Select binding.
    if window.is_key_down(Key::Backspace) {
        gb.set_button(Button::Select, true);
    }
}

/// Map the core's shade indices to ARGB and present the frame.
fn render(
    gb: &GameBoy,
    pixels: &mut [u32],
    window: &mut Window,
) -> Result<(), Box<dyn Error>> {
    for (out, &shade) in pixels.iter_mut().zip(gb.framebuffer()) {
        *out = PALETTE[(shade & 0x03) as usize];
    }
    window
        .update_with_buffer(pixels, SCREEN_WIDTH, SCREEN_HEIGHT)
        .map_err(|e| format!("failed to draw frame: {e}"))?;
    Ok(())
}

/// Write the cartridge's battery RAM to `<rom>.sav` if it changed.
fn persist_save(gb: &mut GameBoy, save_path: &Path) {
    if !gb.save_is_dirty() {
        return;
    }
    if let Some(data) = gb.save_data() {
        match std::fs::write(save_path, &data) {
            Ok(()) => gb.mark_saved(),
            Err(e) => eprintln!("could not write save '{}': {e}", save_path.display()),
        }
    } else {
        // No battery; nothing to persist, but clear the flag so we stop polling.
        gb.mark_saved();
    }
}

/// Push a frame of stereo samples into the ring buffer, trimming any backlog
/// beyond a few frames of latency so audio never drifts behind the picture.
fn push_audio(ring: &Arc<Mutex<VecDeque<f32>>>, samples: Vec<f32>) {
    // ~4 frames of stereo headroom at 44.1 kHz.
    let max_len = (AUDIO_SAMPLE_RATE as usize / 60) * 2 * 4;
    if let Ok(mut buf) = ring.lock() {
        buf.extend(samples);
        if buf.len() > max_len {
            let drop = buf.len() - max_len;
            buf.drain(0..drop);
        }
    }
}

/// Owns the cpal output stream and the shared sample ring buffer. Dropping it
/// stops the stream.
struct AudioOutput {
    ring: Arc<Mutex<VecDeque<f32>>>,
    _stream: cpal::Stream,
}

impl AudioOutput {
    /// Try to open the default output device. Returns `None` (and logs) if no
    /// audio is available, so the emulator can still run silently.
    fn start() -> Option<Self> {
        match Self::try_start() {
            Ok(out) => Some(out),
            Err(e) => {
                eprintln!("audio init failed: {e}");
                None
            }
        }
    }

    fn try_start() -> Result<Self, Box<dyn Error>> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or("no default output device")?;

        // Prefer a stereo f32 config at the core's native 44.1 kHz; fall back to
        // the device default if that exact config isn't offered.
        let config = pick_config(&device)?;
        let device_rate = config.sample_rate();
        let channels = config.channels() as usize;
        let stream_config: cpal::StreamConfig = config.config();

        println!(
            "Audio: {} Hz, {} ch (core produces {} Hz stereo)",
            device_rate, channels, AUDIO_SAMPLE_RATE
        );

        let ring: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::new()));
        let cb_ring = ring.clone();

        // Simple linear ratio resampler: read `step` source frames per output
        // frame. step==1.0 when device_rate == 44_100.
        let step = AUDIO_SAMPLE_RATE as f64 / device_rate as f64;
        let mut src_pos: f64 = 0.0;
        // Last popped stereo frame, held during underrun to avoid clicks.
        let mut held: [f32; 2] = [0.0, 0.0];

        let err_fn = |e| eprintln!("audio stream error: {e}");

        let stream = device.build_output_stream(
            stream_config,
            move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let mut buf = match cb_ring.lock() {
                    Ok(b) => b,
                    Err(_) => {
                        out.fill(0.0);
                        return;
                    }
                };
                for frame in out.chunks_mut(channels) {
                    // Advance through the source buffer at the resample ratio.
                    while src_pos >= 1.0 {
                        if buf.len() >= 2 {
                            held[0] = buf.pop_front().unwrap();
                            held[1] = buf.pop_front().unwrap();
                        } else {
                            // Underrun: keep the last frame (silence at startup).
                            held = [0.0, 0.0];
                        }
                        src_pos -= 1.0;
                    }
                    src_pos += step;

                    // Write held frame across all output channels (mono-dup if
                    // the device isn't stereo).
                    for (i, sample) in frame.iter_mut().enumerate() {
                        *sample = held[i.min(1)];
                    }
                }
            },
            err_fn,
            None,
        )?;

        stream.play()?;
        Ok(Self {
            ring,
            _stream: stream,
        })
    }
}

/// Choose an output config. We always render `f32` samples, so we require an
/// `f32` config. Preference order:
///   1. stereo f32 at exactly 44.1 kHz (no resampling, ideal),
///   2. any f32 range covering 44.1 kHz (we use 44.1 kHz, maybe mono),
///   3. any f32 range at its max rate (we resample to fit).
fn pick_config(device: &cpal::Device) -> Result<cpal::SupportedStreamConfig, Box<dyn Error>> {
    let ranges: Vec<cpal::SupportedStreamConfigRange> = device
        .supported_output_configs()
        .map_err(|e| format!("could not query audio configs: {e}"))?
        .filter(|r| r.sample_format() == cpal::SampleFormat::F32)
        .collect();

    let covers = |r: &cpal::SupportedStreamConfigRange| {
        r.min_sample_rate() <= AUDIO_SAMPLE_RATE && AUDIO_SAMPLE_RATE <= r.max_sample_rate()
    };

    // 1. stereo f32 @ 44.1 kHz.
    if let Some(&r) = ranges.iter().find(|r| r.channels() == 2 && covers(r)) {
        return Ok(r.with_sample_rate(AUDIO_SAMPLE_RATE));
    }
    // 2. any f32 covering 44.1 kHz.
    if let Some(&r) = ranges.iter().find(|r| covers(r)) {
        return Ok(r.with_sample_rate(AUDIO_SAMPLE_RATE));
    }
    // 3. any f32 at its max rate (resampled in the callback).
    if let Some(r) = ranges.into_iter().next() {
        return Ok(r.with_max_sample_rate());
    }
    Err("no f32 output config available".into())
}
