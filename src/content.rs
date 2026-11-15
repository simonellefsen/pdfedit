//! Content-stream interpretation: finds text runs and image placements on a
//! page, and rewrites the text-showing operator of a run.

use std::collections::HashMap;
use std::rc::Rc;

use lopdf::content::Operation;
use lopdf::{Object, StringFormat};

use crate::font::{FontInfo, win_ansi_encode};

/// A PDF transformation matrix `[a b c d e f]`.
pub type Matrix = [f64; 6];

const IDENTITY: Matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// `a` followed by `b`.
fn mul(a: &Matrix, b: &Matrix) -> Matrix {
    [
        a[0] * b[0] + a[1] * b[2],
        a[0] * b[1] + a[1] * b[3],
        a[2] * b[0] + a[3] * b[2],
        a[2] * b[1] + a[3] * b[3],
        a[4] * b[0] + a[5] * b[2] + b[4],
        a[4] * b[1] + a[5] * b[3] + b[5],
    ]
}

fn apply(m: &Matrix, x: f64, y: f64) -> [f32; 2] {
    [
        (m[0] * x + m[2] * y + m[4]) as f32,
        (m[1] * x + m[3] * y + m[5]) as f32,
    ]
}

/// One text-showing operator (`Tj`, `TJ`, `'`, `"`) on a page.
#[derive(Debug, Clone)]
pub struct TextRun {
    /// Index of the operator in the page's decoded content.
    pub op_index: usize,
    /// Decoded Unicode text.
    pub text: String,
    /// Font resource name (the `Tf` operand).
    pub font_res: Vec<u8>,
    /// Base font name, for display.
    pub font_base: String,
    /// The `Tf` size operand.
    pub tf_size: f32,
    /// Effective glyph size in user-space units.
    pub size: f32,
    /// Corners in user space: bottom-left, bottom-right, top-right, top-left.
    pub quad: [[f32; 2]; 4],
    /// Baseline start and end in user space.
    pub baseline: [[f32; 2]; 2],
}

/// An image XObject painted with `Do`.
#[derive(Debug, Clone)]
pub struct ImagePlacement {
    /// XObject resource name.
    pub name: Vec<u8>,
    /// Corners of the unit square in user space.
    pub quad: [[f32; 2]; 4],
}

/// A thin straight line (a rule), such as the one under a running header.
#[derive(Debug, Clone)]
pub struct Rule {
    /// First and last operator (inclusive) that draw it.
    pub ops: (usize, usize),
    /// Corners of its bounding box in user space.
    pub quad: [[f32; 2]; 4],
}

/// Result of scanning a page's content.
#[derive(Debug, Clone, Default)]
pub struct PageScan {
    pub runs: Vec<TextRun>,
    pub images: Vec<ImagePlacement>,
    pub rules: Vec<Rule>,
}

/// The path under construction, while it is still a lone rectangle or a
/// single line segment drawn by consecutive operators.
struct SimplePath {
    start: usize,
    ops: usize,
    points: Vec<[f32; 2]>,
}

#[derive(Clone)]
struct State {
    ctm: Matrix,
    font_res: Vec<u8>,
    font: Option<Rc<FontInfo>>,
    size: f64,
    char_spacing: f64,
    word_spacing: f64,
    h_scale: f64,
    leading: f64,
    rise: f64,
}

fn num(obj: &Object) -> f64 {
    match obj {
        Object::Integer(i) => *i as f64,
        Object::Real(r) => f64::from(*r),
        _ => 0.0,
    }
}

fn nums<const N: usize>(ops: &[Object]) -> Option<[f64; N]> {
    if ops.len() < N {
        return None;
    }
    let mut out = [0.0; N];
    for (o, v) in out.iter_mut().zip(ops) {
        *o = num(v);
    }
    Some(out)
}

/// TJ adjustments more negative than this (1/1000 em) read as a word space.
const SPACE_KERN: f64 = -180.0;

