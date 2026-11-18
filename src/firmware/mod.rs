//! Vendor firmware: extraction from KingstVIS, installation, lookup.
//!
//! The FX2 firmware and FPGA bitstreams belong to Qingdao Kingst Electronics and
//! are never bundled. Users obtain them with `la1010 firmware fetch|extract`.

#[cfg(test)]
pub(crate) mod fixture;
pub mod ihex;
pub mod qrc;

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use object::{Object, ObjectSection, ObjectSymbol};

use crate::error::{Error, Result};

pub const FX2: &str = "kingst-la-01a2.fw";
pub const VENDOR_URL: &str = "https://www.qdkingst.com/download/vis_linux";

/// (file, size, crc32) from KingstVIS v3.6.6.
pub const KNOWN: &[(&str, usize, u32)] = &[
    ("kingst-la-01a2.fw", 5430, 0x7205_51a9),
    ("kingst-la1010a0-fpga.bitstream", 124_412, 0xa655_cdd2),
    ("kingst-la1010a1-fpga.bitstream", 125_301, 0x2333_d203),
    ("kingst-la1010a2-fpga.bitstream", 342_524, 0xd2cf_8e9e),
];

pub fn bitstream_name(rev: &str) -> String {
    format!("kingst-la1010{rev}-fpga.bitstream")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blob {
    pub name: String,
    pub data: Vec<u8>,
}

fn fw_err(m: impl Into<String>) -> Error {
    Error::Firmware(m.into())
}

fn symbol<'a>(file: &object::File<'a>, name: &str) -> Result<&'a [u8]> {
    let sym = file
        .symbols()
        .find(|s| s.name().is_ok_and(|n| n == name))
        .ok_or_else(|| fw_err(format!("symbol {name} not found: not a KingstVIS binary?")))?;
    let idx = sym
        .section_index()
        .ok_or_else(|| fw_err(format!("{name}: no section")))?;
    let sec = file
        .section_by_index(idx)
        .map_err(|e| fw_err(format!("{name}: {e}")))?;
    let data = sec.data().map_err(|e| fw_err(format!("{name}: {e}")))?;
    let off = (sym.address() - sec.address()) as usize;
    data.get(off..off + sym.size() as usize)
        .ok_or_else(|| fw_err(format!("{name}: out of bounds")))
}

pub fn extract_from_elf(elf: &[u8]) -> Result<Vec<Blob>> {
    let file = object::File::parse(elf).map_err(|e| fw_err(format!("not an ELF binary: {e}")))?;
    let data = symbol(&file, "_ZL16qt_resource_data")?;
    let names = symbol(&file, "_ZL16qt_resource_name")?;
    let tree = symbol(&file, "_ZL18qt_resource_struct")?;
    let mut blobs = Vec::new();
    for (path, bytes) in qrc::files(data, names, tree)? {
        let name = match path.as_str() {
            "fwusb/fw01A2" => FX2.to_string(),
            p => match p.strip_prefix("fwfpga/LA1010") {
                Some(rev @ ("A0" | "A1" | "A2")) => bitstream_name(&rev.to_ascii_lowercase()),
                _ => continue,
            },
        };
        blobs.push(Blob {
            name,
            data: ihex::maybe_decode(bytes)?,
        });
    }
    if !blobs.iter().any(|b| b.name == FX2) || blobs.len() < 2 {
        return Err(fw_err(
            "KingstVIS binary does not contain the LA1010 firmware files",
        ));
    }
    Ok(blobs)
}

pub fn from_targz(r: impl Read) -> Result<Vec<Blob>> {
    let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(r));
    for entry in ar.entries()? {
        let mut e = entry?;
        let is_bin = e.path()?.file_name() == Some(OsStr::new("KingstVIS"))
            && e.header().entry_type().is_file();
        if is_bin {
            let mut v = Vec::new();
            e.read_to_end(&mut v)?;
            return extract_from_elf(&v);
        }
    }
    Err(fw_err("no KingstVIS/KingstVIS binary in archive"))
}

/// A KingstVIS Linux tarball, an unpacked directory, or the KingstVIS ELF binary.
pub fn extract_from_path(p: &Path) -> Result<Vec<Blob>> {
    let file = if p.is_dir() {
        [p.join("KingstVIS/KingstVIS"), p.join("KingstVIS")]
            .into_iter()
            .find(|c| c.is_file())
            .ok_or_else(|| fw_err(format!("{}: no KingstVIS binary in directory", p.display())))?
    } else {
        p.to_path_buf()
    };
    let data = fs::read(&file)?;
    match data.get(..4) {
        Some([0x1f, 0x8b, ..]) => from_targz(&data[..]),
        Some(b"\x7fELF") => extract_from_elf(&data),
        _ => Err(fw_err(format!(
            "{}: not a KingstVIS tarball or Linux binary",
            file.display()
        ))),
    }
}

/// Download KingstVIS for Linux from `url` and extract the LA1010 files in memory.
pub fn fetch(url: &str) -> Result<Vec<Blob>> {
    let resp = ureq::get(url)
        .call()
        .map_err(|e| fw_err(format!("download {url}: {e}")))?;
    let reader = resp
        .into_body()
        .into_with_config()
        .limit(200 << 20)
        .reader();
    from_targz(reader).map_err(|e| match e {
        Error::Io(io) => fw_err(format!("download {url}: {io}")),
        other => other,
    })
}

pub fn crc32(d: &[u8]) -> u32 {
    let mut c = flate2::Crc::new();
    c.update(d);
    c.sum()
}

