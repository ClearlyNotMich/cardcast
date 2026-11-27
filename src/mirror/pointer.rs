//! The mouse pointer Desktop Duplication reports separately from the picture: its shape as
//! drawable pixels, and where it lands on the card display. No Windows calls.

use super::{Fit, upright_size};

/// `DXGI_OUTDUPL_POINTER_SHAPE_TYPE` values.
pub const MONOCHROME: u32 = 1;
pub const COLOR: u32 = 2;
pub const MASKED_COLOR: u32 = 4;

/// A pointer shape as BGRA pixels with straight alpha, rows packed (`width * 4` bytes each).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PointerImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

const BLACK: [u8; 4] = [0, 0, 0, 255];
const WHITE: [u8; 4] = [255, 255, 255, 255];
const CLEAR: [u8; 4] = [0, 0, 0, 0];

/// Turns the shape from `GetFramePointerShape` into pixels; `None` for an unknown type or short buffer.
/// shortcut: pixels that invert the screen are drawn black; upgrade if a game's pointer vanishes on dark scenes.
pub fn pointer_image(
    kind: u32,
    width: u32,
    height: u32,
    pitch: u32,
    shape: &[u8],
) -> Option<PointerImage> {
    let (width_px, pitch) = (width as usize, pitch as usize);
    // A monochrome shape is an AND mask followed by an XOR mask, so it is half as tall as reported.
    let rows = if kind == MONOCHROME {
        height / 2
    } else {
        height
    };
    let mut pixels = Vec::with_capacity(width_px * rows as usize * 4);
    for y in 0..rows as usize {
        for x in 0..width_px {
            let pixel = match kind {
                MONOCHROME => {
                    let bit = |row: usize| {
                        shape
                            .get(row * pitch + x / 8)
                            .map(|byte| byte & (0x80 >> (x % 8)) != 0)
                    };
                    match (bit(y)?, bit(y + rows as usize)?) {
                        (false, false) => BLACK,
                        (false, true) => WHITE,
                        (true, false) => CLEAR,
                        (true, true) => BLACK,
                    }
                }
                COLOR | MASKED_COLOR => {
                    let at = y * pitch + x * 4;
                    let bgra: [u8; 4] = shape.get(at..at + 4)?.try_into().ok()?;
                    match (kind, bgra) {
                        (COLOR, _) => bgra,
                        // Mask 0: the colour replaces the screen pixel.
                        (_, [b, g, r, 0]) => [b, g, r, 255],
                        // Mask 0xFF: XOR with the screen; XOR with black leaves the screen as it is.
                        (_, [0, 0, 0, _]) => CLEAR,
                        _ => BLACK,
                    }
                }
                _ => return None,
            };
            pixels.extend_from_slice(&pixel);
        }
    }
    Some(PointerImage {
        width,
        height: rows,
        pixels,
    })
}

/// Where a pointer `shape` (width, height) at `position` (its top-left on the source monitor's
/// upright desktop) lands on the card display when the duplicated `surface`, turned by `rotation`
/// (as in `super::upright_size`), is drawn upright into `fit`. The shape comes upright too.
/// Returns left, top, width and height in card display pixels, as a Direct3D viewport takes them.
pub fn pointer_viewport(
    fit: Fit,
    surface: (u32, u32),
    rotation: i32,
    position: (i32, i32),
    shape: (u32, u32),
) -> [f32; 4] {
    let desktop = upright_size(surface, rotation);
    let scale_x = fit.width as f32 / desktop.0.max(1) as f32;
    let scale_y = fit.height as f32 / desktop.1.max(1) as f32;
    [
        fit.left as f32 + position.0 as f32 * scale_x,
        fit.top as f32 + position.1 as f32 * scale_y,
        shape.0 as f32 * scale_x,
        shape.1 as f32 * scale_y,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monochrome_masks_become_black_white_and_clear() {
        // 2×1 pointer, pitch 1: AND row then XOR row. Pixel 0: AND 0 XOR 1 (white); pixel 1: AND 1 XOR 0 (clear).
        let shape = [0b0100_0000, 0b1000_0000];
        let image = pointer_image(MONOCHROME, 2, 2, 1, &shape).unwrap();
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(image.pixels, [WHITE, CLEAR].concat());
    }

    #[test]
    fn monochrome_inverting_pixels_are_drawn_black() {
        let shape = [0b1000_0000, 0b1000_0000];
        let image = pointer_image(MONOCHROME, 1, 2, 1, &shape).unwrap();
        assert_eq!(image.pixels, BLACK);
    }

    #[test]
    fn colour_pointer_keeps_its_alpha_and_skips_row_padding() {
        // 1×2 pointer with an 8-byte pitch: 4 bytes of padding per row are dropped.
        let shape = [1, 2, 3, 128, 9, 9, 9, 9, 4, 5, 6, 0, 9, 9, 9, 9];
        let image = pointer_image(COLOR, 1, 2, 8, &shape).unwrap();
        assert_eq!(image.pixels, [1, 2, 3, 128, 4, 5, 6, 0]);
    }

    #[test]
    fn masked_colour_replaces_or_leaves_the_screen() {
        let shape = [10, 20, 30, 0, 0, 0, 0, 255, 50, 50, 50, 255];
        let image = pointer_image(MASKED_COLOR, 3, 1, 12, &shape).unwrap();
        assert_eq!(image.pixels, [[10, 20, 30, 255], CLEAR, BLACK].concat());
    }

    #[test]
    fn short_buffer_or_unknown_type_gives_no_pointer() {
        assert_eq!(pointer_image(COLOR, 2, 2, 8, &[0; 12]), None);
        assert_eq!(pointer_image(8, 1, 1, 4, &[0; 4]), None);
    }

    #[test]
    fn pointer_scales_with_the_picture() {
        let fit = Fit {
            left: 0,
            top: 0,
            width: 1920,
            height: 1080,
        };
        assert_eq!(
            pointer_viewport(fit, (2560, 1440), 1, (1280, 720), (32, 32)),
            [960.0, 540.0, 24.0, 24.0]
        );
    }

    #[test]
    fn pointer_on_a_portrait_source_monitor_keeps_its_desktop_position() {
        // An 800x600 surface turned 90° is a 600x800 desktop, drawn at twice its size.
        let fit = Fit {
            left: 600,
            top: 0,
            width: 1200,
            height: 1600,
        };
        // PointerPosition is on the upright desktop, so the shape is drawn as it comes.
        assert_eq!(
            pointer_viewport(fit, (800, 600), 2, (300, 400), (32, 16)),
            [1200.0, 800.0, 64.0, 32.0]
        );
    }

    #[test]
    fn pointer_follows_a_letterboxed_picture() {
        let fit = Fit {
            left: 0,
            top: 135,
            width: 1920,
            height: 810,
        };
        assert_eq!(
            pointer_viewport(fit, (2560, 1080), 1, (-4, 0), (32, 32)),
            [-3.0, 135.0, 24.0, 24.0]
        );
    }
}
