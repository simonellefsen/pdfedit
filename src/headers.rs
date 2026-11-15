//! Running headers and footers: text in the top or bottom margin that
//! repeats from page to page, possibly with a changing number in it.

use std::collections::{BTreeSet, HashMap};

use anyhow::Result;

use crate::doc::{PdfDoc, RunEdit};

/// Fraction of the page height, at the top and bottom, searched for
/// headers and footers.
const MARGIN: f32 = 0.10;
/// Only this many lines from the top or bottom edge are considered.
const EDGE_LINES: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Zone {
    Header,
    Footer,
}

/// One occurrence of a header or footer.
#[derive(Debug, Clone)]
pub struct Member {
    pub page: usize,
    /// The text runs making up this occurrence, left to right. Some PDFs
    /// draw a header one letter at a time.
    pub runs: Vec<usize>,
    /// Width the text spans, and how it is anchored (see [`RunEdit::fit`]).
    fit: (f32, f32),
    /// Baseline height on the page, from the top.
    y: f32,
    /// The numbers found in the text, in order.
    numbers: Vec<String>,
}

/// A header or footer text that repeats across pages.
#[derive(Debug, Clone)]
pub struct Group {
    pub zone: Zone,
    /// The shared text. Numbers that change from page to page (page numbers)
    /// appear as `{n}`, or `{n1}`, `{n2}`… when there are several.
    pub template: String,
    pub members: Vec<Member>,
    /// Which of the numbers vary, by position.
    varying: Vec<usize>,
}

/// Split text into literal pieces and the digit runs between them.
fn split_numbers(text: &str) -> (Vec<String>, Vec<String>) {
    // A lone roman numeral (front-matter page numbers: i, ii, iii…).
    let roman = |c: char| "ivxlcdm".contains(c.to_ascii_lowercase());
    if !text.is_empty() && text.len() <= 8 && text.chars().all(roman) {
        return (vec![String::new(), String::new()], vec![text.to_string()]);
    }
    let mut literals = vec![String::new()];
    let mut numbers: Vec<String> = Vec::new();
    let mut in_number = false;
    for c in text.chars() {
        if c.is_ascii_digit() {
            if !in_number {
                numbers.push(String::new());
                in_number = true;
            }
            numbers.last_mut().unwrap().push(c);
        } else {
            if in_number {
                literals.push(String::new());
                in_number = false;
            }
            literals.last_mut().unwrap().push(c);
        }
    }
    if in_number {
        literals.push(String::new());
    }
    (literals, numbers)
}

fn placeholder(position: usize, total: usize) -> String {
    if total == 1 {
        "{n}".to_string()
    } else {
        format!("{{n{}}}", position + 1)
    }
}

impl Group {
    /// Number of distinct pages this group appears on.
    pub fn page_count(&self) -> usize {
        self.members
            .iter()
            .map(|m| m.page)
            .collect::<BTreeSet<_>>()
            .len()
    }

    /// The text for one member when the group's template is `template`.
    pub fn text_for(&self, member: &Member, template: &str) -> String {
        let mut text = template.to_string();
        for (i, index) in self.varying.iter().enumerate() {
            text = text.replace(&placeholder(i, self.varying.len()), &member.numbers[*index]);
        }
        text
    }

    /// The edits that apply `template` to every member.
    /// An empty template removes the text.
    pub fn edits(&self, template: &str) -> Vec<RunEdit> {
        let mut edits = Vec::new();
        for member in &self.members {
            // The first run carries the whole text; the rest are emptied.
            for (i, run) in member.runs.iter().enumerate() {
                let text = if i == 0 {
                    self.text_for(member, template)
                } else {
                    String::new()
                };
                edits.push(RunEdit {
                    page: member.page,
                    run: *run,
                    text,
                    fit: Some(member.fit).filter(|(_, share)| i == 0 && *share > 0.0),
                });
            }
        }
        edits
    }
}

struct Piece {
    run: usize,
    x0: f32,
    x1: f32,
    y: f32,
    top: f32,
    bottom: f32,
    size: f32,
    text: String,
}

/// A stretch of text on one line with no wide gap in it.
struct Segment {
    runs: Vec<usize>,
    x0: f32,
    x1: f32,
    y: f32,
    text: String,
    top: f32,
    bottom: f32,
    size: f32,
}

