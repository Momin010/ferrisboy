//! Picture Processing Unit (LCD controller).
//!
//! ## Interface contract (implemented by the PPU task)
//! The MMU and frontends rely on exactly these public items. Implementations
//! may add private fields/methods freely but must not change these signatures.

pub const SCREEN_WIDTH: usize = 160;
pub const SCREEN_HEIGHT: usize = 144;
const FB_LEN: usize = SCREEN_WIDTH * SCREEN_HEIGHT;

pub struct Ppu {
    vram: [u8; 0x2000],
    oam: [u8; 0xA0],
    /// 2-bit shade index per pixel (0 = lightest .. 3 = darkest).
    framebuffer: [u8; FB_LEN],
    frame_ready: bool,

    // LCD registers (FF40..FF4B).
    lcdc: u8,
    stat: u8,
    scy: u8,
    scx: u8,
    ly: u8,
    lyc: u8,
    bgp: u8,
    obp0: u8,
    obp1: u8,
    wy: u8,
    wx: u8,
    dma: u8,

    // Mode timing accumulator (T-cycles within the current scanline).
    mode_clock: u32,

    /// Window's internal line counter. Advances only on scanlines where the
    /// window is actually drawn (not simply when WY <= LY), which acid2 checks.
    window_line: u8,
    /// Whether the window was drawn on at least one prior line of this frame.
    /// Used together with `window_line` to drive the window's own counter.
    window_active_this_frame: bool,
    /// Previous state of the combined STAT interrupt line, so we can fire
    /// LCD_STAT only on its rising edge instead of every cycle.
    prev_stat_line: bool,
    /// Tracks whether the LCD was enabled on the previous step, so we can
    /// detect the off->on transition and resume cleanly from line 0.
    lcd_was_on: bool,
}

// LCD modes (STAT bits 0-1).
const MODE_HBLANK: u8 = 0;
const MODE_VBLANK: u8 = 1;
const MODE_OAM: u8 = 2;
const MODE_DRAW: u8 = 3;

// Per-scanline timing (T-cycles).
const OAM_CYCLES: u32 = 80;
const DRAW_CYCLES: u32 = 172;
const LINE_CYCLES: u32 = 456;
const VBLANK_START_LINE: u8 = 144;
const TOTAL_LINES: u8 = 154;

impl Ppu {
    pub fn new() -> Self {
        Ppu {
            vram: [0; 0x2000],
            oam: [0; 0xA0],
            framebuffer: [0; FB_LEN],
            frame_ready: false,
            // Post-boot register state (we skip the boot ROM).
            lcdc: 0x91,
            stat: 0x85,
            scy: 0,
            scx: 0,
            ly: 0,
            lyc: 0,
            bgp: 0xFC,
            obp0: 0xFF,
            obp1: 0xFF,
            wy: 0,
            wx: 0,
            dma: 0xFF,
            mode_clock: 0,
            window_line: 0,
            window_active_this_frame: false,
            prev_stat_line: false,
            lcd_was_on: true,
        }
    }

    pub fn read_vram(&self, addr: u16) -> u8 {
        self.vram[(addr & 0x1FFF) as usize]
    }

    pub fn write_vram(&mut self, addr: u16, val: u8) {
        self.vram[(addr & 0x1FFF) as usize] = val;
    }

    pub fn read_oam(&self, addr: u16) -> u8 {
        self.oam[(addr - 0xFE00) as usize]
    }

    pub fn write_oam(&mut self, addr: u16, val: u8) {
        self.oam[(addr - 0xFE00) as usize] = val;
    }

    pub fn read_reg(&self, addr: u16) -> u8 {
        match addr {
            0xFF40 => self.lcdc,
            0xFF41 => self.stat | 0x80,
            0xFF42 => self.scy,
            0xFF43 => self.scx,
            0xFF44 => self.ly,
            0xFF45 => self.lyc,
            0xFF46 => self.dma,
            0xFF47 => self.bgp,
            0xFF48 => self.obp0,
            0xFF49 => self.obp1,
            0xFF4A => self.wy,
            0xFF4B => self.wx,
            _ => 0xFF,
        }
    }

