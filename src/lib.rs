//! pdfedit: a simple PDF reader and editor.
//!
//! The library half holds everything that does not need a window: loading,
//! text-run extraction, text editing, saving and Markdown export.

pub mod content;
pub mod doc;
pub mod font;
pub mod headers;
pub mod markdown;
