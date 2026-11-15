# pdfedit

A simple PDF reader and editor written in Rust.

## What works today

- **Read and present**: open a PDF (button, `Cmd+O`, drag and drop, or as a
  command-line argument), scroll through pages, zoom (buttons, `Cmd +/-`,
  pinch), fit to width, jump to a page.
- **Edit text**: hover highlights a text run, click to edit it in place,
  `Enter` applies, `Esc` cancels. Undo/redo with `Cmd+Z` / `Shift+Cmd+Z`.
- **Headers and footers**: the *Headers & Footers* menu removes all running
  headers, all footers (page numbers included), or both, across the whole
  document in one undoable step. *Edit…* lists what was found and lets you
  reword it; `{n}` stands for the number that changes per page, so `{n}` can
  become `Page {n}`.
- **Save**: back to PDF (`Cmd+S`, or *Save As…*).
- **Export Markdown**: text only, or with images extracted into a
  `<name>_images/` folder next to the `.md` file. Headings are inferred from
  font size.

## Run

```bash
cargo run --release -- path/to/file.pdf
```

Headless commands (pages count from 1, runs from 0):

```bash
pdfedit runs in.pdf [page]                      # list text runs
pdfedit set-text in.pdf <page> <run> "text" out.pdf
pdfedit headers in.pdf                          # list headers/footers found
pdfedit strip in.pdf out.pdf [--headers|--footers]
pdfedit retitle in.pdf "Page {n}" "{n}" out.pdf # reword one of them
pdfedit markdown in.pdf out.md [--images]
pdfedit render in.pdf <page> out.png [scale]
```

## How text editing works, and its limits

A "run" is one text-showing operator in the page's content stream, usually a
line or part of a line. Editing rewrites that operator only:

- Text is re-encoded with the run's own font when the font has glyphs for
  every character. Most PDFs embed only the glyphs they use, so new characters
  are often missing; the run is then switched to the closest standard font
  (Helvetica, Times or Courier, with bold/italic matched) and the status bar
  says so.
- Nothing reflows. Text after the edited run on the same line keeps its
  position, so longer replacements can overlap what follows.
- Text inside form XObjects, and fonts without a usable Unicode map, cannot be
  edited yet.
- Encrypted PDFs are not supported.

### Headers and footers

A header or footer is text on the first or last two lines of a page, inside
the top or bottom 10% of it, no larger than body text, that repeats on other
pages (numbers may differ). Text found on only a few pages counts only when it
sits at the same height as one that runs through the document, so section
titles near the top of a page are left alone. Thin rules at a fixed height in
the same margins are removed along with the text. Logos and other graphics
are not. Reworded text keeps its left, centre or right anchoring, but a PDF
that clips its header area (Chrome's printed headers do) will cut off text
that grows past the clip.

Markdown export reads lines top to bottom, so multi-column layouts come out
interleaved.

## Not built yet

Image editing, adding/deleting/re-arranging pages, page and footnote
re-numbering, a prompt for unsaved changes on close.

## Layout

| File | Role |
| --- | --- |
| `src/font.rs` | character code ↔ Unicode maps, glyph widths |
| `src/content.rs` | content-stream walker (text runs, images), run rewriting |
| `src/doc.rs` | document model: pages, edits, undo/redo, save |
| `src/headers.rs` | header/footer detection, removal and rewording |
| `src/markdown.rs` | Markdown and image export |
| `src/render.rs` | page rasterization on a worker thread (`hayro`) |
| `src/app.rs` | the window (`egui`/`eframe`) |
| `src/script.rs` | scripted UI smoke tests via `PDFEDIT_SCRIPT` |

## Develop

```bash
cargo test
cargo clippy --all-targets
```

`PDFEDIT_SCRIPT` plays input back into the window and saves screenshots, e.g.
`PDFEDIT_SCRIPT="click:341,107;type: 2026;key:Enter;shot:/tmp/a.png;quit"`.
