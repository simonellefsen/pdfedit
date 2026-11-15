//! Markdown export: rebuilds lines, paragraphs and headings from text runs,
//! optionally extracting images next to the Markdown file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use lopdf::{Object, ObjectId, Stream};

use crate::doc::PdfDoc;

/// Options for Markdown export.
#[derive(Debug, Clone, Default)]
pub struct MarkdownOptions {
    /// Directory to write extracted images into. `None` exports text only.
    pub image_dir: Option<PathBuf>,
    /// Prefix used in image links (usually the image directory's name).
    pub image_link_prefix: String,
}

struct Piece {
    x0: f32,
    x1: f32,
    y: f32,
    size: f32,
    bold: bool,
    text: String,
}

struct Line {
    y: f32,
    size: f32,
    bold: bool,
    text: String,
}

enum Block {
    Line(Line),
    Image { y: f32, link: String },
}

impl Block {
    fn y(&self) -> f32 {
        match self {
            Block::Line(l) => l.y,
            Block::Image { y, .. } => *y,
        }
    }
}

fn page_lines(doc: &mut PdfDoc, page: usize) -> Vec<Line> {
    let geom = doc.geom(page);
    let scan = doc.scan(page);
    let mut pieces: Vec<Piece> = scan
        .runs
        .iter()
        .map(|run| {
            let a = geom.to_view(run.baseline[0]);
            let b = geom.to_view(run.baseline[1]);
            Piece {
                x0: a[0].min(b[0]),
                x1: a[0].max(b[0]),
                y: a[1],
                size: run.size.max(1.0),
                bold: run.font_base.to_lowercase().contains("bold"),
                text: run.text.clone(),
            }
        })
        .collect();
    pieces.sort_by(|a, b| a.y.total_cmp(&b.y).then(a.x0.total_cmp(&b.x0)));

    let mut rows: Vec<Vec<Piece>> = Vec::new();
    for piece in pieces {
        match rows.last_mut() {
            Some(row) if (piece.y - row[0].y).abs() < 0.45 * row[0].size.max(piece.size) => {
                row.push(piece);
            }
            _ => rows.push(vec![piece]),
        }
    }

    rows.into_iter()
        .map(|mut row| {
            row.sort_by(|a, b| a.x0.total_cmp(&b.x0));
            let mut text = String::new();
            let mut end = f32::MIN;
            for piece in &row {
                let gap = piece.x0 - end;
                if !text.is_empty()
                    && gap > 0.18 * piece.size
                    && !text.ends_with(' ')
                    && !piece.text.starts_with(' ')
                {
                    text.push(' ');
                }
                text.push_str(&piece.text);
                end = piece.x1;
            }
            let chars: usize = row.iter().map(|p| p.text.len()).sum();
            let weighted: f32 = row.iter().map(|p| p.size * p.text.len() as f32).sum();
            Line {
                y: row[0].y,
                size: weighted / chars.max(1) as f32,
                bold: row.iter().all(|p| p.bold),
                text: text.split_whitespace().collect::<Vec<_>>().join(" "),
            }
        })
        .filter(|line| !line.text.is_empty())
        .collect()
}

fn heading_level(size: f32, body: f32) -> usize {
    let ratio = size / body;
    if ratio >= 1.8 {
        1
    } else if ratio >= 1.4 {
        2
    } else if ratio >= 1.15 {
        3
    } else {
        0
    }
}

fn bullet(text: &str) -> Option<&str> {
    for marker in ["• ", "▪ ", "◦ ", "● ", "– ", "- ", "· "] {
        if let Some(rest) = text.strip_prefix(marker) {
            return Some(rest);
        }
    }
    None
}

