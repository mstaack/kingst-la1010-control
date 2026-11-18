//! nusb-backed transport and device bring-up (FX2 RAM firmware, FPGA bitstream).

use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant};

use nusb::MaybeFuture;
use nusb::transfer::{Buffer, Bulk, ControlIn, ControlOut, ControlType, Out, Recipient};

use crate::device::{self, Identity, PID, REQ_EEPROM, Transport, VID};
use crate::error::{Error, Result};
use crate::firmware;

const TIMEOUT: Duration = Duration::from_millis(500);

pub struct Usb {
    dev: nusb::Device,
    pub(crate) intf: Option<nusb::Interface>,
    ep_out: Option<nusb::Endpoint<Bulk, Out>>,
}

fn usb_err(what: &str, e: impl std::fmt::Display) -> Error {
    Error::Device(format!("{what}: {e}"))
}

impl Transport for Usb {
    fn ctrl_in(&self, request: u8, value: u16, index: u16, len: u16) -> Result<Vec<u8>> {
        let req = ControlIn {
            control_type: ControlType::Vendor,
            recipient: Recipient::Device,
            request,
            value,
            index,
            length: len,
        };
        let d = self
            .dev
            .control_in(req, TIMEOUT)
            .wait()
            .map_err(|e| usb_err(&format!("control in 0x{request:02x}"), e))?;
        if d.len() != usize::from(len) {
            return Err(Error::Device(format!(
                "control in 0x{request:02x}: got {} of {len} bytes",
                d.len()
            )));
        }
        Ok(d)
    }

    fn ctrl_out(&self, request: u8, value: u16, index: u16, data: &[u8]) -> Result<()> {
        let req = ControlOut {
            control_type: ControlType::Vendor,
            recipient: Recipient::Device,
            request,
            value,
            index,
            data,
        };
        self.dev
            .control_out(req, TIMEOUT)
            .wait()
            .map_err(|e| usb_err(&format!("control out 0x{request:02x}"), e))
    }

    fn bulk_out(&mut self, data: &[u8]) -> Result<()> {
        if self.ep_out.is_none() {
            let intf = self
                .intf
                .as_ref()
                .ok_or_else(|| Error::Device("interface not claimed".into()))?;
            self.ep_out = Some(
                intf.endpoint::<Bulk, Out>(0x02)
                    .map_err(|e| usb_err("open EP2", e))?,
            );
        }
        let ep = self.ep_out.as_mut().expect("just opened");
        let c = ep.transfer_blocking(Buffer::from(data.to_vec()), Duration::from_secs(1));
        c.status.map_err(|e| usb_err("bulk out EP2", e))?;
        if c.actual_len != data.len() {
            return Err(Error::Device(format!(
                "bulk out EP2: wrote {} of {} bytes",
                c.actual_len,
                data.len()
            )));
        }
        Ok(())
    }
}

fn open_raw() -> Result<Usb> {
    let info = nusb::list_devices()
        .wait()
        .map_err(|e| usb_err("list USB devices", e))?
        .find(|d| d.vendor_id() == VID && d.product_id() == PID)
        .ok_or_else(|| Error::Device("no Kingst LA1010 found (USB 77a1:01a2)".into()))?;
    let dev = info.open().wait().map_err(|e| usb_err("open device", e))?;
    Ok(Usb {
        dev,
        intf: None,
        ep_out: None,
    })
}

/// The EZ-USB loader stalls vendor requests other than 0xA0; our firmware answers EEPROM reads.
fn fx2_running(u: &Usb) -> bool {
    u.ctrl_in(REQ_EEPROM, 0x20, 0, 4).is_ok()
}

fn wait_reenumeration() -> Result<Usb> {
    sleep(Duration::from_millis(1800));
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Ok(u) = open_raw()
            && fx2_running(&u)
        {
            return Ok(u);
        }
        if Instant::now() > deadline {
            return Err(Error::Device(
                "device did not re-enumerate within 3 s after FX2 firmware upload".into(),
            ));
        }
        sleep(Duration::from_millis(200));
    }
}

pub struct Opened {
    pub usb: Usb,
    pub id: Identity,
    pub fx2_loaded: bool,
    pub fpga_loaded: bool,
}

/// Find the analyzer, load FX2 firmware and FPGA bitstream if needed, leave it halted.
pub fn open(fw_dir: Option<&Path>) -> Result<Opened> {
    let mut usb = open_raw()?;
    let mut fx2_loaded = false;
    if !fx2_running(&usb) {
        let fw = firmware::load(firmware::FX2, fw_dir)?;
        usb.dev
            .set_configuration(1)
            .wait()
            .map_err(|e| usb_err("set configuration", e))?;
        device::fx2_upload(&usb, &fw)?;
        drop(usb);
        usb = wait_reenumeration()?;
        fx2_loaded = true;
    }
    usb.intf = Some(
        usb.dev
            .claim_interface(0)
            .wait()
            .map_err(|e| usb_err("claim interface 0", e))?,
    );
    let id = device::identify(&usb)?;
    let mut fpga_loaded = false;
    if !device::fpga_ready(&usb)? {
        let bits = firmware::load(&id.bitstream, fw_dir)?;
        device::fpga_upload(&mut usb, &bits)?;
        fpga_loaded = true;
    }
    device::halt(&usb)?;
    Ok(Opened {
        usb,
        id,
        fx2_loaded,
        fpga_loaded,
    })
}
