//! LA1010 control protocol (vendor requests on EP0, bitstream on EP2).
//! USB-agnostic: everything goes through `Transport`. See the spec's "Device protocol".

use std::thread::sleep;
use std::time::Duration;

use crate::error::{Error, Result};
use crate::firmware::bitstream_name;

pub const VID: u16 = 0x77a1;
pub const PID: u16 = 0x01a2;

pub const REQ_FX2_RAM: u8 = 0xA0;
pub const REQ_EEPROM: u8 = 0xA2;
pub const REQ_FPGA_INIT: u8 = 0x50;
pub const REQ_FPGA_ENABLE: u8 = 0x10;
pub const REQ_FPGA_REG: u8 = 0x20;
pub const REQ_BULK_RESET: u8 = 0x38;
pub const REQ_BULK_START: u8 = 0x30;

const FX2_CPUCS: u16 = 0xE600;
const FX2_RAM: usize = 0x4000;
const REG_RUN: u16 = 0x00;
const REG_CAPT_MODE: u16 = 0x03;
const REG_PINS: u16 = 0x04;
const REG_SAMPLING: u16 = 0x10;
const REG_CHANNELS: u16 = 0x20;
const REG_THRESHOLD: u16 = 0x68;
const CHUNK: usize = 4096;
/// Hardware sample limit; streaming is stopped by the host long before.
const SAMPLE_LIMIT: u64 = 10_000_000_000;

pub trait Transport {
    fn ctrl_in(&self, request: u8, value: u16, index: u16, len: u16) -> Result<Vec<u8>>;
    fn ctrl_out(&self, request: u8, value: u16, index: u16, data: &[u8]) -> Result<()>;
    fn bulk_out(&mut self, data: &[u8]) -> Result<()>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub magic: u8,
    pub magic2: u8,
    pub revision: &'static str,
    pub bitstream: String,
    pub manufactured: Option<String>,
}

pub fn revision(magic: u8) -> Option<&'static str> {
    match magic {
        0x04 => Some("a0"),
        0x07 => Some("a1"),
        0x0a => Some("a2"),
        _ => None,
    }
}

/// Model magic at EEPROM 0x08 (value, complement; primary and backup copy),
/// manufacturing date (BCD year, month, complements) at 0x20.
pub fn identify(t: &impl Transport) -> Result<Identity> {
    let b = t.ctrl_in(REQ_EEPROM, 0x08, 0, 8)?;
    let ok = |i: usize| b[i] ^ b[i + 1] == 0xff;
    let (magic, magic2) = if ok(0) && ok(2) {
        (b[0], b[2])
    } else if ok(4) && ok(6) {
        (b[4], b[6])
    } else if ok(0) {
        (b[0], 0)
    } else if ok(4) {
        (b[4], 0)
    } else {
        return Err(Error::Device(format!(
            "EEPROM model bytes unreadable: {b:02x?}"
        )));
    };
    let revision = revision(magic).ok_or_else(|| {
        Error::Device(format!(
            "EEPROM magic 0x{magic:02x} is not an LA1010 (other Kingst models are not supported)"
        ))
    })?;
    let d = t.ctrl_in(REQ_EEPROM, 0x20, 0, 4)?;
    let manufactured = (d[0] ^ d[2] == 0xff && d[1] ^ d[3] == 0xff)
        .then(|| format!("20{:02x}-{:02x}", d[0], d[1]));
    Ok(Identity {
        magic,
        magic2,
        revision,
        bitstream: bitstream_name(revision),
        manufactured,
    })
}

/// Load FX2 RAM via the EZ-USB loader (hold CPU in reset, write, release).
pub fn fx2_upload(t: &impl Transport, fw: &[u8]) -> Result<()> {
    if fw.len() > FX2_RAM {
        return Err(Error::Firmware(format!(
            "FX2 image is {} bytes, larger than 16 KiB RAM",
            fw.len()
        )));
    }
    t.ctrl_out(REQ_FX2_RAM, FX2_CPUCS, 0, &[1])?;
    for (i, chunk) in fw.chunks(CHUNK).enumerate() {
        t.ctrl_out(REQ_FX2_RAM, (i * CHUNK) as u16, 0, chunk)?;
    }
    t.ctrl_out(REQ_FX2_RAM, FX2_CPUCS, 0, &[0])
}

/// FPGA init status 0 means a bitstream is loaded.
pub fn fpga_ready(t: &impl Transport) -> Result<bool> {
    Ok(t.ctrl_in(REQ_FPGA_INIT, 0, 0, 1)?[0] == 0)
}

