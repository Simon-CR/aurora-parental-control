//! Stack-only UTF-8 to NUL-terminated UTF-16 encoder for XAM UI calls.

/// Encodes `input` (interpreted as UTF-8, substituting `?` on invalid bytes)
/// into `out` as NUL-terminated UTF-16. Returns `out.as_ptr()`.
pub fn encode_utf16_lossy<const N: usize>(input: &[u8], out: &mut [u16; N]) -> *const u16 {
    const { assert!(N > 0, "UTF-16 buffer must hold at least a NUL terminator") };
    let mut i = 0usize;
    let mut w = 0usize;
    let max = N - 1;

    while i < input.len() && w < max {
        let b0 = input[i];
        if b0 < 0x80 {
            if b0 != 0 {
                out[w] = u16::from(b0);
                w += 1;
            }
            i += 1;
        } else if (b0 & 0xE0) == 0xC0 && i + 1 < input.len() {
            let b1 = input[i + 1];
            if (b1 & 0xC0) == 0x80 {
                let cp = (u32::from(b0 & 0x1F) << 6) | u32::from(b1 & 0x3F);
                out[w] = cp as u16;
                w += 1;
                i += 2;
            } else {
                out[w] = u16::from(b'?');
                w += 1;
                i += 1;
            }
        } else if (b0 & 0xF0) == 0xE0 && i + 2 < input.len() {
            let b1 = input[i + 1];
            let b2 = input[i + 2];
            if (b1 & 0xC0) == 0x80 && (b2 & 0xC0) == 0x80 {
                let cp = (u32::from(b0 & 0x0F) << 12)
                    | (u32::from(b1 & 0x3F) << 6)
                    | u32::from(b2 & 0x3F);
                out[w] = cp as u16;
                w += 1;
                i += 3;
            } else {
                out[w] = u16::from(b'?');
                w += 1;
                i += 1;
            }
        } else if (b0 & 0xF8) == 0xF0 && i + 3 < input.len() && w + 1 < max {
            let b1 = input[i + 1];
            let b2 = input[i + 2];
            let b3 = input[i + 3];
            if (b1 & 0xC0) == 0x80 && (b2 & 0xC0) == 0x80 && (b3 & 0xC0) == 0x80 {
                let cp = (u32::from(b0 & 0x07) << 18)
                    | (u32::from(b1 & 0x3F) << 12)
                    | (u32::from(b2 & 0x3F) << 6)
                    | u32::from(b3 & 0x3F);
                if cp >= 0x1_0000 && cp <= 0x10_FFFF {
                    let off = cp - 0x1_0000;
                    out[w] = (0xD800 | (off >> 10)) as u16;
                    out[w + 1] = (0xDC00 | (off & 0x3FF)) as u16;
                    w += 2;
                    i += 4;
                    continue;
                }
            }
            out[w] = u16::from(b'?');
            w += 1;
            i += 1;
        } else {
            out[w] = u16::from(b'?');
            w += 1;
            i += 1;
        }
    }

    out[w] = 0;
    out.as_ptr()
}
