//! Output to a Linux framebuffer (/dev/fb0). The pixel layout comes from the kernel
//! (FBIOGET_VSCREENINFO): on the Pebble panel it's 24 bpp with blue in the low byte (B G R).
//! Only rows that changed since the last frame are written: the SPI panel needs ~92 ms for a
//! full frame, but a minute-tick of the clock touches ~20 rows.

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;
use std::path::Path;

use crate::error::{Result, err};

const FBIOGET_VSCREENINFO: libc::c_ulong = 0x4600;
const FBIOGET_FSCREENINFO: libc::c_ulong = 0x4602;

/// Where each colour sits in a pixel: (bit offset, bit length).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Format {
    pub bpp: u32,
    pub red: (u32, u32),
    pub green: (u32, u32),
    pub blue: (u32, u32),
}

impl Format {
    /// The Pebble panel (the real one is read from the kernel; this is for tests).
    #[cfg(test)]
    pub const BGR888: Format = Format {
        bpp: 24,
        red: (16, 8),
        green: (8, 8),
        blue: (0, 8),
    };

    fn bytes(&self) -> usize {
        (self.bpp / 8) as usize
    }

    /// One RGBA row → this format, appended to `out`.
    pub fn convert_row(&self, rgba: &[u8], out: &mut Vec<u8>) {
        let n = self.bytes();
        let ch = |v: u8, (off, len): (u32, u32)| ((v as u32) >> (8 - len.min(8))) << off;
        for p in rgba.as_chunks::<4>().0 {
            let v = ch(p[0], self.red) | ch(p[1], self.green) | ch(p[2], self.blue);
            out.extend_from_slice(&v.to_le_bytes()[..n]);
        }
    }
}

/// Runs of rows that differ between two RGBA frames, as (first row, row count).
pub fn changed_bands(old: Option<&[u8]>, new: &[u8], row_bytes: usize) -> Vec<(usize, usize)> {
    let rows = new.len() / row_bytes;
    let mut bands = Vec::new();
    let mut start = None;
    for y in 0..rows {
        let r = y * row_bytes..(y + 1) * row_bytes;
        let changed = old.is_none_or(|o| o[r.clone()] != new[r]);
        match (changed, start) {
            (true, None) => start = Some(y),
            (false, Some(s)) => {
                bands.push((s, y - s));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        bands.push((s, rows - s));
    }
    bands
}

pub struct Framebuffer {
    file: File,
    stride: usize,
    format: Format,
    width: usize,
    height: usize,
    last: Option<Vec<u8>>,
}

impl Framebuffer {
    pub fn open(path: &Path, width: usize, height: usize) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| err(format!("{}: {e}", path.display())))?;
        let mut v = [0u8; 160];
        let mut f = [0u8; 80];
        // SAFETY: both ioctls only fill the buffers we pass, which are larger than the structs.
        let (rv, rf) = unsafe {
            (
                libc::ioctl(file.as_raw_fd(), FBIOGET_VSCREENINFO as _, v.as_mut_ptr()),
                libc::ioctl(file.as_raw_fd(), FBIOGET_FSCREENINFO as _, f.as_mut_ptr()),
            )
        };
        if rv != 0 || rf != 0 {
            return Err(err(format!(
                "{}: not a framebuffer ({})",
                path.display(),
                std::io::Error::last_os_error()
            )));
        }
        let u = |b: &[u8], at: usize| u32::from_ne_bytes(b[at..at + 4].try_into().unwrap());
        let (xres, yres, bpp) = (u(&v, 0) as usize, u(&v, 4) as usize, u(&v, 24));
        let mut format = Format {
            bpp,
            red: (u(&v, 32), u(&v, 36)),
            green: (u(&v, 44), u(&v, 48)),
            blue: (u(&v, 56), u(&v, 60)),
        };
        // Some SPI panels are wired BGR while the driver reports RGB (the Pebble's does: red
        // showed as purple, amber as light blue). PIOS_FB_SWAP_RB=1 swaps them back.
        if std::env::var("PIOS_FB_SWAP_RB").is_ok_and(|v| v == "1") {
            std::mem::swap(&mut format.red, &mut format.blue);
        }
        if (xres, yres) != (width, height) || !matches!(bpp, 16 | 24 | 32) {
            return Err(err(format!(
                "{}: {xres}x{yres} {bpp} bpp, expected {width}x{height} at 16/24/32 bpp",
                path.display()
            )));
        }
        // line_length sits at byte 48 of fb_fix_screeninfo on 64-bit
        let mut stride = u(&f, 48) as usize;
        if stride < xres * format.bytes() {
            stride = xres * format.bytes();
        }
        Ok(Framebuffer {
            file,
            stride,
            format,
            width,
            height,
            last: None,
        })
    }

    /// Writes the rows of `rgba` that changed since the last call; returns how many rows were sent.
    pub fn present(&mut self, rgba: &[u8]) -> Result<usize> {
        let row_bytes = self.width * 4;
        if rgba.len() != row_bytes * self.height {
            return Err(err("frame has the wrong size"));
        }
        let mut sent = 0;
        let mut buf = Vec::with_capacity(self.stride * 32);
        for (y0, n) in changed_bands(self.last.as_deref(), rgba, row_bytes) {
            buf.clear();
            for y in y0..y0 + n {
                self.format
                    .convert_row(&rgba[y * row_bytes..(y + 1) * row_bytes], &mut buf);
                buf.resize((y - y0 + 1) * self.stride, 0);
            }
            self.file.write_all_at(&buf, (y0 * self.stride) as u64)?;
            sent += n;
        }
        self.last = Some(rgba.to_vec());
        Ok(sent)
    }

    /// Next `present` sends the whole frame (after the screen was off or something else drew).
    pub fn invalidate(&mut self) {
        self.last = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swapped_red_and_blue() {
        let mut f = Format::BGR888;
        std::mem::swap(&mut f.red, &mut f.blue);
        let mut out = Vec::new();
        f.convert_row(&[0x11, 0x22, 0x33, 0xFF], &mut out);
        assert_eq!(out, [0x11, 0x22, 0x33]);
    }

    #[test]
    fn bgr888_puts_blue_first() {
        let mut out = Vec::new();
        Format::BGR888.convert_row(&[0x11, 0x22, 0x33, 0xFF, 0xAA, 0xBB, 0xCC, 0xFF], &mut out);
        assert_eq!(out, [0x33, 0x22, 0x11, 0xCC, 0xBB, 0xAA]);
    }

    #[test]
    fn rgb565() {
        let f = Format {
            bpp: 16,
            red: (11, 5),
            green: (5, 6),
            blue: (0, 5),
        };
        let mut out = Vec::new();
        f.convert_row(&[0xFF, 0x00, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF], &mut out);
        assert_eq!(out, [0x00, 0xF8, 0xE0, 0x07]);
    }

    #[test]
    fn bands() {
        let row = 4;
        let old = vec![0u8; row * 6];
        let mut new = old.clone();
        new[row] = 1; // row 1
        new[row * 2] = 1; // row 2
        new[row * 5] = 1; // row 5
        assert_eq!(changed_bands(Some(&old), &new, row), [(1, 2), (5, 1)]);
        assert_eq!(changed_bands(Some(&old), &old, row), []);
        assert_eq!(changed_bands(None, &old, row), [(0, 6)]);
    }
}
