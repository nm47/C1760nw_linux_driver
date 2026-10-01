//! HBPL version 1 decoder: turns a print job back into pixels. Useful for
//! checking our own output and for picking apart captures from other
//! drivers.

use crate::bits::BitReader;
use crate::hbpl::MAX_RUN;
use std::fmt::Write as _;

pub struct DecodedPage {
    pub number: u32,
    pub width: usize,
    pub height: usize,
    pub color: bool,
    /// K,C,M,Y bytes per pixel, K in the low byte (as in `Raster`).
    pub px: Vec<u32>,
}

impl DecodedPage {
    /// As a binary PPM (color) or PGM (mono) image.
    pub fn to_pnm(&self) -> Vec<u8> {
        let mut out = format!("P{} {} {} 255\n", if self.color { 6 } else { 5 }, self.width, self.height)
            .into_bytes();
        for &p in &self.px {
            let [k, c, m, y] = p.to_le_bytes();
            if self.color {
                let white = 255 - k as u32;
                out.extend([c, m, y].map(|v| (white * (255 - v as u32) / 255) as u8));
            } else {
                out.push(255 - k);
            }
        }
        out
    }
}

pub struct Decoded {
    /// Human readable dump of the PJL header and HBPL records.
    pub log: String,
    pub pages: Vec<DecodedPage>,
}

fn le(b: &[u8]) -> usize {
    b.iter().rev().fold(0, |v, &x| v << 8 | x as usize)
}

pub fn decode(data: &[u8]) -> Result<Decoded, String> {
    let mut log = String::new();
    let mut pos = 0;

    // PJL text up to ENTER LANGUAGE.
    loop {
        let end = data[pos..].iter().position(|&b| b == b'\n').ok_or("no ENTER LANGUAGE=HBPL")?;
        let line = String::from_utf8_lossy(&data[pos..pos + end + 1]).replace('\x1b', "<ESC>");
        log += &line;
        pos += end + 1;
        if line.trim_end() == "@PJL ENTER LANGUAGE=HBPL" {
            break;
        }
    }

    let mut pages = Vec::new();
    let (mut page, mut color, mut width, mut height) = (0u32, false, 0usize, 0usize);
    let byte = |pos: &mut usize| -> Result<u8, String> {
        let b = *data.get(*pos).ok_or("unexpected end of data")?;
        *pos += 1;
        Ok(b)
    };

    loop {
        let rec = byte(&mut pos)?;
        let _ = writeln!(log, "{:#08x}: record {:#04x} '{}'", pos - 1, rec, rec as char);
        let stop = match rec {
            0x41 => 0x83,
            0x43 => 0xa2,
            0x52 => 0xa4,
            0x20 | 0x44 | 0x51 | 0x53 => continue,
            0x42 => break,
            _ => return Err(format!("unknown record {rec:#04x} at {:#x}", pos - 1)),
        };
        loop {
            let attr = byte(&mut pos)?;
            let mut kind = byte(&mut pos)?;
            if kind == 0xb1 {
                // byte array, followed by its length
                kind = byte(&mut pos)?;
            }
            let (size, pair) = match kind {
                0xa1 => (1, false),
                0xa2 => (2, false),
                0xa4 | 0xc3 => (4, false),
                0xc2 => (2, true),
                0xc4 => (4, true),
                _ => return Err(format!("unknown attribute type {kind:#04x} at {:#x}", pos - 1)),
            };
            let mut vals = [0usize; 2];
            for v in vals.iter_mut().take(1 + pair as usize) {
                *v = le(data.get(pos..pos + size).ok_or("truncated attribute")?);
                pos += size;
            }
            let desc = match attr {
                0x94 => format!("paper code/2 = {}", vals[0]),
                0x95 => format!("custom size = {}x{} (0.1 mm)", vals[0], vals[1]),
                0x99 => {
                    page = vals[0] as u32;
                    format!("page {page}")
                }
                0x9a | 0xa2 if rec == 0x43 => {
                    (width, height) = (vals[0], vals[1]);
                    format!("{width}x{height} pixels")
                }
                0x9d => {
                    color = vals[0] != 9;
                    format!("{} ({})", vals[0], if color { "color" } else { "mono" })
                }
                0xa4 if rec == 0x52 => format!("{} bytes of image data", vals[0]),
                _ if pair => format!("{} {}", vals[0], vals[1]),
                _ => format!("{}", vals[0]),
            };
            let _ = writeln!(log, "    attr {attr:#04x} type {kind:#04x}: {desc}");
            if attr == stop {
                if rec == 0x52 {
                    let len = vals[0];
                    let img = data.get(pos..pos + len).ok_or("truncated image data")?;
                    pages.push(decode_image(img, page, color, width, height)?);
                    pos += len;
                }
                break;
            }
        }
    }
    let _ = writeln!(log, "{:#08x}: trailer {:?}", pos, String::from_utf8_lossy(&data[pos..]));
    Ok(Decoded { log, pages })
}

