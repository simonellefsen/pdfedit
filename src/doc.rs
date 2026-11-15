//! The editable document: wraps `lopdf` and exposes pages, text runs, text
//! editing with undo/redo, and saving.

use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use anyhow::{Context, Result, anyhow, bail};
use lopdf::content::Content;
use lopdf::{Dictionary, Document, Object, ObjectId, Stream, dictionary};

use crate::content::{EditOutcome, PageScan, replace_run_text, scan};
use crate::font::{FontInfo, standard_fallback};

/// Visible box and rotation of a page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageGeom {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    /// Clockwise rotation in degrees: 0, 90, 180 or 270.
    pub rotate: i32,
}

impl PageGeom {
    /// Size of the page as displayed, in points.
    pub fn view_size(&self) -> [f32; 2] {
        let (w, h) = (self.x1 - self.x0, self.y1 - self.y0);
        if self.rotate % 180 == 0 {
            [w, h]
        } else {
            [h, w]
        }
    }

    /// Map a user-space point to display space (origin top-left, y down).
    pub fn to_view(&self, p: [f32; 2]) -> [f32; 2] {
        let [x, y] = p;
        match self.rotate {
            90 => [y - self.y0, x - self.x0],
            180 => [self.x1 - x, y - self.y0],
            270 => [self.y1 - y, self.x1 - x],
            _ => [x - self.x0, self.y1 - y],
        }
    }
}

/// A page dictionary as it was before an edit.
type PageState = (usize, Dictionary);

/// A text replacement for one run.
#[derive(Debug, Clone)]
pub struct RunEdit {
    pub page: usize,
    pub run: usize,
    pub text: String,
    /// Keeps centred or right-aligned text in place: the width (in page
    /// units) the text used to span, and the share of any width change to
    /// shift the start by (0.5 centres, 1.0 keeps the right edge).
    pub fit: Option<(f32, f32)>,
}

/// An open PDF document.
pub struct PdfDoc {
    inner: Document,
    pages: Vec<ObjectId>,
    fonts: HashMap<ObjectId, Rc<FontInfo>>,
    scans: HashMap<usize, Rc<PageScan>>,
    undo: Vec<Vec<PageState>>,
    redo: Vec<Vec<PageState>>,
    /// Whether there are unsaved changes.
    pub dirty: bool,
}

impl PdfDoc {
    /// Load a document from a file.
    pub fn open(path: &Path) -> Result<Self> {
        let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Self::from_bytes(&data)
    }

