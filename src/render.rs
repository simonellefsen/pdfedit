//! Page rasterization with `hayro`, run on a worker thread so the UI stays
//! responsive.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use eframe::egui::{ColorImage, Context};
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings, render};

/// A request to rasterize one page.
pub struct RenderRequest {
    /// Serialized PDF. Requests sharing the same `Arc` reuse the parsed file.
    pub data: Arc<Vec<u8>>,
    pub page: usize,
    /// Pixels per PDF point.
    pub scale: f32,
    /// Opaque value echoed back in the result.
    pub tag: (u64, u64),
}

/// The outcome of a [`RenderRequest`].
pub struct RenderResult {
    pub page: usize,
    pub scale: f32,
    pub tag: (u64, u64),
    pub image: Result<ColorImage, String>,
}

fn render_one<'a>(
    pdf: &'a Pdf,
    cache: &RenderCache<'a>,
    settings: &InterpreterSettings,
    page: usize,
    scale: f32,
) -> Result<ColorImage, String> {
    let page = pdf
        .pages()
        .get(page)
        .ok_or_else(|| "page not found".to_string())?;
    let pixmap = render(
        page,
        cache,
        settings,
        &RenderSettings::default(),
        &PixmapSettings {
            x_scale: scale,
            y_scale: scale,
            bg_color: WHITE,
        },
    );
    let size = [pixmap.width() as usize, pixmap.height() as usize];
    if size[0] == 0 || size[1] == 0 {
        return Err("empty page".to_string());
    }
    Ok(ColorImage::from_rgba_premultiplied(
        size,
        pixmap.data_as_u8_slice(),
    ))
}

/// Rasterize one page synchronously.
pub fn render_page(data: Arc<Vec<u8>>, page: usize, scale: f32) -> Result<ColorImage, String> {
    let pdf = Pdf::new(data).map_err(|e| format!("cannot render: {e:?}"))?;
    let cache = RenderCache::new();
    render_one(&pdf, &cache, &InterpreterSettings::default(), page, scale)
}

/// Handle to the render worker thread.
pub struct Renderer {
    tx: Sender<RenderRequest>,
    rx: Receiver<RenderResult>,
}

impl Renderer {
    /// Start the worker. `ctx` is repainted whenever a page finishes.
    pub fn new(ctx: Context) -> Self {
        let (tx, worker_rx) = channel::<RenderRequest>();
        let (worker_tx, rx) = channel::<RenderResult>();
        std::thread::Builder::new()
            .name("pdf-render".into())
            .spawn(move || worker(worker_rx, worker_tx, ctx))
            .expect("spawn render thread");
        Self { tx, rx }
    }

    pub fn request(&self, request: RenderRequest) {
        let _ = self.tx.send(request);
    }

    /// Finished pages since the last call.
    pub fn poll(&self) -> Vec<RenderResult> {
        self.rx.try_iter().collect()
    }
}

fn worker(rx: Receiver<RenderRequest>, tx: Sender<RenderResult>, ctx: Context) {
    let settings = InterpreterSettings::default();
    let mut pending: Option<RenderRequest> = None;
    loop {
        let mut request = match pending.take() {
            Some(r) => r,
            None => match rx.recv() {
                Ok(r) => r,
                Err(_) => return,
            },
        };
        let data = request.data.clone();
        let pdf = Pdf::new(data.clone());
        let cache = RenderCache::new();
        // Serve requests for this version of the file until a different one arrives.
        loop {
            let image = match &pdf {
                Ok(pdf) => render_one(pdf, &cache, &settings, request.page, request.scale),
                Err(e) => Err(format!("cannot render: {e:?}")),
            };
            let result = RenderResult {
                page: request.page,
                scale: request.scale,
                tag: request.tag,
                image,
            };
            if tx.send(result).is_err() {
                return;
            }
            ctx.request_repaint();
            match rx.recv() {
                Ok(next) if Arc::ptr_eq(&next.data, &data) => request = next,
                Ok(next) => {
                    pending = Some(next);
                    break;
                }
                Err(_) => return,
            }
        }
    }
}
