//! Text extraction from attachments, with size limits.
//!
//! Plain formats are read here. PDF and word-processing files are converted
//! by macOS itself (PDFKit, textutil) in a separate process with a timeout,
//! so a hostile file is never parsed inside Timbre.

use crate::{html, mime};

/// Largest attachment Timbre will try to read as text.
const MAX_INPUT: usize = 25 * 1024 * 1024;

fn ext(name: &str) -> String {
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

fn decode_text(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.trim_start_matches('\u{FEFF}').to_string();
    }
    // Not UTF-8: Windows-1252 is the usual suspect for European mail.
    let (cow, _, _) = encoding_rs::WINDOWS_1252.decode(bytes);
    cow.into_owned()
}

pub fn to_text(name: &str, mime_type: &str, bytes: &[u8]) -> Result<String, String> {
    if bytes.len() > MAX_INPUT {
        return Err(format!(
            "This attachment is too large to read as text ({} MB). Use save_attachment.",
            bytes.len() / 1_048_576
        ));
    }
    let e = ext(name);
    let m = mime_type.to_ascii_lowercase();
    let texty = m.starts_with("text/")
        || matches!(
            m.as_str(),
            "application/json" | "application/xml" | "application/csv" | "application/x-yaml" | "application/yaml"
        )
        || matches!(
            e.as_str(),
            "txt" | "md" | "csv" | "tsv" | "json" | "xml" | "yaml" | "yml" | "ics" | "vcf" | "log" | "ini" | "toml"
        );
    if m == "text/html" || matches!(e.as_str(), "html" | "htm") {
        return Ok(html::to_text(&decode_text(bytes)).text);
    }
    if m == "message/rfc822" || e == "eml" {
        let p = mime::parse(bytes).ok_or("could not parse the attached message")?;
        let mut out = String::new();
        if let Some((n, a)) = p.from.first() {
            out.push_str(&format!("From: {}\n", crate::util::fmt_addr(Some(n), Some(a))));
        }
        out.push_str(&format!("Subject: {}\n\n{}", p.subject, p.body));
        return Ok(out);
    }
    // Before the plain-text case: RTF is often typed text/rtf.
    if let Some(kind) = native_kind(&e, &m) {
        return native::convert(kind, bytes).map(|t| crate::util::strip_invisible(&t).0);
    }
    if texty {
        let (clean, _) = crate::util::strip_invisible(&decode_text(bytes));
        return Ok(clean);
    }
    Err(format!(
        "Reading text from '{name}' ({mime_type}) is not supported: Timbre reads text, HTML, mail, PDF and \
         word-processing files (doc, docx, rtf, odt). Use save_attachment to save it to the attachments folder."
    ))
}

/// Formats macOS converts: PDF, or a word-processing format given by the
/// file extension textutil expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeKind {
    Pdf,
    Word(&'static str),
}

fn native_kind(ext: &str, mime_type: &str) -> Option<NativeKind> {
    if ext == "pdf" || mime_type == "application/pdf" {
        return Some(NativeKind::Pdf);
    }
    let word = match (ext, mime_type) {
        ("docx", _) | (_, "application/vnd.openxmlformats-officedocument.wordprocessingml.document") => "docx",
        ("doc", _) | (_, "application/msword") => "doc",
        ("rtf", _) | (_, "application/rtf") | (_, "text/rtf") => "rtf",
        ("odt", _) | (_, "application/vnd.oasis.opendocument.text") => "odt",
        ("wordml", _) => "wordml",
        _ => return None,
    };
    Some(NativeKind::Word(word))
}

#[cfg(not(target_os = "macos"))]
mod native {
    pub fn convert(_kind: super::NativeKind, _bytes: &[u8]) -> Result<String, String> {
        Err("PDF and word-processing attachments can only be read on macOS. Use save_attachment.".into())
    }
}

#[cfg(target_os = "macos")]
mod native {
    use std::io::{Read, Write};
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use super::NativeKind;

