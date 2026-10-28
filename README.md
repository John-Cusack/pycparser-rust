# pycparser-rust

Rust port of [pycparser](https://github.com/eliben/pycparser) (C parser in
Python): recursive-descent C lexer + parser in a reusable `pycparser-core`
crate with a thin `pycparser` Python binding (PyO3, same import name —
drop-in for `pip install pycparser`).

Upstream: eliben/pycparser, BSD-3-Clause, by Eli Bendersky. This port
ships under MIT; see NOTICE for attribution.

Status: lane in progress (rustsmith `fm/rs-port-pycparser`).
