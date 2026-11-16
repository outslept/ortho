use std::env;
use std::error::Error;
use std::fs;
use std::path::PathBuf;

// Icon width and height in pixels.
const ICON_DIM: u32 = 32;

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").ok_or("OUT_DIR not set")?);
    let ico_path = out_dir.join("ortho.ico");
    fs::write(&ico_path, build_ortho_ico())?;

    let ico_str = ico_path.to_str().ok_or("non-UTF8 OUT_DIR path")?;

    let mut res = winresource::WindowsResource::new();
    res.set_icon_with_id(ico_str, "1");
    res.set("ProductName", "Ortho");
    res.set("FileDescription", "Ortho - Fast Rectangle-style Window Manager");
    res.set("LegalCopyright", "MIT License");
    res.compile()?;

    Ok(())
}

// Generates a 32x32 32-bit BGRA `.ico` depicting a 3-pane tiled desktop layout.
fn build_ortho_ico() -> Vec<u8> {
    let xor_bytes: u32 = ICON_DIM * ICON_DIM * 4;
    let and_bytes: u32 = ICON_DIM * 4;
    let dib_size: u32 = 40 + xor_bytes + and_bytes;

    let mut buf = Vec::with_capacity(6400);

    // ICONDIR header (6 bytes): reserved = 0, type = 1 (ICO), count = 1
    buf.extend_from_slice(&[0, 0, 1, 0, 1, 0]);

    // ICONDIRENTRY (16 bytes): 32x32, 32-bit BGRA, offset = 22
    buf.extend_from_slice(&[32, 32, 0, 0, 1, 0, 32, 0]);
    buf.extend_from_slice(&dib_size.to_le_bytes());
    buf.extend_from_slice(&22_u32.to_le_bytes());

    // BITMAPINFOHEADER (40 bytes)
    buf.extend_from_slice(&40_u32.to_le_bytes());
    buf.extend_from_slice(&ICON_DIM.to_le_bytes());
    buf.extend_from_slice(&(ICON_DIM * 2).to_le_bytes()); // Combined height of XOR + AND masks
    buf.extend_from_slice(&1_u16.to_le_bytes());
    buf.extend_from_slice(&32_u16.to_le_bytes());
    buf.extend_from_slice(&0_u32.to_le_bytes());
    buf.extend_from_slice(&xor_bytes.to_le_bytes());
    buf.extend_from_slice(&[0_u8; 16]);

    // Bottom-up 32x32 BGRA pixel grid
    for row_bottom in 0..ICON_DIM {
        let y = (ICON_DIM - 1) - row_bottom;
        for x in 0..ICON_DIM {
            buf.extend_from_slice(&pixel_bgra(x, y));
        }
    }

    // 1-bit AND mask (all 0 = alpha channel controls transparency)
    for _ in 0..and_bytes {
        buf.push(0);
    }

    buf
}

// Computes the BGRA color for pixel `(x, y)` (top-down coordinates `0..32`).
const fn pixel_bgra(x: u32, y: u32) -> [u8; 4] {
    // Transparent outer margin + rounded corners
    if x < 2 || x > 29 || y < 2 || y > 29 {
        return [0, 0, 0, 0];
    }
    if (x == 2 || x == 29) && (y == 2 || y == 29) {
        return [0, 0, 0, 0];
    }

    // Left Half tile pane (bright cyan-blue #38BDF8 -> BGRA [248, 189, 56, 255])
    if x >= 5 && x <= 14 && y >= 5 && y <= 26 {
        return [248, 189, 56, 255];
    }

    // Top-Right Quarter tile pane (soft indigo #818CF8 -> BGRA [248, 140, 129, 255])
    if x >= 17 && x <= 26 && y >= 5 && y <= 14 {
        return [248, 140, 129, 255];
    }

    // Bottom-Right Quarter tile pane (emerald-teal #2DD4BF -> BGRA [191, 212, 45, 255])
    if x >= 17 && x <= 26 && y >= 17 && y <= 26 {
        return [191, 212, 45, 255];
    }

    // Subtle 1px outer border (#3F3F46 -> BGRA [70, 63, 63, 255])
    if x == 2 || x == 29 || y == 2 || y == 29 {
        return [70, 63, 63, 255];
    }

    // Dark zinc desktop background (#18181B -> BGRA [27, 24, 24, 255])
    [27, 24, 24, 255]
}
