# Architecture

ferrisboy is split into a **pure-logic core** and **thin platform frontends**. The core knows nothing about windows, audio devices, files, or the web - it just emulates hardware and exposes a small API. Everything platform-specific lives in a frontend.

## The core API

A frontend only ever touches these methods on `GameBoy` (`core/src/lib.rs`):

```rust
let mut gb = GameBoy::new(rom_bytes)?;       // or ::with_save(rom, Some(sav))
gb.run_frame();                              // emulate one ~16.7ms frame
let pixels: &[u8] = gb.framebuffer();        // 160×144 shade indices 0..3
gb.framebuffer_rgba(&mut rgba);              // …or RGBA8888, DMG green palette
gb.set_button(Button::A, pressed);           // input
let audio: Vec<f32> = gb.take_audio();       // stereo samples @ 44.1kHz
let save: Option<Vec<u8>> = gb.save_data();  // battery RAM to persist
```

Because the core has **zero dependencies and does no I/O**, the exact same crate compiles for native targets and for `wasm32-unknown-unknown` without changes or feature flags.

## The bus (`mmu.rs`)

Every memory access the CPU makes goes through `Mmu::read_byte` / `write_byte`, which decode the 16-bit address space and dispatch to a component:

| Range | Component |
|---|---|
| `0000–7FFF`, `A000–BFFF` | Cartridge (banked ROM / external RAM) |
| `8000–9FFF`, `FE00–FE9F` | PPU (VRAM / OAM) |
| `C000–FDFF` | Work RAM (+ echo) |
| `FF00` / `FF01–02` / `FF04–07` | Joypad / Serial / Timer |
| `FF0F`, `FFFF` | Interrupt flags (IF) / enable (IE) |
| `FF10–FF3F` | APU |
| `FF40–FF4B` | PPU registers (incl. `FF46` OAM DMA) |
| `FF80–FFFE` | High RAM |

`mmu.rs` is the **integration contract**: the method names and signatures it calls on each component are exactly what each component must expose. Pinning this in compiling code first is what allowed the CPU, PPU, APU, and timer to be implemented independently and in parallel without integration drift.

## Timing model

ferrisboy uses an **instruction-stepped** model. The run loop is:

```rust
loop {
    let cycles = cpu.step(&mut mmu); // execute one whole instruction
    mmu.tick(cycles);                // advance PPU, timer, APU, serial by that many T-cycles
    if mmu.ppu.take_frame_ready() { break; }
}
```

The CPU executes a complete instruction and reports its T-cycle cost; the bus then advances every peripheral by that amount, OR-ing any interrupts they raise into IF. This is simpler than a per-M-cycle model and is accurate enough to pass `cpu_instrs`, `instr_timing`, `dmg-acid2`, `dmg_sound`, and to run commercial games correctly. The trade-off is that sub-instruction memory timing (Blargg's `mem_timing`) is not modeled.

Interrupt dispatch, the one-instruction `EI` enable delay, and the `HALT` bug are handled in `cpu/step` and `cpu/fetch_byte`, so the opcode tables stay free of interrupt bookkeeping.

## Components

- **CPU (`cpu/`)** - `mod.rs` holds the two opcode dispatch tables and the fetch/stack/interrupt machinery; `alu.rs` centralizes every flag-setting primitive (add/adc/sub/sbc/cp, rotates/shifts, daa, …) so flag logic lives in exactly one place; `registers.rs` is the register file with the AF/BC/DE/HL pair views.
- **PPU (`ppu.rs`)** - a cycle accumulator walks the mode state machine (OAM scan → draw → HBlank, then VBlank), renders each visible scanline once into the framebuffer, tracks the window's own line counter, and fires VBlank/STAT interrupts on the rising edge of the STAT line.
- **APU (`apu.rs`)** - one struct per channel driven by a 512 Hz frame sequencer (length @256 Hz, envelope @64 Hz, sweep @128 Hz); a fractional accumulator emits one stereo pair every ~95.11 T-cycles.
- **Cartridge (`cartridge.rs`)** - parses the header, then models the mapper. Bank math is computed from the mapper's registers on each access; external RAM is bounds-checked and, for battery carts, exported via `save_data()`.
- **Timer (`timer.rs`)** - a 16-bit divider whose selected bit's falling edge clocks TIMA, with reload-from-TMA and a timer interrupt on overflow.

## Verification harness

`core/examples/run_rom.rs` boots a ROM with no window. It captures the serial port (Blargg test ROMs stream their pass/fail text there) and can dump the framebuffer to PNG for visual conformance checks (e.g. `dmg-acid2`). This is how the emulator is tested in CI/headless environments and how every accuracy claim in the README was confirmed.
