//! Capture files: VCD and sigrok .sr (read/write), CSV (write).

pub mod vcd;

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;

use crate::error::{Error, Result};
use crate::stream::Edge;

pub trait Writer {
    fn edges(&mut self, edges: &[Edge]) -> Result<()>;
    fn finish(self: Box<Self>, end: u64) -> Result<()>;
}

#[derive(Debug, PartialEq)]
pub struct Recording {
    pub rate: f64,
    pub mask: u16,
    pub edges: Vec<Edge>,
    pub end: u64,
}

pub fn invalid(msg: impl Into<String>) -> Error {
    Error::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        msg.into(),
    ))
}

fn ext(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

pub fn create(path: &Path, mask: u16, rate: f64) -> Result<Box<dyn Writer>> {
    match ext(path).as_str() {
        "vcd" => Ok(Box::new(vcd::VcdWriter::new(
            BufWriter::new(File::create(path)?),
            mask,
            rate,
        )?)),
        _ => Err(Error::Usage(format!(
            "{}: output must end in .vcd, .sr or .csv",
            path.display()
        ))),
    }
}

pub fn read(path: &Path) -> Result<Recording> {
    match ext(path).as_str() {
        "vcd" => vcd::read(BufReader::new(File::open(path)?)),
        _ => Err(Error::Usage(format!(
            "{}: input must be .vcd or .sr",
            path.display()
        ))),
    }
}
