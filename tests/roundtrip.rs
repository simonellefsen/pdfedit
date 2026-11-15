//! End-to-end checks: build a small PDF, read it, edit it, save and reload.

use lopdf::content::{Content, Operation};
use lopdf::{Document, Object, Stream, dictionary};
use pdfedit::content::EditOutcome;
use pdfedit::doc::PdfDoc;
use pdfedit::markdown::{MarkdownOptions, export};

fn text_op(font: &str, size: i64, x: i64, y: i64, show: Operation) -> Vec<Operation> {
    vec![
        Operation::new("BT", vec![]),
        Operation::new("Tf", vec![font.into(), size.into()]),
        Operation::new("Td", vec![x.into(), y.into()]),
        show,
        Operation::new("ET", vec![]),
    ]
}

fn sample() -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
        "Encoding" => "WinAnsiEncoding",
    });
    let resources = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font_id } });
    let mut kids = Vec::new();
    for page in 0..2 {
        let mut ops = Vec::new();
        ops.extend(text_op(
            "F1",
            24,
            72,
            720,
            Operation::new(
                "Tj",
                vec![Object::string_literal(format!("Title {}", page + 1))],
            ),
        ));
        ops.extend(text_op(
            "F1",
            12,
            72,
            680,
            Operation::new(
                "TJ",
                vec![Object::Array(vec![
                    Object::string_literal("Hello"),
                    Object::Integer(-300),
                    Object::string_literal("wor"),
                    Object::Integer(-20),
                    Object::string_literal("ld"),
                ])],
            ),
        ));
        ops.extend(text_op(
            "F1",
            12,
            72,
            666,
            Operation::new("Tj", vec![Object::string_literal("second line")]),
        ));
        let content = Content { operations: ops };
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => content_id,
        });
        kids.push(Object::Reference(page_id));
    }
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages", "Kids" => kids, "Count" => 2,
            "Resources" => resources,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        }),
    );
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog);
    let mut out = Vec::new();
    doc.save_to(&mut out).unwrap();
    out
}

fn texts(doc: &mut PdfDoc, page: usize) -> Vec<String> {
    doc.scan(page).runs.iter().map(|r| r.text.clone()).collect()
}

#[test]
fn reads_runs_and_geometry() {
    let mut doc = PdfDoc::from_bytes(&sample()).unwrap();
    assert_eq!(doc.page_count(), 2);
    assert_eq!(doc.geom(0).view_size(), [612.0, 792.0]);
    assert_eq!(
        texts(&mut doc, 1),
        ["Title 2", "Hello world", "second line"]
    );

    let scan = doc.scan(0);
    let title = &scan.runs[0];
    assert_eq!(title.size, 24.0);
    assert_eq!(title.quad[0][0], 72.0);
    // "Title 1" in 24pt Helvetica is about 65pt wide.
    let width = title.quad[1][0] - title.quad[0][0];
    assert!((63.0..68.0).contains(&width), "width {width}");
    let top = doc.geom(0).to_view(title.baseline[0]);
    assert_eq!(top, [72.0, 72.0]);
}

