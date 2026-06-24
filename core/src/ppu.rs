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
}

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
    pub fn step(&mut self, _cycles: u32) -> u8 {
        // Implemented by the PPU task.
        0
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
