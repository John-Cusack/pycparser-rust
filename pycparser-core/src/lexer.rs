//! C lexer: byte-oriented transcription of `pycparser.c_lexer.CLexer`.
//!
//! Token boundaries are computed on bytes (all C punctuation, keywords,
//! identifiers and number syntax are ASCII; non-ASCII bytes can only appear
//! inside string/char-literal spans, where byte scanning finds the same
//! boundaries). Columns are character columns: an ASCII fast path with a
//! slow path for lines containing non-ASCII bytes.

use crate::ast::{LexError, RawToken, TokenKind};

/// Lexer event: mirrors one `CLexer.token()` step. Recoverable diagnostics
/// are returned as `Error` values so the host can invoke `error_func` in
/// the exact order the Python loop would. The resume position travels with
/// the error and is applied only when the *next* event is requested, so a
/// raising `error_func` observes the same lexer state as upstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LexEvent {
    Token(RawToken),
    Error(LexError, usize),
    End,
}

pub struct Lexer {
    text: Vec<u8>,
    filename: String,
    /// Filename generation: bumped on every `input` and every `#line`
    /// filename assignment, mirroring upstream rebinding `self._filename`
    /// (tokens share the string object within a generation).
    filename_gen: u64,
    pos: usize,
    line_start: usize,
    lineno: u32,
    pending: Option<RawToken>,
    in_attribute: bool,
    c23: bool,
    /// Resume position saved alongside the last returned `Error`.
    after_error: Option<usize>,
    /// Diagnostics saved by scanners that report-and-continue
    /// (attribute-argument clauses); drained before further scanning.
    queued_errors: Vec<LexError>,
}

impl Lexer {
    pub fn new(c23: bool) -> Self {
        Self {
            text: Vec::new(),
            filename: String::new(),
            filename_gen: 0,
            pos: 0,
            line_start: 0,
            lineno: 1,
            pending: None,
            in_attribute: false,
            c23,
            after_error: None,
            queued_errors: Vec::new(),
        }
    }

    pub fn input(&mut self, text: &str, filename: &str) {
        self.text = text.as_bytes().to_vec();
        self.filename = filename.to_string();
        self.filename_gen += 1;
        self.pos = 0;
        self.line_start = 0;
        self.lineno = 1;
        self.pending = None;
        self.in_attribute = false;
        self.after_error = None;
        self.queued_errors.clear();
    }

    pub fn filename(&self) -> &str {
        &self.filename
    }

    /// One `token()` call: pending token, queued errors, then the
    /// whitespace/directive/token loop. At most one error is reported per
    /// call; scanning resumes exactly where upstream would continue.
    pub fn next_event(&mut self) -> LexEvent {
        if let Some(tok) = self.pending.take() {
            return LexEvent::Token(tok);
        }
        // Apply the previous error's resume position lazily: a raising
        // `error_func` abandons the lexer before this point, so its state
        // still matches upstream (which advances only after `_error`
        // returns). Normal flow (fire, return, continue) advances here.
        if let Some(resume) = self.after_error.take() {
            self.pos = resume;
        }
        if !self.queued_errors.is_empty() {
            return LexEvent::Error(self.queued_errors.remove(0), self.pos);
        }
        let n = self.text.len();
        while self.pos < n {
            let b = self.text[self.pos];
            match b {
                b' ' | b'\t' | b'\x0c' | b'\x0b' => {
                    self.pos += 1;
                }
                b'\n' => {
                    self.lineno += 1;
                    self.pos += 1;
                    self.line_start = self.pos;
                }
                b'#' => {
                    if is_line_start(&self.text, self.pos + 1) {
                        self.pos += 1;
                        self.handle_ppline();
                        continue;
                    }
                    if is_pragma_start(&self.text, self.pos + 1) {
                        self.pos += 1;
                        match self.handle_pppragma() {
                            PragmaOutcome::Tokens(first, second) => {
                                self.pending = second;
                                return LexEvent::Token(first);
                            }
                            PragmaOutcome::Empty => continue,
                            PragmaOutcome::Error(err, resume) => {
                                self.after_error = Some(resume);
                                return LexEvent::Error(err, resume);
                            }
                        }
                    }
                    let tok = self.make_token(TokenKind::PpHash, "#", self.pos);
                    self.pos += 1;
                    return LexEvent::Token(tok);
                }
                _ => match self.match_token() {
                    TokenOutcome::Token(tok) => return LexEvent::Token(tok),
                    TokenOutcome::Error(err, resume) => {
                        self.after_error = Some(resume);
                        return LexEvent::Error(err, resume);
                    }
                    TokenOutcome::Retry => {
                        if !self.queued_errors.is_empty() {
                            let resume = self.pos;
                            self.after_error = Some(resume);
                            return LexEvent::Error(self.queued_errors.remove(0), resume);
                        }
                        continue;
                    }
                },
            }
        }
        if !self.queued_errors.is_empty() {
            let resume = self.pos;
            self.after_error = Some(resume);
            return LexEvent::Error(self.queued_errors.remove(0), resume);
        }
        LexEvent::End
    }

    fn make_token(&self, kind: TokenKind, value: &str, pos: usize) -> RawToken {
        RawToken {
            kind,
            value: value.to_string(),
            lineno: self.lineno,
            column: column(&self.text, self.line_start, pos),
            filename: self.filename.clone(),
            file_gen: self.filename_gen,
        }
    }

    fn error(&self, msg: String, pos: usize) -> LexError {
        LexError {
            msg,
            line: self.lineno,
            column: column(&self.text, self.line_start, pos),
        }
    }

    // ------------------------------------------------------------------
    // Single-token matching (mirrors `_match_token`).
    // ------------------------------------------------------------------
    fn match_token(&mut self) -> TokenOutcome {
        let pos = self.pos;
        let b = self.text[pos];
        // Attribute handling.
        if self.in_attribute {
            if b == b'(' {
                return match self.attribute_arguments() {
                    Some(tok) => TokenOutcome::Token(tok),
                    // Error already queued; input consumed.
                    None => TokenOutcome::Retry,
                };
            }
            if let Some(len) = match_attribute_end(&self.text, pos) {
                self.in_attribute = false;
                let tok = self.attribute_token(TokenKind::AttributeEnd, pos, pos + len);
                return TokenOutcome::Token(tok);
            }
        } else if b == b'[' {
            if let Some(len) = match_attribute_begin(&self.text, pos) {
                self.in_attribute = true;
                let tok = self.attribute_token(TokenKind::AttributeBegin, pos, pos + len);
                return TokenOutcome::Token(tok);
            }
        }

        // Master-regex winner (first matching rule in rule order) versus
        // the longest fixed literal starting here; the fixed literal wins
        // only when strictly longer.
        let master = match_master(&self.text, pos);
        let fixed = match_fixed(&self.text, pos);
        let best = match (master, fixed) {
            (None, None) => {
                // `chr` may mis-decode a non-ASCII byte; decode the character
                // starting here and advance past the whole sequence.
                let ch = decode_char_at(&self.text, pos);
                let err = self.error(format!("Illegal character {ch:?}"), pos);
                return TokenOutcome::Error(err, pos + ch.len_utf8());
            }
            (Some(m), None) => m,
            (None, Some(f)) => {
                let tok = self.make_token(f.kind, f.literal, pos);
                self.pos += f.literal.len();
                return TokenOutcome::Token(tok);
            }
            (Some(m), Some(f)) => {
                if f.literal.len() > m.length {
                    let tok = self.make_token(f.kind, f.literal, pos);
                    self.pos += f.literal.len();
                    return TokenOutcome::Token(tok);
                }
                m
            }
        };
        match best.action {
            RuleAction::Token => {
                let tok = self.make_token(best.kind, &best.value, pos);
                self.pos += best.length;
                TokenOutcome::Token(tok)
            }
            RuleAction::Error => {
                let msg = if best.kind == TokenKind::BadCharConst {
                    format!("Invalid char constant {}", best.value)
                } else {
                    best.message.to_string()
                };
                let err = self.error(msg, pos);
                let resume = pos + best.length.max(1);
                TokenOutcome::Error(err, resume)
            }
            RuleAction::Id => {
                let kind = if self.in_attribute {
                    TokenKind::Id
                } else {
                    keyword_kind(&best.value, self.c23).unwrap_or(TokenKind::Id)
                };
                let tok = self.make_token(kind, &best.value, pos);
                self.pos += best.length;
                TokenOutcome::Token(tok)
            }
        }
    }

