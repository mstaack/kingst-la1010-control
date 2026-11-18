//! Synthetic KingstVIS artefacts for tests. Never contains vendor data.

use std::io::Write;

use flate2::Compression;
use flate2::write::{GzEncoder, ZlibEncoder};

fn name(names: &mut Vec<u8>, s: &str) -> u32 {
    let off = names.len() as u32;
    let u: Vec<u16> = s.encode_utf16().collect();
    names.extend((u.len() as u16).to_be_bytes());
    names.extend(0u32.to_be_bytes());
    for c in u {
        names.extend(c.to_be_bytes());
    }
    off
}

fn dir_entry(tree: &mut Vec<u8>, n: u32, count: usize, first: usize) {
    tree.extend(n.to_be_bytes());
    tree.extend(2u16.to_be_bytes());
    tree.extend((count as u32).to_be_bytes());
    tree.extend((first as u32).to_be_bytes());
}

type Dir<'a> = (&'a str, Vec<(&'a str, Vec<u8>)>);

/// Build (data, names, tree) for root -> dirs -> zlib-compressed files.
pub fn rcc(dirs: &[Dir]) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let (mut data, mut names, mut tree) = (Vec::new(), Vec::new(), Vec::new());
    let root = name(&mut names, "");
    dir_entry(&mut tree, root, dirs.len(), 1);
    let mut next = 1 + dirs.len();
    for (d, files) in dirs {
        let n = name(&mut names, d);
        dir_entry(&mut tree, n, files.len(), next);
        next += files.len();
    }
    for (_, files) in dirs {
        for (f, content) in files {
            let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
            z.write_all(content).unwrap();
            let mut blob = (content.len() as u32).to_be_bytes().to_vec();
            blob.extend(z.finish().unwrap());
            let off = data.len() as u32;
            data.extend((blob.len() as u32).to_be_bytes());
            data.extend(blob);
            let n = name(&mut names, f);
            tree.extend(n.to_be_bytes());
            tree.extend(1u16.to_be_bytes());
            tree.extend(0u16.to_be_bytes());
            tree.extend(0u16.to_be_bytes());
            tree.extend(off.to_be_bytes());
        }
    }
    (data, names, tree)
}

/// Relocatable x86-64 ELF with the three Qt resource symbols in .rodata.
pub fn elf(data: &[u8], names: &[u8], tree: &[u8]) -> Vec<u8> {
    use object::write::{Object, Symbol, SymbolSection};
    use object::{
        Architecture, BinaryFormat, Endianness, SectionKind, SymbolFlags, SymbolKind, SymbolScope,
    };
    let mut obj = Object::new(BinaryFormat::Elf, Architecture::X86_64, Endianness::Little);
    let sec = obj.add_section(Vec::new(), b".rodata".to_vec(), SectionKind::ReadOnlyData);
    for (sym, bytes) in [
        ("_ZL16qt_resource_data", data),
        ("_ZL16qt_resource_name", names),
        ("_ZL18qt_resource_struct", tree),
    ] {
        let off = obj.append_section_data(sec, bytes, 1);
        obj.add_symbol(Symbol {
            name: sym.as_bytes().to_vec(),
            value: off,
            size: bytes.len() as u64,
            kind: SymbolKind::Data,
            scope: SymbolScope::Compilation,
            weak: false,
            section: SymbolSection::Section(sec),
            flags: SymbolFlags::None,
        });
    }
    obj.write().unwrap()
}

pub fn targz(path: &str, content: &[u8]) -> Vec<u8> {
    let mut b = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
    let mut h = tar::Header::new_gnu();
    h.set_size(content.len() as u64);
    h.set_mode(0o755);
    h.set_entry_type(tar::EntryType::Regular);
    h.set_cksum();
    b.append_data(&mut h, path, content).unwrap();
    b.into_inner().unwrap().finish().unwrap()
}

/// A fake KingstVIS binary: FX2 image as Intel HEX, LA1010A2 and LA2016 bitstreams.
pub fn kingstvis() -> Vec<u8> {
    let (d, n, t) = rcc(&[
        (
            "fwusb",
            vec![("fw01A2", b":0300000002000AF1\n:00000001FF\n".to_vec())],
        ),
        (
            "fwfpga",
            vec![
                ("LA1010A2", b"BITSTREAM-A2".to_vec()),
                ("LA2016", b"OTHER".to_vec()),
            ],
        ),
    ]);
    elf(&d, &n, &t)
}
