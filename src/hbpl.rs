//! HBPL version 1 encoder.
//!
//! Wire format and compression follow Dave Coffin's reverse engineering
//! (foo2hbpl1.c, 2014). See PROTOCOL.md for a description of the format.

use crate::bits::BitWriter;

/// Longest run a single copy token can express.
pub const MAX_RUN: usize = 17057;

/// A page raster in the printer's native pixel layout: one u32 per pixel
/// holding the K, C, M, Y ink bytes (K in the low byte, 255 = full ink).
/// Mono pages only use the K byte.
pub struct Raster {
    /// Width in pixels as rendered.
    pub width: usize,
    pub height: usize,
    /// `width` rounded up to a multiple of 8; `px` rows are this long.
    pub stride: usize,
    pub color: bool,
    pub px: Vec<u32>,
}

impl Raster {
    pub fn new(width: usize, height: usize, color: bool) -> Self {
        let stride = (width + 7) & !7;
        Raster { width, height, stride, color, px: vec![0; stride * height] }
    }
}

pub struct Paper {
    pub name: &'static str,
    /// Paper code; odd codes are envelopes. The page header carries code/2.
    pub code: u16,
    /// Size at 600 dpi.
    pub width: usize,
    pub height: usize,
}

pub const PAPERS: &[Paper] = &[
    Paper { name: "letter", code: 0, width: 5100, height: 6600 },
    Paper { name: "legal", code: 2, width: 5100, height: 8400 },
    Paper { name: "a4", code: 4, width: 4961, height: 7016 },
    Paper { name: "executive", code: 6, width: 4350, height: 6300 },
    Paper { name: "env10", code: 13, width: 2475, height: 5700 },
    Paper { name: "monarch", code: 15, width: 2325, height: 4500 },
    Paper { name: "c5", code: 17, width: 3827, height: 5409 },
    Paper { name: "dl", code: 19, width: 2599, height: 5197 },
    Paper { name: "b5", code: 22, width: 4299, height: 6071 },
    Paper { name: "a5", code: 30, width: 3496, height: 4961 },
    Paper { name: "folio", code: 410, width: 5100, height: 7800 },
];

const CUSTOM_PAPER: u16 = 510;

/// Printer-side names for the `-m` media codes (1-based).
pub const MEDIA_NAMES: &[&str] = &[
    "NORMAL", "THICK", "HIGHQUALITY", "COAT2", "LABEL", "ENVELOPE", "RECYCLED",
    "NORMALREV", "THICKSIDE2", "HIGHQUALITYREV", "COATEDPAPER2REV", "RECYCLEREV",
];

/// Paper code for a raster size, allowing about 1.5 mm of slop.
pub fn paper_code(width: usize, height: usize) -> u16 {
    PAPERS
        .iter()
        .find(|p| width.abs_diff(p.width) < 36 && height.abs_diff(p.height) < 36)
        .map_or(CUSTOM_PAPER, |p| p.code)
}

/// Media code to use when none is given: envelope for envelopes, else plain.
pub fn default_media(width: usize, height: usize) -> u8 {
    if paper_code(width, height) & 1 == 1 { 6 } else { 1 }
}

pub struct Job {
    pub color: bool,
    /// 1-based index into MEDIA_NAMES.
    pub media: u8,
    pub filename: String,
    pub username: String,
    pub hostname: String,
    /// MM/DD/YYYY and HH:MM:SS, informational only.
    pub date: String,
    pub time: String,
}

