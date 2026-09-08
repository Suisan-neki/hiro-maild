use std::io::{Cursor, Read};
use std::path::Path;

use anyhow::{Context, Result};
use quick_xml::{events::Event, reader::Reader, XmlVersion};
use zip::ZipArchive;

const MAX_EXTRACTED_TEXT: usize = 200_000;
const MAX_EXTRACT_INPUT_BYTES: usize = 32 * 1024 * 1024;
const MAX_XML_BYTES_PER_PART: u64 = 4 * 1024 * 1024;

pub fn extract_text(filename: &str, bytes: &[u8]) -> Option<String> {
    if bytes.len() > MAX_EXTRACT_INPUT_BYTES {
        return None;
    }

    let ext = Path::new(filename)
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    let result = match ext.as_str() {
        "txt" | "md" | "csv" | "tsv" | "ics" | "json" | "xml" | "html" | "htm" => {
            Some(String::from_utf8_lossy(bytes).into_owned())
        }
        "pdf" => pdf_extract::extract_text_from_mem(bytes).ok(),
        "docx" => extract_ooxml(bytes, OoxmlKind::Docx).ok(),
        "pptx" => extract_ooxml(bytes, OoxmlKind::Pptx).ok(),
        "xlsx" => extract_ooxml(bytes, OoxmlKind::Xlsx).ok(),
        _ => None,
    }?;

    let trimmed = result.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(truncate_chars(trimmed, MAX_EXTRACTED_TEXT))
    }
}

#[derive(Clone, Copy)]
enum OoxmlKind {
    Docx,
    Pptx,
    Xlsx,
}

fn extract_ooxml(bytes: &[u8], kind: OoxmlKind) -> Result<String> {
    let mut archive = ZipArchive::new(Cursor::new(bytes)).context("invalid OOXML zip")?;
    let mut names = Vec::new();
    for i in 0..archive.len() {
        let file = archive.by_index(i)?;
        let name = file.name().to_string();
        let include = match kind {
            OoxmlKind::Docx => {
                name == "word/document.xml"
                    || name.starts_with("word/header")
                    || name.starts_with("word/footer")
                    || name == "word/footnotes.xml"
                    || name == "word/endnotes.xml"
            }
            OoxmlKind::Pptx => name.starts_with("ppt/slides/slide") && name.ends_with(".xml"),
            OoxmlKind::Xlsx => {
                name == "xl/sharedStrings.xml"
                    || (name.starts_with("xl/worksheets/sheet") && name.ends_with(".xml"))
            }
        };
        if include {
            names.push(name);
        }
    }
    names.sort();

    let mut out = String::new();
    for name in names {
        let mut file = archive.by_name(&name)?;
        let mut xml = Vec::new();
        file.take(MAX_XML_BYTES_PER_PART).read_to_end(&mut xml)?;
        let text = xml_text(&xml)?;
        if !text.trim().is_empty() {
            out.push_str(&text);
            out.push('\n');
        }
        if out.len() >= MAX_EXTRACTED_TEXT {
            break;
        }
    }
    Ok(truncate_chars(out.trim(), MAX_EXTRACTED_TEXT))
}

fn xml_text(xml: &[u8]) -> Result<String> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut out = String::new();

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Text(text) => {
                let text = text.xml_content(XmlVersion::Implicit1_0);
                let text = text.trim();
                if !text.is_empty() {
                    if !out.ends_with(' ') && !out.ends_with('\n') && !out.is_empty() {
                        out.push(' ');
                    }
                    out.push_str(text);
                }
            }
            Event::End(end) => {
                let name = end.name();
                let local = name.as_ref();
                if (local == b"w:p" || local == b"a:p" || local == b"row" || local == b"sheetData")
                    && !out.ends_with('\n')
                {
                    out.push('\n');
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    Ok(out)
}

pub fn sanitize_filename(input: &str) -> String {
    let basename = input
        .rsplit(|ch| ch == '/' || ch == '\\')
        .next()
        .unwrap_or("attachment")
        .trim();

    let mut out = String::with_capacity(basename.len());
    for ch in basename.chars() {
        if ch.is_control() || matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
            out.push('_');
        } else {
            out.push(ch);
        }
    }

    let out = out.trim_matches(|ch| ch == '.' || ch == ' ');
    if out.is_empty() || out == "." || out == ".." {
        "attachment.bin".to_string()
    } else {
        truncate_chars(out, 160)
    }
}

pub fn truncate_chars(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        input.to_string()
    } else {
        input.chars().take(max_chars).collect::<String>() + "\n[truncated]"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_is_contained() {
        assert_eq!(sanitize_filename("../../secret.txt"), "secret.txt");
        assert_eq!(sanitize_filename("a/b\\c?.pdf"), "c_.pdf");
    }
}