    /// PDFKit's text layer, via JavaScript for Automation (no Apple Events,
    /// so no permission prompt).
    const PDF_SCRIPT: &str = r#"function run(argv) {
  try { ObjC.import('PDFKit'); } catch (e) { ObjC.import('Quartz'); }
  const doc = $.PDFDocument.alloc.initWithURL($.NSURL.fileURLWithPath(argv[0]));
  if (doc.isNil()) return '\u0001unreadable';
  if (doc.isLocked) return '\u0001locked';
  const text = doc.string;
  return text.isNil() ? '' : text.js;
}"#;
    const TIMEOUT: Duration = Duration::from_secs(30);
    const MAX_OUTPUT: u64 = 8 * 1024 * 1024;

    /// A private directory removed with everything in it when dropped.
    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch() -> std::io::Result<Scratch> {
        use std::hash::{BuildHasher, Hasher};
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
        let dir = std::env::temp_dir().join(format!("timbre-{}-{:016x}", std::process::id(), h.finish()));
        std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
        Ok(Scratch(dir))
    }

    pub fn convert(kind: NativeKind, bytes: &[u8]) -> Result<String, String> {
        let dir = scratch().map_err(|e| format!("cannot create a temporary folder: {e}"))?;
        let ext = match kind {
            NativeKind::Pdf => "pdf",
            NativeKind::Word(ext) => ext,
        };
        let file = dir.0.join(format!("attachment.{ext}"));
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&file)
            .and_then(|mut f| f.write_all(bytes))
            .map_err(|e| format!("cannot write a temporary file: {e}"))?;
        let mut cmd = match kind {
            NativeKind::Pdf => {
                let mut c = Command::new("/usr/bin/osascript");
                c.args(["-l", "JavaScript", "-e", PDF_SCRIPT]).arg(&file);
                c
            }
            NativeKind::Word(_) => {
                let mut c = Command::new("/usr/bin/textutil");
                c.args(["-convert", "txt", "-encoding", "UTF-8", "-stdout"]).arg(&file);
                c
            }
        };
        let text = run(&mut cmd, &dir.0)?;
        match (kind, text.strip_prefix('\u{1}')) {
            (NativeKind::Pdf, Some(why)) if why.starts_with("locked") => {
                Err("This PDF is password-protected; Timbre cannot read it.".into())
            }
            (NativeKind::Pdf, Some(_)) => Err("macOS could not open this PDF (damaged or not a PDF).".into()),
            (NativeKind::Pdf, None) if text.trim().is_empty() => {
                Err("This PDF has no text layer (probably a scan). Use save_attachment to open it.".into())
            }
            _ => Ok(text.trim_end().to_string()),
        }
    }

    fn run(cmd: &mut Command, cwd: &Path) -> Result<String, String> {
        cmd.current_dir(cwd)
            .env("LANG", "en_US.UTF-8")
            .env("LC_ALL", "en_US.UTF-8")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = cmd.spawn().map_err(|e| format!("cannot start the converter: {e}"))?;
        let mut stdout = child.stdout.take().ok_or("no converter output")?;
        let reader = std::thread::spawn(move || {
            let mut out = Vec::new();
            let _ = (&mut stdout).take(MAX_OUTPUT).read_to_end(&mut out);
            // Keep draining: a converter killed by a closed pipe would turn
            // a long document into an error instead of a truncated text.
            let _ = std::io::copy(&mut stdout, &mut std::io::sink());
            out
        });
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if started.elapsed() < TIMEOUT => std::thread::sleep(Duration::from_millis(25)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
            }
        };
        let out = reader.join().unwrap_or_default();
        match status {
            None => Err("Converting this attachment took too long; use save_attachment instead.".into()),
            Some(s) if !s.success() => Err("macOS could not convert this attachment; use save_attachment.".into()),
            Some(_) => Ok(String::from_utf8_lossy(&out).into_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converter_is_chosen_by_extension_or_type() {
        assert_eq!(native_kind("pdf", ""), Some(NativeKind::Pdf));
        assert_eq!(native_kind("", "application/pdf"), Some(NativeKind::Pdf));
        assert_eq!(native_kind("docx", "application/octet-stream"), Some(NativeKind::Word("docx")));
        assert_eq!(native_kind("", "application/msword"), Some(NativeKind::Word("doc")));
        assert_eq!(native_kind("exe", "application/octet-stream"), None);
    }

    #[test]
    fn rtf_typed_as_text_goes_to_the_converter() {
        let r = to_text("note.rtf", "text/rtf", br"{\rtf1\ansi Bonjour}");
        if cfg!(target_os = "macos") {
            assert_eq!(r.unwrap().trim(), "Bonjour");
        } else {
            assert!(r.unwrap_err().contains("only be read on macOS"));
        }
    }

    #[test]
    fn plain_formats_are_read_here() {
        assert_eq!(to_text("a.csv", "text/csv", b"x;y\n1;2").unwrap(), "x;y\n1;2");
        assert!(to_text("a.bin", "application/octet-stream", b"\0\0").unwrap_err().contains("not supported"));
    }

    /// A one-page PDF showing `text` in Helvetica, with a correct xref.
    #[cfg(target_os = "macos")]
    fn tiny_pdf(text: &str) -> Vec<u8> {
        let stream = format!("BT /F1 18 Tf 20 60 Td ({text}) Tj ET");
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 144] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>"
                .to_string(),
            format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        ];
        let mut out = String::from("%PDF-1.4\n");
        let mut offsets = vec![];
        for (i, o) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.push_str(&format!("{} 0 obj\n{o}\nendobj\n", i + 1));
        }
        let xref = out.len();
        out.push_str(&format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1));
        for off in offsets {
            out.push_str(&format!("{off:010} 00000 n \n"));
        }
        out.push_str(&format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1));
        out.into_bytes()
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_converts_rtf_and_pdf() {
        let rtf = to_text("note.rtf", "application/rtf", br"{\rtf1\ansi Bonjour Timbre \'e9t\'e9}").unwrap();
        assert!(rtf.contains("Bonjour Timbre été"), "{rtf}");
        let pdf = to_text("facture.pdf", "application/pdf", &tiny_pdf("Hello Timbre")).unwrap();
        assert!(pdf.contains("Hello Timbre"), "{pdf}");
        let err = to_text("scan.pdf", "application/pdf", b"not a pdf").unwrap_err();
        assert!(err.contains("could not open"), "{err}");
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn other_platforms_explain_pdf() {
        let err = to_text("facture.pdf", "application/pdf", b"%PDF-1.4").unwrap_err();
        assert!(err.contains("only be read on macOS"), "{err}");
    }
}
