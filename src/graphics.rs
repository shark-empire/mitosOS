//! Framebuffer Graphics Engine for mitosOS.

#[derive(Debug, Clone, Copy)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const BLACK: Color = Color { r: 0, g: 0, b: 0 };
    pub const WHITE: Color = Color { r: 255, g: 255, b: 255 };
    pub const CYAN: Color  = Color { r: 0, g: 255, b: 255 };
    
    pub fn to_u32(&self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | (self.b as u32)
    }
}

pub struct Framebuffer {
    pub address: *mut u32,
    pub width: usize,
    pub height: usize,
    pub pitch: usize, // Bytes per scanline
}

impl Framebuffer {
    pub unsafe fn new(address: usize, width: usize, height: usize, pitch: usize) -> Self {
        Self {
            address: address as *mut u32,
            width,
            height,
            pitch,
        }
    }

    /// Draws a single RGB pixel at (x, y) coordinates.
    pub fn draw_pixel(&mut self, x: usize, y: usize, color: Color) {
        if x >= self.width || y >= self.height {
            return;
        }
        let offset = (y * (self.pitch / 4)) + x;
        unsafe {
            self.address.add(offset).write_volatile(color.to_u32());
        }
    }

    /// Fills the entire screen with a solid color.
    pub fn clear(&mut self, color: Color) {
        for y in 0..self.height {
            for x in 0..self.width {
                self.draw_pixel(x, y, color);
            }
        }
    }

    /// Draws a filled rectangle on screen.
    pub fn draw_rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: Color) {
        for row in y..(y + h) {
            for col in x..(x + w) {
                self.draw_pixel(col, row, color);
            }
        }
    }
}
