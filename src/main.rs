//! pdfedit: window application plus a few headless commands.
//!
//! ```text
//! pdfedit [file.pdf]                                  open the editor
//! pdfedit runs <in.pdf> [page]                        list text runs
//! pdfedit set-text <in.pdf> <page> <run> <text> <out.pdf>
//! pdfedit headers <in.pdf>                             list headers/footers
//! pdfedit retitle <in.pdf> <old> <new> <out.pdf>        change a header/footer
//! pdfedit strip <in.pdf> <out.pdf> [--headers|--footers]
//! pdfedit markdown <in.pdf> <out.md> [--images]       export Markdown
//! pdfedit render <in.pdf> <page> <out.png> [scale]    rasterize a page
//! ```
//! Pages are numbered from 1, runs from 0.

mod app;
mod render;
mod script;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use eframe::egui;
use pdfedit::doc::PdfDoc;
use pdfedit::{headers, markdown};

fn page_arg(arg: Option<&String>) -> Result<usize> {
    let n: usize = arg.ok_or_else(|| anyhow!("missing page number"))?.parse()?;
    n.checked_sub(1).ok_or_else(|| anyhow!("pages start at 1"))
}

fn cli(args: &[String]) -> Result<()> {
    let input = args.get(1).ok_or_else(|| anyhow!("missing input file"))?;
    let mut doc = PdfDoc::open(Path::new(input))?;
    match args[0].as_str() {
        "runs" => {
            let pages = match args.get(2) {
                Some(_) => {
                    let p = page_arg(args.get(2))?;
                    p..p + 1
                }
                None => 0..doc.page_count(),
            };
            for page in pages {
                println!("== page {} ==", page + 1);
                for (i, run) in doc.scan(page).runs.iter().enumerate() {
                    println!(
                        "{i:4}  {:6.1}pt  {:24}  {:?}",
                        run.size, run.font_base, run.text
                    );
                }
            }
        }
        "set-text" => {
            let page = page_arg(args.get(2))?;
            let run: usize = args.get(3).ok_or_else(|| anyhow!("missing run"))?.parse()?;
            let text = args.get(4).ok_or_else(|| anyhow!("missing text"))?;
            let out = args.get(5).ok_or_else(|| anyhow!("missing output file"))?;
            let outcome = doc.set_run_text(page, run, text)?;
            doc.save(Path::new(out))?;
            println!("{outcome:?}");
        }
        "headers" => {
            for group in headers::detect(&mut doc) {
                println!(
                    "{:?}  {:3} pages  {:?}",
                    group.zone,
                    group.page_count(),
                    group.template
                );
            }
        }
        "retitle" => {
            // retitle <in> <old template> <new template> <out>
            let old = args.get(2).ok_or_else(|| anyhow!("missing old text"))?;
            let new = args.get(3).ok_or_else(|| anyhow!("missing new text"))?;
            let out = args.get(4).ok_or_else(|| anyhow!("missing output file"))?;
            let groups = headers::detect(&mut doc);
            let group = groups
                .iter()
                .find(|g| &g.template == old)
                .ok_or_else(|| anyhow!("no header or footer reads {old:?}"))?;
            let fallbacks = doc.set_many(&group.edits(new))?;
            doc.save(Path::new(out))?;
            println!(
                "changed {} pages ({fallbacks} with a standard font)",
                group.page_count()
            );
        }
        "strip" => {
            let out = args.get(2).ok_or_else(|| anyhow!("missing output file"))?;
            let only = |flag: &str| args.iter().any(|a| a == flag);
            let (h, f) = match (only("--headers"), only("--footers")) {
                (false, false) => (true, true),
                other => other,
            };
            let removed = headers::remove(&mut doc, h, f)?;
            doc.save(Path::new(out))?;
            println!("changed {removed} pages");
        }
        "markdown" => {
            let out = args.get(2).ok_or_else(|| anyhow!("missing output file"))?;
            let images = args.iter().any(|a| a == "--images");
            markdown::export_to_file(&mut doc, Path::new(out), images)?;
        }
        "render" => {
            let page = page_arg(args.get(2))?;
            let out = args.get(3).ok_or_else(|| anyhow!("missing output file"))?;
            let scale: f32 = args.get(4).map(|s| s.parse()).transpose()?.unwrap_or(1.5);
            let image = render::render_page(Arc::new(doc.to_bytes()?), page, scale)
                .map_err(|e| anyhow!(e))?;
            image::save_buffer(
                out,
                image.as_raw(),
                image.width() as u32,
                image.height() as u32,
                image::ExtendedColorType::Rgba8,
            )
            .context("writing PNG")?;
        }
        other => bail!("unknown command {other}"),
    }
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(first) = args.first()
        && [
            "runs", "set-text", "markdown", "render", "headers", "strip", "retitle",
        ]
        .contains(&first.as_str())
    {
        return cli(&args);
    }
    let path = args.first().map(PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("pdfedit")
            .with_inner_size([1100.0, 900.0]),
        ..Default::default()
    };
    eframe::run_native(
        "pdfedit",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, path)))),
    )
    .map_err(|e| anyhow!("{e}"))
}
