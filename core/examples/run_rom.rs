//! Headless ROM runner — the verification harness for ferrisboy-core.
//!
//! It boots a ROM with no window or speaker, so it works in CI / sandboxes and
//! lets us check correctness automatically.
//!
//! Usage:
//!   cargo run -p ferrisboy-core --example run_rom -- <rom.gb> [options]
//!
//! Options:
//!   --cycles N     Run until N T-cycles elapse (default 250_000_000).
//!   --frames N     Run N full frames (needs a working PPU; ignores --cycles).
//!   --serial       Print bytes the ROM wrote to the serial port (Blargg output).
//!   --until-serial Stop early once serial output contains "Passed" or "Failed".
//!   --png PATH      Dump the final framebuffer to a PNG at PATH.
//!   --ascii         Print the framebuffer as ASCII art to stdout.
//!   --quiet         Suppress the human-readable summary.
//!
//! Exit code is 0 if no "Failed"/"Error" appeared in serial output, else 1 —
//! so a shell loop can gate on Blargg results.

use ferrisboy_core::{Button, GameBoy, FRAMEBUFFER_RGBA_LEN, SCREEN_HEIGHT, SCREEN_WIDTH};
use std::process::ExitCode;

/// Parse a `name@frame` input event, e.g. `start@200`. The button is held for
/// ~12 frames starting at that frame, then released.
fn parse_press(s: &str) -> Option<(u64, Button)> {
    let (name, frame) = s.split_once('@')?;
    let button = match name.to_ascii_lowercase().as_str() {
        "right" => Button::Right,
        "left" => Button::Left,
        "up" => Button::Up,
        "down" => Button::Down,
        "a" => Button::A,
        "b" => Button::B,
        "select" => Button::Select,
        "start" => Button::Start,
        _ => return None,
    };
    Some((frame.parse().ok()?, button))
}

struct Opts {
    rom: String,
    cycles: u64,
    frames: Option<u64>,
    print_serial: bool,
    until_serial: bool,
    png: Option<String>,
    ascii: bool,
    quiet: bool,
    presses: Vec<(u64, Button)>,
}

fn parse_opts() -> Opts {
    let mut args = std::env::args().skip(1);
    let mut o = Opts {
        rom: String::new(),
        cycles: 250_000_000,
        frames: None,
        print_serial: false,
        until_serial: false,
        png: None,
        ascii: false,
        quiet: false,
        presses: Vec::new(),
    };
    while let Some(a) = args.next() {
        match a.as_str() {
            "--cycles" => o.cycles = args.next().and_then(|v| v.parse().ok()).unwrap_or(o.cycles),
            "--frames" => o.frames = args.next().and_then(|v| v.parse().ok()),
            "--serial" => o.print_serial = true,
            "--until-serial" => o.until_serial = true,
            "--png" => o.png = args.next(),
            "--ascii" => o.ascii = true,
            "--quiet" => o.quiet = true,
            "--press" => {
                if let Some((f, b)) = args.next().as_deref().and_then(parse_press) {
                    o.presses.push((f, b));
                }
            }
            other => o.rom = other.to_string(),
        }
    }
    if o.rom.is_empty() {
        eprintln!("usage: run_rom <rom.gb> [--cycles N | --frames N] [--serial] [--until-serial] [--png PATH] [--ascii]");
        std::process::exit(2);
    }
    o
}

fn main() -> ExitCode {
    let opts = parse_opts();
    let rom = match std::fs::read(&opts.rom) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("error: cannot read {}: {e}", opts.rom);
            return ExitCode::from(2);
        }
    };
    let mut gb = match GameBoy::new(rom) {
        Ok(gb) => gb,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };

    if !opts.quiet {
        eprintln!("loaded: \"{}\"", gb.title());
    }

    let mut serial = String::new();
    let mut done_reason;

    if let Some(frames) = opts.frames {
        // Frame-based: requires a PPU that sets frame-ready.
        const HOLD: u64 = 12; // frames to hold each scripted button press
        for frame in 0..frames {
            for &(f, button) in &opts.presses {
                if frame == f {
                    gb.set_button(button, true);
                } else if frame == f + HOLD {
                    gb.set_button(button, false);
                }
            }
            gb.run_frame();
            append_serial(&mut serial, &mut gb);
        }
        done_reason = format!("ran {frames} frames");
    } else {
        // Cycle-based stepping: works even before the PPU is implemented.
        let mut elapsed: u64 = 0;
        done_reason = format!("reached cycle cap {}", opts.cycles);
        while elapsed < opts.cycles {
            elapsed += gb.step() as u64;
            append_serial(&mut serial, &mut gb);
            if opts.until_serial && (serial.contains("Passed") || serial.contains("Failed")) {
                done_reason = "serial reported a result".to_string();
                break;
            }
        }
    }

    if opts.print_serial {
        print!("{serial}");
    }

    if opts.ascii {
        print_ascii(gb.framebuffer());
    }

    if let Some(path) = &opts.png {
        match write_png(path, &gb) {
            Ok(()) => {
                if !opts.quiet {
                    eprintln!("wrote {path}");
                }
            }
            Err(e) => eprintln!("error writing png: {e}"),
        }
    }

    if !opts.quiet {
        eprintln!("stopped: {done_reason}");
    }

    let failed = serial.contains("Failed") || serial.contains("Error");
    if failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn append_serial(buf: &mut String, gb: &mut GameBoy) {
    for b in gb.take_serial() {
        buf.push(b as char);
    }
}

fn print_ascii(fb: &[u8]) {
    const RAMP: [char; 4] = [' ', '.', '+', '#'];
    // Downsample 2x vertically to fit a terminal better.
    for y in (0..SCREEN_HEIGHT).step_by(2) {
        let mut line = String::with_capacity(SCREEN_WIDTH);
        for x in 0..SCREEN_WIDTH {
            line.push(RAMP[(fb[y * SCREEN_WIDTH + x] & 3) as usize]);
        }
        println!("{line}");
    }
}

fn write_png(path: &str, gb: &GameBoy) -> Result<(), Box<dyn std::error::Error>> {
    let mut rgba = vec![0u8; FRAMEBUFFER_RGBA_LEN];
    gb.framebuffer_rgba(&mut rgba);
    let file = std::fs::File::create(path)?;
    let w = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(w, SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&rgba)?;
    Ok(())
}
