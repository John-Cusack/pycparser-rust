# pycparser-rust

Rust port of [pycparser](https://github.com/eliben/pycparser) (C parser in
Python, 3.12): recursive-descent C lexer + parser in a reusable
`pycparser-rust-core` crate with a thin `pycparser` Python binding (PyO3,
same import name — drop-in for `pip install pycparser`).

Upstream: eliben/pycparser, BSD-3-Clause, by Eli Bendersky. This port
ships under MIT; see NOTICE for attribution. This is a clean-room
implementation: it incorporates no upstream code, but behavior, message
texts, coordinates, and the AST shape are modelled on pycparser 3.12 for
drop-in compatibility.

## Layout

- `pycparser-core/` — C lexer + recursive-descent parser
  (`pycparser_rust_core`, no Python dependency; reusable from Rust).
- `src/lib.rs` — PyO3 bindings (`pycparser._core`: `parse_ast`, `LexerState`).
- `pycparser/` — Python face: upstream `c_ast`/`c_generator`/
  `ast_transforms` shipped verbatim plus an accelerator seam in
  `c_lexer.py`/`c_parser.py` (Rust-backed `CLexer`/`CParser`, pure-Python
  `_PureCLexer`/`_PureCParser` fallback, see `specs/seam.md`).
- `tests/c_files/` + `tests/test_*.py` — the full upstream 3.12 suite,
  run unmodified against the port.
- `tests/test_port_equiv.py` — equivalence corpus: pure-vs-fast `show()`
  dumps, `CGenerator` output, coordinates, and `ParseError` texts.
- `tests/test_port_seam.py` — seam behavior (fallback, custom lexers,
  dialect validation, error-callback order).
- `benches/two_track.py` — two-track wall/RSS bench (measured only).

## Parity

`venv/bin/python -m pytest tests/` against this package: 451 passed +
500 subtests (the complete upstream 3.12 suite) plus 14 port tests,
all green. Equivalence corpus: real preprocessed TUs, synthetic
grammar-covering TUs, error cases, and the c23 dialect — identical trees,
generator output, and diagnostics on both sides.

## API

Drop-in: `from pycparser import CParser`, `parse_file`, `c_ast`,
`c_generator`, `CLexer` all keep their upstream signatures and semantics,
including `ParseError` messages, coordinates, `#line` handling, dialects
(`legacy` default, `c23`), and `NodeVisitor`/`CGenerator` behavior.
`CParser(lexer=CustomLexer)` and a missing extension (or
`PYC_PARSER_NO_RUST=1`) take the pure-Python path automatically.

## Benches

| input | parse speedup | round-trip speedup |
| --- | --- | --- |
| memmgr_with_h.c (4 KB, real) | 5.5x | 2.6x |
| cppd_with_stdio_h.c (30 KB, real preprocessed TU) | 6.3x | 4.3x |
| synth-small decl+func (~7 KB) | 8.0x | 4.1x |
| synth-big func-heavy (~390 KB) | 2.4x | 1.9x |

Ranges are min–max across three full runs on 2026-10-28 (box shared with
other lanes, load avg ~5–8; per-run tables vary with contention — rerun
`benches/two_track.py` to reproduce). Representative-TU parse clears the
2x rewrite bar in every run (min 2.4x on the 390 KB TU; 4–8x on real
files and the headroom probe workload).

Peak RSS is comparable both sides (within ~10% on small inputs; the
extension holds the Rust tree alongside the converted tree, ~1.5x on
the 390 KB input — see the bench table).

## Development

```sh
cargo build                    # core + bindings (debug)
cargo test --workspace         # Rust unit tests
cargo clippy --all-targets -- -D warnings
maturin develop                # editable install into the active venv
venv/bin/python -m pytest tests/
venv/bin/python benches/two_track.py
```

## License

MIT (see LICENSE). Upstream attribution in NOTICE.