/// Group a page's runs into lines (top to bottom) of segments.
fn page_lines(doc: &mut PdfDoc, page: usize) -> Vec<Vec<Segment>> {
    let geom = doc.geom(page);
    let scan = doc.scan(page);
    let mut pieces: Vec<Piece> = scan
        .runs
        .iter()
        .enumerate()
        .map(|(run, r)| {
            let pts = r.quad.map(|p| geom.to_view(p));
            let a = geom.to_view(r.baseline[0]);
            let b = geom.to_view(r.baseline[1]);
            Piece {
                run,
                x0: a[0].min(b[0]),
                x1: a[0].max(b[0]),
                y: a[1],
                top: pts.iter().map(|p| p[1]).fold(f32::MAX, f32::min),
                bottom: pts.iter().map(|p| p[1]).fold(f32::MIN, f32::max),
                size: r.size.max(1.0),
                text: r.text.clone(),
            }
        })
        .collect();
    pieces.sort_by(|a, b| a.y.total_cmp(&b.y).then(a.x0.total_cmp(&b.x0)));

    let mut rows: Vec<Vec<Piece>> = Vec::new();
    for piece in pieces {
        match rows.last_mut() {
            Some(row) if (piece.y - row[0].y).abs() < 0.3 * row[0].size.max(piece.size) => {
                row.push(piece);
            }
            _ => rows.push(vec![piece]),
        }
    }

    rows.into_iter()
        .map(|mut row| {
            row.sort_by(|a, b| a.x0.total_cmp(&b.x0));
            let mut segments: Vec<Segment> = Vec::new();
            let mut end = f32::MIN;
            for piece in row {
                let gap = piece.x0 - end;
                end = piece.x1;
                match segments.last_mut() {
                    Some(segment) if gap < 1.5 * piece.size => {
                        if gap > 0.12 * piece.size
                            && !segment.text.ends_with(' ')
                            && !piece.text.starts_with(' ')
                        {
                            segment.text.push(' ');
                        }
                        segment.text.push_str(&piece.text);
                        segment.runs.push(piece.run);
                        segment.x1 = piece.x1;
                        segment.top = segment.top.min(piece.top);
                        segment.bottom = segment.bottom.max(piece.bottom);
                        segment.size = segment.size.max(piece.size);
                    }
                    _ => segments.push(Segment {
                        runs: vec![piece.run],
                        x0: piece.x0,
                        x1: piece.x1,
                        y: piece.y,
                        text: piece.text,
                        top: piece.top,
                        bottom: piece.bottom,
                        size: piece.size,
                    }),
                }
            }
            segments
        })
        .collect()
}

/// The font size most of the document's text is set in.
fn body_size(doc: &mut PdfDoc) -> f32 {
    let mut histogram: HashMap<i32, usize> = HashMap::new();
    for page in 0..doc.page_count() {
        for run in &doc.scan(page).runs {
            *histogram
                .entry((run.size * 2.0).round() as i32)
                .or_default() += run.text.len();
        }
    }
    histogram
        .into_iter()
        .max_by_key(|(size, count)| (*count, -*size))
        .map(|(size, _)| size as f32 / 2.0)
        .unwrap_or(12.0)
}

/// Find the headers and footers of a document.
///
/// Text qualifies when it is on one of the first or last lines of the page,
/// inside the top or bottom margin, no larger than body text, and the same
/// text (ignoring the value of any numbers) appears there on at least two
/// pages.
pub fn detect(doc: &mut PdfDoc) -> Vec<Group> {
    let max_size = body_size(doc) * 1.15;
    let mut found: HashMap<(Zone, Vec<String>), Vec<Member>> = HashMap::new();
    let mut order: Vec<(Zone, Vec<String>)> = Vec::new();
    for page in 0..doc.page_count() {
        let [width, height] = doc.geom(page).view_size();
        let lines = page_lines(doc, page);
        let count = lines.len();
        for (index, line) in lines.into_iter().enumerate() {
            for segment in line {
                let zone = if index < EDGE_LINES && segment.bottom < MARGIN * height {
                    Zone::Header
                } else if index + EDGE_LINES >= count && segment.top > (1.0 - MARGIN) * height {
                    Zone::Footer
                } else {
                    continue;
                };
                if segment.size > max_size {
                    continue;
                }
                let (literals, numbers) = split_numbers(segment.text.trim());
                let key = (zone, literals);
                if !found.contains_key(&key) {
                    order.push(key.clone());
                }
                // Anchor by which page edge the text sits closer to.
                let (left, right) = (segment.x0, width - segment.x1);
                let share = if (left - right).abs() < 0.05 * width {
                    0.5
                } else if right < left {
                    1.0
                } else {
                    0.0
                };
                found.entry(key).or_default().push(Member {
                    page,
                    fit: (segment.x1 - segment.x0, share),
                    y: segment.y,
                    runs: segment.runs,
                    numbers,
                });
            }
        }
    }

    let mut groups = Vec::new();
    for key in order {
        let members = found.remove(&key).unwrap_or_default();
        let (zone, literals) = key;
        let pages: BTreeSet<usize> = members.iter().map(|m| m.page).collect();
        if pages.len() < 2 {
            continue;
        }
        let first = &members[0].numbers;
        let varying: Vec<usize> = (0..first.len())
            .filter(|i| members.iter().any(|m| m.numbers[*i] != first[*i]))
            .collect();
        let mut template = String::new();
        for (i, literal) in literals.iter().enumerate() {
            template.push_str(literal);
            if let Some(number) = first.get(i) {
                match varying.iter().position(|v| *v == i) {
                    Some(position) => template.push_str(&placeholder(position, varying.len())),
                    None => template.push_str(number),
                }
            }
        }
        groups.push(Group {
            zone,
            template,
            members,
            varying,
        });
    }
    // Body text near the page edge can repeat on a few pages too (section
    // titles, say). Text on few pages only counts when it sits at the same
    // height as a header or footer that runs through the document.
    let widespread = (doc.page_count() / 10).max(2);
    let height_of = |g: &Group| {
        let mut ys: Vec<f32> = g.members.iter().map(|m| m.y).collect();
        ys.sort_by(f32::total_cmp);
        ys[ys.len() / 2]
    };
    let anchors: Vec<(Zone, f32)> = groups
        .iter()
        .filter(|g| g.page_count() >= widespread)
        .map(|g| (g.zone, height_of(g)))
        .collect();
    groups.retain(|g| {
        let y = height_of(g);
        g.page_count() >= widespread
            || anchors
                .iter()
                .any(|(zone, anchor)| *zone == g.zone && (anchor - y).abs() <= 3.0)
    });
    groups.sort_by_key(|g| g.zone);
    groups
}

