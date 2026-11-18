//! Intel HEX -> flat binary (FX2 images are stored as HEX in KingstVIS).

use crate::error::{Error, Result};

const MAX_IMAGE: usize = 16 << 20;

fn bad(m: impl Into<String>) -> Error {
    Error::Firmware(format!("Intel HEX: {}", m.into()))
}

pub fn decode(text: &[u8]) -> Result<Vec<u8>> {
    let text = std::str::from_utf8(text).map_err(|_| bad("not text"))?;
    let mut img: Vec<u8> = Vec::new();
    let mut base: usize = 0;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let hex = line
            .strip_prefix(':')
            .ok_or_else(|| bad(format!("bad line '{line}'")))?;
        let rec: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| {
                hex.get(i..i + 2)
                    .and_then(|b| u8::from_str_radix(b, 16).ok())
            })
            .collect::<Option<_>>()
            .ok_or_else(|| bad(format!("bad hex in '{line}'")))?;
        if rec.len() < 5 || rec.len() != rec[0] as usize + 5 {
            return Err(bad(format!("bad length in '{line}'")));
        }
        if rec.iter().fold(0u8, |a, b| a.wrapping_add(*b)) != 0 {
            return Err(bad(format!("checksum mismatch in '{line}'")));
        }
        let data = &rec[4..rec.len() - 1];
        match rec[3] {
            0 => {
                let addr = base + usize::from(u16::from_be_bytes([rec[1], rec[2]]));
                let end = addr + data.len();
                if end > MAX_IMAGE {
                    return Err(bad("image larger than 16 MiB"));
                }
                if img.len() < end {
                    img.resize(end, 0);
                }
                img[addr..end].copy_from_slice(data);
            }
            1 => break,
            4 if data.len() == 2 => {
                base = usize::from(u16::from_be_bytes([data[0], data[1]])) << 16;
            }
            4 => return Err(bad("bad extended address record")),
            _ => {}
        }
    }
    Ok(img)
}

pub fn maybe_decode(data: Vec<u8>) -> Result<Vec<u8>> {
    if data.first() == Some(&b':') {
        decode(&data)
    } else {
        Ok(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_records() {
        assert_eq!(
            decode(b":0300000002000AF1\n:00000001FF\n").unwrap(),
            vec![0x02, 0x00, 0x0A]
        );
        assert_eq!(
            decode(b":0100040055A6\r\n:00000001FF").unwrap(),
            vec![0, 0, 0, 0, 0x55]
        );
    }

    #[test]
    fn rejects_bad_checksum() {
        assert_eq!(decode(b":0300000002000AF2\n").unwrap_err().exit_code(), 2);
    }

    #[test]
    fn binary_passes_through() {
        assert_eq!(maybe_decode(vec![0xff, 0x00]).unwrap(), vec![0xff, 0x00]);
    }
}