/// Walk the operators of a page and collect text runs and image placements.
pub fn scan(ops: &[Operation], fonts: &HashMap<Vec<u8>, Rc<FontInfo>>) -> PageScan {
    let mut out = PageScan::default();
    let mut state = State {
        ctm: IDENTITY,
        font_res: Vec::new(),
        font: None,
        size: 0.0,
        char_spacing: 0.0,
        word_spacing: 0.0,
        h_scale: 1.0,
        leading: 0.0,
        rise: 0.0,
    };
    let mut stack: Vec<State> = Vec::new();
    let mut tm = IDENTITY;
    let mut tlm = IDENTITY;
    let mut path: Option<SimplePath> = None;
    let mut path_ops = 0usize;

    for (index, op) in ops.iter().enumerate() {
        let a = &op.operands;
        match op.operator.as_str() {
            "re" | "m" | "l" | "c" | "v" | "y" | "h" => {
                let m = &state.ctm;
                match (op.operator.as_str(), path_ops, &mut path) {
                    ("re", 0, _) => {
                        if let Some([x, y, w, h]) = nums::<4>(a) {
                            path = Some(SimplePath {
                                start: index,
                                ops: 1,
                                points: vec![
                                    apply(m, x, y),
                                    apply(m, x + w, y),
                                    apply(m, x + w, y + h),
                                    apply(m, x, y + h),
                                ],
                            });
                        }
                    }
                    ("m", 0, _) => {
                        if let Some([x, y]) = nums::<2>(a) {
                            path = Some(SimplePath {
                                start: index,
                                ops: 1,
                                points: vec![apply(m, x, y)],
                            });
                        }
                    }
                    ("l", 1, Some(p)) if p.points.len() == 1 => {
                        if let Some([x, y]) = nums::<2>(a) {
                            p.points.push(apply(m, x, y));
                            p.ops = 2;
                        }
                    }
                    _ => path = None,
                }
                path_ops += 1;
            }
            "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" | "S" | "s" | "n" => {
                if let Some(p) = path.take()
                    && op.operator != "n"
                    && index == p.start + p.ops
                    && p.points.len() >= 2
                {
                    let xs = p.points.iter().map(|q| q[0]);
                    let ys = p.points.iter().map(|q| q[1]);
                    let (x0, x1) = (
                        xs.clone().fold(f32::MAX, f32::min),
                        xs.fold(f32::MIN, f32::max),
                    );
                    let (y0, y1) = (
                        ys.clone().fold(f32::MAX, f32::min),
                        ys.fold(f32::MIN, f32::max),
                    );
                    let (w, h) = (x1 - x0, y1 - y0);
                    if w.min(h) <= 2.0 && w.max(h) >= 40.0 {
                        out.rules.push(Rule {
                            ops: (p.start, index),
                            quad: [[x0, y0], [x1, y0], [x1, y1], [x0, y1]],
                        });
                    }
                }
                path_ops = 0;
            }
            "q" => stack.push(state.clone()),
            "Q" => {
                if let Some(s) = stack.pop() {
                    state = s;
                }
            }
            "cm" => {
                if let Some(m) = nums::<6>(a) {
                    state.ctm = mul(&m, &state.ctm);
                }
            }
            "BT" => {
                tm = IDENTITY;
                tlm = IDENTITY;
            }
            "Tf" => {
                if let (Some(Object::Name(name)), Some(size)) = (a.first(), a.get(1)) {
                    state.font_res = name.clone();
                    state.font = fonts.get(name).cloned();
                    state.size = num(size);
                }
            }
            "Tc" => state.char_spacing = a.first().map(num).unwrap_or(0.0),
            "Tw" => state.word_spacing = a.first().map(num).unwrap_or(0.0),
            "Tz" => state.h_scale = a.first().map(num).unwrap_or(100.0) / 100.0,
            "TL" => state.leading = a.first().map(num).unwrap_or(0.0),
            "Ts" => state.rise = a.first().map(num).unwrap_or(0.0),
            "Td" | "TD" => {
                if let Some([tx, ty]) = nums::<2>(a) {
                    if op.operator == "TD" {
                        state.leading = -ty;
                    }
                    tlm = mul(&[1.0, 0.0, 0.0, 1.0, tx, ty], &tlm);
                    tm = tlm;
                }
            }
            "Tm" => {
                if let Some(m) = nums::<6>(a) {
                    tm = m;
                    tlm = m;
                }
            }
            "T*" => next_line(&state, &mut tm, &mut tlm),
            "Tj" => {
                if let Some(s) = a.first() {
                    show(&mut out, index, &state, &mut tm, std::slice::from_ref(s));
                }
            }
            "'" => {
                next_line(&state, &mut tm, &mut tlm);
                if let Some(s) = a.first() {
                    show(&mut out, index, &state, &mut tm, std::slice::from_ref(s));
                }
            }
            "\"" => {
                if a.len() >= 3 {
                    state.word_spacing = num(&a[0]);
                    state.char_spacing = num(&a[1]);
                    next_line(&state, &mut tm, &mut tlm);
                    show(&mut out, index, &state, &mut tm, &a[2..3]);
                }
            }
            "TJ" => {
                if let Some(Object::Array(items)) = a.first() {
                    show(&mut out, index, &state, &mut tm, items);
                }
            }
            "Do" => {
                if let Some(Object::Name(name)) = a.first() {
                    let m = &state.ctm;
                    out.images.push(ImagePlacement {
                        name: name.clone(),
                        quad: [
                            apply(m, 0.0, 0.0),
                            apply(m, 1.0, 0.0),
                            apply(m, 1.0, 1.0),
                            apply(m, 0.0, 1.0),
                        ],
                    });
                }
            }
            _ => {}
        }
    }
    out
}