    fn attribute_token(&mut self, kind: TokenKind, start: usize, end: usize) -> RawToken {
        let value = String::from_utf8_lossy(&self.text[start..end]).into_owned();
        let tok = RawToken {
            kind,
            value: value.clone(),
            lineno: self.lineno,
            column: column(&self.text, self.line_start, start),
            filename: self.filename.clone(),
            file_gen: self.filename_gen,
        };
        self.lineno += value.bytes().filter(|&c| c == b'\n').count() as u32;
        if let Some(off) = value.rfind('\n') {
            self.line_start = start + off + 1;
        }
        self.pos = end;
        tok
    }

    /// Balanced attribute-argument clause (mirrors `_attribute_arguments`).
    /// Queues the error and consumes the input on failure, returning `None`.
    fn attribute_arguments(&mut self) -> Option<RawToken> {
        let start = self.pos;
        let n = self.text.len();
        let mut pos = start + 1;
        let mut closers: Vec<u8> = vec![b')'];
        let mut error = "Unterminated attribute arguments";
        while pos < n {
            if self.text[pos] == b'/' && pos + 1 < n && self.text[pos + 1] == b'*' {
                match find_bytes(&self.text[pos + 2..], b"*/") {
                    Some(off) => {
                        pos += 2 + off + 2;
                        continue;
                    }
                    None => break,
                }
            }
            if self.text[pos] == b'/' && pos + 1 < n && self.text[pos + 1] == b'/' {
                match find_byte(&self.text[pos + 2..], b'\n') {
                    Some(off) => pos += 2 + off,
                    None => pos = n,
                }
                continue;
            }
            if let Some(end) = match_attribute_word(&self.text, pos) {
                pos = end;
                continue;
            }
            let ch = self.text[pos];
            if ch == b'\'' || ch == b'"' {
                pos += 1;
                let mut closed = false;
                while pos < n && self.text[pos] != ch && self.text[pos] != b'\n' {
                    pos += if self.text[pos] == b'\\' { 2 } else { 1 };
                }
                if pos < n && self.text[pos] == ch {
                    pos += 1;
                    closed = true;
                }
                if !closed {
                    break;
                }
                continue;
            }
            match ch {
                b'(' => closers.push(b')'),
                b'[' => closers.push(b']'),
                b'{' => closers.push(b'}'),
                b')' | b']' | b'}' => {
                    if closers.last().copied() != Some(ch) {
                        error = "Mismatched delimiter in attribute arguments";
                        break;
                    }
                    closers.pop();
                    if closers.is_empty() {
                        let mut tok =
                            self.attribute_token(TokenKind::AttributeArguments, start, pos + 1);
                        tok.value = tok.value[1..tok.value.len() - 1].to_string();
                        return Some(tok);
                    }
                }
                _ => {}
            }
            pos += 1;
        }
        self.queued_errors
            .push(self.error(error.to_string(), start));
        self.pos = n;
        None
    }

    // ------------------------------------------------------------------
    // `#line` handling (mirrors `_handle_ppline`).
    // ------------------------------------------------------------------
    fn handle_ppline(&mut self) {
        let n = self.text.len();
        let mut line_end = find_byte(&self.text[self.pos..], b'\n')
            .map(|o| self.pos + o)
            .unwrap_or(n);
        if line_end > n {
            line_end = n;
        }
        let line = &self.text[self.pos..line_end];
        let mut p = 0usize;

        macro_rules! fail {
            ($msg:expr, $off:expr) => {{
                let err = self.error($msg.to_string(), self.pos + $off);
                self.after_error_immediate(err);
                self.pos = line_end + 1;
                self.line_start = self.pos;
                return;
            }};
        }
        macro_rules! succeed {
            ($num:expr, $file:expr) => {{
                match $num {
                    None => {
                        let err = self.error(
                            "line number missing in #line".to_string(),
                            self.pos + line.len(),
                        );
                        self.after_error_immediate(err);
                    }
                    Some(num) => {
                        // Digit separators are allowed: strip them before the
                        // conversion, exactly like upstream `int(s.replace)`.
                        let clean: String = num.chars().filter(|&c| c != '\'').collect();
                        match clean.parse::<u32>() {
                            Err(_) => {
                                let err = self.error(
                                    "invalid #line directive".to_string(),
                                    self.pos + line.len(),
                                );
                                self.after_error_immediate(err);
                                return;
                            }
                            Ok(v) => {
                                self.lineno = v;
                                if let Some(f) = $file {
                                    self.filename = f;
                                    self.filename_gen += 1;
                                }
                            }
                        }
                    }
                }
                self.pos = line_end + 1;
                self.line_start = self.pos;
                return;
            }};
        }

        while p < line.len() && (line[p] == b' ' || line[p] == b'\t') {
            p += 1;
        }
        if line[p..].starts_with(b"line") {
            p += 4;
        }
        while p < line.len() && (line[p] == b' ' || line[p] == b'\t') {
            p += 1;
        }
        if p >= line.len() {
            succeed!(None::<String>, None::<String>);
        }
        if line[p] == b'"' {
            fail!("filename before line number in #line", p);
        }
        let Some(digits) = match_decimal_digits(line, p) else {
            fail!("invalid #line directive", p);
        };
        let pp_line = std::str::from_utf8(&line[p..p + digits])
            .unwrap_or("")
            .to_string();
        // Digit separators are allowed but no integer suffixes; the number
        // itself must parse after removing separators (upstream `int()` on
        // the raw text would fail with separators — mirror it exactly).
        p += digits;
        while p < line.len() && (line[p] == b' ' || line[p] == b'\t') {
            p += 1;
        }
        if p >= line.len() {
            succeed!(Some(pp_line), None);
        }
        if line[p] != b'"' {
            fail!("invalid #line directive", p);
        }
        let Some(slen) = match_string_literal(line, p) else {
            fail!("invalid #line directive", p);
        };
        let raw = std::str::from_utf8(&line[p..p + slen]).unwrap_or("");
        let pp_filename = raw.trim_matches('"').to_string();
        p += slen;
        loop {
            while p < line.len() && (line[p] == b' ' || line[p] == b'\t') {
                p += 1;
            }
            if p >= line.len() {
                break;
            }
            let Some(dlen) = match_decimal_digits(line, p) else {
                fail!("invalid #line directive", p);
            };
            p += dlen;
        }
        succeed!(Some(pp_line), Some(pp_filename));
    }

