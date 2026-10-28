//! Reusable Rust core: C lexer + recursive-descent parser.
//!
//! Clean-room port of the pycparser 3.12 hot unit (lexer + parser) to Rust.
//! The Python face (`pycparser` package) converts [`Node`] trees into the
//! genuine `c_ast` classes; this crate has no Python dependency.

pub mod ast;
pub mod lexer;
pub mod parser;