fn next_line(state: &State, tm: &mut Matrix, tlm: &mut Matrix) {
    *tlm = mul(&[1.0, 0.0, 0.0, 1.0, 0.0, -state.leading], tlm);
    *tm = *tlm;
}

fn show(out: &mut PageScan, index: usize, state: &State, tm: &mut Matrix, items: &[Object]) {
    let fallback;
    let font: &FontInfo = match &state.font {
        Some(f) => f,
        None => {
            fallback = FontInfo::default();
            &fallback
        }
    };
    let mut text = String::new();
    let mut advance = 0.0f64;
    for item in items {
        match item {
            Object::String(bytes, _) => {
                for code in font.codes(bytes) {
                    match font.decode(code) {
                        Some(s) => text.push_str(s),
                        None => text.push('\u{FFFD}'),
                    }
                    let mut tx =
                        f64::from(font.width(code)) / 1000.0 * state.size + state.char_spacing;
                    if code == 32 && !font.two_byte {
                        tx += state.word_spacing;
                    }
                    advance += tx * state.h_scale;
                }
            }
            Object::Integer(_) | Object::Real(_) => {
                let n = num(item);
                if n < SPACE_KERN && !text.is_empty() && !text.ends_with(' ') {
                    text.push(' ');
                }
                advance -= n / 1000.0 * state.size * state.h_scale;
            }
            _ => {}
        }
    }

    let m = mul(tm, &state.ctm);
    let (low, high) = (
        state.rise - 0.22 * state.size,
        state.rise + 0.80 * state.size,
    );
    if !text.trim().is_empty() {
        out.runs.push(TextRun {
            op_index: index,
            text,
            font_res: state.font_res.clone(),
            font_base: font.base_name.clone(),
            tf_size: state.size as f32,
            size: (state.size.abs() * m[2].hypot(m[3])) as f32,
            quad: [
                apply(&m, 0.0, low),
                apply(&m, advance, low),
                apply(&m, advance, high),
                apply(&m, 0.0, high),
            ],
            baseline: [apply(&m, 0.0, state.rise), apply(&m, advance, state.rise)],
        });
    }
    *tm = mul(&[1.0, 0.0, 0.0, 1.0, advance, 0.0], tm);
}

/// How a run's text was written back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditOutcome {
    /// Encoded with the run's own font.
    OriginalFont,
    /// The font lacked glyphs for these characters; a standard font was used.
    FallbackFont(Vec<char>),
}