    /// Errors from `#line` handling fire immediately (upstream calls
    /// `self._error`, i.e. `error_func`, synchronously inside `token()`).
    /// The event loop drains the queue right after this returns.
    fn after_error_immediate(&mut self, err: LexError) {
        self.queued_errors.push(err);
    }

    // ------------------------------------------------------------------
    // `#pragma` handling (mirrors `_handle_pppragma`).
    // ------------------------------------------------------------------
    fn handle_pppragma(&mut self) -> PragmaOutcome {
        let n = self.text.len();
        let mut pos = self.pos;
        while pos < n && (self.text[pos] == b' ' || self.text[pos] == b'\t') {
            pos += 1;
        }
        if pos >= n {
            self.pos = pos;
            return PragmaOutcome::Empty;
        }
        if !self.text[pos..].starts_with(b"pragma") {
            let err = self.error("invalid #pragma directive".to_string(), pos);
            return PragmaOutcome::Error(err, pos + 1);
        }
        let pragma_pos = pos;
        pos += 6;
        let first = self.make_token(TokenKind::PpPragma, "pragma", pragma_pos);
        while pos < n && (self.text[pos] == b' ' || self.text[pos] == b'\t') {
            pos += 1;
        }
        let start = pos;
        while pos < n && self.text[pos] != b'\n' {
            pos += 1;
        }
        let mut second = None;
        if pos > start {
            let value = String::from_utf8_lossy(&self.text[start..pos]).into_owned();
            second = Some(RawToken {
                kind: TokenKind::PpPragmaStr,
                value,
                lineno: self.lineno,
                column: column(&self.text, self.line_start, start),
                filename: self.filename.clone(),
                file_gen: self.filename_gen,
            });
        }
        if pos < n && self.text[pos] == b'\n' {
            self.lineno += 1;
            pos += 1;
            self.line_start = pos;
        }
        self.pos = pos;
        PragmaOutcome::Tokens(first, second)
    }
}

enum TokenOutcome {
    Token(RawToken),
    Error(LexError, usize),
    Retry,
}

enum PragmaOutcome {
    Tokens(RawToken, Option<RawToken>),
    Empty,
    Error(LexError, usize),
}

// ----------------------------------------------------------------------
// Byte helpers.
// ----------------------------------------------------------------------
fn find_byte(hay: &[u8], needle: u8) -> Option<usize> {
    hay.iter().position(|&b| b == needle)
}

fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// 1-based character column of byte offset `pos` on the line starting at
/// `line_start`. ASCII fast path; counts characters when the line prefix
/// contains non-ASCII bytes (mirrors Python `pos - line_start + 1` on a
/// `str`, where indices are characters).
pub fn column(text: &[u8], line_start: usize, pos: usize) -> u32 {
    let seg = &text[line_start..pos];
    if seg.is_ascii() {
        return (seg.len() + 1) as u32;
    }
    seg.iter().filter(|b| (**b & 0xC0) != 0x80).count() as u32 + 1
}

// ----------------------------------------------------------------------
// Master-regex rules, in upstream `_regex_rules` order. Each matcher is
// anchored at `pos` and returns the match length; the first matching rule
// wins (Python `re` alternation semantics, not longest-match).
// ----------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleAction {
    Token,
    Id,
    Error,
}

pub struct MasterMatch {
    pub kind: TokenKind,
    pub value: String,
    pub length: usize,
    pub action: RuleAction,
    pub message: &'static str,
}

pub struct FixedMatch {
    pub kind: TokenKind,
    pub literal: &'static str,
}

fn matched(
    text: &[u8],
    pos: usize,
    len: usize,
    kind: TokenKind,
    action: RuleAction,
    message: &'static str,
) -> MasterMatch {
    MasterMatch {
        kind,
        value: String::from_utf8_lossy(&text[pos..pos + len]).into_owned(),
        length: len,
        action,
        message,
    }
}

pub fn match_master(text: &[u8], pos: usize) -> Option<MasterMatch> {
    if text[pos..].starts_with(b"/*") {
        return Some(matched(
            text,
            pos,
            2,
            TokenKind::UnsupportedCStyleComment,
            RuleAction::Error,
            "Comments are not supported, see https://github.com/eliben/pycparser#3using.",
        ));
    }
    if text[pos..].starts_with(b"//") {
        return Some(matched(
            text,
            pos,
            2,
            TokenKind::UnsupportedCxxStyleComment,
            RuleAction::Error,
            "Comments are not supported, see https://github.com/eliben/pycparser#3using.",
        ));
    }
    if let Some(len) = match_bad_string_literal(text, pos) {
        return Some(matched(
            text,
            pos,
            len,
            TokenKind::BadStringLiteral,
            RuleAction::Error,
            "String contains invalid escape code",
        ));
    }
    if let Some(len) = match_wstring_literal(text, pos) {
        return Some(matched(
            text,
            pos,
            len,
            TokenKind::WstringLiteral,
            RuleAction::Token,
            "",
        ));
    }
    if let Some(len) = match_u8string_literal(text, pos) {
        return Some(matched(
            text,
            pos,
            len,
            TokenKind::U8StringLiteral,
            RuleAction::Token,
            "",
        ));
    }
    if let Some(len) = match_u16string_literal(text, pos) {
        return Some(matched(
            text,
            pos,
            len,
            TokenKind::U16StringLiteral,
            RuleAction::Token,
            "",
        ));
    }
    if let Some(len) = match_u32string_literal(text, pos) {
        return Some(matched(
            text,
            pos,
            len,
            TokenKind::U32StringLiteral,
            RuleAction::Token,
            "",
        ));
    }
    if let Some(len) = match_string_literal(text, pos) {
        return Some(matched(
            text,
            pos,
            len,
            TokenKind::StringLiteral,
            RuleAction::Token,
            "",
        ));
    }
    if let Some(len) = match_hex_float(text, pos) {
        return Some(matched(
            text,
            pos,
            len,
            TokenKind::HexFloatConst,
            RuleAction::Token,
            "",
        ));
    }
    if let Some(len) = match_float(text, pos) {
        return Some(matched(
            text,
            pos,
            len,
            TokenKind::FloatConst,
            RuleAction::Token,
            "",
        ));
    }
    if let Some(len) = match_int_hex(text, pos) {
        return Some(matched(
            text,
            pos,
            len,
            TokenKind::IntConstHex,
            RuleAction::Token,
            "",
        ));
    }
    if let Some(len) = match_int_bin(text, pos) {
        return Some(matched(
            text,
            pos,
            len,
            TokenKind::IntConstBin,
            RuleAction::Token,
            "",
        ));
    }
    if let Some(len) = match_bad_oct(text, pos) {
        return Some(matched(
            text,
            pos,
            len,
            TokenKind::BadConstOct,
            RuleAction::Error,
            "Invalid octal constant",
        ));
    }
    if let Some(len) = match_int_oct(text, pos) {
        return Some(matched(
            text,
            pos,
            len,
            TokenKind::IntConstOct,
            RuleAction::Token,
            "",
        ));
    }
    if let Some(len) = match_int_dec(text, pos) {
        return Some(matched(
            text,
            pos,
            len,
            TokenKind::IntConstDec,
            RuleAction::Token,
            "",
        ));
    }
    if let Some(len) = match_multichar(text, pos) {
        return Some(matched(
            text,
            pos,
            len,
            TokenKind::IntConstChar,
            RuleAction::Token,
            "",
        ));
    }
    if let Some(len) = match_char(text, pos) {
        return Some(matched(
            text,
            pos,
            len,
            TokenKind::CharConst,
            RuleAction::Token,
            "",
        ));
    }
    if let Some(len) = match_wchar(text, pos) {
        return Some(matched(
            text,
            pos,
            len,
            TokenKind::WcharConst,
            RuleAction::Token,
            "",
        ));
    }
    if let Some(len) = match_u8char(text, pos) {
        return Some(matched(
            text,
            pos,
            len,
            TokenKind::U8CharConst,
            RuleAction::Token,
            "",
        ));
    }
    if let Some(len) = match_u16char(text, pos) {
        return Some(matched(
            text,
            pos,
            len,
            TokenKind::U16CharConst,
            RuleAction::Token,
            "",
        ));
    }
    if let Some(len) = match_u32char(text, pos) {
        return Some(matched(
            text,
            pos,
            len,
            TokenKind::U32CharConst,
            RuleAction::Token,
            "",
        ));
    }
    if let Some(len) = match_unmatched_quote(text, pos) {
        return Some(matched(
            text,
            pos,
            len,
            TokenKind::UnmatchedQuote,
            RuleAction::Error,
            "Unmatched '",
        ));
    }
    if let Some(len) = match_bad_char(text, pos) {
        return Some(matched(
            text,
            pos,
            len,
            TokenKind::BadCharConst,
            RuleAction::Error,
            "",
        ));
    }
    if let Some(len) = match_identifier(text, pos) {
        return Some(matched(text, pos, len, TokenKind::Id, RuleAction::Id, ""));
    }
    None
}

