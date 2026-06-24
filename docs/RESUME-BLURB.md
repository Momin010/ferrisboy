# Resume / portfolio copy

Ready-to-paste descriptions of ferrisboy at a few lengths. Edit the GitHub URL once you push it.

## One-line resume bullet

> **ferrisboy — Game Boy emulator (Rust → WebAssembly).** Built a cycle-aware Sharp LR35902 emulator from scratch in safe, dependency-free Rust; verified against the standard hardware conformance suites (Blargg `cpu_instrs` 11/11, `dmg-acid2`); ships as a native app and an in-browser WebAssembly build from one shared core.

## Two-line variant

> **ferrisboy — Game Boy (DMG) emulator in Rust.** Implemented the full CPU (512 opcodes, interrupts, the HALT bug), a scanline PPU, a 4-channel APU, and MBC1/2/3/5 cartridge mappers as a zero-dependency, `unsafe`-free core. Validated with the Blargg and dmg-acid2 test ROMs; compiled the same core to WebAssembly to run in the browser and to a native desktop app.

## Short paragraph (LinkedIn / portfolio site)

> ferrisboy is a Game Boy emulator I wrote from scratch in Rust. It's the same class of software as a virtual machine: it reimplements the console's CPU, memory bus, LCD, and sound hardware so that unmodified commercial game ROMs run against it. I prioritized correctness — it passes the community's standard hardware conformance tests (Blargg's CPU and timing suites, Matt Currie's dmg-acid2 PPU test) — and portability: the emulator core has zero third-party dependencies and no `unsafe`, so the identical code compiles to a native macOS/Linux/Windows app and to WebAssembly for the browser. Tech: Rust, WebAssembly (wasm-bindgen), the Web Audio and Canvas APIs, and cpal/minifb on the desktop.

## Interview talking points

- **Why it's impressive:** emulators are hard for a clear reason — you're implementing a published hardware spec exactly, and "almost right" produces visibly broken games. Passing Blargg + acid2 is objective proof of correctness.
- **Architecture decision:** a pure-logic core with no I/O, behind a ~8-method API, so the same crate targets native and `wasm32` unchanged. Frontends are thin.
- **Timing model:** instruction-stepped (peripherals advance per instruction) — a deliberate trade-off that passes `cpu_instrs`/`instr_timing`/`dmg-acid2` and runs games, while keeping the code tractable; the limitation (sub-instruction `mem_timing`) is documented.
- **Testing:** a headless harness boots ROMs, reads the serial port for Blargg's pass/fail text, and dumps the framebuffer to PNG for visual conformance — so correctness is checked automatically, not by eyeballing a game.