/// PJL job setup followed by the HBPL document header record.
pub fn job_header(job: &Job) -> Vec<u8> {
    // Lines end with \n, not \r\n, like the Windows driver.
    let mut s = String::new();
    s += "\x1b%-12345X@PJL SET STRINGCODESET=UTF8\n";
    s += &format!("@PJL COMMENT DATE={}\n", job.date);
    s += &format!("@PJL COMMENT TIME={}\n", job.time);
    s += &format!("@PJL COMMENT DNAME={}\n", job.filename);
    s += "@PJL JOB MODE=PRINTER\n";
    s += &format!("@PJL SET JOBATTR=\"@LUNA={}\"\n", job.username);
    s += "@PJL SET JOBATTR=\"@TRCH=OFF\"\n";
    s += "@PJL SET DUPLEX=OFF\n";
    s += "@PJL SET BINDING=LONGEDGE\n";
    s += "@PJL SET IWAMANUALDUP=OFF\n";
    s += &format!("@PJL SET JOBATTR=\"@MSIP={}\"\n", MEDIA_NAMES[job.media as usize - 1]);
    s += &format!("@PJL SET RENDERMODE={}\n", if job.color { "COLOR" } else { "GRAYSCALE" });
    s += "@PJL SET ECONOMODE=OFF\n";
    s += "@PJL SET RET=ON\n";
    s += "@PJL SET JOBATTR=\"@IREC=OFF\"\n";
    s += "@PJL SET JOBATTR=\"@TRAP=ON\"\n";
    s += &format!("@PJL SET JOBATTR=\"@JOAU={}\"\n", job.username);
    s += &format!("@PJL SET JOBATTR=\"@CNAM={}\"\n", job.hostname);
    s += "@PJL SET COPIES=1\n";
    s += "@PJL SET QTY=1\n";
    s += "@PJL SET PAPERDIRECTION=SEF\n";
    s += "@PJL SET RESOLUTION=600\n";
    s += "@PJL SET BITSPERPIXEL=8\n";
    // Screening / tone curve / color profile / UCR per object type
    // (Text, Graphics, Image). These are the Windows driver's defaults.
    s += "@PJL SET JOBATTR=\"@DRDM=XRC\"\n";
    for attr in ["SCR", "TRC", "CPR", "UCR"] {
        s += &format!("@PJL SET JOBATTR=\"@T{attr}=11\"\n");
        s += &format!("@PJL SET JOBATTR=\"@G{attr}=11\"\n");
        s += &format!("@PJL SET JOBATTR=\"@I{attr}=12\"\n");
    }
    s += "@PJL SET JOBATTR=\"@BSPM=OFF\"\n";
    s += "@PJL SET JOBATTR=\"@TDFT=0\"\n";
    s += "@PJL SET JOBATTR=\"@GDFT=0\"\n";
    s += "@PJL SET JOBATTR=\"@IDFT=0\"\n";
    s += "@PJL ENTER LANGUAGE=HBPL\n";

    let mut out = s.into_bytes();
    // 'A' (begin document) with attributes 0x81=0, 0x82=7, 0x83=1.
    out.extend_from_slice(&[0x41, 0x81, 0xa1, 0x00, 0x82, 0xa2, 0x07, 0x00, 0x83, 0xa2, 0x01, 0x00]);
    out
}

/// 'B' (end document) and the PJL end of job.
pub fn job_footer() -> Vec<u8> {
    b"B\x1b%-12345X@PJL EOJ\n".to_vec()
}

/// Which margins to force blank, in pixels: left, top, right, bottom.
#[derive(Clone, Copy)]
pub struct Clip(pub [usize; 4]);

impl Default for Clip {
    fn default() -> Self {
        Clip([8; 4])
    }
}

fn setle(buf: &mut [u8], val: usize) {
    for (i, b) in buf.iter_mut().enumerate() {
        *b = (val >> (8 * i)) as u8;
    }
}

/// Copy token codes as (code << 4 | length), for text/graphics and for
/// images. Tokens 0-4 pick a copy direction, 5 is a literal pixel, 6
/// switches tables.
const HUFF: [[u16; 7]; 2] = [
    [0x01, 0x63, 0x1c5, 0x1d5, 0x1e5, 0x22, 0x3e6],
    [0x22, 0x63, 0x1c5, 0x1d5, 0x1e5, 0x01, 0x3e6],
];

fn put_token(s: &mut BitWriter, hsel: usize, token: usize) {
    let v = HUFF[hsel][token] as u32;
    s.put(v >> 4, v & 15);
}

