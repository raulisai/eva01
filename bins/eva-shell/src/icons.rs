//! The tray icons, drawn in memory: a disc whose color says what EVA is
//! doing at a glance. No asset files to bundle, sign or lose — and drawing
//! them is a few lines of geometry, unit-tested below.

use crate::model::TrayIcon;
use tray_icon::Icon;

/// Side of the (square) icon in pixels — drawn at 2x so it stays sharp on a
/// Retina menu bar; the menu bar scales it down.
const SIZE: u32 = 44;

/// The icon for `state`.
pub fn render(state: TrayIcon) -> Icon {
    let rgba = pixels(state);
    #[allow(clippy::expect_used)] // a buffer built from SIZE×SIZE×4 always matches its own dimensions
    Icon::from_rgba(rgba, SIZE, SIZE).expect("el ícono generado en memoria siempre tiene dimensiones válidas")
}

/// The RGBA pixels: an anti-aliased disc, or — for [`TrayIcon::Attention`] —
/// a ring, so "something needs you" is not just another shade of the solid
/// red of "listening".
pub(crate) fn pixels(state: TrayIcon) -> Vec<u8> {
    let (r, g, b) = color(state);
    let center = (SIZE as f32 - 1.0) / 2.0;
    let radius = SIZE as f32 / 2.0 - 3.0;
    let ring_thickness = 6.0;

    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let distance = ((x as f32 - center).powi(2) + (y as f32 - center).powi(2)).sqrt();
            // Signed distance to the shape's edge: negative inside.
            let edge = match state {
                TrayIcon::Attention => (distance - radius).max(radius - ring_thickness - distance),
                _ => distance - radius,
            };
            // One pixel of smoothing at the edge.
            let coverage = (0.5 - edge).clamp(0.0, 1.0);
            rgba.extend_from_slice(&[r, g, b, (coverage * 255.0).round() as u8]);
        }
    }
    rgba
}

fn color(state: TrayIcon) -> (u8, u8, u8) {
    match state {
        TrayIcon::Idle => (0x2E, 0xA0, 0x43),
        TrayIcon::Listening => (0xE5, 0x3E, 0x3E),
        TrayIcon::Busy => (0xF5, 0xA6, 0x23),
        TrayIcon::Attention => (0xE5, 0x3E, 0x3E),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests are exempt from the workspace error-handling rule, see docs/ENGINEERING.md #2
mod tests {
    use super::*;

    fn alpha_at(pixels: &[u8], x: u32, y: u32) -> u8 {
        pixels[((y * SIZE + x) * 4 + 3) as usize]
    }

    #[test]
    fn a_disc_is_solid_in_the_middle_and_transparent_in_the_corners() {
        for state in [TrayIcon::Idle, TrayIcon::Listening, TrayIcon::Busy] {
            let px = pixels(state);
            assert_eq!(alpha_at(&px, SIZE / 2, SIZE / 2), 255, "{state:?}");
            assert_eq!(alpha_at(&px, 0, 0), 0, "{state:?}");
            assert_eq!(alpha_at(&px, SIZE - 1, SIZE - 1), 0, "{state:?}");
        }
    }

    #[test]
    fn the_attention_icon_is_a_ring_hollow_in_the_middle() {
        let px = pixels(TrayIcon::Attention);
        assert_eq!(alpha_at(&px, SIZE / 2, SIZE / 2), 0, "the centre is empty");
        assert_eq!(alpha_at(&px, SIZE / 2, 4), 255, "the rim is solid");
    }

    #[test]
    fn edges_are_smoothed_not_jagged() {
        let px = pixels(TrayIcon::Idle);
        let partial = (0..SIZE).filter(|&x| (1..255).contains(&alpha_at(&px, x, SIZE / 2))).count();
        assert!(partial >= 2, "anti-aliasing must leave partially covered pixels at the edge");
    }

    #[test]
    fn every_state_looks_different_from_every_other() {
        let all = [TrayIcon::Idle, TrayIcon::Listening, TrayIcon::Busy, TrayIcon::Attention];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(pixels(*a), pixels(*b), "{a:?} and {b:?} must be distinguishable");
            }
        }
    }

    #[test]
    fn rendering_produces_a_valid_icon_for_every_state() {
        for state in [TrayIcon::Idle, TrayIcon::Listening, TrayIcon::Busy, TrayIcon::Attention] {
            let _ = render(state);
        }
    }
}
