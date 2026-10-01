//! MSB-first bit streams, as used by every HBPLv1 data stream.

#[derive(Default)]
pub struct BitWriter {
    buf: Vec<u8>,
    acc: u64,
    nacc: u32,
}

impl BitWriter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append the low `nbits` bits of `val`, most significant first.
    pub fn put(&mut self, val: u32, nbits: u32) {
        debug_assert!(nbits <= 32);
        self.acc = (self.acc << nbits) | (val as u64 & ((1u64 << nbits) - 1));
        self.nacc += nbits;
        while self.nacc >= 8 {
            self.nacc -= 8;
            self.buf.push((self.acc >> self.nacc) as u8);
        }
        self.acc &= (1u64 << self.nacc) - 1;
    }

    /// Pad the trailing partial byte with 1-bits and return the bytes.
    pub fn finish(mut self) -> Vec<u8> {
        if self.nacc > 0 {
            let pad = 8 - self.nacc;
            self.put((1 << pad) - 1, pad);
        }
        self.buf
    }
}

pub struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    acc: u64,
    nacc: u32,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0, acc: 0, nacc: 0 }
    }

    /// Read `nbits` bits. Past the end of the data we read 1-bits, which is
    /// what the encoder pads with.
    pub fn get(&mut self, nbits: u32) -> u32 {
        while self.nacc < nbits {
            let byte = self.data.get(self.pos).copied().unwrap_or(0xff);
            self.pos += 1;
            self.acc = (self.acc << 8) | byte as u64;
            self.nacc += 8;
        }
        self.nacc -= nbits;
        ((self.acc >> self.nacc) & ((1u64 << nbits) - 1)) as u32
    }

    pub fn exhausted(&self) -> bool {
        self.pos >= self.data.len() && self.nacc == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_padding() {
        let mut w = BitWriter::new();
        w.put(0b101, 3);
        w.put(0x3fff, 14);
        w.put(0, 1);
        let bytes = w.finish();
        // 101 11111111111111 0 + 111111 pad
        assert_eq!(bytes, vec![0b1011_1111, 0b1111_1111, 0b1011_1111]);
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.get(3), 0b101);
        assert_eq!(r.get(14), 0x3fff);
        assert_eq!(r.get(1), 0);
    }
}