/// Run lengths 1..=17057 (see PROTOCOL.md for the code table).
fn put_len(s: &mut BitWriter, val: usize) {
    // (first length, code for it, bits)
    const CODE: [(usize, u32, u32); 11] = [
        (1, 0, 2),
        (2, 2, 3),
        (4, 8, 4),
        (6, 0x14, 5),
        (10, 0x60, 7),
        (26, 0x1c0, 9),
        (50, 0x3b0, 10),
        (66, 0x780, 11),
        (98, 0xf40, 12),
        (162, 0x7c00, 15),
        (674, 0xfc000, 20),
    ];
    debug_assert!((1..=MAX_RUN).contains(&val));
    let &(first, code, bits) = CODE.iter().rev().find(|c| val >= c.0).unwrap();
    s.put((val - first) as u32 + code, bits);
}

/// Signed byte deltas for literal pixels, one stream per ink channel.
fn put_diff(s: &mut BitWriter, val: i8) {
    match val {
        0 => return s.put(0, 3),
        1 => return s.put(1, 3),
        -1 => return s.put(2, 3),
        _ => {}
    }
    // (smallest magnitude, prefix, prefix bits, magnitude bits)
    const CODE: [(u32, u32, u32, u32); 6] =
        [(2, 3, 3, 1), (4, 4, 3, 2), (8, 5, 3, 3), (16, 6, 3, 5), (48, 14, 4, 5), (80, 15, 4, 6)];
    let mag = val.unsigned_abs() as u32;
    let &(first, prefix, pbits, mbits) = CODE.iter().rev().find(|c| mag >= c.0).unwrap();
    s.put(prefix, pbits);
    s.put((val < 0) as u32, 1);
    s.put(mag - first, mbits);
}