    pub fn write_reg(&mut self, addr: u16, val: u8) {
        match addr {
            0xFF40 => self.lcdc = val,
            0xFF41 => self.stat = (self.stat & 0x07) | (val & 0x78),
            0xFF42 => self.scy = val,
            0xFF43 => self.scx = val,
            0xFF44 => {} // LY is read-only
            0xFF45 => self.lyc = val,
            0xFF46 => self.dma = val, // actual transfer handled by the MMU
            0xFF47 => self.bgp = val,
            0xFF48 => self.obp0 = val,
            0xFF49 => self.obp1 = val,
            0xFF4A => self.wy = val,
            0xFF4B => self.wx = val,
            _ => {}
        }
    }

    /// Advance the PPU by `cycles` T-cycles. Returns interrupt request bits
    /// (`interrupts::VBLANK` and/or `interrupts::LCD_STAT`).
    pub fn step(&mut self, cycles: u32) -> u8 {
        use crate::interrupts;

        // LCDC bit7 = LCD enable. When off, the LCD is fully reset: LY=0,
        // mode/clock cleared, screen blanked. Nothing else happens until it
        // turns back on, at which point we resume from line 0.
        let lcd_on = self.lcdc & 0x80 != 0;
        if !lcd_on {
            if self.lcd_was_on {
                // Just turned off: blank everything.
                self.ly = 0;
                self.mode_clock = 0;
                self.window_line = 0;
                self.window_active_this_frame = false;
                self.prev_stat_line = false;
                self.stat &= !0x07; // mode 0, clear coincidence
                for p in self.framebuffer.iter_mut() {
                    *p = 0;
                }
                self.lcd_was_on = false;
            }
            return 0;
        }
        if !self.lcd_was_on {
            // Just turned back on: resume cleanly from the top.
            self.lcd_was_on = true;
            self.ly = 0;
            self.mode_clock = 0;
            self.window_line = 0;
            self.window_active_this_frame = false;
            self.prev_stat_line = false;
        }

        let mut interrupts: u8 = 0;

        // Drain the requested cycles in chunks that never cross a mode
        // boundary, so mode transitions and per-line work happen exactly once.
        let mut remaining = cycles;
        while remaining > 0 {
            let mode = self.current_mode();
            // Cycles left in the current mode/segment on this line.
            let segment_end = match mode {
                MODE_OAM => OAM_CYCLES,
                MODE_DRAW => OAM_CYCLES + DRAW_CYCLES,
                _ => LINE_CYCLES, // HBLANK or VBLANK run to end of line
            };
            let step_amt = (segment_end - self.mode_clock).min(remaining);
            self.mode_clock += step_amt;
            remaining -= step_amt;

            // Did we just cross into HBlank (end of drawing)? Render the line.
            if mode == MODE_DRAW && self.mode_clock >= OAM_CYCLES + DRAW_CYCLES {
                if self.ly < VBLANK_START_LINE {
                    self.render_scanline();
                }
            }

            // End of the scanline: advance LY and possibly the frame.
            if self.mode_clock >= LINE_CYCLES {
                self.mode_clock -= LINE_CYCLES;
                self.ly += 1;

                if self.ly == VBLANK_START_LINE {
                    // Entered VBlank: a full frame is ready.
                    self.frame_ready = true;
                    interrupts |= interrupts::VBLANK;
                } else if self.ly >= TOTAL_LINES {
                    // Wrap to the top of the next frame.
                    self.ly = 0;
                    self.window_line = 0;
                    self.window_active_this_frame = false;
                }
            }

            // Refresh STAT (mode + coincidence) and evaluate the STAT line
            // after every segment so rising edges are caught precisely.
            self.update_stat();
            if self.poll_stat_line() {
                interrupts |= interrupts::LCD_STAT;
            }
        }

        interrupts
    }

    /// Determine the current LCD mode from LY and the line clock.
    fn current_mode(&self) -> u8 {
        if self.ly >= VBLANK_START_LINE {
            MODE_VBLANK
        } else if self.mode_clock < OAM_CYCLES {
            MODE_OAM
        } else if self.mode_clock < OAM_CYCLES + DRAW_CYCLES {
            MODE_DRAW
        } else {
            MODE_HBLANK
        }
    }

    /// Write the current mode (bits 0-1) and LYC=LY coincidence (bit 2) into STAT.
    fn update_stat(&mut self) {
        let mode = self.current_mode();
        self.stat = (self.stat & !0x07) | (mode & 0x03);
        if self.ly == self.lyc {
            self.stat |= 0x04;
        }
    }

