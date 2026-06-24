//! WebAssembly frontend for the ferrisboy Game Boy core.
//!
//! This crate is deliberately thin: it wraps [`ferrisboy_core::GameBoy`] in a
//! `#[wasm_bindgen]` type and exposes just enough to drive it from JavaScript.
//! All rendering, audio output, and input handling live in `index.js`; Rust
//! only steps the emulator and hands out buffers.

use ferrisboy_core::{Button, GameBoy, FRAMEBUFFER_RGBA_LEN};
use wasm_bindgen::prelude::*;

/// Map a JS button index (0..=7) to a core [`Button`].
///
/// The order matches the `Button` enum and the legend shown in the UI:
/// 0 Right, 1 Left, 2 Up, 3 Down, 4 A, 5 B, 6 Select, 7 Start.
fn button_from_index(index: u8) -> Option<Button> {
    Some(match index {
        0 => Button::Right,
        1 => Button::Left,
        2 => Button::Up,
        3 => Button::Down,
        4 => Button::A,
        5 => Button::B,
        6 => Button::Select,
        7 => Button::Start,
        _ => return None,
    })
}

/// A running Game Boy, owned by JavaScript.
#[wasm_bindgen]
pub struct Emulator {
    gb: GameBoy,
    /// Reused RGBA scratch buffer so each frame avoids a fresh allocation.
    frame: Vec<u8>,
}

#[wasm_bindgen]
impl Emulator {
    /// Construct an emulator from cartridge ROM bytes.
    #[wasm_bindgen(constructor)]
    pub fn new(rom: &[u8]) -> Result<Emulator, JsValue> {
        Self::with_optional_save(rom, None)
    }

    /// Construct an emulator, restoring previously saved battery RAM.
    #[wasm_bindgen(js_name = newWithSave)]
    pub fn new_with_save(rom: &[u8], save: &[u8]) -> Result<Emulator, JsValue> {
        Self::with_optional_save(rom, Some(save.to_vec()))
    }

    fn with_optional_save(rom: &[u8], save: Option<Vec<u8>>) -> Result<Emulator, JsValue> {
        console_error_panic_hook::set_once();
        let gb = GameBoy::with_save(rom.to_vec(), save)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(Emulator {
            gb,
            frame: vec![0u8; FRAMEBUFFER_RGBA_LEN],
        })
    }

    /// Advance the emulator by one full video frame (~1/60 s).
    pub fn run_frame(&mut self) {
        self.gb.run_frame();
    }

    /// The current frame as RGBA8888 (160×144×4 bytes), DMG green palette.
    ///
    /// Returns a copy suitable for constructing an `ImageData` in JS.
    pub fn frame_rgba(&mut self) -> Vec<u8> {
        self.gb.framebuffer_rgba(&mut self.frame);
        self.frame.clone()
    }

    /// Drain accumulated audio: interleaved stereo `f32` at 44100 Hz.
    pub fn take_audio(&mut self) -> Vec<f32> {
        self.gb.take_audio()
    }

    /// Press or release a button by index (0..=7). Out-of-range indices are
    /// ignored.
    pub fn set_button(&mut self, index: u8, pressed: bool) {
        if let Some(button) = button_from_index(index) {
            self.gb.set_button(button, pressed);
        }
    }

    /// The cartridge's internal title string.
    pub fn title(&self) -> String {
        self.gb.title().to_string()
    }

    /// Battery-backed cartridge RAM to persist, if any.
    pub fn save_data(&self) -> Option<Vec<u8>> {
        self.gb.save_data()
    }

    /// Whether the save RAM has changed since the last [`Self::mark_saved`].
    #[wasm_bindgen(js_name = isSaveDirty)]
    pub fn is_save_dirty(&self) -> bool {
        self.gb.save_is_dirty()
    }

    /// Acknowledge that the current save RAM has been persisted.
    #[wasm_bindgen(js_name = markSaved)]
    pub fn mark_saved(&mut self) {
        self.gb.mark_saved();
    }
}
