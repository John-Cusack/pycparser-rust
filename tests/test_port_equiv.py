"""Port equivalence tests: Rust-backed vs pure-Python parses must agree.

Compares `_PureCParser` (byte-identical upstream code) against the fast
`CParser` over the repo corpus plus synthetic TUs: full `show()` dumps,
`CGenerator` output, coordinates, and `ParseError` messages.
"""

import io
import os
import unittest

from pycparser import CParser, c_ast
from pycparser.c_generator import CGenerator
from pycparser.c_parser import _PureCParser

PORT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

CORPUS_FILES = [
    "tests/c_files/memmgr_with_h.c",
    "tests/c_files/cppd_with_stdio_h.c",
    "tests/c_files/example_c_file.c",
    "tests/c_files/c11.c",
    "examples/c_files/funky.c",
    "examples/c_files/pragmas.c",
    "examples/c_files/basic.c",
    "examples/c_files/year.c",
]

# Inputs that must fail identically on both sides (message + position).
ERROR_CASES = [
    "int a = ;",
    "struct S { int x; int x; }; int y = }",
    "[[a b]] int x;",
    "int [[a]] const x;",
    "typedef _Atomic(char[3]) t;",
    "enum E : _Atomic(unsigned int) { A };",  # legacy dialect rejects this
    "foo bar;",
]

SYNTH_SEED_TUS = [
    ("synth-small", 8, 20261028),
    ("synth-big", 60, 20261029),
]


def make_synth(nfuncs, seed):
    import random

    rng = random.Random(seed)
    lines = ['#line 1 "synth.c"', "", "typedef unsigned long size_t;",
             "struct point { int x; int y; };",
             "static const double kPi = 3.141592653589793;",
             "int g_counter = 0;", ""]
    for i in range(nfuncs):
        t = rng.choice(["int", "long", "unsigned", "char", "double"])
        lines.append(f"static {t} fn{i}(int a{i}, long b{i}) {{")
        lines.append(f"    {t} acc = ({t})a{i} + ({t})b{i} * 3;")
        lines.append("    int arr[8];")
        lines.append("    for (int k = 0; k < 8; ++k) { arr[k] = (int)acc; }")
        lines.append("    if (acc > 0) { acc += sizeof(arr); } else { acc -= 1; }")
        lines.append("    switch (a & 3) { case 0: acc |= 1u; break; default: break; }")
        lines.append("    while (acc > 100) { acc /= 2; }")
        lines.append("    return acc;")
        lines.append("}")
        lines.append("")
    return "\n".join(lines) + "\n"


def dump(tree):
    buf = io.StringIO()
    tree.show(buf=buf, attrnames=True, showcoord=True)
    return buf.getvalue()


class TestPortEquivalence(unittest.TestCase):
    def assertSameParse(self, text, filename="equiv.c", dialect="legacy"):
        fast = CParser(dialect=dialect)
        pure = _PureCParser(dialect=dialect)
        try:
            fast_tree = fast.parse(text, filename)
        except Exception as fast_e:
            with self.assertRaises(type(fast_e)) as ctx:
                pure.parse(text, filename)
            self.assertEqual(str(ctx.exception), str(fast_e))
            return
        pure_tree = pure.parse(text, filename)
        self.assertEqual(dump(pure_tree), dump(fast_tree))
        gen = CGenerator()
        self.assertEqual(gen.visit(pure_tree), gen.visit(fast_tree))

    def test_corpus_files(self):
        for rel in CORPUS_FILES:
            path = os.path.join(PORT, rel)
            if not os.path.isfile(path):
                continue
            with open(path) as f:
                text = f.read()
            try:
                _PureCParser().parse(text, path)
            except Exception:
                # Not parseable standalone (needs cpp); skip on both sides.
                continue
            with self.subTest(path=rel):
                self.assertSameParse(text, filename=path)

    def test_synthetic_tus(self):
        for name, nfuncs, seed in SYNTH_SEED_TUS:
            with self.subTest(tu=name):
                self.assertSameParse(make_synth(nfuncs, seed), filename=name + ".c")

    def test_error_cases(self):
        for text in ERROR_CASES:
            with self.subTest(text=text[:30]):
                self.assertSameParse(text)

    def test_c23_dialect(self):
        cases = [
            "bool f(_Bool x) { return x; }",
            "enum E : unsigned { A };",
            "static_assert(sizeof(int) >= 2, \"ok\");",
            "int x = _Generic(1, int: 2, default: 3);",
            "[[maybe_unused]] int y;",
        ]
        for text in cases:
            with self.subTest(text=text[:30]):
                self.assertSameParse(text, dialect="c23")

    def test_coordinates_and_types(self):
        text = "#line 7 \"inc.h\"\nint foo(int a, char *b) { return a; }\n"
        fast = CParser().parse(text, "main.c")
        pure = _PureCParser().parse(text, "main.c")
        self.assertEqual(dump(pure), dump(fast))
        func = fast.ext[0]
        self.assertIsInstance(func, c_ast.FuncDef)
        # `#line 7 "inc.h"` rewrites the coordinates (both sides agree).
        self.assertEqual(str(func.coord), "inc.h:7:5")
        self.assertEqual(str(func.decl.type.args.params[1].coord), "inc.h:7:21")


if __name__ == "__main__":
    unittest.main()
