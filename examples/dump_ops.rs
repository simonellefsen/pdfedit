//! Print the operators of a page's content stream (debugging aid).
//! Usage: cargo run --example dump_ops -- file.pdf <page>

use lopdf::content::Content;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let doc = pdfedit::doc::PdfDoc::open(std::path::Path::new(&args[1])).unwrap();
    let page: usize = args[2].parse().unwrap();
    let id = doc.page_id(page - 1).unwrap();
    let content = Content::decode(&doc.inner().get_page_content(id)).unwrap();
    for (i, op) in content.operations.iter().enumerate() {
        println!("{i:5} {} {:?}", op.operator, op.operands);
    }
}