#[test]
fn edits_undo_redo_and_survives_save() {
    let mut doc = PdfDoc::from_bytes(&sample()).unwrap();
    let outcome = doc.set_run_text(0, 1, "Goodbye (edited) world").unwrap();
    assert_eq!(outcome, EditOutcome::OriginalFont);
    assert!(doc.dirty);
    assert_eq!(texts(&mut doc, 0)[1], "Goodbye (edited) world");
    assert_eq!(
        texts(&mut doc, 1)[1],
        "Hello world",
        "other pages untouched"
    );

    assert_eq!(doc.undo(), Some(vec![0]));
    assert_eq!(texts(&mut doc, 0)[1], "Hello world");
    assert_eq!(doc.redo(), Some(vec![0]));
    assert_eq!(texts(&mut doc, 0)[1], "Goodbye (edited) world");

    let path = std::env::temp_dir().join(format!("pdfedit-test-{}.pdf", std::process::id()));
    doc.save(&path).unwrap();
    assert!(!doc.dirty);
    let mut reloaded = PdfDoc::open(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert_eq!(
        texts(&mut reloaded, 0),
        ["Title 1", "Goodbye (edited) world", "second line"]
    );
}

#[test]
fn falls_back_to_standard_font_for_missing_glyphs() {
    let mut doc = PdfDoc::from_bytes(&sample()).unwrap();
    let outcome = doc.set_run_text(0, 2, "caf\u{e9} \u{65e5}").unwrap();
    assert_eq!(outcome, EditOutcome::FallbackFont(vec!['\u{65e5}']));
    let scan = doc.scan(0);
    assert_eq!(scan.runs[2].text, "caf\u{e9} ?");
    assert_eq!(scan.runs[2].font_base, "Helvetica");
    // The font switch must not leak into later text.
    let bytes = doc.to_bytes().unwrap();
    let mut again = PdfDoc::from_bytes(&bytes).unwrap();
    assert_eq!(texts(&mut again, 0).len(), 3);
}

#[test]
fn exports_markdown_with_headings() {
    let mut doc = PdfDoc::from_bytes(&sample()).unwrap();
    let md = export(&mut doc, &MarkdownOptions::default()).unwrap();
    assert_eq!(
        md,
        "# Title 1\n\nHello world second line\n\n<!-- page 2 -->\n\n# Title 2\n\nHello world second line\n"
    );
}

/// Three pages with a running header, a rule under it, body text and a
/// page-number footer.
fn book() -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
        "Encoding" => "WinAnsiEncoding",
    });
    let resources = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font_id } });
    let mut kids = Vec::new();
    for page in 1..=3 {
        let show = |s: String| Operation::new("Tj", vec![Object::string_literal(s)]);
        let mut ops = text_op("F1", 9, 72, 770, show("My Book".into()));
        ops.push(Operation::new(
            "re",
            vec![60.into(), 762.into(), 480.into(), Object::Real(0.5)],
        ));
        ops.push(Operation::new("f", vec![]));
        ops.extend(text_op(
            "F1",
            12,
            72,
            600,
            show(format!("Body of page {page}")),
        ));
        ops.extend(text_op("F1", 12, 72, 580, show("Chapter 12 begins".into())));
        ops.extend(text_op("F1", 9, 500, 30, show(format!("Page {page}"))));
        let content = Content { operations: ops };
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => content_id,
        });
        kids.push(Object::Reference(page_id));
    }
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages", "Kids" => kids, "Count" => 3,
            "Resources" => resources,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        }),
    );
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog);
    let mut out = Vec::new();
    doc.save_to(&mut out).unwrap();
    out
}

#[test]
fn detects_headers_and_page_number_footers() {
    use pdfedit::headers::{Zone, detect};
    let mut doc = PdfDoc::from_bytes(&book()).unwrap();
    let groups = detect(&mut doc);
    let found: Vec<(Zone, &str, usize)> = groups
        .iter()
        .map(|g| (g.zone, g.template.as_str(), g.page_count()))
        .collect();
    assert_eq!(
        found,
        [(Zone::Header, "My Book", 3), (Zone::Footer, "Page {n}", 3)]
    );
}

#[test]
fn removes_headers_footers_and_rules_in_one_undo_step() {
    let mut doc = PdfDoc::from_bytes(&book()).unwrap();
    assert_eq!(doc.scan(1).rules.len(), 1);
    assert_eq!(pdfedit::headers::remove(&mut doc, true, true).unwrap(), 3);
    for page in 0..3 {
        assert_eq!(
            texts(&mut doc, page),
            [
                format!("Body of page {}", page + 1),
                "Chapter 12 begins".to_string()
            ]
        );
        assert!(doc.scan(page).rules.is_empty());
    }
    assert!(pdfedit::headers::detect(&mut doc).is_empty());

    assert_eq!(doc.undo(), Some(vec![0, 1, 2]));
    assert_eq!(texts(&mut doc, 2)[0], "My Book");
    assert_eq!(texts(&mut doc, 2)[3], "Page 3");
    assert_eq!(doc.scan(2).rules.len(), 1);
    assert!(!doc.can_undo());

    // Footers only: the header and its rule stay.
    pdfedit::headers::remove(&mut doc, false, true).unwrap();
    assert_eq!(
        texts(&mut doc, 0),
        ["My Book", "Body of page 1", "Chapter 12 begins"]
    );
    assert_eq!(doc.scan(0).rules.len(), 1);
}

#[test]
fn rewrites_a_footer_keeping_each_page_number() {
    let mut doc = PdfDoc::from_bytes(&book()).unwrap();
    let groups = pdfedit::headers::detect(&mut doc);
    let footer = &groups[1];
    doc.set_many(&footer.edits("Side {n} af 3")).unwrap();
    assert_eq!(texts(&mut doc, 0)[3], "Side 1 af 3");
    assert_eq!(texts(&mut doc, 2)[3], "Side 3 af 3");
    // The footer sits near the right edge, so it keeps its right edge.
    let scan = doc.scan(2);
    let right = scan.runs[3].quad[1][0];
    assert!((right - 528.5).abs() < 0.5, "right edge {right}");
}