/// Operator ranges, per page, of the thin lines that accompany headers
/// and/or footers: rules in the top or bottom margin that sit at the same
/// height on many pages.
pub fn rule_drops(
    doc: &mut PdfDoc,
    headers: bool,
    footers: bool,
) -> Vec<(usize, Vec<(usize, usize)>)> {
    // (zone, height) -> (page, operator range) of each rule found there.
    type Hits = Vec<(usize, (usize, usize))>;
    let mut found: HashMap<(Zone, i32), Hits> = HashMap::new();
    for page in 0..doc.page_count() {
        let geom = doc.geom(page);
        let height = geom.view_size()[1];
        for rule in &doc.scan(page).rules {
            let ys = rule.quad.map(|p| geom.to_view(p)[1]);
            let top = ys.iter().copied().fold(f32::MAX, f32::min);
            let bottom = ys.iter().copied().fold(f32::MIN, f32::max);
            let zone = if bottom < MARGIN * height {
                Zone::Header
            } else if top > (1.0 - MARGIN) * height {
                Zone::Footer
            } else {
                continue;
            };
            if bottom - top > 2.0 {
                continue; // vertical
            }
            found
                .entry((zone, (top / 2.0).round() as i32))
                .or_default()
                .push((page, rule.ops));
        }
    }
    let widespread = (doc.page_count() / 10).max(2);
    let mut per_page: HashMap<usize, Vec<(usize, usize)>> = HashMap::new();
    for ((zone, _), hits) in found {
        let wanted = match zone {
            Zone::Header => headers,
            Zone::Footer => footers,
        };
        let pages: BTreeSet<usize> = hits.iter().map(|(page, _)| *page).collect();
        if wanted && pages.len() >= widespread {
            for (page, ops) in hits {
                per_page.entry(page).or_default().push(ops);
            }
        }
    }
    let mut drops: Vec<_> = per_page.into_iter().collect();
    drops.sort();
    drops
}

/// Remove every detected header and/or footer, with the rules that go with
/// them, as one undo step. Returns the number of pages changed.
pub fn remove(doc: &mut PdfDoc, headers: bool, footers: bool) -> Result<usize> {
    let edits: Vec<RunEdit> = detect(doc)
        .iter()
        .filter(|g| match g.zone {
            Zone::Header => headers,
            Zone::Footer => footers,
        })
        .flat_map(|g| g.edits(""))
        .collect();
    let drops = rule_drops(doc, headers, footers);
    let pages: BTreeSet<usize> = edits
        .iter()
        .map(|e| e.page)
        .chain(drops.iter().map(|(page, _)| *page))
        .collect();
    doc.apply_batch(&drops, &edits)?;
    Ok(pages.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_text_around_numbers() {
        let (literals, numbers) = split_numbers("Page 3 of 10");
        assert_eq!(literals, ["Page ", " of ", ""]);
        assert_eq!(numbers, ["3", "10"]);
        let (literals, numbers) = split_numbers("xiv");
        assert_eq!((literals.len(), numbers), (2, vec!["xiv".to_string()]));
        let (literals, numbers) = split_numbers("Draft");
        assert_eq!(literals, ["Draft"]);
        assert!(numbers.is_empty());
    }
}