// ----------------------------------------------------------------------
// Fixed literals, bucketed by first byte, longest-first within a bucket
// (mirrors the `_fixed_tokens_by_first` construction).
// ----------------------------------------------------------------------

pub fn match_fixed(text: &[u8], pos: usize) -> Option<FixedMatch> {
    let b = text[pos];
    let rest = &text[pos..];
    let hit = |lits: &[(&'static str, TokenKind)]| -> Option<FixedMatch> {
        for (lit, kind) in lits {
            if rest.starts_with(lit.as_bytes()) {
                return Some(FixedMatch {
                    kind: *kind,
                    literal: lit,
                });
            }
        }
        None
    };
    match b {
        b'.' => hit(&[("...", TokenKind::Ellipsis), (".", TokenKind::Period)]),
        b'<' => hit(&[
            ("<<=", TokenKind::LshiftEqual),
            ("<<", TokenKind::Lshift),
            ("<=", TokenKind::Le),
            ("<", TokenKind::Lt),
        ]),
        b'>' => hit(&[
            (">>=", TokenKind::RshiftEqual),
            (">>", TokenKind::Rshift),
            (">=", TokenKind::Ge),
            (">", TokenKind::Gt),
        ]),
        b'+' => hit(&[
            ("++", TokenKind::PlusPlus),
            ("+=", TokenKind::PlusEqual),
            ("+", TokenKind::Plus),
        ]),
        b'-' => hit(&[
            ("--", TokenKind::MinusMinus),
            ("->", TokenKind::Arrow),
            ("-=", TokenKind::MinusEqual),
            ("-", TokenKind::Minus),
        ]),
        b'*' => hit(&[("*=", TokenKind::TimesEqual), ("*", TokenKind::Times)]),
        b'/' => hit(&[("/=", TokenKind::DivEqual), ("/", TokenKind::Divide)]),
        b'%' => hit(&[("%=", TokenKind::ModEqual), ("%", TokenKind::Mod)]),
        b'=' => hit(&[("==", TokenKind::Eq), ("=", TokenKind::Equals)]),
        b'!' => hit(&[("!=", TokenKind::Ne), ("!", TokenKind::Lnot)]),
        b'&' => hit(&[
            ("&&", TokenKind::Land),
            ("&=", TokenKind::AndEqual),
            ("&", TokenKind::And),
        ]),
        b'|' => hit(&[
            ("||", TokenKind::Lor),
            ("|=", TokenKind::OrEqual),
            ("|", TokenKind::Or),
        ]),
        b'^' => hit(&[("^=", TokenKind::XorEqual), ("^", TokenKind::Xor)]),
        b':' => hit(&[("::", TokenKind::ColonColon), (":", TokenKind::Colon)]),
        b'(' => Some(FixedMatch {
            kind: TokenKind::Lparen,
            literal: "(",
        }),
        b')' => Some(FixedMatch {
            kind: TokenKind::Rparen,
            literal: ")",
        }),
        b'[' => Some(FixedMatch {
            kind: TokenKind::Lbracket,
            literal: "[",
        }),
        b']' => Some(FixedMatch {
            kind: TokenKind::Rbracket,
            literal: "]",
        }),
        b'{' => Some(FixedMatch {
            kind: TokenKind::Lbrace,
            literal: "{",
        }),
        b'}' => Some(FixedMatch {
            kind: TokenKind::Rbrace,
            literal: "}",
        }),
        b',' => Some(FixedMatch {
            kind: TokenKind::Comma,
            literal: ",",
        }),
        b';' => Some(FixedMatch {
            kind: TokenKind::Semi,
            literal: ";",
        }),
        b'?' => Some(FixedMatch {
            kind: TokenKind::Condop,
            literal: "?",
        }),
        b'~' => Some(FixedMatch {
            kind: TokenKind::Not,
            literal: "~",
        }),
        _ => None,
    }
}

// ----------------------------------------------------------------------
// Keywords.
// ----------------------------------------------------------------------

pub fn keyword_kind(ident: &str, c23: bool) -> Option<TokenKind> {
    let legacy = match ident {
        "auto" => TokenKind::Auto,
        "break" => TokenKind::Break,
        "case" => TokenKind::Case,
        "char" => TokenKind::Char,
        "const" => TokenKind::Const,
        "continue" => TokenKind::Continue,
        "default" => TokenKind::Default,
        "do" => TokenKind::Do,
        "double" => TokenKind::Double,
        "else" => TokenKind::Else,
        "enum" => TokenKind::Enum,
        "extern" => TokenKind::Extern,
        "float" => TokenKind::Float,
        "for" => TokenKind::For,
        "goto" => TokenKind::Goto,
        "if" => TokenKind::If,
        "inline" => TokenKind::Inline,
        "int" => TokenKind::Int,
        "long" => TokenKind::Long,
        "register" => TokenKind::Register,
        "offsetof" => TokenKind::Offsetof,
        "restrict" => TokenKind::Restrict,
        "return" => TokenKind::Return,
        "short" => TokenKind::Short,
        "signed" => TokenKind::Signed,
        "sizeof" => TokenKind::Sizeof,
        "static" => TokenKind::Static,
        "struct" => TokenKind::Struct,
        "switch" => TokenKind::Switch,
        "typedef" => TokenKind::Typedef,
        "union" => TokenKind::Union,
        "unsigned" => TokenKind::Unsigned,
        "void" => TokenKind::Void,
        "volatile" => TokenKind::Volatile,
        "while" => TokenKind::While,
        "__builtin_va_arg" => TokenKind::BuiltinVaArg,
        "__int128" => TokenKind::Int128,
        "_Bool" => TokenKind::Bool,
        "_Complex" => TokenKind::Complex,
        "_Generic" => TokenKind::Generic,
        "_Noreturn" => TokenKind::Noreturn,
        "_Thread_local" => TokenKind::ThreadLocal,
        "_Static_assert" => TokenKind::StaticAssert,
        "_Atomic" => TokenKind::Atomic,
        "_Alignof" => TokenKind::Alignof,
        "_Alignas" => TokenKind::Alignas,
        "_Pragma" => TokenKind::PragmaTok,
        _ => return if c23 { keyword_kind_c23(ident) } else { None },
    };
    Some(legacy)
}

fn keyword_kind_c23(ident: &str) -> Option<TokenKind> {
    Some(match ident {
        "alignas" => TokenKind::Alignas,
        "alignof" => TokenKind::Alignof,
        "bool" => TokenKind::Bool,
        "static_assert" => TokenKind::StaticAssert,
        "thread_local" => TokenKind::ThreadLocal,
        "true" | "false" => TokenKind::BoolConst,
        "nullptr" => TokenKind::Nullptr,
        "constexpr" => TokenKind::Constexpr,
        "typeof" => TokenKind::Typeof,
        "typeof_unqual" => TokenKind::TypeofUnqual,
        "_BitInt" => TokenKind::BitInt,
        "_Decimal32" => TokenKind::Decimal32,
        "_Decimal64" => TokenKind::Decimal64,
        "_Decimal128" => TokenKind::Decimal128,
        "_Imaginary" => TokenKind::Imaginary,
        _ => return None,
    })
}

// ----------------------------------------------------------------------
// Character classes and elementary scanners.
// ----------------------------------------------------------------------

fn is_id_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b'$'
}