/// Export the whole document as Markdown.
pub fn export(doc: &mut PdfDoc, options: &MarkdownOptions) -> Result<String> {
    let pages: Vec<Vec<Line>> = (0..doc.page_count()).map(|p| page_lines(doc, p)).collect();

    // The body size is the one most text is set in.
    let mut histogram: HashMap<i32, usize> = HashMap::new();
    for line in pages.iter().flatten() {
        *histogram
            .entry((line.size * 2.0).round() as i32)
            .or_default() += line.text.len();
    }
    let body = histogram
        .into_iter()
        .max_by_key(|(size, count)| (*count, -*size))
        .map(|(size, _)| size as f32 / 2.0)
        .unwrap_or(12.0);

    if let Some(dir) = &options.image_dir {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let mut saved: HashMap<ObjectId, Option<String>> = HashMap::new();

    let mut out = String::new();
    for (page, lines) in pages.into_iter().enumerate() {
        let mut blocks: Vec<Block> = lines.into_iter().map(Block::Line).collect();
        if let Some(dir) = &options.image_dir {
            for (y, link) in page_images(doc, page, dir, &options.image_link_prefix, &mut saved) {
                blocks.push(Block::Image { y, link });
            }
        }
        blocks.sort_by(|a, b| a.y().total_cmp(&b.y()));

        if page > 0 {
            out.push_str(&format!("<!-- page {} -->\n\n", page + 1));
        }
        let mut paragraph = String::new();
        let mut prev: Option<(f32, f32)> = None;
        let flush = |paragraph: &mut String, out: &mut String| {
            if !paragraph.is_empty() {
                out.push_str(paragraph.trim_end());
                out.push_str("\n\n");
                paragraph.clear();
            }
        };
        for block in blocks {
            let line = match block {
                Block::Image { link, .. } => {
                    flush(&mut paragraph, &mut out);
                    out.push_str(&format!("![]({link})\n\n"));
                    prev = None;
                    continue;
                }
                Block::Line(line) => line,
            };
            let level = heading_level(line.size, body);
            if level > 0 && line.text.len() < 150 {
                flush(&mut paragraph, &mut out);
                out.push_str(&format!("{} {}\n\n", "#".repeat(level), line.text));
                prev = None;
                continue;
            }
            if let Some(item) = bullet(&line.text) {
                flush(&mut paragraph, &mut out);
                paragraph.push_str(&format!("- {item}"));
                prev = Some((line.y, line.size));
                continue;
            }
            let new_paragraph = match prev {
                Some((y, size)) => line.y - y > 1.7 * size.max(line.size),
                None => true,
            };
            if new_paragraph {
                flush(&mut paragraph, &mut out);
            }
            let text = if line.bold && new_paragraph && line.text.len() < 100 {
                format!("**{}**", line.text)
            } else {
                line.text
            };
            if paragraph.is_empty() {
                paragraph = text;
            } else if paragraph.ends_with('-')
                && text.chars().next().is_some_and(char::is_lowercase)
            {
                paragraph.pop();
                paragraph.push_str(&text);
            } else {
                paragraph.push(' ');
                paragraph.push_str(&text);
            }
            prev = Some((line.y, line.size));
        }
        flush(&mut paragraph, &mut out);
    }
    Ok(out.trim_end().to_string() + "\n")
}

/// Extract the images painted on a page; returns `(y, link)` pairs.
fn page_images(
    doc: &mut PdfDoc,
    page: usize,
    dir: &Path,
    prefix: &str,
    saved: &mut HashMap<ObjectId, Option<String>>,
) -> Vec<(f32, String)> {
    let geom = doc.geom(page);
    let scan = doc.scan(page);
    let mut out = Vec::new();
    for placement in &scan.images {
        let Some(id) = doc
            .resources(page)
            .and_then(|r| r.get_deref(b"XObject", doc.inner()).ok())
            .and_then(|x| x.as_dict().ok())
            .and_then(|x| x.get(&placement.name).ok())
            .and_then(|o| o.as_reference().ok())
        else {
            continue;
        };
        let file = saved.entry(id).or_insert_with(|| {
            let stream = doc.inner().get_object(id).ok()?.as_stream().ok()?;
            let (ext, data) = decode_image(doc, stream)?;
            let name = format!("image-{}.{ext}", id.0);
            std::fs::write(dir.join(&name), data).ok()?;
            Some(name)
        });
        if let Some(name) = file {
            let y = placement
                .quad
                .iter()
                .map(|p| geom.to_view(*p)[1])
                .fold(f32::MAX, f32::min);
            let link = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            out.push((y, link));
        }
    }
    out
}

/// Turn an image XObject into file bytes: JPEG data is passed through, plain
/// 8-bit gray/RGB/CMYK samples are re-encoded as PNG. Other kinds are skipped.
fn decode_image(doc: &PdfDoc, stream: &Stream) -> Option<(&'static str, Vec<u8>)> {
    let inner = doc.inner();
    let dict = &stream.dict;
    if dict.get_deref(b"Subtype", inner).ok()?.as_name().ok()? != b"Image" {
        return None;
    }
    let int = |key: &[u8]| dict.get_deref(key, inner).ok()?.as_i64().ok();
    let (width, height) = (int(b"Width")? as u32, int(b"Height")? as u32);
    if width < 8 || height < 8 {
        return None;
    }
    let filters: Vec<Vec<u8>> = match dict.get_deref(b"Filter", inner) {
        Ok(Object::Name(n)) => vec![n.clone()],
        Ok(Object::Array(a)) => a
            .iter()
            .filter_map(|o| o.as_name().ok().map(<[u8]>::to_vec))
            .collect(),
        _ => vec![],
    };
    match filters.last().map(Vec::as_slice) {
        Some(b"DCTDecode") if filters.len() == 1 => return Some(("jpg", stream.content.clone())),
        Some(b"JPXDecode") if filters.len() == 1 => return Some(("jp2", stream.content.clone())),
        Some(b"DCTDecode" | b"JPXDecode" | b"CCITTFaxDecode" | b"JBIG2Decode") => return None,
        _ => {}
    }
    if int(b"BitsPerComponent")? != 8 {
        return None;
    }
    let data = if filters.is_empty() {
        stream.content.clone()
    } else {
        stream.decompressed_content().ok()?
    };
    let pixels = (width as usize) * (height as usize);
    let channels = data.len() / pixels.max(1);
    if data.len() < pixels * channels {
        return None;
    }
    let color = match channels {
        1 => image::ExtendedColorType::L8,
        3 => image::ExtendedColorType::Rgb8,
        4 => image::ExtendedColorType::Rgb8,
        _ => return None,
    };
    let samples: Vec<u8> = if channels == 4 {
        data.as_chunks::<4>()
            .0
            .iter()
            .take(pixels)
            .flat_map(|p| {
                let k = 255 - u16::from(p[3]);
                [0, 1, 2].map(|i| ((255 - u16::from(p[i])) * k / 255) as u8)
            })
            .collect()
    } else {
        data[..pixels * channels].to_vec()
    };
    let mut png = Vec::new();
    image::write_buffer_with_format(
        &mut std::io::Cursor::new(&mut png),
        &samples,
        width,
        height,
        color,
        image::ImageFormat::Png,
    )
    .ok()?;
    Some(("png", png))
}

/// Export to `path`. With `with_images`, images go into `<stem>_images/`.
pub fn export_to_file(doc: &mut PdfDoc, path: &Path, with_images: bool) -> Result<()> {
    let mut options = MarkdownOptions::default();
    if with_images {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("document");
        let dir_name = format!("{stem}_images");
        options.image_dir = Some(path.with_file_name(&dir_name));
        options.image_link_prefix = dir_name;
    }
    let markdown = export(doc, &options)?;
    std::fs::write(path, markdown).with_context(|| format!("writing {}", path.display()))
}