fn string_object(bytes: Vec<u8>, hex: bool) -> Object {
    let format = if hex {
        StringFormat::Hexadecimal
    } else {
        StringFormat::Literal
    };
    Object::String(bytes, format)
}

/// Width of TJ items in text-space units for a font size.
fn items_width(items: &[Object], font: &FontInfo, size: f32) -> f32 {
    let mut width = 0.0;
    for item in items {
        match item {
            Object::String(bytes, _) => {
                for code in font.codes(bytes) {
                    width += font.width(code) / 1000.0 * size;
                }
            }
            other => width -= num(other) as f32 / 1000.0 * size,
        }
    }
    width
}

/// Replace the text shown by `run` with `text`.
///
/// `fallback` is the resource name and metrics of a WinAnsi standard font
/// that is used when the run's own font cannot encode the new text.
///
/// `fit` keeps centred or right-aligned text in place: it is the width (in
/// text-space units) the text used to span and the share of any width
/// change to shift the start by (0.5 centres, 1.0 keeps the right edge).
pub fn replace_run_text(
    ops: &mut Vec<Operation>,
    run: &TextRun,
    font: Option<&FontInfo>,
    text: &str,
    fallback: (&[u8], &FontInfo),
    fit: Option<(f32, f32)>,
) -> EditOutcome {
    let old = ops[run.op_index].clone();
    let mut new_ops: Vec<Operation> = Vec::new();

    // `'` and `"` also move to the next line; spell that out so the show
    // operator itself can be swapped freely.
    match old.operator.as_str() {
        "'" => new_ops.push(Operation::new("T*", vec![])),
        "\"" => {
            new_ops.push(Operation::new("Tw", vec![old.operands[0].clone()]));
            new_ops.push(Operation::new("Tc", vec![old.operands[1].clone()]));
            new_ops.push(Operation::new("T*", vec![]));
        }
        _ => {}
    }

    let show = |mut items: Vec<Object>, font: &FontInfo| {
        if let Some((old_width, share)) = fit
            && run.tf_size != 0.0
            && !items.is_empty()
        {
            let shift = (old_width - items_width(&items, font, run.tf_size)) * share;
            items.insert(0, Object::Real(-shift / run.tf_size * 1000.0));
        }
        if let [single @ Object::String(..)] = items.as_slice() {
            Operation::new("Tj", vec![single.clone()])
        } else {
            Operation::new("TJ", vec![Object::Array(items)])
        }
    };

    let encoded = font.and_then(|font| Some((encode_with_font(font, text)?, font)));
    let outcome = match encoded {
        Some((items, font)) => {
            new_ops.push(show(items, font));
            EditOutcome::OriginalFont
        }
        None => {
            let mut missing: Vec<char> = Vec::new();
            for c in text.chars() {
                if font.is_some_and(|f| !f.can_encode(c)) && !missing.contains(&c) {
                    missing.push(c);
                }
            }
            let size = Object::Real(run.tf_size);
            new_ops.push(Operation::new(
                "Tf",
                vec![Object::Name(fallback.0.to_vec()), size.clone()],
            ));
            new_ops.push(show(
                vec![string_object(win_ansi_encode(text), false)],
                fallback.1,
            ));
            new_ops.push(Operation::new(
                "Tf",
                vec![Object::Name(run.font_res.clone()), size],
            ));
            EditOutcome::FallbackFont(missing)
        }
    };
    ops.splice(run.op_index..=run.op_index, new_ops);
    outcome
}

/// Encode with the run's font, as `TJ` items. Spaces the font has no glyph
/// for become kerning gaps, which is how such PDFs space words anyway.
fn encode_with_font(font: &FontInfo, text: &str) -> Option<Vec<Object>> {
    if let Ok(bytes) = font.encode(text) {
        return Some(vec![string_object(bytes, font.two_byte)]);
    }
    if font.can_encode(' ') {
        return None;
    }
    let mut items = Vec::new();
    for (i, word) in text.split(' ').enumerate() {
        if i > 0 {
            items.push(Object::Integer(-280));
        }
        if !word.is_empty() {
            items.push(string_object(font.encode(word).ok()?, font.two_byte));
        }
    }
    Some(items)
}