fn is_id_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

fn is_hex(b: u8) -> bool {
    b.is_ascii_hexdigit()
}

fn is_oct(b: u8) -> bool {
    matches!(b, b'0'..=b'7')
}

/// `[0-9]('?[0-9])*` — digit separators must join two digits.
pub fn match_decimal_digits(s: &[u8], pos: usize) -> Option<usize> {
    if pos >= s.len() || !s[pos].is_ascii_digit() {
        return None;
    }
    let mut j = pos + 1;
    while j < s.len() {
        if s[j].is_ascii_digit() {
            j += 1;
        } else if s[j] == b'\'' && j + 1 < s.len() && s[j + 1].is_ascii_digit() {
            j += 2;
        } else {
            break;
        }
    }
    Some(j - pos)
}

fn match_hex_digits(s: &[u8], pos: usize) -> Option<usize> {
    if pos >= s.len() || !is_hex(s[pos]) {
        return None;
    }
    let mut j = pos + 1;
    while j < s.len() {
        if is_hex(s[j]) {
            j += 1;
        } else if s[j] == b'\'' && j + 1 < s.len() && is_hex(s[j + 1]) {
            j += 2;
        } else {
            break;
        }
    }
    Some(j - pos)
}

fn match_bin_digits(s: &[u8], pos: usize) -> Option<usize> {
    if pos >= s.len() || (s[pos] != b'0' && s[pos] != b'1') {
        return None;
    }
    let mut j = pos + 1;
    while j < s.len() {
        if s[j] == b'0' || s[j] == b'1' {
            j += 1;
        } else if s[j] == b'\'' && j + 1 < s.len() && (s[j + 1] == b'0' || s[j + 1] == b'1') {
            j += 2;
        } else {
            break;
        }
    }
    Some(j - pos)
}

/// Integer suffix, alternatives in upstream order. Case matters per
/// alternative (`wb`/`WB` exact, `ll`/`LL` exact pairs); only the
/// single-letter ends are case-free.
fn match_int_suffix(s: &[u8], pos: usize) -> usize {
    let at = |i: usize| -> u8 {
        if pos + i < s.len() {
            s[pos + i]
        } else {
            0
        }
    };
    let is_u = |c: u8| c == b'u' || c == b'U';
    // ([uU](wb|WB))
    if is_u(at(0)) && ((at(1) == b'w' && at(2) == b'b') || (at(1) == b'W' && at(2) == b'B')) {
        return 3;
    }
    // ((wb|WB)[uU]?)
    if (at(0) == b'w' && at(1) == b'b') || (at(0) == b'W' && at(1) == b'B') {
        return if is_u(at(2)) { 3 } else { 2 };
    }
    // ([uU]ll) / ([uU]LL)
    if is_u(at(0)) && at(1) == b'l' && at(2) == b'l' {
        return 3;
    }
    if is_u(at(0)) && at(1) == b'L' && at(2) == b'L' {
        return 3;
    }
    // (ll[uU]?) / (LL[uU]?)
    if at(0) == b'l' && at(1) == b'l' {
        return if is_u(at(2)) { 3 } else { 2 };
    }
    if at(0) == b'L' && at(1) == b'L' {
        return if is_u(at(2)) { 3 } else { 2 };
    }
    // ([uU][lL])
    if is_u(at(0)) && (at(1) == b'l' || at(1) == b'L') {
        return 2;
    }
    // ([lL][uU]?)
    if at(0) == b'l' || at(0) == b'L' {
        return if is_u(at(1)) { 2 } else { 1 };
    }
    // [uU]
    if is_u(at(0)) {
        return 1;
    }
    0
}

fn match_int_dec(s: &[u8], pos: usize) -> Option<usize> {
    if pos >= s.len() || !s[pos].is_ascii_digit() {
        return None;
    }
    let mut j = pos;
    if s[j] == b'0' {
        j += 1;
    } else {
        j += match_decimal_digits(s, j)?;
    }
    j += match_int_suffix(s, j);
    Some(j - pos)
}

fn match_int_oct(s: &[u8], pos: usize) -> Option<usize> {
    if pos >= s.len() || s[pos] != b'0' {
        return None;
    }
    let mut j = pos + 1;
    loop {
        if j < s.len() && is_oct(s[j]) {
            j += 1;
        } else if j + 1 < s.len() && s[j] == b'\'' && is_oct(s[j + 1]) {
            j += 2;
        } else {
            break;
        }
    }
    j += match_int_suffix(s, j);
    Some(j - pos)
}

fn match_bad_oct(s: &[u8], pos: usize) -> Option<usize> {
    if pos >= s.len() || s[pos] != b'0' {
        return None;
    }
    let mut j = pos + 1;
    loop {
        if j < s.len() && is_oct(s[j]) {
            j += 1;
        } else if j + 1 < s.len() && s[j] == b'\'' && is_oct(s[j + 1]) {
            j += 2;
        } else {
            break;
        }
    }
    // A separator quote must join two valid digits; `'?[89]` may still
    // follow the octal run (or a lone quote after `0`).
    if j < s.len() && s[j] == b'\'' && j + 1 < s.len() && matches!(s[j + 1], b'8' | b'9') {
        return Some(j + 2 - pos);
    }
    if j < s.len() && matches!(s[j], b'8' | b'9') {
        return Some(j + 1 - pos);
    }
    None
}

