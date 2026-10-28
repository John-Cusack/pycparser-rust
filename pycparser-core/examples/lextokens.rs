//! Differential-test helper: lex a file, print one event per line.
//! Usage: lextokens <dialect: legacy|c23> <path>
use pycparser_rust_core::lexer::{LexEvent, Lexer};

fn main() {
    let dialect = std::env::args().nth(1).expect("dialect");
    let path = std::env::args().nth(2).expect("path");
    let text = std::fs::read_to_string(&path).expect("read");
    let mut lx = Lexer::new(dialect == "c23");
    lx.input(&text, &path);
    loop {
        match lx.next_event() {
            LexEvent::Token(t) => println!(
                "T|{}|{}|{}|{}|{}",
                t.kind.name(),
                escape(&t.value),
                t.lineno,
                t.column,
                t.filename
            ),
            LexEvent::Error(e, _) => {
                println!("E|{}|{}|{}", escape(&e.msg), e.line, e.column)
            }
            LexEvent::End => break,
        }
    }
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n")
}