    /// Compute the combined STAT interrupt line and return true on its rising
    /// edge. Sources: bit3 (HBlank), bit4 (VBlank), bit5 (OAM), bit6 (LYC=LY).
    fn poll_stat_line(&mut self) -> bool {
        let mode = self.current_mode();
        let line = (self.stat & 0x08 != 0 && mode == MODE_HBLANK)
            || (self.stat & 0x10 != 0 && mode == MODE_VBLANK)
            || (self.stat & 0x20 != 0 && mode == MODE_OAM)
            || (self.stat & 0x40 != 0 && self.stat & 0x04 != 0);
        let rising = line && !self.prev_stat_line;
        self.prev_stat_line = line;
        rising
    }

    /// Render one full visible scanline (`self.ly`) into the framebuffer.
    fn render_scanline(&mut self) {
        let ly = self.ly as usize;
        let row = ly * SCREEN_WIDTH;

        // Per-pixel background/window color INDEX (pre-palette, 0..3). Needed so
        // sprites can apply the OBJ-to-BG priority rule against BG color 0.
        let mut bg_color_idx = [0u8; SCREEN_WIDTH];

        // --- Background & Window ---
        // LCDC bit0: on DMG this is the BG/Window master enable. When clear,
        // both BG and window are forced to color 0 (which maps to shade 0).
        let bg_enable = self.lcdc & 0x01 != 0;

        // Whether the window is enabled and could appear on this line.
        let window_enable = self.lcdc & 0x20 != 0;
        let wx = self.wx as i32 - 7; // screen x of the window's left edge
        let window_on_line = window_enable && bg_enable && self.ly >= self.wy && wx < SCREEN_WIDTH as i32;

        if bg_enable {
            // Tile data area: LCDC bit4. 0x8000 method = unsigned tile index;
            // 0x8800 method = signed index based at 0x9000.
            let signed_tiles = self.lcdc & 0x10 == 0;
            let bg_map_base: u16 = if self.lcdc & 0x08 != 0 { 0x9C00 } else { 0x9800 };
            let win_map_base: u16 = if self.lcdc & 0x40 != 0 { 0x9C00 } else { 0x9800 };

            for x in 0..SCREEN_WIDTH {
                let screen_x = x as i32;
                let (map_base, map_x, map_y);

                if window_on_line && screen_x >= wx {
                    // Window pixel: uses the window's own line counter.
                    map_base = win_map_base;
                    map_x = (screen_x - wx) as u32;
                    map_y = self.window_line as u32;
                } else {
                    // Background pixel: scrolled, wrapping in the 256x256 map.
                    map_base = bg_map_base;
                    map_x = (x as u32 + self.scx as u32) & 0xFF;
                    map_y = (ly as u32 + self.scy as u32) & 0xFF;
                }

                let color = self.bg_pixel_color(map_base, map_x, map_y, signed_tiles);
                bg_color_idx[x] = color;
                self.framebuffer[row + x] = (self.bgp >> (color * 2)) & 0x03;
            }
        } else {
            // BG/Window off: blank line (color 0 -> shade 0).
            for x in 0..SCREEN_WIDTH {
                self.framebuffer[row + x] = 0;
            }
        }

        // Advance the window's internal line counter only when the window was
        // actually drawn on this line.
        if window_on_line {
            self.window_line = self.window_line.wrapping_add(1);
            self.window_active_this_frame = true;
        }

        // --- Sprites (OBJ) ---
        if self.lcdc & 0x02 != 0 {
            self.render_sprites(ly, row, &bg_color_idx);
        }
    }

    /// Fetch the 2-bit color index of a BG/window pixel at map coordinates
    /// (`map_x`, `map_y`) in pixels, given the tile map base and addressing mode.
    fn bg_pixel_color(&self, map_base: u16, map_x: u32, map_y: u32, signed_tiles: bool) -> u8 {
        let tile_col = map_x / 8;
        let tile_row = map_y / 8;
        let map_index = map_base + (tile_row * 32 + tile_col) as u16;
        let tile_num = self.read_vram(map_index);

        // Resolve the start address of the tile's bitmap.
        let tile_addr: u16 = if signed_tiles {
            // 0x8800 method: index is signed, base 0x9000.
            (0x9000_i32 + (tile_num as i8 as i32) * 16) as u16
        } else {
            // 0x8000 method: index is unsigned.
            0x8000 + (tile_num as u16) * 16
        };

        let line_in_tile = (map_y % 8) as u16;
        let lo = self.read_vram(tile_addr + line_in_tile * 2);
        let hi = self.read_vram(tile_addr + line_in_tile * 2 + 1);

        let bit = 7 - (map_x % 8) as u8;
        ((hi >> bit) & 1) << 1 | ((lo >> bit) & 1)
    }

