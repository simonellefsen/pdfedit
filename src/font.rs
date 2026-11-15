//! Font decoding/encoding: maps character codes in content streams to Unicode
//! and back, and provides glyph advance widths.

use std::collections::HashMap;

use lopdf::{Dictionary, Document, Object};

/// Everything the editor needs to know about one font resource.
#[derive(Debug, Clone, Default)]
pub struct FontInfo {
    /// `/BaseFont` name without the subset prefix (`ABCDEF+`).
    pub base_name: String,
    /// Composite (Type0) fonts use two-byte character codes.
    pub two_byte: bool,
    to_unicode: HashMap<u32, String>,
    from_unicode: HashMap<char, u32>,
    widths: HashMap<u32, f32>,
    default_width: Option<f32>,
    monospace: bool,
}

impl FontInfo {
    /// Build font information from a font dictionary.
    pub fn from_dict(doc: &Document, dict: &Dictionary) -> Self {
        let mut info = FontInfo::default();
        let base = dict
            .get_deref(b"BaseFont", doc)
            .and_then(Object::as_name)
            .map(|n| String::from_utf8_lossy(n).into_owned())
            .unwrap_or_default();
        info.base_name = match base.split_once('+') {
            Some((prefix, rest)) if prefix.len() == 6 => rest.to_string(),
            _ => base,
        };
        info.monospace = {
            let lower = info.base_name.to_lowercase();
            lower.contains("courier") || lower.contains("mono")
        };
        let subtype = dict
            .get_deref(b"Subtype", doc)
            .and_then(Object::as_name)
            .unwrap_or(b"");
        info.two_byte = subtype == b"Type0";

        if let Ok(stream) = dict
            .get_deref(b"ToUnicode", doc)
            .and_then(Object::as_stream)
        {
            let data = stream
                .decompressed_content()
                .unwrap_or_else(|_| stream.content.clone());
            info.to_unicode = parse_to_unicode(&data);
        }

        if info.two_byte {
            info.load_cid_widths(doc, dict);
        } else {
            info.load_simple(doc, dict);
        }

        for (code, text) in &info.to_unicode {
            let mut chars = text.chars();
            if let (Some(c), None) = (chars.next(), chars.next()) {
                let entry = info.from_unicode.entry(c).or_insert(*code);
                if *code < *entry {
                    *entry = *code;
                }
            }
        }
        info
    }

    /// A standard Helvetica-like font with WinAnsi encoding (used for the
    /// fallback font the editor adds when a subset font lacks glyphs).
    pub fn win_ansi(base_name: &str) -> Self {
        let mut info = FontInfo {
            base_name: base_name.to_string(),
            monospace: base_name.starts_with("Courier"),
            ..Default::default()
        };
        for b in 32u32..=255 {
            if let Some(c) = win_ansi_decode(b as u8) {
                info.to_unicode.insert(b, c.to_string());
                info.from_unicode.entry(c).or_insert(b);
            }
        }
        info
    }

    fn load_simple(&mut self, doc: &Document, dict: &Dictionary) {
        if let Ok(encoding) = dict.get_font_encoding(doc) {
            for b in 0u32..=255 {
                if self.to_unicode.contains_key(&b) {
                    continue;
                }
                if let Ok(s) = encoding.bytes_to_string(&[b as u8])
                    && !s.is_empty()
                    && !s.contains('\u{FFFD}')
                {
                    self.to_unicode.insert(b, s);
                }
            }
        }
        let first = dict
            .get_deref(b"FirstChar", doc)
            .and_then(Object::as_i64)
            .unwrap_or(0);
        if let Ok(widths) = dict.get_deref(b"Widths", doc).and_then(Object::as_array) {
            for (i, w) in widths.iter().enumerate() {
                if let Some(w) = number(doc, w) {
                    self.widths.insert((first + i as i64) as u32, w);
                }
            }
            self.default_width = dict
                .get_deref(b"FontDescriptor", doc)
                .and_then(Object::as_dict)
                .ok()
                .and_then(|d| d.get_deref(b"MissingWidth", doc).ok())
                .and_then(|o| number(doc, o))
                .or(Some(0.0));
        }
    }

