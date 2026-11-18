//! Streaming acquisition: queued bulk IN transfers on EP6, halt guaranteed on exit.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use nusb::transfer::{Bulk, In};

use crate::device;
use crate::error::{Error, Result};
use crate::usb::Usb;

const XFERS: usize = 8;

pub struct Setup {
    pub mask: u16,
    pub divider: u16,
    pub rate: f64,
    pub threshold: f64,
}

pub struct Stats {
    pub bytes: u64,
    pub elapsed: Duration,
}

/// About 50 ms of data per transfer, in whole 512-byte packets, max 512 KiB.
pub fn transfer_size(bytes_per_sec: f64) -> usize {
    ((bytes_per_sec * 0.05) as usize / 512 * 512).clamp(512, 512 * 1024)
}

struct Halt<'a>(&'a Usb);

impl Drop for Halt<'_> {
    fn drop(&mut self) {
        let _ = device::halt(self.0);
    }
}

pub fn stream(
    usb: &Usb,
    s: &Setup,
    stop: &AtomicBool,
    mut sink: impl FnMut(&[u8]) -> Result<bool>,
) -> Result<Stats> {
    let bps = s.rate * f64::from(s.mask.count_ones()) / 8.0;
    let size = transfer_size(bps);
    device::configure(usb, s.mask, s.divider, s.threshold)?;
    device::flush(usb)?;
    let intf = usb
        .intf
        .as_ref()
        .ok_or_else(|| Error::Device("interface not claimed".into()))?;
    let mut ep = intf
        .endpoint::<Bulk, In>(0x86)
        .map_err(|e| Error::Device(format!("open EP6: {e}")))?;
    for _ in 0..XFERS {
        let b = ep.allocate(size);
        ep.submit(b);
    }
    let halt = Halt(usb);
    device::start(usb)?;
    let t0 = Instant::now();
    let mut last = t0;
    let mut total = 0u64;
    let slack = (XFERS * size) as f64 + 8.0 * 1024.0 * 1024.0;
    let stall = Duration::from_secs(2) + Duration::from_secs_f64(2.0 * size as f64 / bps);
    let result = loop {
        if stop.load(Ordering::Relaxed) {
            break Ok(());
        }
        let Some(c) = ep.wait_next_complete(Duration::from_millis(100)) else {
            if last.elapsed() > stall {
                break Err(Error::Device(format!(
                    "stream stalled: no data for {:.1} s (the device stops when rate x channels exceeds about 320 Mbit/s; lower them)",
                    last.elapsed().as_secs_f64()
                )));
            }
            continue;
        };
        if let Err(e) = c.status {
            break Err(Error::Device(format!("bulk read EP6: {e}")));
        }
        last = Instant::now();
        total += c.actual_len as u64;
        match sink(&c.buffer[..c.actual_len]) {
            Ok(true) => {}
            Ok(false) => break Ok(()),
            Err(e) => break Err(e),
        }
        let el = t0.elapsed().as_secs_f64();
        if el > 0.5 && bps * el - total as f64 > slack {
            break Err(Error::Device(format!(
                "overrun: received {total} of ~{:.0} bytes after {el:.2} s; lower the rate or channel count",
                bps * el
            )));
        }
        ep.submit(c.buffer);
    };
    drop(halt);
    ep.cancel_all();
    while ep.pending() > 0 && ep.wait_next_complete(Duration::from_secs(1)).is_some() {}
    result.map(|()| Stats {
        bytes: total,
        elapsed: t0.elapsed(),
    })
}

#[cfg(test)]
mod tests {
    use super::transfer_size;

    #[test]
    fn transfer_size_targets_50ms_in_512_byte_packets() {
        assert_eq!(transfer_size(25e6), 512 * 1024); // 2 ch x 100 MHz: capped
        assert_eq!(transfer_size(125e3), 6144); // 1 ch x 1 MHz
        assert_eq!(transfer_size(1e3), 512); // floor: one packet
    }
}