pub fn fpga_upload(t: &mut impl Transport, bits: &[u8]) -> Result<()> {
    t.ctrl_out(REQ_FPGA_INIT, 0, 0, &(bits.len() as u32).to_le_bytes())?;
    let mut padded = bits.to_vec();
    padded.resize(bits.len().div_ceil(CHUNK) * CHUNK, 0);
    for chunk in padded.chunks(CHUNK) {
        t.bulk_out(chunk)?;
    }
    let st = t.ctrl_in(REQ_FPGA_INIT, 0, 0, 1)?[0];
    if st != 0 {
        return Err(Error::Device(format!(
            "FPGA rejected the bitstream (status 0x{st:02x})"
        )));
    }
    sleep(Duration::from_millis(30));
    t.ctrl_out(REQ_FPGA_ENABLE, 1, 0, &[])?;
    sleep(Duration::from_millis(40));
    Ok(())
}

/// Two PWM duty values (R56, R79) driving the threshold bias DAC.
pub fn threshold_regs(v: f64) -> Result<[u8; 4]> {
    if !(-4.0..=4.0).contains(&v) {
        return Err(Error::Usage(format!(
            "threshold {v} V is outside -4.0..4.0 V"
        )));
    }
    let (r79, r56): (u16, f64) = if v >= 2.9 {
        (0, 302.0 * v - 363.0)
    } else if v > -0.4 {
        (0x00f2, 302.0 * v + 121.0)
    } else {
        (0x02d7, 302.0 * v + 1090.0)
    };
    let r56 = (r56 as i64).clamp(10, 1100) as u16;
    let mut b = [0u8; 4];
    b[..2].copy_from_slice(&r56.to_le_bytes());
    b[2..].copy_from_slice(&r79.to_le_bytes());
    Ok(b)
}

/// u40 sample limit, u40 pre-trigger samples (0), u24 pre-trigger memory >> 8 (1),
/// u16 divider of the 800 MHz base clock, u8 0.
pub fn sample_config(divider: u16) -> [u8; 16] {
    let mut b = [0u8; 16];
    b[..5].copy_from_slice(&SAMPLE_LIMIT.to_le_bytes()[..5]);
    b[10] = 1;
    b[13..15].copy_from_slice(&divider.to_le_bytes());
    b
}

/// u32 enabled-channel mask, then three u32 trigger words (unused: no HW trigger when streaming).
pub fn channel_config(mask: u16) -> [u8; 16] {
    let mut b = [0u8; 16];
    b[..4].copy_from_slice(&u32::from(mask).to_le_bytes());
    b
}

pub fn configure(t: &impl Transport, mask: u16, divider: u16, volts: f64) -> Result<()> {
    t.ctrl_out(REQ_FPGA_REG, REG_THRESHOLD, 0, &threshold_regs(volts)?)?;
    t.ctrl_out(REQ_FPGA_REG, REG_CAPT_MODE, 0, &[1])?;
    t.ctrl_out(REQ_FPGA_REG, REG_CHANNELS, 16, &channel_config(mask))?;
    t.ctrl_out(REQ_FPGA_REG, REG_SAMPLING, 0, &sample_config(divider))
}

pub fn flush(t: &impl Transport) -> Result<()> {
    t.ctrl_out(REQ_BULK_RESET, 0, 0, &[])
}

/// Run, then start bulk data. Bulk IN transfers must already be queued.
pub fn start(t: &impl Transport) -> Result<()> {
    t.ctrl_out(REQ_FPGA_REG, REG_RUN, 0, &[3])?;
    t.ctrl_out(REQ_BULK_START, 0, 0, &[])
}

pub fn halt(t: &impl Transport) -> Result<()> {
    t.ctrl_out(REQ_FPGA_REG, REG_RUN, 0, &[0])
}