fn match_int_hex(s: &[u8], pos: usize) -> Option<usize> {
    if !(s.len() >= pos + 3 && s[pos] == b'0' && (s[pos + 1] == b'x' || s[pos + 1] == b'X')) {
        return None;
    }
    let mut j = pos + 2 + match_hex_digits(s, pos + 2)?;
    j += match_int_suffix(s, j);
    Some(j - pos)
}

fn match_int_bin(s: &[u8], pos: usize) -> Option<usize> {
    if !(s.len() >= pos + 3 && s[pos] == b'0' && (s[pos + 1] == b'b' || s[pos + 1] == b'B')) {
        return None;
    }
    let mut j = pos + 2 + match_bin_digits(s, pos + 2)?;
    j += match_int_suffix(s, j);
    Some(j - pos)
}

/// Floating suffix: `(df|dd|dl|DF|DD|DL|[FfLl])?` in upstream order.
fn match_float_suffix(s: &[u8], pos: usize) -> usize {
    if s.len() >= pos + 2 {
        let w = &s[pos..pos + 2];
        if w == b"df" || w == b"dd" || w == b"dl" || w == b"DF" || w == b"DD" || w == b"DL" {
            return 2;
        }
    }
    if pos < s.len() && matches!(s[pos], b'f' | b'F' | b'l' | b'L') {
        return 1;
    }
    0
}

fn match_float(s: &[u8], pos: usize) -> Option<usize> {
    let mut j = pos;
    // Fractional constant: (digits? . digits | digits .).
    let mut frac = false;
    if let Some(d) = match_decimal_digits(s, j) {
        j += d;
        if j < s.len() && s[j] == b'.' {
            frac = true;
            j += 1;
            if let Some(d2) = match_decimal_digits(s, j) {
                j += d2;
            }
        }
    } else if j < s.len() && s[j] == b'.' {
        if let Some(d2) = match_decimal_digits(s, j + 1) {
            frac = true;
            j += 1 + d2;
        }
    }
    if frac {
        // Optional exponent.
        if j < s.len() && (s[j] == b'e' || s[j] == b'E') {
            if let Some(el) = match_exponent(s, j) {
                j += el;
            }
        }
        j += match_float_suffix(s, j);
        return Some(j - pos);
    }
    // Digits with a mandatory exponent (restart from `pos`: the
    // fractional attempt above may have consumed digits already).
    let mut j = pos;
    if let Some(d) = match_decimal_digits(s, j) {
        j += d;
        if j < s.len() && (s[j] == b'e' || s[j] == b'E') {
            if let Some(el) = match_exponent(s, j) {
                j += el;
                j += match_float_suffix(s, j);
                return Some(j - pos);
            }
        }
    }
    None
}

fn match_exponent(s: &[u8], pos: usize) -> Option<usize> {
    if pos >= s.len() || (s[pos] != b'e' && s[pos] != b'E') {
        return None;
    }
    let mut j = pos + 1;
    if j < s.len() && (s[j] == b'+' || s[j] == b'-') {
        j += 1;
    }
    j += match_decimal_digits(s, j)?;
    Some(j - pos)
}

fn match_hex_float(s: &[u8], pos: usize) -> Option<usize> {
    if !(s.len() >= pos + 3 && s[pos] == b'0' && (s[pos + 1] == b'x' || s[pos + 1] == b'X')) {
        return None;
    }
    let mut j = pos + 2;
    let mut ok = false;
    // hexdigits | hex_fractional_constant
    if let Some(d) = match_hex_digits(s, j) {
        j += d;
        ok = true;
        if j < s.len() && s[j] == b'.' {
            j += 1;
            if let Some(d2) = match_hex_digits(s, j) {
                j += d2;
            }
        }
    } else if j < s.len() && s[j] == b'.' {
        if let Some(d2) = match_hex_digits(s, j + 1) {
            j += 1 + d2;
            ok = true;
        }
    }
    if !ok {
        return None;
    }
    // Mandatory binary exponent.
    if j >= s.len() || (s[j] != b'p' && s[j] != b'P') {
        return None;
    }
    j += 1;
    if j < s.len() && (s[j] == b'+' || s[j] == b'-') {
        j += 1;
    }
    j += match_decimal_digits(s, j)?;
    if j < s.len() && matches!(s[j], b'f' | b'F' | b'l' | b'L') {
        j += 1;
    }
    Some(j - pos)
}

// ----------------------------------------------------------------------
// String and character literals.
// ----------------------------------------------------------------------

/// Permitted byte after a backslash inside a *string* literal.
fn is_string_escape_start(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'.' | b'_' | b'~' | b'!' | b'=' | b'&' | b'^' | b'-' | b'\\' | b'?' | b'\'' | b'"'
        )
}

/// Byte that may NOT follow a backslash in `_bad_escape`.
fn is_bad_escape_second(b: u8) -> bool {
    !(b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'.' | b'_' | b'~' | b'^' | b'!' | b'=' | b'&' | b'\\' | b'-' | b'?' | b'\'' | b'"'
        ))
}

fn match_string_at(s: &[u8], pos: usize) -> Option<usize> {
    if pos >= s.len() || s[pos] != b'"' {
        return None;
    }
    let mut j = pos + 1;
    while j < s.len() {
        let c = s[j];
        if c == b'"' {
            return Some(j + 1 - pos);
        }
        if c == b'\\' {
            if j + 1 < s.len() && is_string_escape_start(s[j + 1]) {
                j += 2;
                continue;
            }
            return None;
        }
        if c == b'\n' {
            return None;
        }
        j += 1;
    }
    None
}

pub fn match_string_literal(s: &[u8], pos: usize) -> Option<usize> {
    match_string_at(s, pos)
}

fn match_wstring_literal(s: &[u8], pos: usize) -> Option<usize> {
    if pos < s.len() && s[pos] == b'L' {
        if let Some(l) = match_string_at(s, pos + 1) {
            return Some(1 + l);
        }
    }
    None
}

fn match_u8string_literal(s: &[u8], pos: usize) -> Option<usize> {
    if s.len() > pos + 2 && s[pos] == b'u' && s[pos + 1] == b'8' {
        if let Some(l) = match_string_at(s, pos + 2) {
            return Some(2 + l);
        }
    }
    None
}

fn match_u16string_literal(s: &[u8], pos: usize) -> Option<usize> {
    if pos < s.len() && s[pos] == b'u' {
        if let Some(l) = match_string_at(s, pos + 1) {
            return Some(1 + l);
        }
    }
    None
}

fn match_u32string_literal(s: &[u8], pos: usize) -> Option<usize> {
    if pos < s.len() && s[pos] == b'U' {
        if let Some(l) = match_string_at(s, pos + 1) {
            return Some(1 + l);
        }
    }
    None
}

/// `"` string_char* BAD string_char* `"` — latest valid middle wins
/// (greedy-prefix backtracking semantics).
fn match_bad_string_literal(s: &[u8], pos: usize) -> Option<usize> {
    if pos >= s.len() || s[pos] != b'"' {
        return None;
    }
    // Scan the prefix, collecting bad-escape positions.
    let mut bad_positions: Vec<usize> = Vec::new();
    let mut j = pos + 1;
    let mut stopped_at_quote = false;
    while j < s.len() {
        let c = s[j];
        if c == b'"' {
            stopped_at_quote = true;
            break;
        }
        if c == b'\\' {
            if j + 1 < s.len() && is_string_escape_start(s[j + 1]) {
                j += 2;
                continue;
            }
            bad_positions.push(j);
            j += 2.min(s.len() - j);
            continue;
        }
        if c == b'\n' {
            break;
        }
        j += 1;
    }
    if stopped_at_quote && bad_positions.is_empty() {
        return None;
    }
    // Latest middle first.
    for &bad in bad_positions.iter().rev() {
        let mut k = bad + 2;
        let mut ok = false;
        while k < s.len() {
            let c = s[k];
            if c == b'"' {
                ok = true;
                break;
            }
            if c == b'\\' {
                if k + 1 < s.len() && is_string_escape_start(s[k + 1]) {
                    k += 2;
                    continue;
                }
                break;
            }
            if c == b'\n' {
                break;
            }
            k += 1;
        }
        if ok {
            return Some(k + 1 - pos);
        }
    }
    None
}