    fn load_cid_widths(&mut self, doc: &Document, dict: &Dictionary) {
        let descendant = dict
            .get_deref(b"DescendantFonts", doc)
            .and_then(Object::as_array)
            .ok()
            .and_then(|a| a.first())
            .and_then(|o| doc.dereference(o).ok())
            .and_then(|(_, o)| o.as_dict().ok());
        let Some(desc) = descendant else {
            self.default_width = Some(1000.0);
            return;
        };
        self.default_width = Some(
            desc.get_deref(b"DW", doc)
                .ok()
                .and_then(|o| number(doc, o))
                .unwrap_or(1000.0),
        );
        let Ok(w) = desc.get_deref(b"W", doc).and_then(Object::as_array) else {
            return;
        };
        let mut i = 0;
        while i < w.len() {
            let Some(start) = number(doc, &w[i]) else {
                break;
            };
            let start = start as u32;
            let Some(next) = w.get(i + 1) else { break };
            let next = doc.dereference(next).map(|(_, o)| o).unwrap_or(next);
            if let Ok(list) = next.as_array() {
                for (k, item) in list.iter().enumerate() {
                    if let Some(width) = number(doc, item) {
                        self.widths.insert(start + k as u32, width);
                    }
                }
                i += 2;
            } else {
                let (Some(end), Some(width)) =
                    (number(doc, next), w.get(i + 2).and_then(|o| number(doc, o)))
                else {
                    break;
                };
                let end = (end as u32).min(start.saturating_add(65535));
                for cid in start..=end {
                    self.widths.insert(cid, width);
                }
                i += 3;
            }
        }
    }

    /// Split a content-stream string into character codes.
    pub fn codes(&self, bytes: &[u8]) -> Vec<u32> {
        if self.two_byte {
            bytes
                .chunks(2)
                .map(|c| {
                    if c.len() == 2 {
                        u32::from(c[0]) << 8 | u32::from(c[1])
                    } else {
                        u32::from(c[0])
                    }
                })
                .collect()
        } else {
            bytes.iter().map(|b| u32::from(*b)).collect()
        }
    }

    /// Unicode text for a character code, if known.
    pub fn decode(&self, code: u32) -> Option<&str> {
        self.to_unicode.get(&code).map(String::as_str)
    }

    /// Advance width of a character code in 1/1000 text-space units.
    pub fn width(&self, code: u32) -> f32 {
        if let Some(w) = self.widths.get(&code) {
            return *w;
        }
        if let Some(w) = self.default_width {
            return w;
        }
        if self.monospace {
            return 600.0;
        }
        self.decode(code)
            .and_then(|s| s.chars().next())
            .map(helvetica_width)
            .unwrap_or(556.0)
    }

    /// Whether this font can show the given character.
    pub fn can_encode(&self, c: char) -> bool {
        self.from_unicode.contains_key(&c)
    }

    /// Encode text into content-stream string bytes. Characters without a
    /// glyph are returned as the error.
    pub fn encode(&self, text: &str) -> Result<Vec<u8>, Vec<char>> {
        let mut out = Vec::new();
        let mut missing = Vec::new();
        for c in text.chars() {
            match self.from_unicode.get(&c) {
                Some(code) if self.two_byte => out.extend_from_slice(&(*code as u16).to_be_bytes()),
                Some(code) => out.push(*code as u8),
                None => {
                    if !missing.contains(&c) {
                        missing.push(c);
                    }
                }
            }
        }
        if missing.is_empty() {
            Ok(out)
        } else {
            Err(missing)
        }
    }
}

fn number(doc: &Document, obj: &Object) -> Option<f32> {
    let obj = doc.dereference(obj).map(|(_, o)| o).unwrap_or(obj);
    match obj {
        Object::Integer(i) => Some(*i as f32),
        Object::Real(r) => Some(*r),
        _ => None,
    }
}