pub fn pins(t: &impl Transport) -> Result<u16> {
    let b = t.ctrl_in(REQ_FPGA_REG, REG_PINS, 0, 2)?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    #[derive(Debug, Clone, PartialEq)]
    enum Call {
        In(u8, u16, u16, u16),
        Out(u8, u16, u16, Vec<u8>),
        Bulk(usize),
    }

    #[derive(Default)]
    struct Mock {
        calls: RefCell<Vec<Call>>,
        replies: HashMap<(u8, u16), Vec<u8>>,
    }

    impl Transport for Mock {
        fn ctrl_in(&self, request: u8, value: u16, index: u16, len: u16) -> Result<Vec<u8>> {
            self.calls
                .borrow_mut()
                .push(Call::In(request, value, index, len));
            self.replies
                .get(&(request, value))
                .map(|d| d[..len as usize].to_vec())
                .ok_or_else(|| Error::Device("stall".into()))
        }
        fn ctrl_out(&self, request: u8, value: u16, index: u16, data: &[u8]) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(Call::Out(request, value, index, data.to_vec()));
            Ok(())
        }
        fn bulk_out(&mut self, data: &[u8]) -> Result<()> {
            self.calls.borrow_mut().push(Call::Bulk(data.len()));
            Ok(())
        }
    }

    fn eeprom(model: [u8; 8]) -> Mock {
        let mut m = Mock::default();
        m.replies.insert((REQ_EEPROM, 0x08), model.to_vec());
        m.replies
            .insert((REQ_EEPROM, 0x20), vec![0x24, 0x03, 0xdb, 0xfc]);
        m
    }

    #[test]
    fn identifies_users_la1010_a2() {
        // EEPROM bytes read from the user's unit on 2026-11-18.
        let id = identify(&eeprom([0x0a, 0xf5, 0x00, 0x00, 0x0a, 0xf5, 0x10, 0xef])).unwrap();
        assert_eq!(
            id,
            Identity {
                magic: 0x0a,
                magic2: 0x10,
                revision: "a2",
                bitstream: "kingst-la1010a2-fpga.bitstream".into(),
                manufactured: Some("2024-03".into()),
            }
        );
    }

    #[test]
    fn primary_magic_and_unsupported_model() {
        assert_eq!(
            identify(&eeprom([0x07, 0xf8, 0x00, 0xff, 0, 0, 0, 0]))
                .unwrap()
                .revision,
            "a1"
        );
        let err = identify(&eeprom([0x02, 0xfd, 0x00, 0xff, 0, 0, 0, 0])).unwrap_err();
        assert!(err.to_string().contains("not an LA1010"));
        assert!(identify(&eeprom([0; 8])).is_err());
    }

    #[test]
    fn threshold_encoding() {
        assert_eq!(threshold_regs(1.4).unwrap(), [0x1f, 0x02, 0xf2, 0x00]); // R56 543, R79 0xf2
        assert_eq!(threshold_regs(3.3).unwrap(), [0x79, 0x02, 0x00, 0x00]); // R56 633, R79 0
        assert_eq!(threshold_regs(-1.0).unwrap(), [0x14, 0x03, 0xd7, 0x02]); // R56 788, R79 0x2d7
        assert_eq!(threshold_regs(5.0).unwrap_err().exit_code(), 1);
    }

    #[test]
    fn sample_and_channel_config() {
        assert_eq!(
            sample_config(8),
            [
                0x00, 0xe4, 0x0b, 0x54, 0x02, 0, 0, 0, 0, 0, 0x01, 0, 0, 0x08, 0x00, 0
            ]
        );
        assert_eq!(channel_config(0x8001)[..4], [0x01, 0x80, 0, 0]);
    }

    #[test]
    fn configure_sequence() {
        let m = Mock::default();
        configure(&m, 0b11, 80, 1.4).unwrap();
        let c = m.calls.into_inner();
        assert_eq!(
            c[0],
            Call::Out(REQ_FPGA_REG, 0x68, 0, vec![0x1f, 0x02, 0xf2, 0x00])
        );
        assert_eq!(c[1], Call::Out(REQ_FPGA_REG, 0x03, 0, vec![1]));
        assert_eq!(
            c[2],
            Call::Out(REQ_FPGA_REG, 0x20, 16, channel_config(0b11).to_vec())
        );
        assert_eq!(
            c[3],
            Call::Out(REQ_FPGA_REG, 0x10, 0, sample_config(80).to_vec())
        );
    }

    #[test]
    fn fx2_upload_sequence() {
        let m = Mock::default();
        fx2_upload(&m, &[0xaa; 5000]).unwrap();
        let c = m.calls.into_inner();
        assert_eq!(c.len(), 4);
        assert_eq!(c[0], Call::Out(REQ_FX2_RAM, 0xE600, 0, vec![1]));
        assert!(matches!(&c[1], Call::Out(REQ_FX2_RAM, 0, 0, d) if d.len() == 4096));
        assert!(matches!(&c[2], Call::Out(REQ_FX2_RAM, 4096, 0, d) if d.len() == 904));
        assert_eq!(c[3], Call::Out(REQ_FX2_RAM, 0xE600, 0, vec![0]));
    }

    #[test]
    fn fpga_upload_pads_and_checks_status() {
        let mut m = Mock::default();
        m.replies.insert((REQ_FPGA_INIT, 0), vec![0]);
        fpga_upload(&mut m, &[1; 5000]).unwrap();
        let c = m.calls.into_inner();
        assert_eq!(
            c[0],
            Call::Out(REQ_FPGA_INIT, 0, 0, 5000u32.to_le_bytes().to_vec())
        );
        assert_eq!(c[1..3], [Call::Bulk(4096), Call::Bulk(4096)]);
        assert_eq!(c[3], Call::In(REQ_FPGA_INIT, 0, 0, 1));
        assert_eq!(c[4], Call::Out(REQ_FPGA_ENABLE, 1, 0, vec![]));

        let mut bad = Mock::default();
        bad.replies.insert((REQ_FPGA_INIT, 0), vec![7]);
        assert!(fpga_upload(&mut bad, &[1; 10]).is_err());
    }

    #[test]
    fn run_control() {
        let m = Mock::default();
        start(&m).unwrap();
        halt(&m).unwrap();
        assert_eq!(
            m.calls.into_inner(),
            vec![
                Call::Out(REQ_FPGA_REG, 0x00, 0, vec![3]),
                Call::Out(REQ_BULK_START, 0, 0, vec![]),
                Call::Out(REQ_FPGA_REG, 0x00, 0, vec![0]),
            ]
        );
    }
}