/// Encode one page as an HBPL page record ('C' ... 'S' 'D').
/// `page_num` counts from 1.
pub fn encode_page(r: &Raster, page_num: u32, clip: Clip) -> Vec<u8> {
    let (w, h, color) = (r.stride, r.height, r.color);
    let n = w * h;
    let deep = if color { 4 } else { 1 };
    let fill = if color { u32::MAX } else { 0 };

    // Linear pixel buffer with a row (+1 pixel) of context in front, which
    // the printer starts out filled with `fill`, and zeros after so copy
    // runs can look past the end.
    let front = w + 1;
    let mut px = vec![0u32; front + n + MAX_RUN + w];
    px[..front].fill(fill);
    px[front..front + n].copy_from_slice(&r.px);

    let [cl, ct, cr, cb] = clip.0;
    for row in 0..h {
        let line = &mut px[front + row * w..front + (row + 1) * w];
        line[..cl.min(w)].fill(0);
        line[r.width.saturating_sub(cr).min(w)..].fill(0);
    }
    px[front..front + ct.min(h) * w].fill(0);
    px[front + h.saturating_sub(cb) * w..front + n].fill(0);
    // The first pixel of every row is always full ink. It's in the
    // unprintable margin, and the reference encoder does the same.
    for row in 0..h {
        px[front + row * w] = if color { u32::MAX } else { 0xff };
    }

    // Rows with any ink get their printable middle marked, and copies may
    // not cross a marked/unmarked boundary.
    let mut blank = vec![0u8; px.len()];
    if color {
        blank[..front].fill(1);
    }
    let first_checked = if color { 1 } else { 7 };
    let margin = r.width as isize - 96;
    let full_bytes_end = (margin / 8).max(12) as usize;
    let tail_bits = (margin & 7) as usize;
    for row in 0..h {
        let base = front + row * w;
        if px[base + first_checked..base + w - 1].iter().any(|&p| p != 0) {
            let end = (full_bytes_end * 8 + tail_bits).min(w);
            blank[base + 96.min(end)..base + end].fill(1);
        }
    }

    let wi = w as isize;
    let dirs: [isize; 5] = [-1, -wi, -wi - 1, 1 - wi, if color { 2 - wi } else { -8 }];
    let mut rotor = [0usize, 1, 2, 3, 4];
    let mut tokens = BitWriter::new();
    let mut deltas: Vec<BitWriter> = (0..deep).map(|_| BitWriter::new()).collect();
    let (mut off, mut bit, mut stat, mut hsel) = (0usize, 0usize, 0i32, 0usize);

    while off < n {
        let a = front + off;
        let (mut bdir, mut brun) = (0, 0);
        for (dir, &d) in rotor.iter().enumerate() {
            let b = (a as isize + dirs[d]) as usize;
            let run = px[a..a + MAX_RUN]
                .iter()
                .zip(&px[b..b + MAX_RUN])
                .zip(blank[a..a + MAX_RUN].iter().zip(&blank[b..b + MAX_RUN]))
                .take_while(|((p, q), (x, y))| p == q && x == y)
                .count();
            if run > brun {
                bdir = dir;
                brun = run;
            }
        }

        if brun == 0 {
            put_token(&mut tokens, hsel, 5);
            let (cur, prev) = (px[a], px[a - 1]);
            for (i, s) in deltas.iter_mut().enumerate() {
                let d = ((cur >> (8 * i)) as u8).wrapping_sub((prev >> (8 * i)) as u8);
                put_diff(s, d as i8);
            }
            bit = 0;
            off += 1;
            stat -= 1;
            continue;
        }

        brun = brun.min(n - off);
        rotor[..=bdir].rotate_right(1);

        // At each row boundary, switch tables if the last row was mostly
        // literals (image) or mostly repeated copies (text).
        let prev = off as isize - 1;
        if (prev + brun as isize) / wi != prev / wi {
            if stat.abs() > 8 && (stat < 0) as usize != hsel {
                hsel ^= 1;
                put_token(&mut tokens, hsel, 6);
            }
            stat = 0;
        }
        stat += (bdir == bit) as i32;

        // After a run that stopped on a mismatch, direction 0 can't match,
        // so token numbers shift down by one.
        put_token(&mut tokens, hsel, bdir - bit);
        put_len(&mut tokens, brun);
        bit = (brun < MAX_RUN) as usize;
        off += brun;
    }
    tokens.put(0xff, 8);

    let streams: Vec<Vec<u8>> =
        std::iter::once(tokens).chain(deltas).map(BitWriter::finish).collect();

    // 'C' begin page, then its attributes.
    #[rustfmt::skip]
    let mut head: [u8; 90] = [
        0x43, 0x91,0xa1,0x00, 0x92,0xa1,0x01, 0x93,0xa1,0x01,
        0x94,0xa1,0x00,                 // paper code / 2
        0x95,0xc2,0x00,0x00,0x00,0x00,  // custom size, 0.1 mm
        0x96,0xa1,0x00,                 // 2 = custom size
        0x97,0xc3,0x00,0x00,0x00,0x00,
        0x98,0xa1,0x00,                 // duplex side
        0x99,0xa4,0x00,0x00,0x00,0x00,  // page number
        0x9a,0xc4,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00, // width, height
        0x9b,0xa1,0x00, 0x9c,0xa1,0x01,
        0x9d,0xa1,0x00,                 // 9 = mono, 139 = color
        0x9e,0xa1,0x02, 0x9f,0xa1,0x05,
        0xa0,0xa1,0x08,                 // bits per pixel
        0xa1,0xa1,0x00,
        0xa2,0xc4,0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00, // width, height
        0x51,                           // 'Q' end page attributes
        0x52, 0xa3,0xa1,0x00,           // 'R' begin image data
        0xa4,0xb1,0xa2,0x00,0x00,0x00,0x00, // byte array, length
    ];
    let paper = paper_code(r.width, h);
    head[12] = (paper >> 1) as u8;
    if paper == CUSTOM_PAPER {
        setle(&mut head[15..17], (r.width * 254 + 300) / 600);
        setle(&mut head[17..19], (h * 254 + 300) / 600);
        head[21] = 2;
    }
    setle(&mut head[33..37], page_num as usize);
    setle(&mut head[39..43], w);
    setle(&mut head[43..47], h);
    setle(&mut head[70..74], w);
    setle(&mut head[74..78], h);
    head[55] = if color { 139 } else { 9 };

    #[rustfmt::skip]
    let mut body: [u8; 52] = [
        0x20,0x00,0x00,0x00, 0x00,0x01,0x00,0x00, 0x10,0x32,0x04,0x00,
        0xa1,0x42,0x00,0x00,0x00,0x00,0xff, 0,0,0,0,0,0,0,0,0,0,0,0,0,
        // stream lengths
        0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    ];
    if color {
        body[6] = 1;
    } else {
        body[4] = 8;
    }
    for (i, s) in streams.iter().enumerate() {
        setle(&mut body[32 + i * 4..36 + i * 4], s.len());
    }
    // Only 48 bytes go out, so the last color stream's length is implied.
    let total = 48 + streams.iter().map(Vec::len).sum::<usize>();
    let head_len = if total > 0xffff {
        head[85] = 0xa4;
        90
    } else {
        88
    };
    setle(&mut head[86..90], total);

    let mut out = Vec::with_capacity(head_len + total + 2);
    out.extend_from_slice(&head[..head_len]);
    out.extend_from_slice(&body[..48]);
    for s in &streams {
        out.extend_from_slice(s);
    }
    out.extend_from_slice(b"SD"); // end image, end page
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::decode;

    /// What the printer should reconstruct: the raster with margins
    /// cleared and the forced first column.
    fn expected(r: &Raster, clip: Clip) -> Vec<u32> {
        let [cl, ct, cr, cb] = clip.0;
        let mut px = r.px.clone();
        for (row, line) in px.chunks_mut(r.stride).enumerate() {
            for (x, p) in line.iter_mut().enumerate() {
                if x < cl || x >= r.width.saturating_sub(cr) || row < ct || row >= r.height.saturating_sub(cb) {
                    *p = 0;
                }
            }
            line[0] = if r.color { u32::MAX } else { 0xff };
        }
        px
    }

    /// Blank bands, noise (literals and table switches), text-like blocks,
    /// gradients, and repeated patterns for every copy direction.
    fn synthetic(width: usize, height: usize, color: bool) -> Raster {
        let mut r = Raster::new(width, height, color);
        let mut seed = 0x1760u32;
        let mut rand = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let mask = if color { u32::MAX } else { 0xff };
        for y in 0..height {
            for x in 0..width {
                let p = match (y / 40) % 5 {
                    0 => 0,
                    1 => rand() & mask,
                    2 if (x / 7 + y / 5) % 3 == 0 => mask,
                    3 => ((x * 255 / width) as u32 * 0x0101_0101) & mask,
                    4 if x % 9 < 4 => (x as u32 * 0x0103_0507 + y as u32) & mask,
                    _ => 0,
                };
                r.px[y * r.stride + x] = p;
            }
        }
        r
    }

    fn round_trip(r: &Raster) {
        let job_info = Job {
            color: r.color,
            media: 1,
            filename: "t".into(),
            username: "u".into(),
            hostname: "h".into(),
            date: String::new(),
            time: String::new(),
        };
        let mut job = job_header(&job_info);
        job.extend(encode_page(r, 1, Clip::default()));
        job.extend(encode_page(r, 2, Clip([0; 4])));
        job.extend(job_footer());
        let d = decode(&job).unwrap();
        assert_eq!(d.pages.len(), 2);
        for (page, clip) in d.pages.iter().zip([Clip::default(), Clip([0; 4])]) {
            assert_eq!((page.width, page.height, page.color), (r.stride, r.height, r.color));
            assert!(page.px == expected(r, clip), "page {} differs", page.number);
        }
    }

    #[test]
    fn round_trip_color() {
        round_trip(&synthetic(1203, 400, true));
    }

    #[test]
    fn round_trip_mono() {
        round_trip(&synthetic(1203, 400, false));
    }

    #[test]
    fn long_blank_runs() {
        // Wide enough that runs hit MAX_RUN and wrap across rows.
        round_trip(&Raster::new(20000, 30, true));
    }

    #[test]
    fn paper_detection() {
        assert_eq!(paper_code(5100, 6600), 0);
        assert_eq!(paper_code(4958, 7017), 4);
        assert_eq!(paper_code(1000, 1000), CUSTOM_PAPER);
        assert_eq!(default_media(2475, 5700), 6);
    }
}
