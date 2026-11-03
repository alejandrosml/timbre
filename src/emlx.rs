//! Apple Mail's on-disk message files.
//!
//! An `.emlx` file is `<byte count>\n<RFC 5322 message of that many bytes><XML plist>`.
//! `.partial.emlx` files have their attachment bodies stripped; the bodies
//! live under `…/<mailbox>.mbox/<uuid>/Data/<x>/<y>/Attachments/<id>/<part>/<name>`.
//! Messages are stored under `Data/<d1>/<d2>/…/Messages/<id>.emlx`, where the
//! directories are the decimal digits of `id / 1000`, least significant first.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use walkdir::WalkDir;

/// Read the RFC 5322 message bytes out of an `.emlx` file.
pub fn read_message(path: &Path) -> Result<Vec<u8>> {
    let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    extract_message(data).with_context(|| format!("parsing {}", path.display()))
}

/// Strip the length line and the trailing plist from raw `.emlx` bytes.
pub fn extract_message(mut data: Vec<u8>) -> Result<Vec<u8>> {
    let Some(nl) = data.iter().take(32).position(|b| *b == b'\n') else {
        bail!("not an emlx file (no length line)");
    };
    let len: usize = std::str::from_utf8(&data[..nl]).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(usize::MAX);
    if len == usize::MAX {
        bail!("not an emlx file (bad length line)");
    }
    let start = nl + 1;
    let end = start.saturating_add(len).min(data.len());
    data.truncate(end);
    data.drain(..start);
    Ok(data)
}

/// Read only the header block (cheap: stops at the first blank line).
pub fn read_headers(path: &Path) -> Result<Vec<u8>> {
    let mut f = std::fs::File::open(path)?;
    let mut buf = Vec::with_capacity(16 * 1024);
    let mut chunk = [0u8; 16 * 1024];
    loop {
        let n = f.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(end) = find_header_end(&buf) {
            buf.truncate(end);
            break;
        }
        if buf.len() > 512 * 1024 {
            break;
        }
    }
    let nl = buf.iter().position(|b| *b == b'\n').unwrap_or(0);
    Ok(buf[(nl + 1).min(buf.len())..].to_vec())
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    let first_nl = buf.iter().position(|b| *b == b'\n')?;
    let body = &buf[first_nl + 1..];
    body.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| first_nl + 1 + p + 4)
        .or_else(|| body.windows(2).position(|w| w == b"\n\n").map(|p| first_nl + 1 + p + 2))
}

/// The `Data/…` subdirectory for a message id.
pub fn data_subdirs(id: i64) -> PathBuf {
    let mut p = PathBuf::new();
    let mut n = id / 1000;
    while n > 0 {
        p.push((n % 10).to_string());
        n /= 10;
    }
    p
}

/// Message id encoded in an emlx file name (`123.emlx`, `123.partial.emlx`).
pub fn id_from_name(name: &str) -> Option<(i64, bool)> {
    let partial = name.ends_with(".partial.emlx");
    if !partial && !name.ends_with(".emlx") {
        return None;
    }
    let stem = name.split('.').next()?;
    stem.parse::<i64>().ok().map(|id| (id, partial))
}

/// Location of one message file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub path: PathBuf,
    pub partial: bool,
}

/// Walk a Mail version directory and map message ids to their files.
/// A full `.emlx` wins over a `.partial.emlx` for the same id.
pub fn scan(root: &Path) -> HashMap<i64, Located> {
    let mut map: HashMap<i64, Located> = HashMap::with_capacity(1 << 16);
    for entry in WalkDir::new(root).follow_links(false).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        let Some((id, partial)) = id_from_name(&name) else { continue };
        // Only message stores: …/Messages/<id>.emlx
        if entry.path().parent().and_then(|p| p.file_name()).is_none_or(|n| n != "Messages") {
            continue;
        }
        let new = Located { path: entry.into_path(), partial };
        match map.get(&id) {
            Some(existing) if !existing.partial => {}
            _ => {
                map.insert(id, new);
            }
        }
    }
    map
}

/// The `Data` directory of the store holding `path`, if any.
pub fn data_root(path: &Path) -> Option<PathBuf> {
    path.ancestors().find(|a| a.file_name().is_some_and(|n| n == "Data")).map(Path::to_path_buf)
}

/// Predict where a message of `id` lives inside a store's `Data` dir.
pub fn predict(data_root: &Path, id: i64) -> Option<Located> {
    let dir = data_root.join(data_subdirs(id)).join("Messages");
    let full = dir.join(format!("{id}.emlx"));
    if full.is_file() {
        return Some(Located { path: full, partial: false });
    }
    let partial = dir.join(format!("{id}.partial.emlx"));
    partial.is_file().then_some(Located { path: partial, partial: true })
}

/// Directory holding the stripped attachment bodies of a partial message.
pub fn attachments_dir(emlx: &Path, id: i64) -> Option<PathBuf> {
    let d = emlx.parent()?.parent()?.join("Attachments").join(id.to_string());
    d.is_dir().then_some(d)
}

/// Find a stripped attachment body for a partial message by file name.
pub fn find_external_attachment(emlx: &Path, id: i64, name: &str) -> Option<PathBuf> {
    let dir = attachments_dir(emlx, id)?;
    let files: Vec<PathBuf> = WalkDir::new(&dir)
        .max_depth(3)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .collect();
    files
        .iter()
        .find(|p| p.file_name().is_some_and(|n| n.to_string_lossy() == name))
        .cloned()
        .or_else(|| (files.len() == 1).then(|| files[0].clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subdirs_scheme() {
        assert_eq!(data_subdirs(283_766), PathBuf::from("3/8/2"));
        assert_eq!(data_subdirs(999), PathBuf::new());
        assert_eq!(data_subdirs(1_000), PathBuf::from("1"));
        assert_eq!(data_subdirs(12_345), PathBuf::from("2/1"));
    }

    #[test]
    fn names() {
        assert_eq!(id_from_name("123.emlx"), Some((123, false)));
        assert_eq!(id_from_name("123.partial.emlx"), Some((123, true)));
        assert_eq!(id_from_name("x.emlx"), None);
        assert_eq!(id_from_name("123.plist"), None);
    }

    #[test]
    fn extracts_message() {
        let raw = b"13\nSubject: hi\n\n<?xml version=\"1.0\"?><plist/>".to_vec();
        assert_eq!(extract_message(raw).unwrap(), b"Subject: hi\n\n");
    }
}
