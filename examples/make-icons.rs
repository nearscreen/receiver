//! Draws the application icons from the mark, so there is one source of truth
//! for what Nearscreen looks like — no exported bitmaps to keep in step.
//!
//! `cargo run --example make-icons` writes:
//!   packaging/icons/icon-<size>.png   — for macOS's iconutil and anything else
//!   packaging/windows/nearscreen-receiver.ico — embedded into the .exe
//!
//! The macOS .icns is assembled from these PNGs by the release workflow.

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::Path;

use nearscreen_receiver::ui::icon_pixels;

/// What Windows wants inside an .ico, and what macOS wants in an iconset.
const SIZES: [u32; 8] = [16, 32, 48, 64, 128, 256, 512, 1024];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let png_dir = Path::new("packaging/icons");
    let ico_dir = Path::new("packaging/windows");
    fs::create_dir_all(png_dir)?;
    fs::create_dir_all(ico_dir)?;

    let mut pngs = Vec::new();
    for size in SIZES {
        let rgba = icon_pixels(size, false);
        let path = png_dir.join(format!("icon-{size}.png"));
        let bytes = encode_png(size, size, &rgba)?;
        fs::write(&path, &bytes)?;
        println!("{} ({} bytes)", path.display(), bytes.len());
        pngs.push((size, bytes));
    }

    // An .ico is a small directory of images; from Vista onwards each entry
    // may simply be a PNG, which is what every size here is.
    let wanted: Vec<_> = pngs
        .iter()
        .filter(|(size, _)| matches!(size, 16 | 32 | 48 | 64 | 128 | 256))
        .collect();
    let path = ico_dir.join("nearscreen-receiver.ico");
    let mut out = BufWriter::new(File::create(&path)?);
    out.write_all(&0u16.to_le_bytes())?; // reserved
    out.write_all(&1u16.to_le_bytes())?; // an icon, not a cursor
    out.write_all(&(wanted.len() as u16).to_le_bytes())?;
    let mut offset = 6 + 16 * wanted.len() as u32;
    for (size, bytes) in &wanted {
        // 256 is written as zero, the format's way of saying "the big one".
        let dimension = if *size >= 256 { 0u8 } else { *size as u8 };
        out.write_all(&[dimension, dimension, 0, 0])?;
        out.write_all(&1u16.to_le_bytes())?; // colour planes
        out.write_all(&32u16.to_le_bytes())?; // bits per pixel
        out.write_all(&(bytes.len() as u32).to_le_bytes())?;
        out.write_all(&offset.to_le_bytes())?;
        offset += bytes.len() as u32;
    }
    for (_, bytes) in &wanted {
        out.write_all(bytes)?;
    }
    out.flush()?;
    println!("{} ({} sizes)", path.display(), wanted.len());
    Ok(())
}

/// Writes a PNG the plain way: one uncompressed deflate block per row-set, so
/// no compression library is needed for a handful of small icons.
fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut raw = Vec::with_capacity((width * height * 4 + height) as usize);
    for row in 0..height as usize {
        raw.push(0); // no filter
        let from = row * width as usize * 4;
        raw.extend_from_slice(&rgba[from..from + width as usize * 4]);
    }

    let mut png = Vec::new();
    png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    let mut header = Vec::new();
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA
    chunk(&mut png, b"IHDR", &header);
    chunk(&mut png, b"IDAT", &deflate_stored(&raw));
    chunk(&mut png, b"IEND", &[]);
    Ok(png)
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

/// A zlib stream of stored (uncompressed) deflate blocks.
fn deflate_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01]; // zlib header, no compression
    for (index, block) in data.chunks(0xFFFF).enumerate() {
        let last = (index + 1) * 0xFFFF >= data.len();
        out.push(if last { 1 } else { 0 });
        out.extend_from_slice(&(block.len() as u16).to_le_bytes());
        out.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        out.extend_from_slice(block);
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for byte in data {
        a = (a + u32::from(*byte)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}