/// One `_cconst_char` (no quotes).
fn match_cconst_char(s: &[u8], pos: usize) -> Option<usize> {
    if pos >= s.len() {
        return None;
    }
    let c = s[pos];
    if c == b'\'' || c == b'\\' || c == b'\n' {
        if c != b'\\' {
            return None;
        }
        return match_escape(s, pos);
    }
    Some(1)
}

/// Full escape sequence after a backslash (mirrors `_escape_sequence` with
/// its lookahead guards, reordered into direct checks).
fn match_escape(s: &[u8], pos: usize) -> Option<usize> {
    if pos + 1 >= s.len() || s[pos] != b'\\' {
        return None;
    }
    let c = s[pos + 1];
    // Complete Unicode escapes take precedence over simple `\u` / `\U`.
    if c == b'u' && is_complete_unicode(s, pos + 1, 4) {
        return Some(6);
    }
    if c == b'U' && is_complete_unicode(s, pos + 1, 8) {
        return Some(10);
    }
    // Simple escapes (x excluded; handled with its guard below).
    if matches!(c, b'a'..=b'w' | b'y' | b'z' | b'A'..=b'Z' | b'.' | b'_' | b'~' | b'!'
        | b'=' | b'&' | b'^' | b'-' | b'\\' | b'?' | b'\'' | b'"')
    {
        return Some(2);
    }
    if c == b'x' {
        // `x(?![0-9a-fA-F])`: bare `\x` only when no hex digit follows.
        if pos + 2 >= s.len() || !is_hex(s[pos + 2]) {
            return Some(2);
        }
        // Hex escape: consume the whole run.
        let mut j = pos + 2;
        while j < s.len() && is_hex(s[j]) {
            j += 1;
        }
        return Some(j - pos);
    }
    // Decimal escape: consume the whole digit run.
    if c.is_ascii_digit() {
        let mut j = pos + 2;
        while j < s.len() && s[j].is_ascii_digit() {
            j += 1;
        }
        return Some(j - pos);
    }
    None
}

fn is_complete_unicode(s: &[u8], pos: usize, width: usize) -> bool {
    if s.len() < pos + 1 + width {
        return false;
    }
    s[pos + 1..pos + 1 + width].iter().all(|&b| is_hex(b))
}

fn match_char(s: &[u8], pos: usize) -> Option<usize> {
    if pos >= s.len() || s[pos] != b'\'' {
        return None;
    }
    let l = match_cconst_char(s, pos + 1)?;
    if pos + 1 + l < s.len() && s[pos + 1 + l] == b'\'' {
        return Some(2 + l);
    }
    None
}

fn match_multichar(s: &[u8], pos: usize) -> Option<usize> {
    if pos >= s.len() || s[pos] != b'\'' {
        return None;
    }
    // Greedy `{2,4}`: longest first.
    for count in (2..=4usize).rev() {
        let mut j = pos + 1;
        let mut ok = true;
        for _ in 0..count {
            match match_cconst_char(s, j) {
                Some(l) => j += l,
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if ok && j < s.len() && s[j] == b'\'' {
            return Some(j + 1 - pos);
        }
    }
    None
}

fn match_wchar(s: &[u8], pos: usize) -> Option<usize> {
    if pos < s.len() && s[pos] == b'L' {
        if let Some(l) = match_char(s, pos + 1) {
            return Some(1 + l);
        }
    }
    None
}

fn match_u8char(s: &[u8], pos: usize) -> Option<usize> {
    if s.len() > pos + 2 && s[pos] == b'u' && s[pos + 1] == b'8' {
        if let Some(l) = match_char(s, pos + 2) {
            return Some(2 + l);
        }
    }
    None
}

fn match_u16char(s: &[u8], pos: usize) -> Option<usize> {
    if pos < s.len() && s[pos] == b'u' {
        if let Some(l) = match_char(s, pos + 1) {
            return Some(1 + l);
        }
    }
    None
}

fn match_u32char(s: &[u8], pos: usize) -> Option<usize> {
    if pos < s.len() && s[pos] == b'U' {
        if let Some(l) = match_char(s, pos + 1) {
            return Some(1 + l);
        }
    }
    None
}

fn match_unmatched_quote(s: &[u8], pos: usize) -> Option<usize> {
    if pos >= s.len() || s[pos] != b'\'' {
        return None;
    }
    let mut j = pos + 1;
    while let Some(l) = match_cconst_char(s, j) {
        j += l;
    }
    if j < s.len() && s[j] == b'\n' {
        return Some(j + 1 - pos);
    }
    if j >= s.len() {
        return Some(j - pos);
    }
    None
}

fn match_bad_char(s: &[u8], pos: usize) -> Option<usize> {
    if pos >= s.len() || s[pos] != b'\'' {
        return None;
    }
    // Alternative 1: `'` cconst_char `[^'\n]+` `'`.
    if let Some(l) = match_cconst_char(s, pos + 1) {
        let mut j = pos + 1 + l;
        let mut count = 0;
        while j < s.len() && s[j] != b'\'' && s[j] != b'\n' {
            j += 1;
            count += 1;
        }
        if count >= 1 && j < s.len() && s[j] == b'\'' {
            return Some(j + 1 - pos);
        }
    }
    // Alternative 2: `''`.
    if s.len() > pos + 1 && s[pos + 1] == b'\'' {
        return Some(2);
    }
    // Alternative 3: `'` bad_escape `[^'\n]*` `'`.
    if s.len() > pos + 2 && s[pos + 1] == b'\\' && is_bad_escape_second(s[pos + 2]) {
        let mut j = pos + 3;
        while j < s.len() && s[j] != b'\'' && s[j] != b'\n' {
            j += 1;
        }
        if j < s.len() && s[j] == b'\'' {
            return Some(j + 1 - pos);
        }
    }
    None
}

fn match_identifier(s: &[u8], pos: usize) -> Option<usize> {
    if pos >= s.len() || !is_id_start(s[pos]) {
        return None;
    }
    let mut j = pos + 1;
    while j < s.len() && is_id_char(s[j]) {
        j += 1;
    }
    Some(j - pos)
}

// ----------------------------------------------------------------------
// Attribute matchers.
// ----------------------------------------------------------------------

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c | 0x0b)
}

fn match_attribute_begin(s: &[u8], pos: usize) -> Option<usize> {
    if pos >= s.len() || s[pos] != b'[' {
        return None;
    }
    let mut j = pos + 1;
    while j < s.len() && is_space(s[j]) {
        j += 1;
    }
    if j < s.len() && s[j] == b'[' {
        return Some(j + 1 - pos);
    }
    None
}

