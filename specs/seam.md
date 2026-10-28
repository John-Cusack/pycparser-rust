# pycparser-rust seam contract

The hot unit (lexer + recursive-descent parser + AST construction) runs
in the Rust core. Everything else stays Python behind a documented seam.

## What is ported

- `pycparser-core` (`lexer.rs`, `parser.rs`, `ast.rs`): tokenization,
  whole-TU parsing, and AST construction. The whole translation unit
  crosses the FFI boundary exactly once (string in); per-token callbacks
  into Python never happen in the parse loop. No `unsafe` outside the
  PyO3 boundary; no hot-path allocation beyond one token string and one
  AST node per grammar item.
- `src/lib.rs` (PyO3): `parse_ast` (parse + convert to genuine `c_ast`
  objects) and `LexerState` (one `token()` step per event so the facade
  invokes `error_func` in the exact upstream order).

## What stays Python (verbatim upstream 3.12, except the seam blocks)

- `c_ast.py` (generated node classes), `c_generator.py` (tree-walking C
  emitter), `ast_transforms.py` (switch-case/atomic fixes, applied to
  converted objects at the same call sites as upstream),
  `_c_ast.cfg`, `__init__.py` (`parse_file`/`preprocess_file` unchanged).
- `c_lexer.py` + `c_parser.py`: verbatim upstream code plus a trailing
  accelerator block each (clearly marked). The block rebinds `CLexer` /
  `CParser` to Rust-backed subclasses when `_core` imports (and
  `PYC_PARSER_NO_RUST` is unset); the pure implementations stay
  importable as `_PureCLexer` / `_PureCParser`.

## Fallback rule

- Extension missing or `PYC_PARSER_NO_RUST=1` → pure-Python classes serve
  the full API (the upstream suite passes unmodified in this mode).
- `CParser(lexer=CustomLexer)` with a non-default lexer class → the
  pure-Python parse path owns that instance (the core cannot drive
  Python callbacks in its hot loop). Default and pure lexer classes take
  the Rust path with identical token streams.
- A raising `error_func` aborts lexing with the lexer positioned exactly
  where upstream leaves it (resume positions apply lazily); continuing
  to drive a lexer after a raising `error_func` is outside the contract.

## Fidelity notes (all covered by tests)

- Error strings and coordinates are byte-identical to upstream
  (`ParseError` text, `Coord` rendering, `...: At end of input`,
  `None: ...` for missing coordinates, `"?"` fallback in the invalid
  redeclaration path). Overflow-adjacent lexer matches (`max(1, len)`)
  and `#line` digit separators replicate upstream exactly.
- Specifier nodes are shared, not cloned, across comma-separated
  declarators (`Node::Shared` + converter memoization), because the
  generator groups definitions by `is`. Qualifier/storage lists are
  equal but not identical across declarators (upstream shares the
  lists; no test observes identity and the generator compares by `==`).
- Filename objects are shared per lexer generation (`is`-identical
  within a generation, distinct across `input`/`#line` reassignments);
  converted coordinates intern filenames per parse.
- `ValueError` from constant suffix counting is unreachable from lexer
  output (mirrored as `unreachable!`); genuinely unreachable sites use
  `unreachable!` rather than silent divergence.
- Attribute-error resume: after a non-raising `error_func`, scanning
  continues exactly where upstream continues.
