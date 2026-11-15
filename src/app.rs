//! The window: toolbar, scrolling page view, in-place text editing.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui::{
    self, Align, Color32, FontId, Key, KeyboardShortcut, Modifiers, Pos2, Rect, Sense, Stroke,
    StrokeKind, TextureHandle, TextureOptions, UiBuilder, Vec2, vec2,
};

use pdfedit::content::{EditOutcome, TextRun};
use pdfedit::doc::{PageGeom, PdfDoc};
use pdfedit::markdown;

use crate::render::{RenderRequest, Renderer};
use crate::script::Script;

const PAGE_GAP: f32 = 12.0;
const MAX_TEXTURE_SIDE: f32 = 8000.0;

struct PageTexture {
    texture: TextureHandle,
    scale: f32,
    version: u64,
}

struct Editing {
    page: usize,
    run: usize,
    text: String,
    original: String,
    focused: bool,
}

enum Action {
    Open,
    OpenPath(PathBuf),
    Save,
    SaveAs,
    ExportMarkdown { images: bool },
    Undo,
    Redo,
    StartEdit { page: usize, run: usize },
    CommitEdit,
    CancelEdit,
}

pub struct App {
    renderer: Renderer,
    doc: Option<PdfDoc>,
    path: Option<PathBuf>,
    data: Arc<Vec<u8>>,
    generation: u64,
    geoms: Vec<PageGeom>,
    versions: Vec<u64>,
    textures: HashMap<usize, PageTexture>,
    errors: HashMap<usize, String>,
    in_flight: HashSet<(usize, u64, u32)>,
    zoom: f32,
    zoom_time: f64,
    scroll_offset: Vec2,
    pending_offset: Option<Vec2>,
    view_width: f32,
    current_page: usize,
    scroll_to: Option<usize>,
    editing: Option<Editing>,
    status: String,
    title: String,
    script: Option<Script>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, path: Option<PathBuf>) -> Self {
        let mut app = Self {
            renderer: Renderer::new(cc.egui_ctx.clone()),
            doc: None,
            path: None,
            data: Arc::new(Vec::new()),
            generation: 0,
            geoms: Vec::new(),
            versions: Vec::new(),
            textures: HashMap::new(),
            errors: HashMap::new(),
            in_flight: HashSet::new(),
            zoom: 1.25,
            zoom_time: 0.0,
            scroll_offset: Vec2::ZERO,
            pending_offset: None,
            view_width: 800.0,
            current_page: 0,
            scroll_to: None,
            editing: None,
            status: "Open a PDF to get started (or drop one on the window).".into(),
            title: String::new(),
            script: Script::from_env(),
        };
        if let Some(path) = path {
            app.open(&path);
        }
        app
    }

    fn open(&mut self, path: &Path) {
        match PdfDoc::open(path).and_then(|mut doc| doc.to_bytes().map(|bytes| (doc, bytes))) {
            Ok((doc, bytes)) => {
                self.generation += 1;
                self.geoms = (0..doc.page_count()).map(|p| doc.geom(p)).collect();
                self.versions = vec![0; doc.page_count()];
                self.status = format!("Opened {} ({} pages)", path.display(), doc.page_count());
                self.doc = Some(doc);
                self.data = Arc::new(bytes);
                self.path = Some(path.to_path_buf());
                self.textures.clear();
                self.errors.clear();
                self.in_flight.clear();
                self.editing = None;
                self.current_page = 0;
                self.pending_offset = Some(Vec2::ZERO);
            }
            Err(e) => self.status = format!("Could not open {}: {e:#}", path.display()),
        }
    }

    /// Re-serialize after a change to `page` so it is rendered again.
    fn page_changed(&mut self, page: usize) {
        let Some(doc) = &mut self.doc else { return };
        match doc.to_bytes() {
            Ok(bytes) => {
                self.data = Arc::new(bytes);
                if let Some(v) = self.versions.get_mut(page) {
                    *v += 1;
                }
            }
            Err(e) => self.status = format!("Could not update the view: {e:#}"),
        }
    }

    fn save_to(&mut self, path: &Path) {
        let Some(doc) = &mut self.doc else { return };
        match doc.save(path) {
            Ok(()) => {
                self.status = format!("Saved {}", path.display());
                self.path = Some(path.to_path_buf());
            }
            Err(e) => self.status = format!("Save failed: {e:#}"),
        }
    }

    fn commit_edit(&mut self) {
        let Some(edit) = self.editing.take() else {
            return;
        };
        if edit.text == edit.original {
            return;
        }
        let Some(doc) = &mut self.doc else { return };
        match doc.set_run_text(edit.page, edit.run, &edit.text) {
            Ok(EditOutcome::OriginalFont) => {
                self.status = format!("Edited text on page {}", edit.page + 1);
            }
            Ok(EditOutcome::FallbackFont(missing)) => {
                let chars: String = missing.iter().take(12).collect();
                self.status = format!(
                    "Edited text on page {} with a standard font: the embedded font has no \
                     glyphs for \"{chars}\"",
                    edit.page + 1
                );
            }
            Err(e) => {
                self.status = format!("Edit failed: {e:#}");
                return;
            }
        }
        self.page_changed(edit.page);
    }

    fn run_action(&mut self, action: Action) {
        match action {
            Action::Open => {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("PDF", &["pdf"])
                    .pick_file()
                {
                    self.open(&path);
                }
            }
            Action::OpenPath(path) => self.open(&path),
            Action::Save => {
                self.commit_edit();
                if let Some(path) = self.path.clone() {
                    self.save_to(&path);
                }
            }
            Action::SaveAs => {
                self.commit_edit();
                let mut dialog = rfd::FileDialog::new().add_filter("PDF", &["pdf"]);
                if let Some(path) = &self.path {
                    if let Some(dir) = path.parent() {
                        dialog = dialog.set_directory(dir);
                    }
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        dialog = dialog.set_file_name(format!("{stem}-edited.pdf"));
                    }
                }
                if let Some(path) = dialog.save_file() {
                    self.save_to(&path);
                }
            }
            Action::ExportMarkdown { images } => {
                self.commit_edit();
                let mut dialog = rfd::FileDialog::new().add_filter("Markdown", &["md"]);
                if let Some(path) = &self.path {
                    if let Some(dir) = path.parent() {
                        dialog = dialog.set_directory(dir);
                    }
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        dialog = dialog.set_file_name(format!("{stem}.md"));
                    }
                }
                if let (Some(path), Some(doc)) = (dialog.save_file(), &mut self.doc) {
                    self.status = match markdown::export_to_file(doc, &path, images) {
                        Ok(()) => format!("Exported {}", path.display()),
                        Err(e) => format!("Export failed: {e:#}"),
                    };
                }
            }
            Action::Undo | Action::Redo => {
                self.editing = None;
                let Some(doc) = &mut self.doc else { return };
                let undo = matches!(action, Action::Undo);
                let page = if undo { doc.undo() } else { doc.redo() };
                if let Some(page) = page {
                    self.status = format!(
                        "{} on page {}",
                        if undo { "Undo" } else { "Redo" },
                        page + 1
                    );
                    self.page_changed(page);
                    self.scroll_to = Some(page);
                }
            }
            Action::StartEdit { page, run } => {
                self.commit_edit();
                let Some(doc) = &mut self.doc else { return };
                if let Some(r) = doc.scan(page).runs.get(run) {
                    self.editing = Some(Editing {
                        page,
                        run,
                        text: r.text.clone(),
                        original: r.text.clone(),
                        focused: false,
                    });
                }
            }
            Action::CommitEdit => self.commit_edit(),
            Action::CancelEdit => self.editing = None,
        }
    }

    fn set_zoom(&mut self, zoom: f32, now: f64) {
        let zoom = zoom.clamp(0.25, 6.0);
        if (zoom - self.zoom).abs() < 1e-4 {
            return;
        }
        self.pending_offset = Some(self.scroll_offset * (zoom / self.zoom));
        self.zoom = zoom;
        self.zoom_time = now;
    }

    fn receive_renders(&mut self, ctx: &egui::Context) {
        for result in self.renderer.poll() {
            let (generation, version) = result.tag;
            self.in_flight
                .remove(&(result.page, version, result.scale.to_bits()));
            if generation != self.generation || self.versions.get(result.page) != Some(&version) {
                continue;
            }
            match result.image {
                Ok(image) => {
                    let texture = ctx.load_texture(
                        format!("page-{}", result.page),
                        image,
                        TextureOptions::LINEAR,
                    );
                    self.errors.remove(&result.page);
                    self.textures.insert(
                        result.page,
                        PageTexture {
                            texture,
                            scale: result.scale,
                            version,
                        },
                    );
                }
                Err(e) => {
                    self.errors.insert(result.page, e);
                }
            }
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>, now: f64) {
        let has_doc = self.doc.is_some();
        let (can_undo, can_redo, dirty) = self
            .doc
            .as_ref()
            .map(|d| (d.can_undo(), d.can_redo(), d.dirty))
            .unwrap_or_default();
        ui.horizontal(|ui| {
            if ui.button("Open…").clicked() {
                actions.push(Action::Open);
            }
            if ui
                .add_enabled(has_doc && dirty, egui::Button::new("Save"))
                .clicked()
            {
                actions.push(Action::Save);
            }
            if ui
                .add_enabled(has_doc, egui::Button::new("Save As…"))
                .clicked()
            {
                actions.push(Action::SaveAs);
            }
            ui.add_enabled_ui(has_doc, |ui| {
                ui.menu_button("Export Markdown", |ui| {
                    if ui.button("Text only…").clicked() {
                        actions.push(Action::ExportMarkdown { images: false });
                        ui.close();
                    }
                    if ui.button("With images…").clicked() {
                        actions.push(Action::ExportMarkdown { images: true });
                        ui.close();
                    }
                });
            });
            ui.separator();
            if ui
                .add_enabled(can_undo, egui::Button::new("Undo"))
                .clicked()
            {
                actions.push(Action::Undo);
            }
            if ui
                .add_enabled(can_redo, egui::Button::new("Redo"))
                .clicked()
            {
                actions.push(Action::Redo);
            }
            ui.separator();
            if ui.button("−").clicked() {
                self.set_zoom(self.zoom / 1.2, now);
            }
            ui.label(format!("{:.0}%", self.zoom * 100.0));
            if ui.button("+").clicked() {
                self.set_zoom(self.zoom * 1.2, now);
            }
            if ui
                .add_enabled(has_doc, egui::Button::new("Fit width"))
                .clicked()
            {
                let widest = self
                    .geoms
                    .iter()
                    .map(|g| g.view_size()[0])
                    .fold(1.0, f32::max);
                self.set_zoom((self.view_width - 2.0 * PAGE_GAP - 16.0) / widest, now);
            }
            if has_doc {
                ui.separator();
                let count = self.geoms.len();
                let mut page = self.current_page + 1;
                ui.label("Page");
                if ui
                    .add(egui::DragValue::new(&mut page).range(1..=count).speed(0.1))
                    .changed()
                {
                    self.scroll_to = Some(page - 1);
                }
                ui.label(format!("of {count}"));
            }
        });
    }

    fn pages(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>, now: f64) {
        let Some(doc) = &mut self.doc else {
            ui.centered_and_justified(|ui| {
                ui.label("No document open");
            });
            return;
        };
        self.view_width = ui.available_width();
        let ppp = ui.ctx().pixels_per_point();
        let zoom = self.zoom;
        let zoom_settled = now - self.zoom_time > 0.2;
        if !zoom_settled {
            ui.ctx().request_repaint();
        }
        let mut scroll = egui::ScrollArea::both().auto_shrink(false);
        if let Some(offset) = self.pending_offset.take() {
            scroll = scroll.scroll_offset(offset);
        }
        let viewport_center = ui.max_rect().center().y;
        let mut visible = (usize::MAX, 0usize);

        let output = scroll.show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.spacing_mut().item_spacing.y = PAGE_GAP;
                ui.add_space(PAGE_GAP);
                for page in 0..self.geoms.len() {
                    let geom = self.geoms[page];
                    let view = geom.view_size();
                    let size = vec2(view[0], view[1]) * zoom;
                    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
                    if self.scroll_to == Some(page) {
                        ui.scroll_to_rect(rect, Some(Align::TOP));
                        self.scroll_to = None;
                    }
                    if !ui.is_rect_visible(rect) {
                        continue;
                    }
                    visible = (visible.0.min(page), visible.1.max(page));
                    if rect.top() <= viewport_center {
                        self.current_page = page;
                    }

                    let painter = ui.painter_at(rect.expand(2.0));
                    painter.rect_filled(rect, 0.0, Color32::WHITE);
                    let version = self.versions[page];
                    let max_scale = MAX_TEXTURE_SIDE / view[0].max(view[1]);
                    let scale = ((zoom * ppp * 20.0).round() / 20.0).min(max_scale);
                    let mut fresh = false;
                    if let Some(tex) = self.textures.get(&page) {
                        let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
                        painter.image(tex.texture.id(), rect, uv, Color32::WHITE);
                        fresh = tex.version == version && (tex.scale == scale || !zoom_settled);
                    }
                    if !fresh {
                        let key = (page, version, scale.to_bits());
                        if self.in_flight.insert(key) {
                            self.renderer.request(RenderRequest {
                                data: self.data.clone(),
                                page,
                                scale,
                                tag: (self.generation, version),
                            });
                        }
                    }
                    if let Some(error) = self.errors.get(&page) {
                        painter.text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            error,
                            FontId::proportional(14.0),
                            Color32::DARK_RED,
                        );
                    }
                    painter.rect_stroke(
                        rect,
                        0.0,
                        Stroke::new(1.0, Color32::from_gray(120)),
                        StrokeKind::Outside,
                    );

                    let scan = doc.scan(page);
                    let run_rect = |run: &TextRun| {
                        let mut r = Rect::NOTHING;
                        for p in run.quad {
                            let v = geom.to_view(p);
                            r.extend_with(rect.min + vec2(v[0], v[1]) * zoom);
                        }
                        r
                    };

                    let editing_here = self
                        .editing
                        .as_ref()
                        .filter(|e| e.page == page)
                        .map(|e| e.run);
                    let hovered = response.hover_pos().and_then(|pos| {
                        scan.runs
                            .iter()
                            .enumerate()
                            .map(|(i, run)| (i, run_rect(run)))
                            .filter(|(_, r)| r.expand(1.0).contains(pos))
                            .min_by(|a, b| a.1.area().total_cmp(&b.1.area()))
                    });
                    if let Some((index, r)) = hovered
                        && editing_here != Some(index)
                    {
                        painter.rect_filled(
                            r,
                            2.0,
                            Color32::from_rgba_unmultiplied(40, 120, 255, 28),
                        );
                        painter.rect_stroke(
                            r,
                            2.0,
                            Stroke::new(1.0, Color32::from_rgb(40, 120, 255)),
                            StrokeKind::Outside,
                        );
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
                    }
                    if response.clicked() {
                        match hovered {
                            Some((index, _)) => {
                                actions.push(Action::StartEdit { page, run: index })
                            }
                            None => actions.push(Action::CommitEdit),
                        }
                    }

                    if let Some(edit) = self.editing.as_mut().filter(|e| e.page == page)
                        && let Some(run) = scan.runs.get(edit.run)
                    {
                        let r = run_rect(run);
                        let font_px = (run.size * zoom * 0.92).clamp(9.0, 72.0);
                        let edit_rect = Rect::from_min_size(
                            r.min - vec2(5.0, 3.0),
                            vec2((r.width() + 80.0).max(180.0), r.height().max(font_px) + 6.0),
                        );
                        let mut child = ui.new_child(UiBuilder::new().max_rect(edit_rect));
                        let field = child.add_sized(
                            edit_rect.size(),
                            egui::TextEdit::singleline(&mut edit.text)
                                .font(FontId::proportional(font_px))
                                .text_color(Color32::BLACK)
                                .background_color(Color32::from_rgb(255, 252, 220)),
                        );
                        if !edit.focused {
                            field.request_focus();
                            edit.focused = true;
                        } else if field.lost_focus() {
                            if ui.input(|i| i.key_pressed(Key::Escape)) {
                                actions.push(Action::CancelEdit);
                            } else {
                                actions.push(Action::CommitEdit);
                            }
                        }
                    }
                }
                ui.add_space(PAGE_GAP);
            });
        });
        self.scroll_offset = output.state.clamped_offset();

        // Keep textures only near what is on screen.
        if visible.0 != usize::MAX {
            let (lo, hi) = (visible.0.saturating_sub(6), visible.1 + 6);
            self.textures.retain(|page, _| (lo..=hi).contains(page));
        }
    }
}