/// Parse the `bfchar` / `bfrange` sections of a ToUnicode CMap.
pub fn parse_to_unicode(data: &[u8]) -> HashMap<u32, String> {
    #[derive(Debug, PartialEq)]
    enum Tok {
        Hex(Vec<u8>),
        Open,
        Close,
        Word(Vec<u8>),
    }

    let mut toks = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let b = data[i];
        match b {
            b if b.is_ascii_whitespace() => i += 1,
            b'%' => {
                while i < data.len() && data[i] != b'\n' && data[i] != b'\r' {
                    i += 1;
                }
            }
            b'<' if data.get(i + 1) == Some(&b'<') => i += 2,
            b'>' => i += 1,
            b'<' => {
                i += 1;
                let mut digits = Vec::new();
                while i < data.len() && data[i] != b'>' {
                    if data[i].is_ascii_hexdigit() {
                        digits.push(data[i]);
                    }
                    i += 1;
                }
                i += 1;
                if digits.len() % 2 == 1 {
                    digits.push(b'0');
                }
                let bytes = digits
                    .chunks(2)
                    .filter_map(|p| u8::from_str_radix(std::str::from_utf8(p).ok()?, 16).ok())
                    .collect();
                toks.push(Tok::Hex(bytes));
            }
            b'[' => {
                toks.push(Tok::Open);
                i += 1;
            }
            b']' => {
                toks.push(Tok::Close);
                i += 1;
            }
            b'(' => {
                let mut depth = 0;
                while i < data.len() {
                    match data[i] {
                        b'\\' => i += 1,
                        b'(' => depth += 1,
                        b')' => depth -= 1,
                        _ => {}
                    }
                    i += 1;
                    if depth == 0 {
                        break;
                    }
                }
            }
            _ => {
                let start = i;
                i += 1;
                while i < data.len()
                    && !data[i].is_ascii_whitespace()
                    && !b"<>[]()/%".contains(&data[i])
                {
                    i += 1;
                }
                toks.push(Tok::Word(data[start..i].to_vec()));
            }
        }
    }

    fn code(bytes: &[u8]) -> u32 {
        bytes.iter().fold(0u32, |acc, b| acc << 8 | u32::from(*b))
    }
    fn utf16(bytes: &[u8]) -> Vec<u16> {
        bytes
            .chunks(2)
            .map(|c| {
                if c.len() == 2 {
                    u16::from(c[0]) << 8 | u16::from(c[1])
                } else {
                    u16::from(c[0])
                }
            })
            .collect()
    }
    fn insert(map: &mut HashMap<u32, String>, code: u32, units: &[u16]) {
        let s = String::from_utf16_lossy(units);
        if !s.is_empty() && s != "\u{0}" {
            map.insert(code, s);
        }
    }

    let mut map = HashMap::new();
    let mut t = 0;
    while t < toks.len() {
        match &toks[t] {
            Tok::Word(w) if w == b"beginbfchar" => {
                t += 1;
                while t + 1 < toks.len() {
                    let (Tok::Hex(src), Tok::Hex(dst)) = (&toks[t], &toks[t + 1]) else {
                        break;
                    };
                    insert(&mut map, code(src), &utf16(dst));
                    t += 2;
                }
            }
            Tok::Word(w) if w == b"beginbfrange" => {
                t += 1;
                while t + 2 < toks.len() {
                    let (Tok::Hex(lo), Tok::Hex(hi)) = (&toks[t], &toks[t + 1]) else {
                        break;
                    };
                    let (lo, hi) = (code(lo), code(hi).min(code(lo).saturating_add(65535)));
                    match &toks[t + 2] {
                        Tok::Hex(dst) => {
                            let base = utf16(dst);
                            for (k, c) in (lo..=hi).enumerate() {
                                let mut units = base.clone();
                                if let Some(last) = units.last_mut() {
                                    *last = last.wrapping_add(k as u16);
                                }
                                insert(&mut map, c, &units);
                            }
                            t += 3;
                        }
                        Tok::Open => {
                            t += 3;
                            let mut c = lo;
                            while t < toks.len() && toks[t] != Tok::Close {
                                if let Tok::Hex(dst) = &toks[t] {
                                    insert(&mut map, c, &utf16(dst));
                                    c += 1;
                                }
                                t += 1;
                            }
                            t += 1;
                        }
                        _ => break,
                    }
                }
            }
            _ => t += 1,
        }
    }
    map
}

