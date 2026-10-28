#!/usr/bin/env python3
"""Two-track bench: pure-Python `pycparser` vs `pycparser-rust`, measured only.

Track A (gate): parse representative translation units (fresh `CParser`
per TU, the codegen-time shape). Track B (ceiling demo): parse +
`CGenerator` round-trip on the same inputs.

Each side runs in a fresh process per input (honest peak RSS); wall time
is the median of 3 reps, reported per iteration. The pure side sets
`PYC_PARSER_NO_RUST=1` (byte-identical fallback code); the fast side uses
the compiled extension. No projections.

Usage: venv/bin/python benches/two_track.py [--iters N]
"""

import argparse
import os
import random
import statistics
import subprocess
import sys
import time

PORT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REAL_FILES = [
    "tests/c_files/memmgr_with_h.c",
    "tests/c_files/cppd_with_stdio_h.c",
    "tests/c_files/example_c_file.c",
    "examples/c_files/funky.c",
    "examples/c_files/pragmas.c",
    "examples/c_files/basic.c",
]

_TYPES = [
    "int", "long", "unsigned int", "unsigned long", "short", "char",
    "signed char", "unsigned char", "float", "double", "long double",
    "unsigned long long",
]


def gen_synth_tu(nfuncs, seed):
    rng = random.Random(seed)
    lines = ['#line 1 "synth.c"', "", "typedef unsigned long size_t;",
             "struct point { int x; int y; };",
             "static const double kPi = 3.141592653589793;",
             "int g_counter = 0;", ""]
    for i in range(nfuncs):
        t = rng.choice(_TYPES)
        lines.append(f"static {t} fn{i}(int a{i}, long b{i}) {{")
        lines.append(f"    {t} acc = ({t})a{i} + ({t})b{i} * 3;")
        for _ in range(rng.randint(6, 10)):
            op = rng.choice(["+", "-", "*", "/", "%", "<<", "&", "|", "<", ">="])
            lines.append(f"    acc = acc {op} (({t}){rng.randint(1, 255)});")
        lines.append("    int arr[8];")
        lines.append("    for (int k = 0; k < 8; ++k) { arr[k] = (int)acc; }")
        lines.append("    if (acc > 0) { acc += sizeof(arr); } else { acc -= 1; }")
        lines.append("    switch (a & 3) { case 0: acc |= 1u; break; default: break; }")
        lines.append("    while (acc > 100) { acc /= 2; }")
        lines.append("    return acc;")
        lines.append("}")
        lines.append("")
    return "\n".join(lines) + "\n"


def bench_worker(path, iters, track, pure):
    import resource

    if pure:
        os.environ["PYC_PARSER_NO_RUST"] = "1"
    from pycparser import CParser
    from pycparser.c_generator import CGenerator

    with open(path) as f:
        src = f.read()
    gen = CGenerator()
    # Warmup + checksum guard so the parse cannot be elided.
    ast = CParser().parse(src, path)
    total = len(ast.ext)
    if track == "roundtrip":
        total += len(gen.visit(ast))
    assert total > 0
    t0 = time.perf_counter()
    for _ in range(iters):
        ast = CParser().parse(src, path)
        total += len(ast.ext)
        if track == "roundtrip":
            total += len(gen.visit(ast))
    wall = (time.perf_counter() - t0) / iters
    assert total > 0
    print(f"wall_ms={wall * 1000:.2f} peak_kb={resource.getrusage(resource.RUSAGE_SELF).ru_maxrss}")


def run_side(python, path, iters, track, pure, reps=3):
    walls, peaks = [], []
    env = dict(os.environ)
    for _ in range(reps):
        cmd = [python, __file__, "--worker", path, str(iters), track]
        if pure:
            cmd.append("pure")
        else:
            cmd.append("fast")
        p = subprocess.run(cmd, capture_output=True, text=True, timeout=900, env=env)
        assert p.returncode == 0, p.stderr[-2000:]
        kv = dict(t.split("=") for t in p.stdout.split())
        walls.append(float(kv["wall_ms"]))
        peaks.append(int(kv["peak_kb"]))
    return statistics.median(walls), max(peaks)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--worker", nargs=4, metavar=("PATH", "ITERS", "TRACK", "SIDE"))
    ap.add_argument("--iters", type=int, default=None)
    args = ap.parse_args()
    if args.worker:
        path, iters, track, side = args.worker
        bench_worker(path, int(iters), track, side == "pure")
        return 0

    python = os.environ.get("BENCH_PYTHON", sys.executable)
    import tempfile

    inputs = []
    for rel in REAL_FILES:
        p = os.path.join(PORT, rel)
        if os.path.isfile(p):
            inputs.append((os.path.basename(p), p, "real-file"))
    with tempfile.TemporaryDirectory() as tmp:
        small = os.path.join(tmp, "synth-small.c")
        big = os.path.join(tmp, "synth-big.c")
        with open(small, "w") as f:
            f.write(gen_synth_tu(8, 20261028))
        with open(big, "w") as f:
            f.write(gen_synth_tu(300, 20261028))
        inputs.append(("synth-small decl+func (~7KB)", small, "synthetic-seed-20261028"))
        inputs.append(("synth-big func-heavy (~390KB)", big, "synthetic-seed-20261028"))

        print("| input | bytes | track | pure wall | rust wall | speedup | pure peak RSS | rust peak RSS |")
        print("| --- | --- | --- | --- | --- | --- | --- | --- |")
        for name, path, kind in inputs:
            size = os.path.getsize(path)
            iters = args.iters or (3 if size > 100_000 else 20)
            for track in ("parse", "roundtrip"):
                py_wall, py_rss = run_side(python, path, iters, track, True)
                rs_wall, rs_rss = run_side(python, path, iters, track, False)
                print(
                    f"| {name} ({kind}) | {size} | {track}"
                    f" | {py_wall:.2f} ms | {rs_wall:.2f} ms | {py_wall / rs_wall:.2f}x"
                    f" | {py_rss} KiB | {rs_rss} KiB |"
                )
    return 0


if __name__ == "__main__":
    sys.exit(main())