    /// Load a document from memory.
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        let inner = Document::load_mem(data).context("parsing PDF")?;
        let pages = inner.get_pages().into_values().collect();
        Ok(Self {
            inner,
            pages,
            fonts: HashMap::new(),
            scans: HashMap::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            dirty: false,
        })
    }

    /// The underlying `lopdf` document.
    pub fn inner(&self) -> &Document {
        &self.inner
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub fn page_id(&self, page: usize) -> Option<ObjectId> {
        self.pages.get(page).copied()
    }

    /// Look up a page attribute, following `/Parent` for inherited ones.
    pub fn inherited(&self, page: usize, key: &[u8]) -> Option<&Object> {
        let mut id = self.page_id(page)?;
        for _ in 0..32 {
            let dict = self.inner.get_dictionary(id).ok()?;
            if let Ok(value) = dict.get_deref(key, &self.inner) {
                return Some(value);
            }
            id = dict.get(b"Parent").ok()?.as_reference().ok()?;
        }
        None
    }

    fn rect(&self, page: usize, key: &[u8]) -> Option<[f32; 4]> {
        let arr = self.inherited(page, key)?.as_array().ok()?;
        if arr.len() != 4 {
            return None;
        }
        let mut v = [0.0f32; 4];
        for (slot, obj) in v.iter_mut().zip(arr) {
            let obj = self.inner.dereference(obj).map(|(_, o)| o).unwrap_or(obj);
            *slot = obj.as_float().ok()?;
        }
        Some([
            v[0].min(v[2]),
            v[1].min(v[3]),
            v[0].max(v[2]),
            v[1].max(v[3]),
        ])
    }

    /// Geometry of a page (crop box intersected with media box, and rotation).
    pub fn geom(&self, page: usize) -> PageGeom {
        let media = self
            .rect(page, b"MediaBox")
            .unwrap_or([0.0, 0.0, 595.0, 842.0]);
        let crop = self.rect(page, b"CropBox").unwrap_or(media);
        let mut b = [
            crop[0].max(media[0]),
            crop[1].max(media[1]),
            crop[2].min(media[2]),
            crop[3].min(media[3]),
        ];
        if b[2] - b[0] < 1.0 || b[3] - b[1] < 1.0 {
            b = [0.0, 0.0, 595.0, 842.0];
        }
        let rotate = self
            .inherited(page, b"Rotate")
            .and_then(|o| o.as_i64().ok())
            .unwrap_or(0);
        PageGeom {
            x0: b[0],
            y0: b[1],
            x1: b[2],
            y1: b[3],
            rotate: (rotate.rem_euclid(360) / 90 * 90) as i32,
        }
    }

    /// The page's resource dictionary.
    pub fn resources(&self, page: usize) -> Option<&Dictionary> {
        self.inherited(page, b"Resources")?.as_dict().ok()
    }

    fn page_fonts(&mut self, page: usize) -> HashMap<Vec<u8>, Rc<FontInfo>> {
        let mut out = HashMap::new();
        let mut entries = Vec::new();
        if let Some(fonts) = self
            .resources(page)
            .and_then(|r| r.get_deref(b"Font", &self.inner).ok())
            .and_then(|f| f.as_dict().ok())
        {
            for (name, obj) in fonts.iter() {
                entries.push((name.clone(), obj.clone()));
            }
        }
        for (name, obj) in entries {
            let info = match obj {
                Object::Reference(id) => {
                    if let Some(info) = self.fonts.get(&id) {
                        Some(info.clone())
                    } else if let Ok(dict) = self.inner.get_dictionary(id) {
                        let info = Rc::new(FontInfo::from_dict(&self.inner, dict));
                        self.fonts.insert(id, info.clone());
                        Some(info)
                    } else {
                        None
                    }
                }
                Object::Dictionary(dict) => Some(Rc::new(FontInfo::from_dict(&self.inner, &dict))),
                _ => None,
            };
            if let Some(info) = info {
                out.insert(name, info);
            }
        }
        out
    }

    fn content(&self, page: usize) -> Result<Content> {
        let id = self.page_id(page).ok_or_else(|| anyhow!("no such page"))?;
        let bytes = self.inner.get_page_content(id);
        Content::decode(&bytes).map_err(|e| anyhow!("cannot parse page content: {e}"))
    }

    /// Text runs and image placements of a page (cached until the page changes).
    pub fn scan(&mut self, page: usize) -> Rc<PageScan> {
        if let Some(scan) = self.scans.get(&page) {
            return scan.clone();
        }
        let fonts = self.page_fonts(page);
        let result = match self.content(page) {
            Ok(content) => scan(&content.operations, &fonts),
            Err(_) => PageScan::default(),
        };
        let result = Rc::new(result);
        self.scans.insert(page, result.clone());
        result
    }

    /// Replace the text of one run on a page.
    pub fn set_run_text(
        &mut self,
        page: usize,
        run_index: usize,
        text: &str,
    ) -> Result<EditOutcome> {
        let (outcome, before) = self.edit_run(page, run_index, text, None)?;
        self.undo.push(vec![before]);
        self.redo.clear();
        Ok(outcome)
    }

    /// Replace the text of many runs as a single undo step. Returns how many needed the fallback font. If any
    /// edit fails, none are kept.
    pub fn set_many(&mut self, edits: &[RunEdit]) -> Result<usize> {
        self.apply_batch(&[], edits)
    }

    /// Delete operator ranges and replace run texts as a single undo step.
    ///
    /// `drops` lists, per page, inclusive operator ranges to delete (see
    /// [`crate::content::Rule::ops`]). Returns how many text edits needed the
    /// fallback font. If anything fails, nothing is kept.
    pub fn apply_batch(
        &mut self,
        drops: &[(usize, Vec<(usize, usize)>)],
        edits: &[RunEdit],
    ) -> Result<usize> {
        let mut group = Vec::new();
        // Operators go first: removing them does not renumber text runs,
        // whereas text edits do move operators.
        for (page, ranges) in drops {
            match self.drop_ops(*page, ranges) {
                Ok(before) => group.push(before),
                Err(e) => {
                    self.swap_group(group);
                    return Err(e);
                }
            }
        }
        // Later runs first: emptying a run removes it and shifts the rest.
        let mut order: Vec<&RunEdit> = edits.iter().collect();
        order.sort_by(|a, b| a.page.cmp(&b.page).then(b.run.cmp(&a.run)));
        let mut fallbacks = 0;
        for edit in order {
            match self.edit_run(edit.page, edit.run, &edit.text, edit.fit) {
                Ok((outcome, before)) => {
                    group.push(before);
                    if matches!(outcome, EditOutcome::FallbackFont(_)) {
                        fallbacks += 1;
                    }
                }
                Err(e) => {
                    self.swap_group(group);
                    return Err(e);
                }
            }
        }
        if !group.is_empty() {
            self.undo.push(group);
            self.redo.clear();
        }
        Ok(fallbacks)
    }

    fn drop_ops(&mut self, page: usize, ranges: &[(usize, usize)]) -> Result<PageState> {
        let page_id = self.page_id(page).ok_or_else(|| anyhow!("no such page"))?;
        let mut content = self.content(page)?;
        let mut ranges = ranges.to_vec();
        ranges.sort_unstable_by(|a, b| b.cmp(a));
        for (start, end) in ranges {
            if start > end || end >= content.operations.len() {
                bail!("page content changed unexpectedly");
            }
            content.operations.drain(start..=end);
        }
        let before = self.inner.get_dictionary(page_id)?.clone();
        self.write_content(page, page_id, &content)?;
        Ok((page, before))
    }

    fn write_content(&mut self, page: usize, page_id: ObjectId, content: &Content) -> Result<()> {
        let encoded = content.encode().context("encoding page content")?;
        let mut stream = Stream::new(dictionary! {}, encoded);
        let _ = stream.compress();
        let stream_id = self.inner.add_object(stream);
        self.inner
            .get_dictionary_mut(page_id)?
            .set("Contents", Object::Reference(stream_id));
        self.scans.remove(&page);
        self.dirty = true;
        Ok(())
    }

    fn edit_run(
        &mut self,
        page: usize,
        run_index: usize,
        text: &str,
        fit: Option<(f32, f32)>,
    ) -> Result<(EditOutcome, PageState)> {
        let page_id = self.page_id(page).ok_or_else(|| anyhow!("no such page"))?;
        let scan = self.scan(page);
        let run = scan
            .runs
            .get(run_index)
            .ok_or_else(|| anyhow!("no such text run"))?
            .clone();
        let mut content = self.content(page)?;
        if run.op_index >= content.operations.len() {
            bail!("page content changed unexpectedly");
        }
        let fonts = self.page_fonts(page);
        let font = fonts.get(&run.font_res);
        let fallback_base = standard_fallback(&run.font_base);
        let fallback_res = format!("PdfEdit{}", fallback_base.replace('-', ""));

        let before = self.inner.get_dictionary(page_id)?.clone();
        let outcome = replace_run_text(
            &mut content.operations,
            &run,
            font.map(Rc::as_ref),
            text,
            (fallback_res.as_bytes(), &FontInfo::win_ansi(fallback_base)),
            fit.filter(|_| run.size > 0.0)
                .map(|(width, share)| (width * run.tf_size.abs() / run.size, share)),
        );
        if matches!(outcome, EditOutcome::FallbackFont(_)) {
            self.add_fallback_font(page, page_id, &fallback_res, fallback_base)?;
        }

        self.write_content(page, page_id, &content)?;
        Ok((outcome, (page, before)))
    }

    /// Give the page its own resource dictionary containing a standard font.
    fn add_fallback_font(
        &mut self,
        page: usize,
        page_id: ObjectId,
        res_name: &str,
        base_font: &str,
    ) -> Result<()> {
        let mut resources = self.resources(page).cloned().unwrap_or_default();
        let mut fonts = resources
            .get_deref(b"Font", &self.inner)
            .and_then(Object::as_dict)
            .cloned()
            .unwrap_or_default();
        if !fonts.has(res_name.as_bytes()) {
            let font_id = self.inner.add_object(dictionary! {
                "Type" => "Font",
                "Subtype" => "Type1",
                "BaseFont" => base_font,
                "Encoding" => "WinAnsiEncoding",
            });
            fonts.set(res_name, Object::Reference(font_id));
        }
        resources.set("Font", Object::Dictionary(fonts));
        self.inner
            .get_dictionary_mut(page_id)?
            .set("Resources", Object::Dictionary(resources));
        Ok(())
    }

    /// Restore the page states of a group (newest first) and return the
    /// states they replaced, ordered so that swapping again reverses it.
    fn swap_group(&mut self, group: Vec<PageState>) -> Vec<PageState> {
        let mut replaced = Vec::new();
        for (page, dict) in group.into_iter().rev() {
            let Some(id) = self.page_id(page) else {
                continue;
            };
            let Ok(current) = self.inner.get_dictionary_mut(id) else {
                continue;
            };
            replaced.push((page, std::mem::replace(current, dict)));
            self.scans.remove(&page);
        }
        self.dirty = true;
        replaced
    }

    fn pages_of(group: &[PageState]) -> Vec<usize> {
        let mut pages: Vec<usize> = group.iter().map(|(page, _)| *page).collect();
        pages.sort_unstable();
        pages.dedup();
        pages
    }

    /// Undo the last edit; returns the affected pages.
    pub fn undo(&mut self) -> Option<Vec<usize>> {
        let group = self.undo.pop()?;
        let replaced = self.swap_group(group);
        let pages = Self::pages_of(&replaced);
        self.redo.push(replaced);
        Some(pages)
    }

    /// Redo the last undone edit; returns the affected pages.
    pub fn redo(&mut self) -> Option<Vec<usize>> {
        let group = self.redo.pop()?;
        let replaced = self.swap_group(group);
        let pages = Self::pages_of(&replaced);
        self.undo.push(replaced);
        Some(pages)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Serialize the current state (used for rendering).
    pub fn to_bytes(&mut self) -> Result<Vec<u8>> {
        let mut buf = Vec::new();
        self.inner.save_to(&mut buf).context("serializing PDF")?;
        Ok(buf)
    }

    /// Save to a file, dropping objects that edits left unreferenced.
    pub fn save(&mut self, path: &Path) -> Result<()> {
        let mut copy = self.inner.clone();
        copy.prune_objects();
        let mut buf = Vec::new();
        copy.save_modern(&mut buf).context("serializing PDF")?;
        let tmp = path.with_extension("pdf.tmp");
        std::fs::write(&tmp, &buf).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
        self.dirty = false;
        Ok(())
    }
}
