//! Prints top-level nodes of the prepared parse buffer with their line ranges.
fn main() {
    let path = std::env::args().nth(1).expect("usage: sexp <file.c>");
    let src = extract::parse_buffer(&std::fs::read(path).unwrap());
    let mut p = tree_sitter::Parser::new();
    p.set_language(&tree_sitter_c::LANGUAGE.into()).unwrap();
    let t = p.parse(&src, None).unwrap();
    let mut c = t.root_node().walk();
    for n in t.root_node().children(&mut c) {
        let s = n.to_sexp();
        println!(
            "{:>5}-{:<5} {} {}",
            n.start_position().row + 1,
            n.end_position().row + 1,
            n.kind(),
            &s[..s.len().min(160)]
        );
    }
}