impl eframe::App for App {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if let Some(script) = &mut self.script {
            script.tick(ctx, raw_input);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let now = ctx.input(|i| i.time);
        self.receive_renders(&ctx);

        let mut actions = Vec::new();
        for file in ctx.input(|i| i.raw.dropped_files.clone()) {
            actions.push(Action::OpenPath(file.path().to_path_buf()));
        }

        let shortcut = |key, modifiers| {
            ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(modifiers, key)))
        };
        let shift_cmd = Modifiers::COMMAND | Modifiers::SHIFT;
        if shortcut(Key::O, Modifiers::COMMAND) {
            actions.push(Action::Open);
        }
        if shortcut(Key::S, shift_cmd) {
            actions.push(Action::SaveAs);
        } else if shortcut(Key::S, Modifiers::COMMAND) {
            actions.push(Action::Save);
        }
        if self.editing.is_none() {
            if shortcut(Key::Z, shift_cmd) {
                actions.push(Action::Redo);
            } else if shortcut(Key::Z, Modifiers::COMMAND) {
                actions.push(Action::Undo);
            }
        }
        if shortcut(Key::Plus, Modifiers::COMMAND) || shortcut(Key::Equals, Modifiers::COMMAND) {
            self.set_zoom(self.zoom * 1.2, now);
        }
        if shortcut(Key::Minus, Modifiers::COMMAND) {
            self.set_zoom(self.zoom / 1.2, now);
        }
        let pinch = ctx.input(|i| i.zoom_delta());
        if pinch != 1.0 {
            self.set_zoom(self.zoom * pinch, now);
        }

        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.add_space(4.0);
            self.toolbar(ui, &mut actions, now);
            ui.add_space(4.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    ui.weak("Click text to edit · Enter applies · Esc cancels");
                    ui.with_layout(egui::Layout::left_to_right(Align::Center), |ui| {
                        ui.add(egui::Label::new(&self.status).truncate());
                    });
                });
            });
        });
        egui::CentralPanel::no_frame().show(ui, |ui| {
            ui.painter()
                .rect_filled(ui.max_rect(), 0.0, Color32::from_gray(70));
            self.pages(ui, &mut actions, now);
        });

        for action in actions {
            self.run_action(action);
        }

        let name = self
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned());
        let dirty = self.doc.as_ref().is_some_and(|d| d.dirty);
        let title = match name {
            Some(name) => format!("{}{name} — pdfedit", if dirty { "• " } else { "" }),
            None => "pdfedit".to_string(),
        };
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }
}