/// Decode one WinAnsi (Windows-1252) byte.
pub fn win_ansi_decode(b: u8) -> Option<char> {
    const HIGH: [char; 32] = [
        '\u{20AC}', '\0', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
        '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\0', '\u{017D}', '\0', '\0',
        '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
        '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}', '\0', '\u{017E}', '\u{0178}',
    ];
    match b {
        0x20..=0x7E | 0xA0..=0xFF => Some(b as char),
        0x80..=0x9F => Some(HIGH[(b - 0x80) as usize]).filter(|c| *c != '\0'),
        _ => None,
    }
}

/// Encode text as WinAnsi, replacing unsupported characters with `?`.
pub fn win_ansi_encode(text: &str) -> Vec<u8> {
    text.chars()
        .map(|c| {
            (0x20u8..=0xFF)
                .find(|b| win_ansi_decode(*b) == Some(c))
                .unwrap_or(b'?')
        })
        .collect()
}

/// Helvetica advance width for a character (1/1000 em).
pub fn helvetica_width(c: char) -> f32 {
    const ASCII: [u16; 95] = [
        278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556,
        556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722,
        722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722,
        667, 944, 667, 667, 611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556, 556, 278, 556,
        556, 222, 222, 500, 222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500,
        500, 334, 260, 334, 584,
    ];
    let code = c as u32;
    if (32..127).contains(&code) {
        f32::from(ASCII[(code - 32) as usize])
    } else {
        556.0
    }
}

/// Pick the standard-14 font that best matches a font name.
pub fn standard_fallback(base_name: &str) -> &'static str {
    let n = base_name.to_lowercase();
    let bold = n.contains("bold") || n.contains("black") || n.contains("heavy");
    let italic = n.contains("italic") || n.contains("oblique");
    let mono = n.contains("courier") || n.contains("mono") || n.contains("consol");
    let serif = n.contains("times")
        || n.contains("serif") && !n.contains("sans")
        || n.contains("roman")
        || n.contains("georgia")
        || n.contains("garamond")
        || n.contains("minion")
        || n.contains("cambria");
    match (mono, serif, bold, italic) {
        (true, _, false, false) => "Courier",
        (true, _, true, false) => "Courier-Bold",
        (true, _, false, true) => "Courier-Oblique",
        (true, _, true, true) => "Courier-BoldOblique",
        (_, true, false, false) => "Times-Roman",
        (_, true, true, false) => "Times-Bold",
        (_, true, false, true) => "Times-Italic",
        (_, true, true, true) => "Times-BoldItalic",
        (_, _, false, false) => "Helvetica",
        (_, _, true, false) => "Helvetica-Bold",
        (_, _, false, true) => "Helvetica-Oblique",
        (_, _, true, true) => "Helvetica-BoldOblique",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bfchar_and_bfrange() {
        let cmap = b"1 beginbfchar <0003> <0020> endbfchar\n\
            2 beginbfrange <0024> <0026> <0041> <0030> <0031> [<0061> <00660069>] endbfrange";
        let map = parse_to_unicode(cmap);
        assert_eq!(map[&3], " ");
        assert_eq!(map[&0x24], "A");
        assert_eq!(map[&0x26], "C");
        assert_eq!(map[&0x30], "a");
        assert_eq!(map[&0x31], "fi");
    }

    #[test]
    fn win_ansi_round_trip() {
        assert_eq!(win_ansi_encode("Aé€"), vec![0x41, 0xE9, 0x80]);
        assert_eq!(win_ansi_encode("日"), vec![b'?']);
    }

    #[test]
    fn fallback_font_matches_style() {
        assert_eq!(standard_fallback("Arial-BoldMT"), "Helvetica-Bold");
        assert_eq!(
            standard_fallback("TimesNewRomanPS-ItalicMT"),
            "Times-Italic"
        );
        assert_eq!(standard_fallback("Menlo-Regular"), "Helvetica");
        assert_eq!(standard_fallback("CourierNewPSMT"), "Courier");
    }
}