#[derive(Debug)]
pub struct Installed {
    pub name: String,
    pub path: PathBuf,
    pub size: usize,
    pub crc: u32,
    /// Some(true): matches v3.6.6; Some(false): differs; None: not in the table.
    pub known: Option<bool>,
}

pub fn install(blobs: &[Blob], dir: &Path) -> Result<Vec<Installed>> {
    fs::create_dir_all(dir)?;
    blobs
        .iter()
        .map(|b| {
            let path = dir.join(&b.name);
            let tmp = dir.join(format!(".{}.tmp", b.name));
            fs::write(&tmp, &b.data)?;
            fs::rename(&tmp, &path)?;
            let crc = crc32(&b.data);
            let known = KNOWN
                .iter()
                .find(|k| k.0 == b.name)
                .map(|k| k.1 == b.data.len() && k.2 == crc);
            Ok(Installed {
                name: b.name.clone(),
                path,
                size: b.data.len(),
                crc,
                known,
            })
        })
        .collect()
}

pub fn dirs(cli: Option<&Path>) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(p) = cli {
        v.push(p.to_path_buf());
    }
    if let Some(p) = env::var_os("LA1010_FIRMWARE_DIR") {
        v.push(p.into());
    }
    if let Some(p) = env::var_os("XDG_DATA_HOME") {
        v.push(PathBuf::from(p).join("la1010"));
    }
    if let Some(h) = env::var_os("HOME") {
        v.push(PathBuf::from(h).join(".local/share/la1010"));
    }
    v
}

pub fn install_dir(cli: Option<&Path>) -> Result<PathBuf> {
    dirs(cli)
        .into_iter()
        .next()
        .ok_or_else(|| fw_err("no firmware directory: set LA1010_FIRMWARE_DIR"))
}

pub fn load(name: &str, cli: Option<&Path>) -> Result<Vec<u8>> {
    let ds = dirs(cli);
    for d in &ds {
        let p = d.join(name);
        if p.is_file() {
            return Ok(fs::read(p)?);
        }
    }
    let searched: Vec<_> = ds.iter().map(|d| d.display().to_string()).collect();
    Err(fw_err(format!(
        "firmware file {name} not found (searched: {}). Run `la1010 firmware fetch` to download \
         it from Kingst, or `la1010 firmware extract <KingstVIS tarball>`.",
        searched.join(", ")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fixture::{kingstvis, targz};

    fn names(b: &[Blob]) -> Vec<&str> {
        b.iter().map(|b| b.name.as_str()).collect()
    }

    #[test]
    fn extracts_la1010_files_from_elf() {
        let blobs = extract_from_elf(&kingstvis()).unwrap();
        assert_eq!(names(&blobs), vec![FX2, "kingst-la1010a2-fpga.bitstream"]);
        assert_eq!(blobs[0].data, vec![0x02, 0x00, 0x0A]);
        assert_eq!(blobs[1].data, b"BITSTREAM-A2");
    }

    #[test]
    fn rejects_non_elf_and_missing_files() {
        assert_eq!(extract_from_elf(b"hello").unwrap_err().exit_code(), 2);
        let (d, n, t) = fixture::rcc(&[("fwfpga", vec![("LA1010A2", b"x".to_vec())])]);
        assert!(extract_from_elf(&fixture::elf(&d, &n, &t)).is_err());
    }

    #[test]
    fn extracts_from_tarball_dir_and_binary() {
        let dir = tempfile::tempdir().unwrap();
        let tgz = dir.path().join("KingstVIS_v3.6.6.tar.gz");
        std::fs::write(&tgz, targz("KingstVIS/KingstVIS", &kingstvis())).unwrap();
        assert_eq!(extract_from_path(&tgz).unwrap().len(), 2);

        let unpacked = dir.path().join("KingstVIS");
        std::fs::create_dir(&unpacked).unwrap();
        std::fs::write(unpacked.join("KingstVIS"), kingstvis()).unwrap();
        assert_eq!(extract_from_path(dir.path()).unwrap().len(), 2);
        assert_eq!(extract_from_path(&unpacked).unwrap().len(), 2);
        assert_eq!(
            extract_from_path(&unpacked.join("KingstVIS"))
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn install_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let blobs = extract_from_elf(&kingstvis()).unwrap();
        let rep = install(&blobs, dir.path()).unwrap();
        assert_eq!(rep[0].known, Some(false)); // synthetic data differs from v3.6.6
        assert_eq!(rep[0].size, 3);
        assert_eq!(load(FX2, Some(dir.path())).unwrap(), vec![0x02, 0x00, 0x0A]);
        let err = load("no-such-file.bin", Some(dir.path())).unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("la1010 firmware fetch"));
    }

    #[test]
    fn fetch_from_local_server() {
        use std::io::{Read as _, Write as _};
        let body = targz("KingstVIS/KingstVIS", &kingstvis());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf);
            write!(
                s,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            s.write_all(&body).unwrap();
        });
        let blobs = fetch(&format!("http://127.0.0.1:{port}/download/vis_linux")).unwrap();
        server.join().unwrap();
        assert_eq!(names(&blobs), vec![FX2, "kingst-la1010a2-fpga.bitstream"]);
    }

    #[test]
    fn fetch_reports_http_errors() {
        let err = fetch("http://127.0.0.1:9/nothing").unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("download"));
    }

    #[test]
    fn crc32_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn cli_dir_comes_first() {
        let d = dirs(Some(Path::new("/x")));
        assert_eq!(d[0], PathBuf::from("/x"));
    }
}
