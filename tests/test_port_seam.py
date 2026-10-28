"""Port seam tests: fallback importability, facade compatibility."""

import os
import subprocess
import sys
import unittest

from pycparser import CParser, parse_file
from pycparser.c_lexer import CLexer, Token, _PureCLexer
from pycparser.c_parser import _PureCParser


class TestSeam(unittest.TestCase):
    def test_fast_classes_active(self):
        self.assertIsNot(CLexer, _PureCLexer)
        self.assertIsNot(CParser, _PureCParser)

    def test_pure_fallback_importable(self):
        # The verbatim pure implementations stay importable without the
        # extension (used when `_core` is missing and for differential tests).
        pure_lex = _PureCLexer(lambda *a: None)
        pure_lex.input("int x;")
        kinds = []
        while True:
            tok = pure_lex.token()
            if tok is None:
                break
            kinds.append(tok.type)
        self.assertEqual(kinds, ["INT", "ID", "SEMI"])
        ast = _PureCParser().parse("int x;", "")
        self.assertEqual(ast.ext[0].name, "x")

    def test_fallback_subprocess_without_extension(self):
        # The package imports and parses with the extension blocked.
        code = (
            "import os; os.environ['PYC_PARSER_NO_RUST'] = '1';"
            "from pycparser import CParser;"
            "from pycparser.c_lexer import CLexer, _PureCLexer;"
            "assert CLexer is _PureCLexer;"
            "print(CParser().parse('int x;', '').ext[0].name)"
        )
        env = dict(os.environ)
        env["PYC_PARSER_NO_RUST"] = "1"
        p = subprocess.run([sys.executable, "-c", code], capture_output=True,
                           text=True, timeout=120, env=env)
        self.assertEqual(p.returncode, 0, p.stderr[-2000:])
        self.assertEqual(p.stdout.strip(), "x")

    def test_dialect_validation(self):
        with self.assertRaises(ValueError):
            CLexer(lambda *a: None, dialect="c99")
        with self.assertRaises(ValueError):
            CParser(dialect="c99")

    def test_token_shape(self):
        toks = []
        clex = CLexer(lambda *a: None)
        clex.input("int x;", filename="t.c")
        while True:
            tok = clex.token()
            if tok is None:
                break
            toks.append(tok)
        self.assertTrue(all(isinstance(t, Token) for t in toks))
        self.assertEqual(
            [(t.type, t.value, t.lineno, t.column, t.filename) for t in toks],
            [("INT", "int", 1, 1, "t.c"), ("ID", "x", 1, 5, "t.c"),
             ("SEMI", ";", 1, 6, "t.c")],
        )

    def test_lexer_error_callback_order(self):
        events = []
        clex = CLexer(lambda msg, line, col: events.append((msg, line, col)))
        clex.input("int \x07;")
        toks = []
        while True:
            tok = clex.token()
            if tok is None:
                break
            toks.append(tok.type)
        self.assertEqual(len(events), 1)
        self.assertIn("Illegal character", events[0][0])
        self.assertEqual(toks, ["INT", "SEMI"])

    def test_custom_lexer_falls_back_to_pure(self):
        seen = []

        class MyLexer(_PureCLexer):
            def token(self):
                seen.append(1)
                return super().token()

        p = CParser(lexer=MyLexer)
        self.assertFalse(p._use_rust)
        ast = p.parse("int x;", "")
        self.assertTrue(seen)
        self.assertEqual(ast.ext[0].name, "x")

    def test_parse_file_untouched(self):
        import tempfile

        with tempfile.NamedTemporaryFile("w", suffix=".c", delete=False) as f:
            f.write("int y;\n")
            path = f.name
        try:
            self.assertEqual(parse_file(path).ext[0].name, "y")
        finally:
            os.unlink(path)

    def test_compat_clex_attribute(self):
        p = CParser()
        self.assertTrue(hasattr(p, "clex"))
        self.assertEqual(p._dialect, "legacy")


if __name__ == "__main__":
    unittest.main()
