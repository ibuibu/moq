use bytes::{Buf, BufMut};
use std::fmt;

/// QUIC 可変長整数 (RFC 9000 Section 16)
/// 最大62ビットの値をエンコードする
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct VarInt(u64);

impl VarInt {
    pub const MAX: u64 = (1 << 62) - 1;

    pub fn from_u64(v: u64) -> anyhow::Result<Self> {
        if v > Self::MAX {
            anyhow::bail!("varint overflow: {v}");
        }
        Ok(Self(v))
    }

    pub fn into_inner(self) -> u64 {
        self.0
    }

    pub fn encode<B: BufMut>(&self, buf: &mut B) {
        let v = self.0;
        if v < (1 << 6) {
            buf.put_u8(v as u8);
        } else if v < (1 << 14) {
            buf.put_u16(0x4000 | v as u16);
        } else if v < (1 << 30) {
            buf.put_u32(0x8000_0000 | v as u32);
        } else {
            buf.put_u64(0xc000_0000_0000_0000 | v);
        }
    }

    pub fn decode<B: Buf>(buf: &mut B) -> anyhow::Result<Self> {
        if !buf.has_remaining() {
            anyhow::bail!("not enough data for varint");
        }

        let first = buf.chunk()[0];
        let prefix = first >> 6;
        let len = 1usize << prefix;

        if buf.remaining() < len {
            anyhow::bail!("not enough data for varint (need {len} bytes)");
        }

        let mut val = (first & 0x3f) as u64;
        buf.advance(1);

        for _ in 1..len {
            val = (val << 8) | buf.get_u8() as u64;
        }

        Ok(Self(val))
    }

    pub fn size(&self) -> usize {
        let v = self.0;
        if v < (1 << 6) {
            1
        } else if v < (1 << 14) {
            2
        } else if v < (1 << 30) {
            4
        } else {
            8
        }
    }
}

impl fmt::Debug for VarInt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VarInt({})", self.0)
    }
}

impl fmt::Display for VarInt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<u32> for VarInt {
    fn from(v: u32) -> Self {
        Self(v as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;

    #[test]
    fn roundtrip() {
        let cases = [0u64, 1, 63, 64, 16383, 16384, (1 << 30) - 1, 1 << 30];
        for v in cases {
            let vi = VarInt::from_u64(v).unwrap();
            let mut buf = BytesMut::new();
            vi.encode(&mut buf);
            assert_eq!(buf.len(), vi.size());
            let decoded = VarInt::decode(&mut buf.freeze()).unwrap();
            assert_eq!(decoded.into_inner(), v);
        }
    }
}