fn match_attribute_end(s: &[u8], pos: usize) -> Option<usize> {
    if pos >= s.len() || s[pos] != b']' {
        return None;
    }
    let mut j = pos + 1;
    while j < s.len() && is_space(s[j]) {
        j += 1;
    }
    if j < s.len() && s[j] == b']' {
        return Some(j + 1 - pos);
    }
    None
}

fn match_attribute_word(s: &[u8], pos: usize) -> Option<usize> {
    if pos >= s.len() {
        return None;
    }
    if is_id_start(s[pos]) {
        let mut j = pos + 1;
        while j < s.len() && is_id_char(s[j]) {
            j += 1;
        }
        return Some(j);
    }
    let first_ok = s[pos].is_ascii_digit()
        || (s[pos] == b'.' && pos + 1 < s.len() && s[pos + 1].is_ascii_digit());
    if !first_ok {
        return None;
    }
    let mut j = pos + 1;
    loop {
        if j < s.len()
            && matches!(s[j], b'e' | b'E' | b'p' | b'P')
            && j + 1 < s.len()
            && matches!(s[j + 1], b'+' | b'-')
        {
            j += 2;
        } else if j < s.len()
            && (s[j].is_ascii_alphanumeric() || s[j] == b'_' || s[j] == b'.' || s[j] == b'\'')
        {
            j += 1;
        } else {
            break;
        }
    }
    Some(j)
}

// ----------------------------------------------------------------------
// `#`-dispatch helpers.
// ----------------------------------------------------------------------

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn is_line_start(text: &[u8], pos: usize) -> bool {
    let mut j = pos;
    while j < text.len() && (text[j] == b' ' || text[j] == b'\t') {
        j += 1;
    }
    if j >= text.len() {
        return false;
    }
    if text[j..].starts_with(b"line") {
        let k = j + 4;
        if k >= text.len() || !is_word(text[k]) {
            return true;
        }
    }
    text[j].is_ascii_digit()
}

fn is_pragma_start(text: &[u8], pos: usize) -> bool {
    let mut j = pos;
    while j < text.len() && (text[j] == b' ' || text[j] == b'\t') {
        j += 1;
    }
    if text[j..].starts_with(b"pragma") {
        let k = j + 6;
        if k >= text.len() || !is_word(text[k]) {
            return true;
        }
    }
    false
}

/// Decode the character starting at byte `pos` (safe on any boundary).
fn decode_char_at(text: &[u8], pos: usize) -> char {
    let n = char_len_at(text, pos);
    let end = (pos + n).min(text.len());
    std::str::from_utf8(&text[pos..end])
        .ok()
        .and_then(|s| s.chars().next())
        .unwrap_or('\u{FFFD}')
}

fn char_len_at(text: &[u8], pos: usize) -> usize {
    if pos >= text.len() {
        return 1;
    }
    let b = text[pos];
    if b < 0x80 {
        1
    } else if (0xC2..0xE0).contains(&b) {
        2.min(text.len() - pos)
    } else if (0xE0..0xF0).contains(&b) {
        3.min(text.len() - pos)
    } else if (0xF0..0xF5).contains(&b) {
        4.min(text.len() - pos)
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex_all(text: &str, c23: bool) -> Vec<LexEvent> {
        let mut lx = Lexer::new(c23);
        lx.input(text, "t.c");
        let mut out = Vec::new();
        loop {
            match lx.next_event() {
                LexEvent::End => break,
                ev => out.push(ev),
            }
        }
        out
    }

    fn kinds(text: &str, c23: bool) -> Vec<(&'static str, String)> {
        lex_all(text, c23)
            .into_iter()
            .filter_map(|ev| match ev {
                LexEvent::Token(t) => Some((t.kind.name(), t.value)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn basic_tokens() {
        assert_eq!(
            kinds("int x;", false),
            [("INT", "int"), ("ID", "x"), ("SEMI", ";")]
                .into_iter()
                .map(|(a, b)| (a, b.to_string()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn longest_match_against_regex() {
        // `.123` must lex as one float, not PERIOD + number.
        assert_eq!(kinds(".123;", false)[0].0, "FLOAT_CONST");
        // `>>=` before `>>` before `>`.
        let ks: Vec<&str> = kinds("a>>=b;", false).iter().map(|(k, _)| *k).collect();
        assert_eq!(ks, ["ID", "RSHIFTEQUAL", "ID", "SEMI"]);
    }

    #[test]
    fn float_suffixes_and_separators() {
        assert_eq!(
            kinds("1'2.3'4e-5'6DF;", false)[0],
            ("FLOAT_CONST", "1'2.3'4e-5'6DF".to_string())
        );
        assert_eq!(
            kinds("4e+2DF;", true)[0],
            ("FLOAT_CONST", "4e+2DF".to_string())
        );
        assert_eq!(
            kinds("3e1DL;", false)[0],
            ("FLOAT_CONST", "3e1DL".to_string())
        );
        assert_eq!(
            kinds("0x1.8p+2;", false)[0],
            ("HEX_FLOAT_CONST", "0x1.8p+2".to_string())
        );
    }

    #[test]
    fn int_suffix_case_matters() {
        // `wB`/`Wb` mixed case are not suffixes (lexer splits the name).
        assert_eq!(kinds("42wB;", false)[0].0, "INT_CONST_DEC");
        assert_eq!(kinds("42wB;", false)[1].0, "ID");
        assert_eq!(kinds("42wb;", false)[0].0, "INT_CONST_DEC");
        assert_eq!(
            kinds("42WB;", false)[0],
            ("INT_CONST_DEC", "42WB".to_string())
        );
        assert_eq!(kinds("42ull;", false)[0].0, "INT_CONST_DEC");
        assert_eq!(kinds("09;", false).len(), 1); // error event, no token
        assert!(matches!(lex_all("09;", false)[0], LexEvent::Error(_, _)));
    }

    #[test]
    fn bad_escapes() {
        // Two bad escapes: greedy-prefix rule reports one spanning error.
        let evs = lex_all("\"a\\#b\\#c\";", false);
        assert!(matches!(evs[0], LexEvent::Error(_, _)));
        // Valid escapes scan clean.
        assert_eq!(kinds("\"a\\t\\x41\";", false)[0].0, "STRING_LITERAL");
    }

    #[test]
    fn char_const_permissive_escapes() {
        assert_eq!(kinds("'\\x41';", false)[0].0, "CHAR_CONST");
        assert_eq!(kinds("'ab';", false)[0].0, "INT_CONST_CHAR");
    }

    #[test]
    fn c23_keywords() {
        assert_eq!(kinds("alignof(x);", true)[0].0, "_ALIGNOF");
        assert_eq!(kinds("alignof(x);", false)[0].0, "ID");
        assert_eq!(kinds("true;", true)[0].0, "BOOL_CONST");
        assert_eq!(kinds("nullptr;", true)[0].0, "NULLPTR");
    }

    #[test]
    fn line_directive_with_separators() {
        let evs = lex_all("#line 1'000 \"f.c\"\nint x;", false);
        match &evs[0] {
            LexEvent::Token(t) => {
                assert_eq!(t.kind, TokenKind::Int);
                assert_eq!((t.lineno, t.column), (1000, 1));
                assert_eq!(t.filename, "f.c");
            }
            _ => panic!("expected token, got {evs:?}"),
        }
    }
}
