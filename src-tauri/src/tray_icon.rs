//! Draws the tray icon: the session % in dark digits on a square coloured by how full it is.

use crate::account;

pub const SIZE: u32 = 32;
const RADIUS: f64 = 6.0;
const INK: [u8; 3] = [0x11, 0x11, 0x11];
const UNKNOWN: [u8; 3] = [0x8a, 0x8a, 0x8f];

// 3x5 pixel digits, one row per byte, bit 2 is the left column. The last glyph is '-'.
const GLYPHS: [[u8; 5]; 11] = [
    [7, 5, 5, 5, 7],
    [2, 6, 2, 2, 7],
    [7, 1, 7, 4, 7],
    [7, 1, 7, 1, 7],
    [5, 5, 7, 1, 1],
    [7, 4, 7, 1, 7],
    [7, 4, 7, 5, 7],
    [7, 1, 1, 1, 1],
    [7, 5, 7, 5, 7],
    [7, 5, 7, 1, 7],
    [0, 0, 7, 0, 0],
];
const DASH: usize = 10;

/// RGBA pixels for `percent`, or "--" in grey when the account numbers are unavailable.
pub fn render(percent: Option<f64>) -> Vec<u8> {
    let (background, glyphs) = match percent {
        Some(p) => (band_colour(p), digits(p.round().clamp(0.0, 999.0) as u32)),
        None => (UNKNOWN, vec![DASH, DASH]),
    };
    // Two digits fit at 4x (12px wide each); "100" needs 2x to stay inside 32px.
    let scale = if glyphs.len() <= 2 { 4 } else { 2 };
    let text_width = glyphs.len() as u32 * 4 * scale - scale;
    let left = (SIZE - text_width) / 2;
    let top = (SIZE - 5 * scale) / 2;

    let mut pixels = vec![0u8; (SIZE * SIZE * 4) as usize];
    for y in 0..SIZE {
        for x in 0..SIZE {
            if !inside_rounded_square(x, y) {
                continue;
            }
            let colour = if is_ink(&glyphs, x as i64 - left as i64, y as i64 - top as i64, scale) { INK } else { background };
            let i = ((y * SIZE + x) * 4) as usize;
            pixels[i..i + 4].copy_from_slice(&[colour[0], colour[1], colour[2], 0xff]);
        }
    }
    pixels
}

fn band_colour(percent: f64) -> [u8; 3] {
    match account::level(percent) {
        "high" => [0xef, 0x6b, 0x5d],
        "warn" => [0xe0, 0xb3, 0x41],
        _ => [0x4c, 0xaf, 0x7a],
    }
}

fn digits(value: u32) -> Vec<usize> {
    value.to_string().bytes().map(|b| (b - b'0') as usize).collect()
}

fn is_ink(glyphs: &[usize], x: i64, y: i64, scale: u32) -> bool {
    let scale = scale as i64;
    if x < 0 || y < 0 || y >= 5 * scale {
        return false;
    }
    let cell = x / scale;
    let (glyph, column) = ((cell / 4) as usize, cell % 4);
    if glyph >= glyphs.len() || column == 3 {
        return false;
    }
    let row = GLYPHS[glyphs[glyph]][(y / scale) as usize];
    row & (4 >> column) != 0
}

fn inside_rounded_square(x: u32, y: u32) -> bool {
    let distance_past_corner = |v: u32| {
        let centre = v as f64 + 0.5;
        (RADIUS - centre).max(centre - (SIZE as f64 - RADIUS)).max(0.0)
    };
    let (dx, dy) = (distance_past_corner(x), distance_past_corner(y));
    dx * dx + dy * dy <= RADIUS * RADIUS
}