fn get_token(s: &mut BitReader, hsel: usize) -> usize {
    let lit_or_0 = |first: usize| if first == hsel { 5 } else { 0 };
    if s.get(1) == 0 {
        return lit_or_0(1);
    }
    if s.get(1) == 0 {
        return lit_or_0(0);
    }
    if s.get(1) == 0 {
        return 1;
    }
    match s.get(2) {
        0 => 2,
        1 => 3,
        2 => 4,
        _ => {
            // Table switch, 111110. 111111 is not a valid token.
            if s.get(1) == 0 { 6 } else { 7 }
        }
    }
}

fn get_len(s: &mut BitReader) -> usize {
    let top = s.get(2);
    match top {
        0 => return 1,
        1 => return 2 + s.get(1) as usize,
        _ => {}
    }
    match top << 1 | s.get(1) {
        4 => 4 + s.get(1) as usize,
        5 => 6 + s.get(2) as usize,
        6 => 10 + s.get(4) as usize,
        _ => {
            const BASE: [usize; 8] = [26, 34, 42, 50, 66, 98, 162, 674];
            const BITS: [u32; 8] = [3, 3, 3, 4, 5, 6, 9, 14];
            let i = s.get(3) as usize;
            BASE[i] + s.get(BITS[i]) as usize
        }
    }
}

fn get_diff(s: &mut BitReader) -> i8 {
    let mut i = s.get(3) as usize;
    match i {
        0 => return 0,
        1 => return 1,
        2 => return -1,
        7 => i += s.get(1) as usize,
        _ => {}
    }
    const BASE: [u32; 6] = [2, 4, 8, 16, 48, 80];
    const BITS: [u32; 6] = [1, 2, 3, 5, 5, 6];
    let neg = s.get(1) == 1;
    let mag = BASE[i - 3] + s.get(BITS[i - 3]);
    (if neg { -(mag as i32) } else { mag as i32 }) as i8
}

fn decode_image(img: &[u8], number: u32, color: bool, w: usize, h: usize) -> Result<DecodedPage, String> {
    if img.len() < 48 || w == 0 || h == 0 {
        return Err("image data too short or page size missing".into());
    }
    let mut starts = [48usize; 5];
    for s in 0..4 {
        starts[s + 1] = starts[s] + le(&img[32 + s * 4..36 + s * 4]);
    }
    let stream = |s: usize| -> Result<&[u8], String> {
        let end = if s < 4 { starts[s + 1] } else { img.len() };
        img.get(starts[s]..end).ok_or_else(|| format!("stream {s} out of bounds"))
    };
    let mut tokens = BitReader::new(stream(0)?);
    let mut deltas: Vec<BitReader> =
        (0..if color { 4 } else { 1 }).map(|s| stream(s + 1).map(BitReader::new)).collect::<Result<_, _>>()?;

    let n = w * h;
    let front = w + 1;
    let mut px = vec![if color { u32::MAX } else { 0 }; front + n];
    let wi = w as isize;
    let dirs: [isize; 5] = [-1, -wi, -wi - 1, 1 - wi, if color { 2 - wi } else { -8 }];
    let mut rotor = [0usize, 1, 2, 3, 4];
    let (mut off, mut hsel, mut bit) = (0usize, 0usize, 0usize);

    while off < n {
        if tokens.exhausted() {
            return Err(format!("page {number}: token stream ended at pixel {off} of {n}"));
        }
        match get_token(&mut tokens, hsel) {
            6 => hsel ^= 1,
            7 => return Err(format!("page {number}: bad token at pixel {off}")),
            5 => {
                let a = front + off;
                let mut bytes = px[a - 1].to_le_bytes();
                for (b, s) in bytes.iter_mut().zip(deltas.iter_mut()) {
                    *b = b.wrapping_add(get_diff(s) as u8);
                }
                px[a] = u32::from_le_bytes(bytes);
                off += 1;
                bit = 0;
            }
            token => {
                let run = get_len(&mut tokens).min(n - off);
                let raw = token + bit;
                let d = dirs[rotor[raw]];
                for a in front + off..front + off + run {
                    px[a] = px[(a as isize + d) as usize];
                }
                off += run;
                bit = (run < MAX_RUN) as usize;
                rotor[..=raw].rotate_right(1);
            }
        }
    }
    px.drain(..front);
    Ok(DecodedPage { number, width: w, height: h, color, px })
}
