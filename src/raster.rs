//! Reading Ghostscript's raster output (pbmraw, pgmraw, ppmraw, pamcmyk32)
//! into printer-native `Raster`s, one page at a time.

use crate::hbpl::Raster;
use std::io::{self, BufRead, ErrorKind};

fn bad(msg: &str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, msg.to_string())
}

fn byte<R: BufRead>(r: &mut R) -> io::Result<Option<u8>> {
    let mut b = [0u8];
    match r.read(&mut b)? {
        0 => Ok(None),
        _ => Ok(Some(b[0])),
    }
}

/// A PNM header integer. Consumes the single whitespace byte after it.
fn header_int<R: BufRead>(r: &mut R) -> io::Result<usize> {
    let mut c = byte(r)?;
    loop {
        match c {
            Some(b'#') => {
                while !matches!(byte(r)?, Some(b'\n') | None) {}
                c = byte(r)?;
            }
            Some(b) if b.is_ascii_whitespace() => c = byte(r)?,
            _ => break,
        }
    }
    let mut val = None::<usize>;
    while let Some(d @ b'0'..=b'9') = c {
        val = Some(val.unwrap_or(0) * 10 + (d - b'0') as usize);
        c = byte(r)?;
    }
    val.ok_or_else(|| bad("bad PNM header"))
}

fn header_line<R: BufRead>(r: &mut R) -> io::Result<String> {
    let mut line = String::new();
    if r.read_line(&mut line)? == 0 {
        return Err(bad("truncated PAM header"));
    }
    Ok(line.trim_end().to_string())
}

/// Read the next page, or None at end of input.
pub fn read_page<R: BufRead>(r: &mut R) -> io::Result<Option<Raster>> {
    let magic = match byte(r)? {
        None => return Ok(None),
        Some(b'P') => byte(r)?.ok_or_else(|| bad("truncated header"))?,
        Some(_) => return Err(bad("not a PBM/PGM/PPM/PAM stream")),
    };
    let (width, height, depth, maxval) = match magic {
        b'4' => (header_int(r)?, header_int(r)?, 0, 255),
        b'5' | b'6' => {
            let (w, h) = (header_int(r)?, header_int(r)?);
            (w, h, if magic == b'5' { 1 } else { 3 }, header_int(r)?)
        }
        b'7' => {
            let (mut w, mut h, mut d, mut m, mut tupl) = (0, 0, 0, 0, String::new());
            loop {
                let line = header_line(r)?;
                let mut kv = line.splitn(2, ' ');
                let (k, v) = (kv.next().unwrap_or(""), kv.next().unwrap_or("").trim());
                match k {
                    "WIDTH" => w = v.parse().map_err(|_| bad("bad WIDTH"))?,
                    "HEIGHT" => h = v.parse().map_err(|_| bad("bad HEIGHT"))?,
                    "DEPTH" => d = v.parse().map_err(|_| bad("bad DEPTH"))?,
                    "MAXVAL" => m = v.parse().map_err(|_| bad("bad MAXVAL"))?,
                    "TUPLTYPE" => tupl = v.to_string(),
                    "ENDHDR" => break,
                    _ => {}
                }
            }
            if d != 4 || tupl != "CMYK" {
                return Err(bad("PAM input must be CMYK (gs -sDEVICE=pamcmyk32)"));
            }
            (w, h, 4, m)
        }
        _ => return Err(bad("unsupported PNM type")),
    };
    if width == 0 || height == 0 || maxval != 255 {
        return Err(bad("need 8-bit samples and a non-empty page"));
    }

    let mut raster = Raster::new(width, height, depth > 1);
    let row_bytes = if depth == 0 { width.div_ceil(8) } else { width * depth };
    let mut line = vec![0u8; row_bytes];
    for row in 0..height {
        r.read_exact(&mut line)?;
        let out = &mut raster.px[row * raster.stride..row * raster.stride + width];
        match depth {
            0 => {
                for (x, p) in out.iter_mut().enumerate() {
                    *p = if line[x >> 3] & (0x80 >> (x & 7)) != 0 { 0xff } else { 0 };
                }
            }
            1 => {
                for (p, &g) in out.iter_mut().zip(&line) {
                    *p = (255 - g) as u32;
                }
            }
            3 => {
                for (p, rgb) in out.iter_mut().zip(line.chunks_exact(3)) {
                    *p = rgb_to_kcmy(rgb[0], rgb[1], rgb[2]);
                }
            }
            _ => {
                for (p, cmyk) in out.iter_mut().zip(line.chunks_exact(4)) {
                    *p = u32::from_le_bytes([cmyk[3], cmyk[0], cmyk[1], cmyk[2]]);
                }
            }
        }
    }
    Ok(Some(raster))
}

/// Naive RGB to CMYK with full black replacement: grays, and black text in
/// particular, come out as K only.
pub fn rgb_to_kcmy(r: u8, g: u8, b: u8) -> u32 {
    let k = r.max(g).max(b) as u32;
    if k == 0 {
        return 0xff;
    }
    let ink = |v: u8| (k - v as u32) * 255 / k;
    (255 - k) | ink(r) << 8 | ink(g) << 16 | ink(b) << 24
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_pgm_and_ppm_pages() {
        let mut data = b"P5\n# comment\n3 2\n255\n".to_vec();
        data.extend_from_slice(&[255, 0, 128, 255, 255, 255]);
        data.extend_from_slice(b"P6 1 1 255\n");
        data.extend_from_slice(&[255, 0, 0]);
        let mut r = io::Cursor::new(data);

        let p = read_page(&mut r).unwrap().unwrap();
        assert_eq!((p.width, p.height, p.stride, p.color), (3, 2, 8, false));
        assert_eq!(&p.px[..3], &[0, 255, 127]);

        let p = read_page(&mut r).unwrap().unwrap();
        assert!(p.color);
        // Red: no black, no cyan, full magenta and yellow.
        assert_eq!(p.px[0], u32::from_le_bytes([0, 0, 255, 255]));
        assert!(read_page(&mut r).unwrap().is_none());
    }

    #[test]
    fn black_and_gray_use_only_k() {
        assert_eq!(rgb_to_kcmy(0, 0, 0), 0xff);
        assert_eq!(rgb_to_kcmy(100, 100, 100), 155);
        assert_eq!(rgb_to_kcmy(255, 255, 255), 0);
    }
}
