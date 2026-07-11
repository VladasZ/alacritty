//! Save the current GL framebuffer to a PNG file.
//!
//! Triggered by SIGUSR1 so a running window can be captured with no screen
//! recording permission. Used to iterate on the window chrome and tab bar.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use log::{error, info};

use crate::gl;

/// Path a screenshot is written to, overridable with `ALACRITTY_SCREENSHOT_PATH`.
pub fn screenshot_path() -> PathBuf {
    std::env::var_os("ALACRITTY_SCREENSHOT_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/alacritty-screenshot.png"))
}

/// Read the back buffer and write it to `path` as an opaque PNG.
///
/// Must be called on the render thread while the context is current, after the
/// frame is drawn and before the buffers are swapped.
pub fn capture(path: &Path, width: u32, height: u32) {
    if width == 0 || height == 0 {
        return;
    }

    let (w, h) = (width as usize, height as usize);
    let mut pixels = vec![0u8; w * h * 4];
    unsafe {
        gl::ReadPixels(
            0,
            0,
            width as i32,
            height as i32,
            gl::RGBA,
            gl::UNSIGNED_BYTE,
            pixels.as_mut_ptr().cast(),
        );
    }

    // GL reads bottom-up, so flip into a top-down image and force full opacity
    // to keep translucent backgrounds from saving as transparent pixels.
    let mut image = vec![0u8; w * h * 4];
    for row in 0..h {
        let src = (h - 1 - row) * w * 4;
        let dst = row * w * 4;
        image[dst..dst + w * 4].copy_from_slice(&pixels[src..src + w * 4]);
        for px in 0..w {
            image[dst + px * 4 + 3] = 255;
        }
    }

    match write_png(path, width, height, &image) {
        Ok(()) => info!("Screenshot saved to {}", path.display()),
        Err(err) => error!("Failed to write screenshot to {}: {err}", path.display()),
    }
}

fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<(), png::EncodingError> {
    let file = File::create(path)?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(rgba)?;
    Ok(())
}