    /// Render sprites overlapping scanline `ly`, honoring the 10-per-line limit,
    /// X-then-OAM-order priority, flips, palettes, and OBJ-to-BG priority.
    fn render_sprites(&mut self, ly: usize, row: usize, bg_color_idx: &[u8; SCREEN_WIDTH]) {
        let tall = self.lcdc & 0x04 != 0; // 8x16 mode
        let height: i32 = if tall { 16 } else { 8 };

        // Collect up to the first 10 sprites (in OAM order) that intersect ly.
        // Each entry: (oam_index, x, y, tile, flags).
        let mut visible: Vec<(usize, i32, i32, u8, u8)> = Vec::with_capacity(10);
        for i in 0..40 {
            let base = i * 4;
            let sy = self.oam[base] as i32 - 16;
            let sx = self.oam[base + 1] as i32 - 8;
            let tile = self.oam[base + 2];
            let flags = self.oam[base + 3];

            if (ly as i32) >= sy && (ly as i32) < sy + height {
                visible.push((i, sx, sy, tile, flags));
                if visible.len() == 10 {
                    break;
                }
            }
        }

        // Draw priority: lower X wins; for equal X, lower OAM index wins. To get
        // that with a simple overwrite, draw lowest-priority first (highest X /
        // highest OAM index first), so higher-priority sprites paint over them.
        visible.sort_by(|a, b| {
            // a < b means "a is lower priority" (drawn first).
            b.1.cmp(&a.1).then(b.0.cmp(&a.0))
        });

        for (_idx, sx, sy, mut tile, flags) in visible {
            let palette = if flags & 0x10 != 0 { self.obp1 } else { self.obp0 };
            let x_flip = flags & 0x20 != 0;
            let y_flip = flags & 0x40 != 0;
            let behind_bg = flags & 0x80 != 0; // OBJ-to-BG priority

            // Row within the sprite, accounting for vertical flip.
            let mut line_in_sprite = ly as i32 - sy;
            if y_flip {
                line_in_sprite = height - 1 - line_in_sprite;
            }

            // In 8x16 mode the low bit of the tile index is ignored; the two
            // 8x8 halves are consecutive tiles.
            if tall {
                tile &= 0xFE;
            }
            let tile_addr = 0x8000 + (tile as u16) * 16 + (line_in_sprite as u16) * 2;
            let lo = self.read_vram(tile_addr);
            let hi = self.read_vram(tile_addr + 1);

            for px in 0..8i32 {
                let screen_x = sx + px;
                if screen_x < 0 || screen_x >= SCREEN_WIDTH as i32 {
                    continue;
                }
                let bit = if x_flip { px } else { 7 - px } as u8;
                let color = ((hi >> bit) & 1) << 1 | ((lo >> bit) & 1);
                if color == 0 {
                    continue; // color 0 is transparent for sprites
                }
                // OBJ-to-BG priority: when set, the sprite is hidden behind any
                // non-zero BG/window color, but still shows over BG color 0.
                if behind_bg && bg_color_idx[screen_x as usize] != 0 {
                    continue;
                }
                let shade = (palette >> (color * 2)) & 0x03;
                self.framebuffer[row + screen_x as usize] = shade;
            }
        }
    }

    /// Returns true once per completed frame, clearing the flag.
    pub fn take_frame_ready(&mut self) -> bool {
        let ready = self.frame_ready;
        self.frame_ready = false;
        ready
    }

    /// 160×144 buffer of 2-bit shade indices.
    pub fn framebuffer(&self) -> &[u8] {
        &self.framebuffer
    }
}

/// Map 2-bit shade indices to RGBA8888 using the classic DMG green palette.
pub fn shades_to_rgba(shades: &[u8], out: &mut [u8]) {
    const PALETTE: [[u8; 4]; 4] = [
        [0x9B, 0xBC, 0x0F, 0xFF], // lightest
        [0x8B, 0xAC, 0x0F, 0xFF],
        [0x30, 0x62, 0x30, 0xFF],
        [0x0F, 0x38, 0x0F, 0xFF], // darkest
    ];
    for (pixel, &shade) in out.chunks_exact_mut(4).zip(shades.iter()) {
        pixel.copy_from_slice(&PALETTE[(shade & 3) as usize]);
    }
}
