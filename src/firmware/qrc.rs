//! Qt rcc resource tree (format version 1) -> (path, bytes).

use std::io::Read;

use flate2::read::ZlibDecoder;

use crate::error::{Error, Result};

const COMPRESSED: u16 = 0x01;
const DIR: u16 = 0x02;
const ENTRY: usize = 14;
const MAX_DEPTH: u32 = 16;

fn bad(m: &str) -> Error {
    Error::Firmware(format!("Qt resources: {m}"))
}

fn be16(b: &[u8], o: usize) -> Result<u16> {
    b.get(o..o + 2)
        .map(|s| u16::from_be_bytes([s[0], s[1]]))
        .ok_or_else(|| bad("truncated"))
}

fn be32(b: &[u8], o: usize) -> Result<u32> {
    b.get(o..o + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| bad("truncated"))
}

fn name_at(names: &[u8], o: usize) -> Result<String> {
    let len = usize::from(be16(names, o)?);
    let raw = names
        .get(o + 6..o + 6 + 2 * len)
        .ok_or_else(|| bad("truncated name"))?;
    let units: Vec<u16> = raw
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16(&units).map_err(|_| bad("bad name"))
}

fn blob_at(data: &[u8], o: usize) -> Result<&[u8]> {
    let len = be32(data, o)? as usize;
    data.get(o + 4..o + 4 + len)
        .ok_or_else(|| bad("truncated data"))
}

pub fn files(data: &[u8], names: &[u8], tree: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    if be16(tree, 4)? & DIR == 0 {
        return Err(bad("root is not a directory"));
    }
    let mut out = Vec::new();
    walk(&Res { data, names, tree }, 0, "", 0, &mut out)?;
    Ok(out)
}

struct Res<'a> {
    data: &'a [u8],
    names: &'a [u8],
    tree: &'a [u8],
}

fn walk(
    r: &Res,
    node: usize,
    prefix: &str,
    depth: u32,
    out: &mut Vec<(String, Vec<u8>)>,
) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(bad("tree too deep"));
    }
    let off = node * ENTRY;
    let count = be32(r.tree, off + 6)? as usize;
    let first = be32(r.tree, off + 10)? as usize;
    for child in first..first.saturating_add(count) {
        let c = child * ENTRY;
        let name = name_at(r.names, be32(r.tree, c)? as usize)?;
        let flags = be16(r.tree, c + 4)?;
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        if flags & DIR != 0 {
            walk(r, child, &path, depth + 1, out)?;
            continue;
        }
        let raw = blob_at(r.data, be32(r.tree, c + 10)? as usize)?;
        let bytes = if flags & COMPRESSED != 0 {
            let mut v = Vec::new();
            ZlibDecoder::new(raw.get(4..).ok_or_else(|| bad("truncated data"))?)
                .read_to_end(&mut v)
                .map_err(|_| bad("bad zlib data"))?;
            v
        } else {
            raw.to_vec()
        };
        out.push((path, bytes));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::firmware::fixture::rcc;

    #[test]
    fn walks_tree() {
        let (d, n, t) = rcc(&[
            ("fwusb", vec![("fw01A2", b"abc".to_vec())]),
            ("x", vec![("y", vec![1, 2])]),
        ]);
        assert_eq!(
            files(&d, &n, &t).unwrap(),
            vec![
                ("fwusb/fw01A2".to_string(), b"abc".to_vec()),
                ("x/y".to_string(), vec![1, 2])
            ]
        );
    }

    #[test]
    fn truncated_tree_is_error_not_panic() {
        let (d, n, t) = rcc(&[("fwusb", vec![("fw01A2", b"abc".to_vec())])]);
        assert!(files(&d, &n, &t[..20]).is_err());
        assert!(files(&d[..3], &n, &t).is_err());
    }
}
