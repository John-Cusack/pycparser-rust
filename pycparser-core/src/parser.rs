//! Recursive-descent C parser: transcription of `pycparser.c_parser`.
//!
//! The parser drives the lexer directly (whole translation unit crosses
//! from Python once; no per-token FFI). Method structure, error strings,
//! coordinates and tree shapes mirror the upstream methods one-to-one; the
//! [`Node`] tree converts field-for-field into the genuine `c_ast` classes.

use std::collections::HashMap;

use crate::ast::{Coord, CoordArg, Node, ParseError, PragmaString, RawToken, TokenKind};
use crate::lexer::{LexEvent, Lexer};

pub type PResult<T> = Result<T, ParseError>;

/// Result of `_try_parse_paren_type_name`: type, backtracking mark,
/// `(` coordinate, C23 storage classes.
pub type ParenTypeName = Option<(Node, usize, Coord, Vec<String>)>;

/// Declaration specifiers under construction (mirrors `_DeclSpec`).
#[derive(Debug, Clone, Default)]
pub struct DeclSpec {
    pub qual: Vec<String>,
    pub storage: Vec<String>,
    pub typ: Vec<Node>,
    pub function: Vec<String>,
    pub alignment: Vec<Node>,
    pub type_attributes: Vec<Node>,
}

/// One init-declarator payload (mirrors `_DeclInfo`).
#[derive(Debug, Clone)]
pub struct DeclInfo {
    pub decl: Option<Node>,
    pub init: Option<Node>,
    pub bitsize: Option<Node>,
}

pub struct Parser {
    lexer: Lexer,
    dialect_c23: bool,
    tokens: Vec<RawToken>,
    index: usize,
    scopes: Vec<HashMap<String, bool>>,
    /// `(buffer index, generation)` of the last classified token.
    checked: Option<(usize, u64)>,
    gen: u64,
}

impl Parser {
    pub fn new(c23: bool) -> Self {
        Self {
            lexer: Lexer::new(c23),
            dialect_c23: c23,
            tokens: Vec::new(),
            index: 0,
            scopes: vec![HashMap::new()],
            checked: None,
            gen: 0,
        }
    }

    /// Parse a translation unit (mirrors `CParser.parse`).
    pub fn parse(&mut self, text: &str, filename: &str) -> PResult<Node> {
        self.scopes = vec![HashMap::new()];
        self.checked = None;
        self.gen = 0;
        self.lexer.input(text, filename);
        self.tokens.clear();
        self.index = 0;
        let ast = self.parse_translation_unit_or_empty()?;
        if self.peek(1)?.is_some() {
            let idx = self.peek_idx(1)?.expect("peeked");
            let tok = &self.tokens[idx];
            let coord = self.tok_coord(tok);
            return Err(self.error(format!("before: {}", tok.value), CoordArg::Full(coord)));
        }
        Ok(ast)
    }

    fn error(&self, msg: String, coord: CoordArg) -> ParseError {
        ParseError::new(msg, &coord)
    }

    fn filename(&self) -> String {
        self.lexer.filename().to_string()
    }

    fn fill(&mut self, n: usize) -> PResult<()> {
        while self.tokens.len() < self.index + n {
            match self.lexer.next_event() {
                LexEvent::Token(tok) => self.tokens.push(tok),
                LexEvent::Error(err, _) => {
                    return Err(self.error(
                        err.msg,
                        CoordArg::Full(Coord {
                            file: self.filename(),
                            line: err.line,
                            col: err.column,
                        }),
                    ));
                }
                // End of input: the buffer simply stops growing (mirrors
                // appending `None` and breaking in `_TokenStream._fill`).
                LexEvent::End => break,
            }
        }
        Ok(())
    }

    fn peek_idx(&mut self, k: usize) -> PResult<Option<usize>> {
        if k == 0 {
            return Ok(None);
        }
        let pos = self.index + k - 1;
        if pos >= self.tokens.len() {
            self.fill(k)?;
        }
        if pos >= self.tokens.len() {
            return Ok(None);
        }
        self.classify(pos);
        Ok(Some(pos))
    }

    fn peek(&mut self, k: usize) -> PResult<Option<usize>> {
        self.peek_idx(k)
    }

    fn next_idx(&mut self) -> PResult<Option<usize>> {
        if self.index >= self.tokens.len() {
            self.fill(1)?;
        }
        if self.index >= self.tokens.len() {
            return Ok(None);
        }
        let pos = self.index;
        self.index += 1;
        self.classify(pos);
        Ok(Some(pos))
    }

    fn classify(&mut self, pos: usize) {
        if self.checked == Some((pos, self.gen)) {
            return;
        }
        let is_type = match self.tokens[pos].kind {
            TokenKind::Id | TokenKind::TypeId => {
                self.is_type_in_scope(&self.tokens[pos].value.clone())
            }
            _ => false,
        };
        if matches!(self.tokens[pos].kind, TokenKind::Id | TokenKind::TypeId) {
            self.tokens[pos].kind = if is_type {
                TokenKind::TypeId
            } else {
                TokenKind::Id
            };
        }
        self.checked = Some((pos, self.gen));
    }

    fn mark(&self) -> usize {
        self.index
    }

    fn reset(&mut self, mark: usize) {
        self.index = mark;
    }

    fn bump_gen(&mut self) {
        self.gen += 1;
    }

    // ------------------------------------------------------------------
    // Scope helpers.
    // ------------------------------------------------------------------
    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
        self.bump_gen();
    }

    fn pop_scope(&mut self) {
        assert!(self.scopes.len() > 1, "Cannot pop the global scope");
        self.scopes.pop();
        self.bump_gen();
    }

    fn add_typedef_name(&mut self, name: Option<&str>, coord: CoordArg) -> PResult<()> {
        let Some(name) = name else { return Ok(()) };
        if self
            .scopes
            .last()
            .expect("scope")
            .get(name)
            .copied()
            .unwrap_or(true)
        {
            // `get(name, True)`: missing means "not an identifier" -> error
            // only when present-and-false.
        } else {
            return Err(self.error(
                format!("Typedef {name:?} previously declared as non-typedef in this scope"),
                coord,
            ));
        }
        self.scopes
            .last_mut()
            .expect("scope")
            .insert(name.to_string(), true);
        self.bump_gen();
        Ok(())
    }

    fn add_identifier(&mut self, name: Option<&str>, coord: CoordArg) -> PResult<()> {
        let Some(name) = name else { return Ok(()) };
        if self
            .scopes
            .last()
            .expect("scope")
            .get(name)
            .copied()
            .unwrap_or(false)
        {
            return Err(self.error(
                format!("Non-typedef {name:?} previously declared as typedef in this scope"),
                coord,
            ));
        }
        self.scopes
            .last_mut()
            .expect("scope")
            .insert(name.to_string(), false);
        self.bump_gen();
        Ok(())
    }

    fn is_type_in_scope(&self, name: &str) -> bool {
        for scope in self.scopes.iter().rev() {
            if let Some(&is_type) = scope.get(name) {
                return is_type;
            }
        }
        false
    }

    fn tok_coord_idx(&self, idx: usize) -> Coord {
        let tok = &self.tokens[idx];
        Coord {
            file: tok.filename.clone(),
            line: tok.lineno,
            col: tok.column,
        }
    }

    fn tok_coord(&self, tok: &RawToken) -> Coord {
        Coord {
            file: tok.filename.clone(),
            line: tok.lineno,
            col: tok.column,
        }
    }
}
impl Parser {
    // ------------------------------------------------------------------
    // Token helpers.
    // ------------------------------------------------------------------
    fn peek_kind(&mut self, k: usize) -> PResult<Option<TokenKind>> {
        Ok(self.peek_idx(k)?.map(|i| self.tokens[i].kind))
    }

    fn advance(&mut self) -> PResult<usize> {
        match self.next_idx()? {
            Some(i) => Ok(i),
            None => Err(self.error(
                "At end of input".to_string(),
                CoordArg::file(self.filename()),
            )),
        }
    }

    fn accept(&mut self, kind: TokenKind) -> PResult<Option<usize>> {
        match self.peek_idx(1)? {
            Some(i) if self.tokens[i].kind == kind => self.next_idx(),
            _ => Ok(None),
        }
    }

    fn expect(&mut self, kind: TokenKind) -> PResult<usize> {
        let i = self.advance()?;
        if self.tokens[i].kind != kind {
            let coord = self.tok_coord_idx(i);
            return Err(self.error(
                format!("before: {}", self.tokens[i].value),
                CoordArg::Full(coord),
            ));
        }
        Ok(i)
    }

    fn starts_declaration_idx(&mut self, idx: Option<usize>) -> PResult<bool> {
        let idx = match idx {
            Some(i) => Some(i),
            None => self.peek_idx(1)?,
        };
        Ok(matches!(idx, Some(i) if decl_starts(self.tokens[i].kind)))
    }

    fn starts_expression_idx(&mut self, idx: Option<usize>) -> PResult<bool> {
        let idx = match idx {
            Some(i) => Some(i),
            None => self.peek_idx(1)?,
        };
        Ok(matches!(idx, Some(i) if expr_starts(self.tokens[i].kind)))
    }

    fn starts_statement(&mut self) -> PResult<bool> {
        if self.peek_kind(1)? == Some(TokenKind::AttributeBegin) {
            let mark = self.mark();
            self.parse_attribute_specifier_sequence()?;
            let starts = !self.starts_declaration_idx(None)?;
            self.reset(mark);
            return Ok(starts);
        }
        let Some(kind) = self.peek_kind(1)? else {
            return Ok(false);
        };
        if stmt_starts(kind) {
            return Ok(true);
        }
        self.starts_expression_idx(None)
    }

    fn starts_declarator(&mut self, id_only: bool) -> PResult<bool> {
        let kind = self.peek_kind(1)?;
        let Some(kind) = kind else { return Ok(false) };
        if matches!(kind, TokenKind::Times | TokenKind::Lparen) {
            return Ok(true);
        }
        if id_only {
            return Ok(kind == TokenKind::Id);
        }
        Ok(matches!(kind, TokenKind::Id | TokenKind::TypeId))
    }

    fn is_assignment_op(&mut self) -> PResult<bool> {
        Ok(matches!(self.peek_idx(1)?, Some(i) if is_assignment_op(self.tokens[i].kind)))
    }

    fn peek_declarator_name_info(&mut self) -> PResult<(Option<TokenKind>, bool)> {
        let mark = self.mark();
        let out = self.scan_declarator_name_info()?;
        self.reset(mark);
        Ok(out)
    }

    /// Mirrors `_parse_any_declarator`: returns `(declarator, is_named)`.
    fn parse_any_declarator(
        &mut self,
        allow_abstract: bool,
        typeid_paren_as_abstract: bool,
    ) -> PResult<(Option<Node>, bool)> {
        let (name_type, saw_paren) = self.peek_declarator_name_info()?;
        if name_type.is_none()
            || (typeid_paren_as_abstract && name_type == Some(TokenKind::TypeId) && saw_paren)
        {
            if !allow_abstract {
                let coord = match self.peek_idx(1)? {
                    Some(i) => CoordArg::Full(self.tok_coord_idx(i)),
                    None => CoordArg::file(self.filename()),
                };
                return Err(self.error("Invalid declarator".to_string(), coord));
            }
            return Ok((self.parse_abstract_declarator_opt()?, false));
        }
        if name_type == Some(TokenKind::TypeId) {
            if typeid_paren_as_abstract {
                return Ok((Some(self.parse_typeid_noparen_declarator()?), true));
            }
            return Ok((Some(self.parse_typeid_declarator()?), true));
        }
        Ok((Some(self.parse_id_declarator()?), true))
    }

    fn scan_declarator_name_info(&mut self) -> PResult<(Option<TokenKind>, bool)> {
        let mut saw_paren = false;
        while self.accept(TokenKind::Times)?.is_some() {
            self.parse_attribute_specifier_sequence()?;
            while matches!(self.peek_kind(1)?, Some(k) if is_type_qualifier(k)) {
                self.advance()?;
            }
        }
        let idx = self.peek_idx(1)?;
        let Some(i) = idx else {
            return Ok((None, saw_paren));
        };
        let kind = self.tokens[i].kind;
        if matches!(kind, TokenKind::Id | TokenKind::TypeId) {
            self.advance()?;
            return Ok((Some(kind), saw_paren));
        }
        if kind == TokenKind::Lparen {
            saw_paren = true;
            self.advance()?;
            let (tok_type, nested_paren) = self.scan_declarator_name_info()?;
            if nested_paren {
                saw_paren = true;
            }
            let mut depth = 1;
            loop {
                let idx = self.peek_idx(1)?;
                let Some(j) = idx else {
                    return Ok((None, saw_paren));
                };
                let k = self.tokens[j].kind;
                if k == TokenKind::Lparen {
                    depth += 1;
                } else if k == TokenKind::Rparen {
                    depth -= 1;
                    self.advance()?;
                    if depth == 0 {
                        break;
                    }
                    continue;
                }
                self.advance()?;
            }
            return Ok((tok_type, saw_paren));
        }
        Ok((None, saw_paren))
    }

    fn starts_direct_abstract_declarator(&mut self) -> PResult<bool> {
        Ok(matches!(
            self.peek_kind(1)?,
            Some(TokenKind::Lparen) | Some(TokenKind::Lbracket)
        ))
    }

    /// Mirrors `_try_parse_paren_type_name`: the parsed type, the
    /// backtracking mark, the `(` coordinate, and C23 storage classes.
    fn try_parse_paren_type_name(&mut self) -> PResult<ParenTypeName> {
        let lparen = self.peek_idx(1)?;
        let Some(lp) = lparen else {
            return Ok(None);
        };
        if self.tokens[lp].kind != TokenKind::Lparen {
            return Ok(None);
        }
        let mark = self.mark();
        self.advance()?;
        if !self.starts_declaration_idx(None)? {
            self.reset(mark);
            return Ok(None);
        }
        let mut storage = Vec::new();
        while matches!(self.peek_kind(1)?, Some(k) if is_storage_class(k)) {
            let t = self.advance()?;
            if !self.dialect_c23 {
                let coord = self.tok_coord_idx(t);
                return Err(self.error(
                    "Storage class specifiers in compound literals require C23".to_string(),
                    CoordArg::Full(coord),
                ));
            }
            storage.push(self.tokens[t].value.clone());
        }
        let typ = self.parse_type_name()?;
        if self.accept(TokenKind::Rparen)?.is_none() {
            self.reset(mark);
            return Ok(None);
        }
        let lparen_coord = self.tok_coord_idx(lp);
        if !storage.is_empty() && self.peek_kind(1)? != Some(TokenKind::Lbrace) {
            return Err(self.error(
                "Storage class specifiers require a compound literal initializer".to_string(),
                CoordArg::Full(lparen_coord),
            ));
        }
        Ok(Some((typ, mark, lparen_coord, storage)))
    }
}
impl Parser {
    // ------------------------------------------------------------------
    // Declaration builders.
    // ------------------------------------------------------------------
    fn add_declaration_specifier(
        &mut self,
        spec: &mut DeclSpec,
        newspec: SpecValue,
        kind: DeclSpecKind,
    ) {
        match (kind, newspec) {
            (DeclSpecKind::Type, SpecValue::Node(n)) => spec.typ.push(n),
            (DeclSpecKind::Alignment, SpecValue::Node(n)) => spec.alignment.push(n),
            (DeclSpecKind::Qual, SpecValue::Str(s)) => spec.qual.push(s),
            (DeclSpecKind::Storage, SpecValue::Str(s)) => spec.storage.push(s),
            (DeclSpecKind::Function, SpecValue::Str(s)) => spec.function.push(s),
            _ => unreachable!("specifier kind/value mismatch"),
        }
    }

    /// Mirrors `_fix_decl_name_type`, including the eager
    /// `fix_atomic_specifiers` application for single complex specifiers.
    ///
    /// `spec_typ` holds one `Rc` per spec element, shared across the
    /// declaration's declarators; the single-complex-specifier path inserts
    /// a `Shared` reference instead of cloning, so converted objects
    /// preserve identity (mirrors upstream object sharing).
    fn fix_decl_name_type(
        &mut self,
        mut decl: Node,
        spec_typ: &[std::rc::Rc<Node>],
    ) -> PResult<Node> {
        // Reach the underlying basic type.
        let declname = match typedecl_tail(&decl) {
            Node::TypeDecl { declname, .. } => declname.clone(),
            _ => unreachable!("declarator chain without TypeDecl"),
        };
        let quals_copy = decl_quals(&decl).map(|q| q.to_vec());
        {
            let tail = typedecl_tail_mut(&mut decl);
            let Node::TypeDecl { quals, .. } = tail else {
                unreachable!("declarator chain without TypeDecl");
            };
            *quals = quals_copy;
        }
        set_decl_name(&mut decl, declname);
        let mut names: Vec<String> = Vec::new();
        for tn in spec_typ {
            if let Node::BitIntType { .. } = tn.as_ref() {
                let mut signedness: Option<String> = None;
                let self_pos = spec_typ.iter().position(|s| std::rc::Rc::ptr_eq(s, tn));
                for (si, spec) in spec_typ.iter().enumerate() {
                    // Skip the BitInt specifier itself.
                    if Some(si) == self_pos {
                        continue;
                    }
                    let ok = match spec.as_ref() {
                        Node::IdentifierType { names, .. } => {
                            names.len() == 1 && (names[0] == "signed" || names[0] == "unsigned")
                        }
                        _ => false,
                    };
                    if signedness.is_some() || !ok {
                        return Err(self.error(
                            "Invalid multiple types specified".to_string(),
                            CoordArg::opt(spec.coord().cloned()),
                        ));
                    }
                    let Node::IdentifierType { names, .. } = spec.as_ref() else {
                        unreachable!()
                    };
                    signedness = Some(names[0].clone());
                }
                // Rebuild the width node: `typ.type = BitIntType(width,
                // signedness, tn.coord)`; specifiers are shared, so the
                // parsed node is left unchanged.
                let Node::BitIntType {
                    width,
                    signedness: _,
                    coord,
                } = tn.as_ref().clone()
                else {
                    unreachable!()
                };
                let bitint = Node::BitIntType {
                    width,
                    signedness,
                    coord,
                };
                set_decl_inner_type(&mut decl, bitint);
                return Ok(decl);
            }
            if !matches!(tn.as_ref(), Node::IdentifierType { .. }) {
                if spec_typ.len() > 1 {
                    return Err(self.error(
                        "Invalid multiple types specified".to_string(),
                        CoordArg::opt(tn.coord().cloned()),
                    ));
                }
                set_decl_inner_type(&mut decl, Node::Shared(std::rc::Rc::clone(tn)));
                return fix_atomic_specifiers(decl);
            }
            let Node::IdentifierType { names: ns, .. } = tn.as_ref() else {
                unreachable!()
            };
            names.extend(ns.iter().cloned());
        }
        if spec_typ.is_empty() {
            // Functions default to returning int.
            let is_func = matches!(decl_inner(&decl), Some(Node::FuncDecl { .. }));
            if !is_func {
                let coord = decl.coord().cloned();
                return Err(self.error(
                    "Missing type in declaration".to_string(),
                    CoordArg::opt(coord),
                ));
            }
            let coord = decl.coord().cloned();
            set_decl_inner_type(
                &mut decl,
                Node::IdentifierType {
                    names: vec!["int".to_string()],
                    coord,
                },
            );
        } else {
            let coord = spec_typ[0].coord().cloned();
            set_decl_inner_type(&mut decl, Node::IdentifierType { names, coord });
        }
        Ok(decl)
    }

    fn attach_type_attributes(&mut self, mut decl: Node, spec: &DeclSpec) -> Node {
        if !spec.type_attributes.is_empty() {
            let target = attributed_target_mut(&mut decl);
            let inner = take_inner(target);
            let coord = inner.coord().cloned();
            set_inner(
                target,
                Node::AttributedType {
                    typ: Box::new(inner),
                    attributes: spec.type_attributes.clone(),
                    coord,
                },
            );
        }
        decl
    }

    fn build_type_name(
        &mut self,
        spec: &DeclSpec,
        decl: Option<Node>,
        coord: Option<Coord>,
    ) -> PResult<Node> {
        let inner = decl.unwrap_or(Node::TypeDecl {
            declname: None,
            quals: None,
            align: None,
            typ: None,
            attributes: None,
            coord: None,
        });
        let typename = Node::Typename {
            name: None,
            quals: spec.qual.clone(),
            align: None,
            typ: Box::new(inner),
            attributes: None,
            coord,
        };
        let shared: Vec<std::rc::Rc<Node>> = spec
            .typ
            .iter()
            .map(|n| std::rc::Rc::new(n.clone()))
            .collect();
        let fixed = self.fix_decl_name_type(typename, &shared)?;
        Ok(self.attach_type_attributes(fixed, spec))
    }

    fn build_declarations(
        &mut self,
        spec: &mut DeclSpec,
        mut decls: Vec<DeclInfo>,
        typedef_namespace: bool,
    ) -> PResult<Vec<Node>> {
        let is_typedef = spec.storage.iter().any(|s| s == "typedef");
        // Bit-fields are allowed to be unnamed.
        if decls[0].bitsize.is_none() {
            if decls[0].decl.is_none() {
                if spec.typ.len() < 2
                    || !matches!(&spec.typ[spec.typ.len() - 1], Node::IdentifierType { names, .. } if names.len() == 1)
                    || {
                        let Node::IdentifierType { names, .. } = &spec.typ[spec.typ.len() - 1]
                        else {
                            unreachable!()
                        };
                        !self.is_type_in_scope(&names[0])
                    }
                {
                    let coord = spec
                        .typ
                        .iter()
                        .find_map(|t| t.coord().cloned())
                        .map(CoordArg::Full)
                        .unwrap_or(CoordArg::Text("?".to_string()));
                    return Err(self.error("Invalid declaration".to_string(), coord));
                }
                let last = spec.typ.pop().expect("spec type");
                let Node::IdentifierType { names, coord } = last else {
                    unreachable!()
                };
                decls[0].decl = Some(Node::TypeDecl {
                    declname: Some(names[0].clone()),
                    typ: None,
                    quals: None,
                    align: Some(spec.alignment.clone()),
                    coord,
                    attributes: None,
                });
            } else if is_declarator_node(decls[0].decl.as_ref().expect("decl")) {
                // A similar problem where the declaration ends up looking
                // like an abstract declarator: give it a name from the last
                // specifier when the TypeDecl tail is unnamed.
                if tail_declname_unset(decls[0].decl.as_ref().expect("decl")) {
                    let last = spec.typ.pop().expect("spec type");
                    let Node::IdentifierType { names, .. } = last else {
                        unreachable!("expected IdentifierType specifier")
                    };
                    set_tail_declname(decls[0].decl.as_mut().expect("decl"), names[0].clone());
                }
            }
        }
        // One `Rc` per spec element for the whole batch: every declarator
        // inserts a `Shared` reference to the same object, mirroring
        // upstream's shared `spec["type"]` nodes.
        let shared: Vec<std::rc::Rc<Node>> = spec
            .typ
            .iter()
            .map(|n| std::rc::Rc::new(n.clone()))
            .collect();
        let mut declarations = Vec::with_capacity(decls.len());
        for decl in &decls {
            let d = decl.decl.clone().expect("decl node");
            let coord = d.coord().cloned();
            let declaration = if is_typedef {
                Node::Typedef {
                    name: None,
                    quals: spec.qual.clone(),
                    storage: spec.storage.clone(),
                    typ: Box::new(d),
                    attributes: None,
                    coord,
                }
            } else {
                Node::Decl {
                    name: None,
                    quals: spec.qual.clone(),
                    align: spec.alignment.clone(),
                    storage: spec.storage.clone(),
                    funcspec: spec.function.clone(),
                    typ: Box::new(d),
                    init: decl.init.clone().map(Box::new),
                    bitsize: decl.bitsize.clone().map(Box::new),
                    attributes: None,
                    coord,
                }
            };
            // `isinstance(declaration.type, _DECLARATOR_NODES)`: only then
            // fix the name/type; otherwise keep the declaration as built.
            let inner_is_declarator = decl_inner(&declaration)
                .map(is_declarator_node)
                .unwrap_or(false);
            let fixed_decl = if !inner_is_declarator {
                declaration
            } else {
                self.fix_decl_name_type(declaration, &shared)?
            };
            // Record the declared name in the current scope.
            if typedef_namespace {
                let arg = CoordArg::opt(fixed_decl.coord().cloned());
                if is_typedef {
                    self.add_typedef_name(decl_name(&fixed_decl).as_deref(), arg)?;
                } else {
                    self.add_identifier(decl_name(&fixed_decl).as_deref(), arg)?;
                }
            }
            declarations.push(self.attach_type_attributes(fixed_decl, spec));
        }
        Ok(declarations)
    }

    fn build_function_definition(
        &mut self,
        spec: &mut DeclSpec,
        decl: Node,
        param_decls: Option<Vec<Node>>,
        body: Node,
    ) -> PResult<Node> {
        if spec.storage.iter().any(|s| s == "typedef") {
            let coord = decl.coord().cloned();
            return Err(self.error("Invalid typedef".to_string(), CoordArg::opt(coord)));
        }
        let mut declaration = self.build_declarations(
            spec,
            vec![DeclInfo {
                decl: Some(decl),
                init: None,
                bitsize: None,
            }],
            true,
        )?;
        let decl_node = declaration.pop().expect("one declaration");
        let coord = decl_node.coord().cloned();
        Ok(Node::FuncDef {
            decl: Box::new(decl_node),
            param_decls,
            body: Box::new(body),
            coord,
        })
    }
}
// ----------------------------------------------------------------------
// Tree navigation (mirrors duck-typed `.type` walks in the Python code).
// All shapes here are construction-guaranteed; anything else is a bug.
// ----------------------------------------------------------------------

/// The `.type`-like child used by modifier splicing, if any.
fn decl_inner(n: &Node) -> Option<&Node> {
    match n {
        Node::TypeDecl { typ: Some(t), .. } => Some(t),
        Node::PtrDecl { typ: Some(t), .. } => Some(t),
        Node::ArrayDecl { typ, .. } => typ.as_deref(),
        Node::FuncDecl { typ: Some(t), .. } => Some(t),
        Node::Typename { typ, .. } => Some(typ),
        Node::Decl { typ, .. } => Some(typ),
        Node::Typedef { typ, .. } => Some(typ),
        _ => None,
    }
}

fn decl_inner_mut(n: &mut Node) -> Option<&mut Node> {
    match n {
        Node::TypeDecl { typ: Some(t), .. } => Some(t),
        Node::PtrDecl { typ: Some(t), .. } => Some(t),
        Node::ArrayDecl { typ, .. } => typ.as_deref_mut(),
        Node::FuncDecl { typ: Some(t), .. } => Some(t),
        Node::Typename { typ, .. } => Some(typ),
        Node::Decl { typ, .. } => Some(typ),
        Node::Typedef { typ, .. } => Some(typ),
        _ => None,
    }
}

/// Set the `.type`-like child (modifier and declaration splicing).
fn set_inner(node: &mut Node, child: Node) {
    match node {
        Node::TypeDecl { typ, .. } => *typ = Some(Box::new(child)),
        Node::PtrDecl { typ, .. } => *typ = Some(Box::new(child)),
        Node::ArrayDecl { typ, .. } => *typ = Some(Box::new(child)),
        Node::FuncDecl { typ, .. } => *typ = Some(Box::new(child)),
        // Declaration wrappers (only reached by the atomic-specifier fix,
        // whose grandparent may be the declaration itself).
        Node::Decl { typ, .. } => **typ = child,
        Node::Typedef { typ, .. } => **typ = child,
        Node::Typename { typ, .. } => **typ = child,
        _ => unreachable!("set_inner on non-declarator"),
    }
}

/// Remove and return the `.type`-like child, leaving a placeholder.
fn take_inner(node: &mut Node) -> Node {
    let placeholder = Node::TypeDecl {
        declname: None,
        quals: None,
        align: None,
        typ: None,
        attributes: None,
        coord: None,
    };
    match node {
        Node::PtrDecl { typ, .. } => typ.take().map(|b| *b).unwrap_or(placeholder),
        Node::FuncDecl { typ, .. } => typ.take().map(|b| *b).unwrap_or(placeholder),
        Node::ArrayDecl { typ, .. } => typ.take().map(|b| *b).unwrap_or(placeholder),
        Node::TypeDecl { typ, .. } => typ.take().map(|b| *b).unwrap_or(placeholder),
        // Declaration wrappers (only reached via the atomic-specifier fix
        // path, mirroring upstream duck-typed `.type` access).
        Node::Decl { typ, .. } => *std::mem::replace(typ, Box::new(placeholder)),
        Node::Typedef { typ, .. } => *std::mem::replace(typ, Box::new(placeholder)),
        Node::Typename { typ, .. } => *std::mem::replace(typ, Box::new(placeholder)),
        _ => unreachable!("take_inner on non-declarator"),
    }
}

/// Deepest node whose child is the underlying `TypeDecl` (mirrors the
/// `decl_tail` walk in `_type_modify_decl`).
fn decl_splice_tail_mut(node: &mut Node) -> &mut Node {
    // Probe immutably first so no borrow is held across the descent.
    let descend: bool = match node as &Node {
        Node::PtrDecl { typ: Some(t), .. } => !matches!(**t, Node::TypeDecl { .. }),
        Node::ArrayDecl { typ: Some(t), .. } => !matches!(**t, Node::TypeDecl { .. }),
        Node::ArrayDecl { typ: None, .. } => false,
        Node::FuncDecl { typ: Some(t), .. } => !matches!(**t, Node::TypeDecl { .. }),
        _ => false,
    };
    if !descend {
        return node;
    }
    match node {
        Node::PtrDecl { typ: Some(t), .. } => decl_splice_tail_mut(t),
        Node::ArrayDecl { typ: Some(t), .. } => decl_splice_tail_mut(t),
        Node::FuncDecl { typ: Some(t), .. } => decl_splice_tail_mut(t),
        _ => unreachable!("splice tail changed under probe"),
    }
}

/// Deepest modifier in a modifier chain (mirrors the `modifier_tail` walk).
fn modifier_tail_mut(node: &mut Node) -> &mut Node {
    match node {
        Node::PtrDecl { typ: Some(t), .. } => modifier_tail_mut(t),
        // Upstream stops when `.type` is `None` (`while modifier_tail.type`).
        Node::ArrayDecl { typ: Some(t), .. } => modifier_tail_mut(t),
        Node::FuncDecl { typ: Some(t), .. } => modifier_tail_mut(t),
        _ => node,
    }
}

/// Mirrors `_type_modify_decl`.
fn type_modify_decl(decl: Node, modifier: Node) -> Node {
    let mut modifier = modifier;
    if matches!(decl, Node::TypeDecl { .. }) {
        let tail = modifier_tail_mut(&mut modifier);
        set_inner(tail, decl);
        return modifier;
    }
    let mut decl = decl;
    let tail = decl_splice_tail_mut(&mut decl);
    let stolen = take_inner(tail);
    let mtail = modifier_tail_mut(&mut modifier);
    set_inner(mtail, stolen);
    set_inner(tail, modifier);
    decl
}

fn is_declarator_node(n: &Node) -> bool {
    matches!(
        n,
        Node::TypeDecl { .. }
            | Node::PtrDecl { .. }
            | Node::ArrayDecl { .. }
            | Node::FuncDecl { .. }
    )
}

/// Declared name of a declaration node (`None` for abstract declarators).
fn decl_name(n: &Node) -> Option<String> {
    match n {
        Node::Decl { name, .. } => name.clone(),
        Node::Typedef { name, .. } => name.clone(),
        Node::Typename { name, .. } => name.clone(),
        _ => None,
    }
}

/// Qualifier list of a declaration node.
fn decl_quals(n: &Node) -> Option<&Vec<String>> {
    match n {
        Node::Decl { quals, .. } => Some(quals),
        Node::Typedef { quals, .. } => Some(quals),
        Node::Typename { quals, .. } => Some(quals),
        _ => None,
    }
}

fn set_decl_name(n: &mut Node, name: Option<String>) {
    match n {
        Node::Decl { name: slot, .. } => *slot = name,
        Node::Typedef { name: slot, .. } => *slot = name,
        Node::Typename { name: slot, .. } => *slot = name,
        _ => unreachable!("set_decl_name on non-declaration"),
    }
}

/// Replace the underlying basic type of a declaration (the `typ.type = ...`
/// assignments in `_fix_decl_name_type`).
fn set_decl_inner_type(n: &mut Node, inner: Node) {
    let mut cursor: &mut Node = n;
    loop {
        match cursor {
            Node::Decl { typ, .. } | Node::Typedef { typ, .. } | Node::Typename { typ, .. } => {
                cursor = typ;
            }
            Node::PtrDecl { typ: Some(t), .. } | Node::FuncDecl { typ: Some(t), .. } => {
                cursor = t;
            }
            Node::ArrayDecl { typ: Some(t), .. } => {
                cursor = t;
            }
            // A `None` array base cannot host the basic type (mirrors the
            // upstream walk stalling on a missing `.type`).
            Node::ArrayDecl { typ: None, .. } => {
                unreachable!("set_decl_inner_type without TypeDecl");
            }
            Node::TypeDecl { typ: slot, .. } => {
                *slot = Some(Box::new(inner));
                return;
            }
            _ => unreachable!("set_decl_inner_type without TypeDecl"),
        }
    }
}

/// Deepest node whose child can host `AttributedType` (mirrors the
/// `while isinstance(target.type, (PtrDecl, ArrayDecl, FuncDecl, TypeDecl))`
/// walk in `_attach_type_attributes`).
fn attributed_target_mut(n: &mut Node) -> &mut Node {
    match n {
        Node::Decl { typ, .. } | Node::Typedef { typ, .. } | Node::Typename { typ, .. } => {
            attributed_target_mut(typ)
        }
        Node::PtrDecl { typ: Some(t), .. } | Node::FuncDecl { typ: Some(t), .. } => {
            attributed_target_mut(t)
        }
        Node::ArrayDecl { typ: Some(t), .. } => attributed_target_mut(t),
        // Upstream stops when `.type` is missing (not one of the four).
        Node::ArrayDecl { typ: None, .. } => n,
        Node::TypeDecl { .. } => n,
        _ => n,
    }
}

// ----------------------------------------------------------------------
// Token classification tables (mirrors the module-level sets in c_parser).
// ----------------------------------------------------------------------

fn is_storage_class(k: TokenKind) -> bool {
    matches!(
        k,
        TokenKind::Auto
            | TokenKind::Constexpr
            | TokenKind::Register
            | TokenKind::Static
            | TokenKind::Extern
            | TokenKind::Typedef
            | TokenKind::ThreadLocal
    )
}

fn is_function_spec(k: TokenKind) -> bool {
    matches!(k, TokenKind::Inline | TokenKind::Noreturn)
}

fn is_type_qualifier(k: TokenKind) -> bool {
    matches!(
        k,
        TokenKind::Const | TokenKind::Restrict | TokenKind::Volatile | TokenKind::Atomic
    )
}

fn is_typeof_spec(k: TokenKind) -> bool {
    matches!(k, TokenKind::Typeof | TokenKind::TypeofUnqual)
}

fn is_type_spec_simple(k: TokenKind) -> bool {
    matches!(
        k,
        TokenKind::Void
            | TokenKind::Bool
            | TokenKind::Char
            | TokenKind::Short
            | TokenKind::Int
            | TokenKind::Long
            | TokenKind::Float
            | TokenKind::Double
            | TokenKind::Decimal32
            | TokenKind::Decimal64
            | TokenKind::Decimal128
            | TokenKind::Complex
            | TokenKind::Signed
            | TokenKind::Unsigned
            | TokenKind::Int128
    )
}

fn is_assignment_op(k: TokenKind) -> bool {
    matches!(
        k,
        TokenKind::Equals
            | TokenKind::XorEqual
            | TokenKind::TimesEqual
            | TokenKind::DivEqual
            | TokenKind::ModEqual
            | TokenKind::PlusEqual
            | TokenKind::MinusEqual
            | TokenKind::LshiftEqual
            | TokenKind::RshiftEqual
            | TokenKind::AndEqual
            | TokenKind::OrEqual
    )
}

fn binary_precedence(k: TokenKind) -> Option<u32> {
    Some(match k {
        TokenKind::Lor => 0,
        TokenKind::Land => 1,
        TokenKind::Or => 2,
        TokenKind::Xor => 3,
        TokenKind::And => 4,
        TokenKind::Eq | TokenKind::Ne => 5,
        TokenKind::Gt | TokenKind::Ge | TokenKind::Lt | TokenKind::Le => 6,
        TokenKind::Rshift | TokenKind::Lshift => 7,
        TokenKind::Plus | TokenKind::Minus => 8,
        TokenKind::Times | TokenKind::Divide | TokenKind::Mod => 9,
        _ => return None,
    })
}

fn decl_starts(k: TokenKind) -> bool {
    is_storage_class(k)
        || is_function_spec(k)
        || is_type_qualifier(k)
        || is_type_spec_simple(k)
        || is_typeof_spec(k)
        || matches!(
            k,
            TokenKind::TypeId
                | TokenKind::Struct
                | TokenKind::Union
                | TokenKind::Enum
                | TokenKind::Alignas
                | TokenKind::Atomic
                | TokenKind::BitInt
                | TokenKind::AttributeBegin
        )
}

fn expr_starts(k: TokenKind) -> bool {
    matches!(
        k,
        TokenKind::Id
            | TokenKind::Lparen
            | TokenKind::PlusPlus
            | TokenKind::MinusMinus
            | TokenKind::Plus
            | TokenKind::Minus
            | TokenKind::Times
            | TokenKind::And
            | TokenKind::Not
            | TokenKind::Lnot
            | TokenKind::Sizeof
            | TokenKind::Alignof
            | TokenKind::Generic
            | TokenKind::Offsetof
            | TokenKind::BuiltinVaArg
            | TokenKind::IntConstDec
            | TokenKind::IntConstOct
            | TokenKind::IntConstHex
            | TokenKind::IntConstBin
            | TokenKind::IntConstChar
            | TokenKind::FloatConst
            | TokenKind::HexFloatConst
            | TokenKind::CharConst
            | TokenKind::WcharConst
            | TokenKind::U8CharConst
            | TokenKind::U16CharConst
            | TokenKind::U32CharConst
            | TokenKind::StringLiteral
            | TokenKind::WstringLiteral
            | TokenKind::U8StringLiteral
            | TokenKind::U16StringLiteral
            | TokenKind::U32StringLiteral
            | TokenKind::BoolConst
            | TokenKind::Nullptr
    )
}

fn stmt_starts(k: TokenKind) -> bool {
    matches!(
        k,
        TokenKind::Lbrace
            | TokenKind::If
            | TokenKind::Switch
            | TokenKind::While
            | TokenKind::Do
            | TokenKind::For
            | TokenKind::Goto
            | TokenKind::Break
            | TokenKind::Continue
            | TokenKind::Return
            | TokenKind::Case
            | TokenKind::Default
            | TokenKind::PpPragma
            | TokenKind::PragmaTok
            | TokenKind::Semi
    )
}

/// Specifier value (mirrors the `str | c_ast.Node` `newspec` parameter).
#[derive(Debug, Clone)]
pub enum SpecValue {
    Str(String),
    Node(Node),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclSpecKind {
    Qual,
    Storage,
    Type,
    Function,
    Alignment,
}

/// Immutable walk to the underlying `TypeDecl` (mirrors the `while not
/// isinstance(typ, TypeDecl)` read in `_fix_decl_name_type`).
fn typedecl_tail(n: &Node) -> &Node {
    if matches!(n, Node::TypeDecl { .. }) {
        return n;
    }
    match decl_inner(n) {
        Some(inner) => typedecl_tail(inner),
        None => unreachable!("declarator chain without underlying TypeDecl"),
    }
}

fn typedecl_tail_mut(n: &mut Node) -> &mut Node {
    if matches!(n, Node::TypeDecl { .. }) {
        return n;
    }
    match decl_inner_mut(n) {
        Some(inner) => typedecl_tail_mut(inner),
        None => unreachable!("declarator chain without underlying TypeDecl"),
    }
}

/// Whether the underlying `TypeDecl` tail is unnamed.
fn tail_declname_unset(n: &Node) -> bool {
    match decl_inner(n) {
        Some(inner) if !matches!(inner, Node::TypeDecl { .. }) => tail_declname_unset(inner),
        Some(Node::TypeDecl { declname, .. }) => declname.is_none(),
        _ => false,
    }
}

/// Fill the underlying `TypeDecl` tail's name.
fn set_tail_declname(n: &mut Node, name: String) {
    match decl_inner_mut(n) {
        Some(inner) if !matches!(inner, Node::TypeDecl { .. }) => set_tail_declname(inner, name),
        Some(Node::TypeDecl { declname, .. }) => *declname = Some(name),
        _ => {}
    }
}

impl Parser {
    // ------------------------------------------------------------------
    // Attributes and specifiers.
    // ------------------------------------------------------------------
    fn parse_attribute_specifier_sequence(&mut self) -> PResult<Vec<Node>> {
        let mut groups = Vec::new();
        while self.peek_kind(1)? == Some(TokenKind::AttributeBegin) {
            let opening = self.advance()?;
            if !self.dialect_c23 {
                let coord = self.tok_coord_idx(opening);
                return Err(self.error("Attributes require C23".to_string(), CoordArg::Full(coord)));
            }
            let mut attributes = Vec::new();
            while self.peek_kind(1)? != Some(TokenKind::AttributeEnd) {
                if self.accept(TokenKind::Comma)?.is_some() {
                    continue;
                }
                let name = self.parse_identifier_or_typeid()?;
                let (mut aname, mut acoord) = match &name {
                    Node::ID { name, coord } => (name.clone(), coord.clone()),
                    _ => unreachable!("identifier_or_typeid"),
                };
                let mut namespace = None;
                if self.accept(TokenKind::ColonColon)?.is_some() {
                    namespace = Some(aname);
                    let name2 = self.parse_identifier_or_typeid()?;
                    let Node::ID {
                        name: n2,
                        coord: c2,
                    } = name2
                    else {
                        unreachable!("identifier_or_typeid")
                    };
                    aname = n2;
                    // The attribute takes the second name's coordinate
                    // (mirrors `name.coord` after the `::` rebinding).
                    acoord = c2;
                }
                let arguments = match self.accept(TokenKind::AttributeArguments)? {
                    Some(t) => Some(self.tokens[t].value.clone()),
                    None => None,
                };
                attributes.push(Node::Attribute {
                    name: aname,
                    namespace,
                    arguments,
                    coord: acoord,
                });
                if self.accept(TokenKind::Comma)?.is_none() {
                    break;
                }
            }
            self.expect(TokenKind::AttributeEnd)?;
            let coord = self.tok_coord_idx(opening);
            groups.push(Node::AttributeSpecifier {
                attributes,
                coord: Some(coord),
            });
        }
        Ok(groups)
    }

    fn attribute_declarations(
        &mut self,
        mut nodes: Vec<Node>,
        attributes: Vec<Node>,
    ) -> PResult<Vec<Node>> {
        let first_is_pragma = matches!(nodes.first(), Some(Node::Pragma { .. }));
        if nodes.is_empty() || first_is_pragma {
            let coord = attributes.first().and_then(|a| a.coord().cloned());
            return Err(self.error(
                "Expected declaration after attributes".to_string(),
                CoordArg::opt(coord),
            ));
        }
        for node in &mut nodes {
            if matches!(node, Node::Pragma { .. }) {
                continue;
            }
            let target = match node {
                Node::FuncDef { decl, .. } => decl.as_mut(),
                other => other,
            };
            if !matches!(target, Node::Decl { .. } | Node::Typedef { .. }) {
                let coord = attributes.first().and_then(|a| a.coord().cloned());
                return Err(self.error(
                    "Expected declaration after attributes".to_string(),
                    CoordArg::opt(coord),
                ));
            }
            set_node_attributes(target, attributes.clone());
        }
        Ok(nodes)
    }

    fn attribute_statement(&mut self, mut stmt: Node, attributes: Vec<Node>) -> PResult<Node> {
        if matches!(
            stmt,
            Node::Label { .. } | Node::Case { .. } | Node::Default { .. }
        ) {
            set_node_attributes(&mut stmt, attributes);
            return Ok(stmt);
        }
        let coord = attributes.first().and_then(|a| a.coord().cloned());
        Ok(Node::AttributedStatement {
            attributes,
            stmt: Box::new(stmt),
            coord,
        })
    }

    fn parse_declaration_specifiers(
        &mut self,
        allow_no_type: bool,
    ) -> PResult<(DeclSpec, bool, Option<Coord>)> {
        let (spec, saw_type, first_coord) = self.parse_specifiers(true)?;
        let Some(spec) = spec else {
            return Err(self.error(
                "Invalid declaration".to_string(),
                CoordArg::file(self.filename()),
            ));
        };
        if !saw_type && !allow_no_type {
            return Err(self.error(
                "Missing type in declaration".to_string(),
                CoordArg::opt(first_coord),
            ));
        }
        Ok((spec, saw_type, first_coord))
    }

    fn parse_specifier_qualifier_list(&mut self) -> PResult<DeclSpec> {
        let (spec, saw_type, first_coord) = self.parse_specifiers(false)?;
        let Some(spec) = spec else {
            return Err(self.error(
                "Invalid specifier list".to_string(),
                CoordArg::file(self.filename()),
            ));
        };
        if !saw_type && spec.alignment.is_empty() {
            return Err(self.error(
                "Missing type in declaration".to_string(),
                CoordArg::opt(first_coord),
            ));
        }
        Ok(spec)
    }

    fn parse_specifiers(
        &mut self,
        allow_storage_and_function: bool,
    ) -> PResult<(Option<DeclSpec>, bool, Option<Coord>)> {
        let mut spec: Option<DeclSpec> = None;
        let mut saw_type = false;
        let mut first_coord: Option<Coord> = None;
        loop {
            let idx = self.peek_idx(1)?;
            let Some(i) = idx else {
                break;
            };
            let kind = self.tokens[i].kind;
            if kind == TokenKind::AttributeBegin {
                if let Some(s) = spec.as_mut() {
                    s.type_attributes = self.parse_attribute_specifier_sequence()?;
                    break;
                }
            }
            // Once a type has been seen, a typedef name can be the declarator.
            if kind == TokenKind::TypeId && saw_type {
                break;
            }
            let is_atomic_specifier =
                kind == TokenKind::Atomic && self.peek_kind(2)? == Some(TokenKind::Lparen);
            if first_coord.is_none() {
                first_coord = Some(self.tok_coord_idx(i));
            }
            if kind == TokenKind::Alignas {
                let node = self.parse_alignment_specifier()?;
                let s = spec.get_or_insert_with(DeclSpec::default);
                self.add_declaration_specifier(s, SpecValue::Node(node), DeclSpecKind::Alignment);
            } else if is_atomic_specifier {
                let node = self.parse_atomic_specifier()?;
                let s = spec.get_or_insert_with(DeclSpec::default);
                self.add_declaration_specifier(s, SpecValue::Node(node), DeclSpecKind::Type);
                saw_type = true;
            } else if is_typeof_spec(kind) {
                let node = self.parse_typeof_specifier()?;
                let s = spec.get_or_insert_with(DeclSpec::default);
                self.add_declaration_specifier(s, SpecValue::Node(node), DeclSpecKind::Type);
                saw_type = true;
            } else if kind == TokenKind::BitInt {
                let node = self.parse_bitint_specifier()?;
                let s = spec.get_or_insert_with(DeclSpec::default);
                self.add_declaration_specifier(s, SpecValue::Node(node), DeclSpecKind::Type);
                saw_type = true;
            } else if is_type_qualifier(kind) {
                let t = self.advance()?;
                let value = self.tokens[t].value.clone();
                let s = spec.get_or_insert_with(DeclSpec::default);
                self.add_declaration_specifier(s, SpecValue::Str(value), DeclSpecKind::Qual);
            } else if allow_storage_and_function && is_storage_class(kind) {
                let t = self.advance()?;
                let value = self.tokens[t].value.clone();
                let s = spec.get_or_insert_with(DeclSpec::default);
                self.add_declaration_specifier(s, SpecValue::Str(value), DeclSpecKind::Storage);
            } else if allow_storage_and_function && is_function_spec(kind) {
                let t = self.advance()?;
                let value = self.tokens[t].value.clone();
                let s = spec.get_or_insert_with(DeclSpec::default);
                self.add_declaration_specifier(s, SpecValue::Str(value), DeclSpecKind::Function);
            } else if is_type_spec_simple(kind) || kind == TokenKind::TypeId {
                let t = self.advance()?;
                let coord = self.tok_coord_idx(t);
                let value = self.tokens[t].value.clone();
                let node = Node::IdentifierType {
                    names: vec![value],
                    coord: Some(coord),
                };
                let s = spec.get_or_insert_with(DeclSpec::default);
                self.add_declaration_specifier(s, SpecValue::Node(node), DeclSpecKind::Type);
                saw_type = true;
            } else if matches!(kind, TokenKind::Struct | TokenKind::Union) {
                let node = self.parse_struct_or_union_specifier()?;
                let s = spec.get_or_insert_with(DeclSpec::default);
                self.add_declaration_specifier(s, SpecValue::Node(node), DeclSpecKind::Type);
                saw_type = true;
            } else if kind == TokenKind::Enum {
                let node = self.parse_enum_specifier()?;
                let s = spec.get_or_insert_with(DeclSpec::default);
                self.add_declaration_specifier(s, SpecValue::Node(node), DeclSpecKind::Type);
                saw_type = true;
            } else {
                break;
            }
        }
        Ok((spec, saw_type, first_coord))
    }

    fn parse_type_qualifier_list(&mut self) -> PResult<Vec<String>> {
        let mut quals = Vec::new();
        while matches!(self.peek_kind(1)?, Some(k) if is_type_qualifier(k)) {
            let t = self.advance()?;
            quals.push(self.tokens[t].value.clone());
        }
        Ok(quals)
    }

    fn parse_alignment_specifier(&mut self) -> PResult<Node> {
        let tok = self.expect(TokenKind::Alignas)?;
        self.expect(TokenKind::Lparen)?;
        let coord = self.tok_coord_idx(tok);
        if self.starts_declaration_idx(None)? {
            let typ = self.parse_type_name()?;
            self.expect(TokenKind::Rparen)?;
            return Ok(Node::Alignas {
                alignment: Box::new(typ),
                coord: Some(coord),
            });
        }
        let expr = self.parse_constant_expression()?;
        self.expect(TokenKind::Rparen)?;
        Ok(Node::Alignas {
            alignment: Box::new(expr),
            coord: Some(coord),
        })
    }

    fn parse_atomic_specifier(&mut self) -> PResult<Node> {
        self.expect(TokenKind::Atomic)?;
        self.expect(TokenKind::Lparen)?;
        let mut typ = self.parse_type_name()?;
        self.expect(TokenKind::Rparen)?;
        match &mut typ {
            Node::Typename { quals, .. } => quals.push("_Atomic".to_string()),
            _ => unreachable!("atomic specifier without Typename"),
        }
        Ok(typ)
    }

    fn parse_typeof_specifier(&mut self) -> PResult<Node> {
        let tok = self.advance()?;
        let op = self.tokens[tok].value.clone();
        let coord = self.tok_coord_idx(tok);
        self.expect(TokenKind::Lparen)?;
        let expr = if self.starts_declaration_idx(None)? {
            self.parse_type_name()?
        } else {
            self.parse_expression()?
        };
        self.expect(TokenKind::Rparen)?;
        Ok(Node::TypeOf {
            op,
            expr: Box::new(expr),
            coord: Some(coord),
        })
    }

    fn parse_bitint_specifier(&mut self) -> PResult<Node> {
        let tok = self.expect(TokenKind::BitInt)?;
        let coord = self.tok_coord_idx(tok);
        self.expect(TokenKind::Lparen)?;
        let width = self.parse_constant_expression()?;
        self.expect(TokenKind::Rparen)?;
        Ok(Node::BitIntType {
            width: Box::new(width),
            signedness: None,
            coord: Some(coord),
        })
    }

    fn parse_init_declarator_list(
        &mut self,
        first: Option<DeclInfo>,
        id_only: bool,
    ) -> PResult<Vec<DeclInfo>> {
        let mut decls = vec![match first {
            Some(f) => f,
            None => self.parse_init_declarator(id_only)?,
        }];
        while self.accept(TokenKind::Comma)?.is_some() {
            decls.push(self.parse_init_declarator(id_only)?);
        }
        Ok(decls)
    }

    fn parse_init_declarator(&mut self, id_only: bool) -> PResult<DeclInfo> {
        let decl = if id_only {
            self.parse_id_declarator()?
        } else {
            self.parse_declarator()?
        };
        if self.accept(TokenKind::Equals)?.is_some() {
            return Ok(DeclInfo {
                decl: Some(decl),
                init: Some(self.parse_initializer()?),
                bitsize: None,
            });
        }
        Ok(DeclInfo {
            decl: Some(decl),
            init: None,
            bitsize: None,
        })
    }

    // ------------------------------------------------------------------
    // Top-level.
    // ------------------------------------------------------------------
    fn parse_translation_unit_or_empty(&mut self) -> PResult<Node> {
        if self.peek_idx(1)?.is_none() {
            return Ok(Node::FileAST { ext: vec![] });
        }
        Ok(Node::FileAST {
            ext: self.parse_translation_unit()?,
        })
    }

    fn parse_translation_unit(&mut self) -> PResult<Vec<Node>> {
        let mut ext = Vec::new();
        while self.peek_idx(1)?.is_some() {
            ext.extend(self.parse_external_declaration()?);
        }
        Ok(ext)
    }

    fn parse_external_declaration(&mut self) -> PResult<Vec<Node>> {
        if self.peek_kind(1)? == Some(TokenKind::AttributeBegin) {
            let attributes = self.parse_attribute_specifier_sequence()?;
            if self.accept(TokenKind::Semi)?.is_some() {
                let coord = attributes.first().and_then(|a| a.coord().cloned());
                return Ok(vec![Node::AttributeDeclaration { attributes, coord }]);
            }
            let inner = self.parse_external_declaration()?;
            return self.attribute_declarations(inner, attributes);
        }
        let idx = self.peek_idx(1)?;
        let Some(t) = idx else {
            return Ok(vec![]);
        };
        let kind = self.tokens[t].kind;
        if kind == TokenKind::PpHash {
            self.parse_pp_directive()?;
            return Ok(vec![]);
        }
        if matches!(kind, TokenKind::PpPragma | TokenKind::PragmaTok) {
            return Ok(vec![self.parse_pppragma_directive()?]);
        }
        if self.accept(TokenKind::Semi)?.is_some() {
            return Ok(vec![]);
        }
        if kind == TokenKind::StaticAssert {
            return Ok(vec![self.parse_static_assert()?]);
        }
        if !self.starts_declaration_idx(Some(t))? {
            // Old-style function definitions with an implicit `int` return.
            let decl = self.parse_id_declarator()?;
            if self.peek_kind(1)? != Some(TokenKind::Lbrace) {
                let coord = decl.coord().cloned();
                return Err(self.error(
                    "Invalid function definition".to_string(),
                    CoordArg::opt(coord),
                ));
            }
            let decl_coord = decl.coord().cloned();
            let mut spec = DeclSpec {
                qual: vec![],
                alignment: vec![],
                storage: vec![],
                typ: vec![Node::IdentifierType {
                    names: vec!["int".to_string()],
                    coord: decl_coord,
                }],
                function: vec![],
                type_attributes: vec![],
            };
            return Ok(vec![self.parse_function_definition(&mut spec, decl)?]);
        }
        let (mut spec, saw_type, spec_coord) = self.parse_declaration_specifiers(true)?;
        if self.is_inferred_auto(&spec) {
            let decls = self.parse_auto_declaration(&mut spec)?;
            return self.parse_declaration_end(decls);
        }
        let (name_type, _) = self.peek_declarator_name_info()?;
        if name_type != Some(TokenKind::Id) {
            let decls = self.parse_decl_body_with_spec(&mut spec, saw_type)?;
            return self.parse_declaration_end(decls);
        }
        let decl = self.parse_id_declarator()?;
        if self.peek_kind(1)? == Some(TokenKind::Lbrace) || self.starts_declaration_idx(None)? {
            if spec.typ.is_empty() {
                spec.typ = vec![Node::IdentifierType {
                    names: vec!["int".to_string()],
                    coord: spec_coord,
                }];
            }
            return Ok(vec![self.parse_function_definition(&mut spec, decl)?]);
        }
        let mut decl_dict = DeclInfo {
            decl: Some(decl),
            init: None,
            bitsize: None,
        };
        if self.accept(TokenKind::Equals)?.is_some() {
            decl_dict.init = Some(self.parse_initializer()?);
        }
        let mut list = vec![decl_dict];
        while self.accept(TokenKind::Comma)?.is_some() {
            list.push(self.parse_init_declarator(false)?);
        }
        let decls = self.build_declarations(&mut spec, list, true)?;
        self.parse_declaration_end(decls)
    }

    fn parse_function_definition(&mut self, spec: &mut DeclSpec, decl: Node) -> PResult<Node> {
        // Open the scope shared by parameters, old-style parameter
        // declarations, and the outermost body.
        self.push_scope();
        if let Node::FuncDecl {
            args: Some(args), ..
        } = &decl
        {
            let Node::ParamList { params, .. } = args.as_ref() else {
                unreachable!("funcdecl args without ParamList")
            };
            for param in params {
                let name = decl_name(param);
                let coord = param.coord().cloned();
                if name.is_some() {
                    self.add_identifier(name.as_deref(), CoordArg::opt(coord))?;
                }
            }
        }
        let param_decls = if self.starts_declaration_idx(None)? {
            Some(self.parse_declaration_list()?)
        } else {
            None
        };
        if self.peek_kind(1)? != Some(TokenKind::Lbrace) {
            let coord = decl.coord().cloned();
            return Err(self.error(
                "Invalid function definition".to_string(),
                CoordArg::opt(coord),
            ));
        }
        let body = self.parse_compound_statement(false)?;
        self.pop_scope();
        self.build_function_definition(spec, decl, param_decls, body)
    }

    fn parse_declaration(&mut self) -> PResult<Vec<Node>> {
        if self.peek_kind(1)? == Some(TokenKind::AttributeBegin) {
            let attributes = self.parse_attribute_specifier_sequence()?;
            if self.accept(TokenKind::Semi)?.is_some() {
                let coord = attributes.first().and_then(|a| a.coord().cloned());
                return Ok(vec![Node::AttributeDeclaration { attributes, coord }]);
            }
            let inner = self.parse_declaration()?;
            return self.attribute_declarations(inner, attributes);
        }
        if self.peek_kind(1)? == Some(TokenKind::StaticAssert) {
            return Ok(vec![self.parse_static_assert()?]);
        }
        let decls = self.parse_decl_body()?;
        self.parse_declaration_end(decls)
    }

    fn parse_declaration_end(&mut self, mut decls: Vec<Node>) -> PResult<Vec<Node>> {
        let mut pragmas = self.parse_pppragma_directive_list()?;
        self.expect(TokenKind::Semi)?;
        decls.append(&mut pragmas);
        Ok(decls)
    }

    fn parse_decl_body(&mut self) -> PResult<Vec<Node>> {
        let (mut spec, saw_type, _) = self.parse_declaration_specifiers(true)?;
        self.parse_decl_body_with_spec(&mut spec, saw_type)
    }

    fn parse_decl_body_with_spec(
        &mut self,
        spec: &mut DeclSpec,
        saw_type: bool,
    ) -> PResult<Vec<Node>> {
        if self.is_inferred_auto(spec) {
            return self.parse_auto_declaration(spec);
        }
        let mut decl_infos: Option<Vec<DeclInfo>> = None;
        if saw_type {
            if self.starts_declarator(false)? {
                decl_infos = Some(self.parse_init_declarator_list(None, false)?);
            }
        } else if self.starts_declarator(true)? {
            decl_infos = Some(self.parse_init_declarator_list(None, true)?);
        }
        if let Some(infos) = decl_infos {
            return self.build_declarations(spec, infos, true);
        }
        // A bare struct/union/enum definition carries its own node; any
        // other lone specifier must go through the empty-declarator path
        // below (which reports `Invalid declaration` for lone type names
        // like `typeof(int);`). No `parse_declaration_end` here: the caller
        // consumes the semicolon, mirroring upstream.
        if spec.typ.len() == 1
            && matches!(
                spec.typ[0],
                Node::Struct { .. } | Node::Union { .. } | Node::Enum { .. }
            )
        {
            let decl_type = spec.typ[0].clone();
            return self.build_declarations(
                spec,
                vec![DeclInfo {
                    decl: Some(decl_type),
                    init: None,
                    bitsize: None,
                }],
                true,
            );
        }
        self.build_declarations(
            spec,
            vec![DeclInfo {
                decl: None,
                init: None,
                bitsize: None,
            }],
            true,
        )
    }

    fn is_inferred_auto(&self, spec: &DeclSpec) -> bool {
        self.dialect_c23 && spec.storage.iter().any(|s| s == "auto") && spec.typ.is_empty()
    }

    fn parse_auto_declaration(&mut self, spec: &mut DeclSpec) -> PResult<Vec<Node>> {
        let decl = self.parse_id_declarator()?;
        if !matches!(decl, Node::TypeDecl { .. }) {
            let coord = decl.coord().cloned();
            return Err(self.error(
                "Inferred auto declaration requires a plain identifier".to_string(),
                CoordArg::opt(coord),
            ));
        }
        self.expect(TokenKind::Equals)?;
        let init = self.parse_assignment_expression()?;
        spec.typ = vec![Node::InferredType {
            coord: decl.coord().cloned(),
        }];
        self.build_declarations(
            spec,
            vec![DeclInfo {
                decl: Some(decl),
                init: Some(init),
                bitsize: None,
            }],
            true,
        )
    }

    fn parse_declaration_list(&mut self) -> PResult<Vec<Node>> {
        let mut decls = Vec::new();
        while self.starts_declaration_idx(None)? {
            decls.extend(self.parse_declaration()?);
            decls.extend(self.parse_pppragma_directive_list()?);
        }
        Ok(decls)
    }

    // ------------------------------------------------------------------
    // Preprocessor-like directives.
    // ------------------------------------------------------------------
    fn parse_pp_directive(&mut self) -> PResult<()> {
        let tok = self.expect(TokenKind::PpHash)?;
        let coord = self.tok_coord_idx(tok);
        Err(self.error(
            "Directives not supported yet".to_string(),
            CoordArg::Full(coord),
        ))
    }

    fn parse_pppragma_directive(&mut self) -> PResult<Node> {
        if self.peek_kind(1)? == Some(TokenKind::PpPragma) {
            let tok = self.advance()?;
            let tok_coord = self.tok_coord_idx(tok);
            if self.peek_kind(1)? == Some(TokenKind::PpPragmaStr) {
                let st = self.advance()?;
                let s = self.tokens[st].value.clone();
                let c = self.tok_coord_idx(st);
                return Ok(Node::Pragma {
                    string: PragmaString::Text(s),
                    coord: Some(c),
                });
            }
            return Ok(Node::Pragma {
                string: PragmaString::Text(String::new()),
                coord: Some(tok_coord),
            });
        }
        if self.peek_kind(1)? == Some(TokenKind::PragmaTok) {
            self.advance()?;
            let lparen = self.expect(TokenKind::Lparen)?;
            let lparen_coord = self.tok_coord_idx(lparen);
            let literal = self.parse_unified_string_literal()?;
            self.expect(TokenKind::Rparen)?;
            return Ok(Node::Pragma {
                string: PragmaString::Literal(Box::new(literal)),
                coord: Some(lparen_coord),
            });
        }
        Err(self.error(
            "Invalid pragma".to_string(),
            CoordArg::file(self.filename()),
        ))
    }

    fn parse_pppragma_directive_list(&mut self) -> PResult<Vec<Node>> {
        let mut pragmas = Vec::new();
        while matches!(
            self.peek_kind(1)?,
            Some(TokenKind::PpPragma) | Some(TokenKind::PragmaTok)
        ) {
            pragmas.push(self.parse_pppragma_directive()?);
        }
        Ok(pragmas)
    }

    fn parse_static_assert(&mut self) -> PResult<Node> {
        let tok = self.expect(TokenKind::StaticAssert)?;
        let coord = self.tok_coord_idx(tok);
        self.expect(TokenKind::Lparen)?;
        let cond = self.parse_constant_expression()?;
        let mut msg = None;
        if self.accept(TokenKind::Comma)?.is_some() {
            msg = Some(self.parse_unified_string_literal()?);
        }
        self.expect(TokenKind::Rparen)?;
        self.expect(TokenKind::Semi)?;
        Ok(Node::StaticAssert {
            cond: Box::new(cond),
            message: msg.map(Box::new),
            coord: Some(coord),
        })
    }
}

/// Set the `attributes` slot of a declaration-like node.
fn set_node_attributes(n: &mut Node, attrs: Vec<Node>) {
    let slot = match n {
        Node::Decl { attributes, .. }
        | Node::Typedef { attributes, .. }
        | Node::Typename { attributes, .. }
        | Node::TypeDecl { attributes, .. }
        | Node::ArrayDecl { attributes, .. }
        | Node::FuncDecl { attributes, .. }
        | Node::PtrDecl { attributes, .. }
        | Node::Enum { attributes, .. }
        | Node::Enumerator { attributes, .. }
        | Node::Struct { attributes, .. }
        | Node::Union { attributes, .. }
        | Node::Label { attributes, .. }
        | Node::Case { attributes, .. }
        | Node::Default { attributes, .. } => attributes,
        _ => unreachable!("attribute target without attributes slot"),
    };
    *slot = Some(attrs);
}

impl Parser {
    // ------------------------------------------------------------------
    // Structs/unions/enums.
    // ------------------------------------------------------------------
    fn parse_struct_or_union_specifier(&mut self) -> PResult<Node> {
        let tok = self.advance()?;
        let is_struct = self.tokens[tok].value == "struct";
        let attributes = self.parse_attribute_specifier_sequence()?;
        let build = |name: Option<String>,
                     decls: Option<Vec<Node>>,
                     coord: Coord,
                     attributes: Vec<Node>|
         -> Node {
            if is_struct {
                Node::Struct {
                    name,
                    decls,
                    attributes: Some(attributes),
                    coord: Some(coord),
                }
            } else {
                Node::Union {
                    name,
                    decls,
                    attributes: Some(attributes),
                    coord: Some(coord),
                }
            }
        };
        if matches!(
            self.peek_kind(1)?,
            Some(TokenKind::Id) | Some(TokenKind::TypeId)
        ) {
            let name_tok = self.advance()?;
            let name = self.tokens[name_tok].value.clone();
            let coord = self.tok_coord_idx(name_tok);
            if self.peek_kind(1)? == Some(TokenKind::Lbrace) {
                self.advance()?;
                if self.accept(TokenKind::Rbrace)?.is_some() {
                    return Ok(build(Some(name), Some(vec![]), coord, attributes));
                }
                let decls = self.parse_struct_declaration_list()?;
                self.expect(TokenKind::Rbrace)?;
                return Ok(build(Some(name), Some(decls), coord, attributes));
            }
            return Ok(build(Some(name), None, coord, attributes));
        }
        if self.peek_kind(1)? == Some(TokenKind::Lbrace) {
            let brace_tok = self.advance()?;
            let coord = self.tok_coord_idx(brace_tok);
            if self.accept(TokenKind::Rbrace)?.is_some() {
                return Ok(build(None, Some(vec![]), coord, attributes));
            }
            let decls = self.parse_struct_declaration_list()?;
            self.expect(TokenKind::Rbrace)?;
            return Ok(build(None, Some(decls), coord, attributes));
        }
        let coord = self.tok_coord_idx(tok);
        Err(self.error(
            "Invalid struct/union declaration".to_string(),
            CoordArg::Full(coord),
        ))
    }

    fn parse_struct_declaration_list(&mut self) -> PResult<Vec<Node>> {
        let mut decls = Vec::new();
        loop {
            match self.peek_kind(1)? {
                None | Some(TokenKind::Rbrace) => break,
                _ => {}
            }
            if let Some(items) = self.parse_struct_declaration()? {
                decls.extend(items);
            }
        }
        Ok(decls)
    }

    fn parse_struct_declaration(&mut self) -> PResult<Option<Vec<Node>>> {
        if self.peek_kind(1)? == Some(TokenKind::AttributeBegin) {
            let attributes = self.parse_attribute_specifier_sequence()?;
            let inner = self.parse_struct_declaration()?.unwrap_or_default();
            return Ok(Some(self.attribute_declarations(inner, attributes)?));
        }
        if self.peek_kind(1)? == Some(TokenKind::StaticAssert) {
            return Ok(Some(vec![self.parse_static_assert()?]));
        }
        if self.peek_kind(1)? == Some(TokenKind::Semi) {
            self.advance()?;
            return Ok(None);
        }
        if matches!(
            self.peek_kind(1)?,
            Some(TokenKind::PpPragma) | Some(TokenKind::PragmaTok)
        ) {
            return Ok(Some(vec![self.parse_pppragma_directive()?]));
        }
        let mut spec = self.parse_specifier_qualifier_list()?;
        debug_assert!(!spec.storage.iter().any(|s| s == "typedef"));
        let mut decls = None;
        if self.starts_declarator(false)? || self.peek_kind(1)? == Some(TokenKind::Colon) {
            decls = Some(self.parse_struct_declarator_list()?);
        }
        if let Some(infos) = decls {
            let built = self.build_declarations(&mut spec, infos, false)?;
            return Ok(Some(self.parse_declaration_end(built)?));
        }
        if spec.typ.len() == 1 {
            let node = spec.typ[0].clone();
            let built = self.build_declarations(
                &mut spec,
                vec![DeclInfo {
                    decl: Some(node),
                    init: None,
                    bitsize: None,
                }],
                false,
            )?;
            return Ok(Some(self.parse_declaration_end(built)?));
        }
        let built = self.build_declarations(
            &mut spec,
            vec![DeclInfo {
                decl: None,
                init: None,
                bitsize: None,
            }],
            false,
        )?;
        Ok(Some(self.parse_declaration_end(built)?))
    }

    fn parse_struct_declarator_list(&mut self) -> PResult<Vec<DeclInfo>> {
        let mut decls = vec![self.parse_struct_declarator()?];
        while self.accept(TokenKind::Comma)?.is_some() {
            decls.push(self.parse_struct_declarator()?);
        }
        Ok(decls)
    }

    fn parse_struct_declarator(&mut self) -> PResult<DeclInfo> {
        if self.accept(TokenKind::Colon)?.is_some() {
            let bitsize = self.parse_constant_expression()?;
            return Ok(DeclInfo {
                decl: Some(Node::TypeDecl {
                    declname: None,
                    quals: None,
                    align: None,
                    typ: None,
                    attributes: None,
                    coord: None,
                }),
                init: None,
                bitsize: Some(bitsize),
            });
        }
        let decl = self.parse_declarator()?;
        if self.accept(TokenKind::Colon)?.is_some() {
            let bitsize = self.parse_constant_expression()?;
            return Ok(DeclInfo {
                decl: Some(decl),
                init: None,
                bitsize: Some(bitsize),
            });
        }
        Ok(DeclInfo {
            decl: Some(decl),
            init: None,
            bitsize: None,
        })
    }

    fn parse_enum_specifier(&mut self) -> PResult<Node> {
        let tok = self.expect(TokenKind::Enum)?;
        let coord = self.tok_coord_idx(tok);
        let attributes = self.parse_attribute_specifier_sequence()?;
        let mut name = None;
        if matches!(
            self.peek_kind(1)?,
            Some(TokenKind::Id) | Some(TokenKind::TypeId)
        ) {
            let t = self.advance()?;
            name = Some(self.tokens[t].value.clone());
        }
        let mut underlying_type = None;
        // A colon followed by an expression belongs to an unnamed bit-field,
        // e.g. `struct S { enum E : 3; }`, rather than a fixed underlying type.
        let peek2 = self.peek_idx(2)?;
        if self.peek_kind(1)? == Some(TokenKind::Colon) && self.starts_declaration_idx(peek2)? {
            underlying_type = Some(Box::new(self.parse_enum_type_specifier()?));
        }
        let mut enums = None;
        if name.is_none() || self.peek_kind(1)? == Some(TokenKind::Lbrace) {
            self.expect(TokenKind::Lbrace)?;
            enums = Some(Box::new(self.parse_enumerator_list()?));
            self.expect(TokenKind::Rbrace)?;
        }
        Ok(Node::Enum {
            name,
            values: enums,
            underlying_type,
            coord: Some(coord),
            attributes: Some(attributes),
        })
    }

    fn parse_enum_type_specifier(&mut self) -> PResult<Node> {
        let colon = self.expect(TokenKind::Colon)?;
        if !self.dialect_c23 {
            let coord = self.tok_coord_idx(colon);
            return Err(self.error(
                "Fixed underlying enum types require C23".to_string(),
                CoordArg::Full(coord),
            ));
        }
        let spec = self.parse_specifier_qualifier_list()?;
        let coord = spec
            .typ
            .first()
            .and_then(|t| t.coord().cloned())
            .unwrap_or_else(|| self.tok_coord_idx(colon));
        self.build_type_name(&spec, None, Some(coord))
    }

    fn parse_enumerator_list(&mut self) -> PResult<Node> {
        let mut members = self.parse_pppragma_directive_list()?;
        let first = self.parse_enumerator()?;
        let coord = first.coord().cloned();
        members.push(first);
        loop {
            members.extend(self.parse_pppragma_directive_list()?);
            if self.accept(TokenKind::Comma)?.is_none() {
                break;
            }
            members.extend(self.parse_pppragma_directive_list()?);
            if self.peek_kind(1)? == Some(TokenKind::Rbrace) {
                break;
            }
            members.push(self.parse_enumerator()?);
        }
        Ok(Node::EnumeratorList {
            enumerators: members,
            coord,
        })
    }

    fn parse_enumerator(&mut self) -> PResult<Node> {
        let name_tok = self.expect(TokenKind::Id)?;
        let name = self.tokens[name_tok].value.clone();
        let coord = self.tok_coord_idx(name_tok);
        let attributes = self.parse_attribute_specifier_sequence()?;
        let mut value = None;
        if self.accept(TokenKind::Equals)?.is_some() {
            value = Some(Box::new(self.parse_constant_expression()?));
        }
        let node = Node::Enumerator {
            name: name.clone(),
            value,
            coord: Some(coord.clone()),
            attributes: Some(attributes),
        };
        self.add_identifier(Some(&name), CoordArg::Full(coord))?;
        Ok(node)
    }

    // ------------------------------------------------------------------
    // Declarators.
    // ------------------------------------------------------------------
    fn parse_declarator(&mut self) -> PResult<Node> {
        let (decl, _) = self.parse_any_declarator(false, false)?;
        decl.ok_or_else(|| {
            self.error(
                "Invalid declarator".to_string(),
                CoordArg::file(self.filename()),
            )
        })
    }

    fn parse_id_declarator(&mut self) -> PResult<Node> {
        self.parse_declarator_kind(DeclaratorKind::Id, true)
    }

    fn parse_typeid_declarator(&mut self) -> PResult<Node> {
        self.parse_declarator_kind(DeclaratorKind::TypeId, true)
    }

    fn parse_typeid_noparen_declarator(&mut self) -> PResult<Node> {
        self.parse_declarator_kind(DeclaratorKind::TypeId, false)
    }

    fn parse_declarator_kind(&mut self, kind: DeclaratorKind, allow_paren: bool) -> PResult<Node> {
        let mut ptr = None;
        if self.peek_kind(1)? == Some(TokenKind::Times) {
            ptr = self.parse_pointer()?;
        }
        let direct = self.parse_direct_declarator(kind, allow_paren)?;
        match ptr {
            Some(p) => Ok(type_modify_decl(direct, p)),
            None => Ok(direct),
        }
    }

    fn parse_direct_declarator(
        &mut self,
        kind: DeclaratorKind,
        allow_paren: bool,
    ) -> PResult<Node> {
        let decl = if allow_paren && self.accept(TokenKind::Lparen)?.is_some() {
            let inner = self.parse_declarator_kind(kind, true)?;
            self.expect(TokenKind::Rparen)?;
            inner
        } else {
            let want = match kind {
                DeclaratorKind::Id => TokenKind::Id,
                DeclaratorKind::TypeId => TokenKind::TypeId,
            };
            let name_tok = self.expect(want)?;
            let coord = self.tok_coord_idx(name_tok);
            let name = self.tokens[name_tok].value.clone();
            let mut node = Node::TypeDecl {
                declname: Some(name),
                typ: None,
                quals: None,
                align: None,
                coord: Some(coord),
                attributes: None,
            };
            let attrs = self.parse_attribute_specifier_sequence()?;
            set_node_attributes(&mut node, attrs);
            node
        };
        self.parse_decl_suffixes(decl)
    }

    fn parse_decl_suffixes(&mut self, mut decl: Node) -> PResult<Node> {
        loop {
            if self.peek_kind(1)? == Some(TokenKind::Lbracket) {
                let arr = self.parse_array_decl(&decl)?;
                decl = type_modify_decl(decl, arr);
                continue;
            }
            if self.peek_kind(1)? == Some(TokenKind::Lparen) {
                let func = self.parse_function_decl(&decl)?;
                decl = type_modify_decl(decl, func);
                continue;
            }
            break;
        }
        Ok(decl)
    }

    fn parse_array_decl(&mut self, base_decl: &Node) -> PResult<Node> {
        let coord = base_decl.coord().cloned();
        self.parse_array_decl_common(None, coord)
    }

    fn parse_array_decl_common(
        &mut self,
        base_type: Option<Node>,
        coord: Option<Coord>,
    ) -> PResult<Node> {
        let lbrack_tok = self.expect(TokenKind::Lbracket)?;
        let coord = coord.or_else(|| Some(self.tok_coord_idx(lbrack_tok)));
        if self.accept(TokenKind::Static)?.is_some() {
            let mut dim_quals = vec!["static".to_string()];
            dim_quals.extend(self.parse_type_qualifier_list()?);
            let dim = self.parse_assignment_expression()?;
            self.expect(TokenKind::Rbracket)?;
            return self.finish_array_decl(&base_type, &coord, Some(dim), dim_quals);
        }
        if matches!(self.peek_kind(1)?, Some(k) if is_type_qualifier(k)) {
            let mut dim_quals = self.parse_type_qualifier_list()?;
            if self.accept(TokenKind::Static)?.is_some() {
                dim_quals.push("static".to_string());
                let dim = self.parse_assignment_expression()?;
                self.expect(TokenKind::Rbracket)?;
                return self.finish_array_decl(&base_type, &coord, Some(dim), dim_quals);
            }
            if let Some(times_tok) = self.accept(TokenKind::Times)? {
                self.expect(TokenKind::Rbracket)?;
                let c = self.tok_coord_idx(times_tok);
                let dim = Node::ID {
                    name: self.tokens[times_tok].value.clone(),
                    coord: Some(c),
                };
                return self.finish_array_decl(&base_type, &coord, Some(dim), dim_quals);
            }
            let mut dim = None;
            if self.starts_expression_idx(None)? {
                dim = Some(self.parse_assignment_expression()?);
            }
            self.expect(TokenKind::Rbracket)?;
            return self.finish_array_decl(&base_type, &coord, dim, dim_quals);
        }
        if let Some(times_tok) = self.accept(TokenKind::Times)? {
            self.expect(TokenKind::Rbracket)?;
            let c = self.tok_coord_idx(times_tok);
            let dim = Node::ID {
                name: self.tokens[times_tok].value.clone(),
                coord: Some(c),
            };
            return self.finish_array_decl(&base_type, &coord, Some(dim), vec![]);
        }
        let mut dim = None;
        if self.starts_expression_idx(None)? {
            dim = Some(self.parse_assignment_expression()?);
        }
        self.expect(TokenKind::Rbracket)?;
        self.finish_array_decl(&base_type, &coord, dim, vec![])
    }

    fn finish_array_decl(
        &mut self,
        base_type: &Option<Node>,
        coord: &Option<Coord>,
        dim: Option<Node>,
        dim_quals: Vec<String>,
    ) -> PResult<Node> {
        let attributes = self.parse_attribute_specifier_sequence()?;
        Ok(Node::ArrayDecl {
            // `None` base mirrors upstream `ArrayDecl(type=None)`; the
            // modifier splice fills it from the declarator being suffixed.
            typ: base_type.clone().map(Box::new),
            dim: dim.map(Box::new),
            dim_quals,
            coord: coord.clone(),
            attributes: Some(attributes),
        })
    }

    fn parse_function_decl(&mut self, base_decl: &Node) -> PResult<Node> {
        self.expect(TokenKind::Lparen)?;
        let args = if self.accept(TokenKind::Rparen)?.is_some() {
            None
        } else {
            let params = if self.starts_declaration_idx(None)?
                || self.peek_kind(1)? == Some(TokenKind::Ellipsis)
            {
                Some(self.parse_parameter_type_list()?)
            } else {
                self.parse_identifier_list_opt()?
            };
            self.expect(TokenKind::Rparen)?;
            params
        };
        Ok(Node::FuncDecl {
            args: args.map(Box::new),
            typ: None,
            coord: base_decl.coord().cloned(),
            attributes: Some(self.parse_attribute_specifier_sequence()?),
        })
    }

    fn parse_pointer(&mut self) -> PResult<Option<Node>> {
        let mut stars: Vec<(Vec<String>, Coord, Vec<Node>)> = Vec::new();
        while let Some(times_tok) = self.accept(TokenKind::Times)? {
            let attributes = self.parse_attribute_specifier_sequence()?;
            let quals = self.parse_type_qualifier_list()?;
            stars.push((quals, self.tok_coord_idx(times_tok), attributes));
        }
        if stars.is_empty() {
            return Ok(None);
        }
        let mut ptr: Option<Node> = None;
        for (quals, coord, attributes) in stars {
            ptr = Some(Node::PtrDecl {
                quals,
                typ: ptr.map(Box::new),
                coord: Some(coord),
                attributes: Some(attributes),
            });
        }
        Ok(ptr)
    }

    fn parse_parameter_type_list(&mut self) -> PResult<Node> {
        if let Some(ell_tok) = self.accept(TokenKind::Ellipsis)? {
            let coord = self.tok_coord_idx(ell_tok);
            if !self.dialect_c23 {
                return Err(self.error(
                    "Ellipsis-only parameter lists require C23".to_string(),
                    CoordArg::Full(coord.clone()),
                ));
            }
            return Ok(Node::ParamList {
                params: vec![Node::EllipsisParam {
                    coord: Some(coord.clone()),
                }],
                coord: Some(coord),
            });
        }
        let mut params = self.parse_parameter_list()?;
        if self.peek_kind(1)? == Some(TokenKind::Comma)
            && self.peek_kind(2)? == Some(TokenKind::Ellipsis)
        {
            self.advance()?;
            let ell_tok = self.advance()?;
            let coord = self.tok_coord_idx(ell_tok);
            let Node::ParamList { params: ps, .. } = &mut params else {
                unreachable!("parameter list without ParamList")
            };
            ps.push(Node::EllipsisParam { coord: Some(coord) });
        }
        Ok(params)
    }

    fn parse_parameter_list(&mut self) -> PResult<Node> {
        let first = self.parse_parameter_declaration()?;
        let coord = first.coord().cloned();
        let mut params = vec![first];
        while self.peek_kind(1)? == Some(TokenKind::Comma)
            && self.peek_kind(2)? != Some(TokenKind::Ellipsis)
        {
            self.advance()?;
            params.push(self.parse_parameter_declaration()?);
        }
        Ok(Node::ParamList { params, coord })
    }

    fn parse_parameter_declaration(&mut self) -> PResult<Node> {
        let attributes = self.parse_attribute_specifier_sequence()?;
        let mut decl = self.parse_parameter_declaration_body()?;
        if !matches!(decl, Node::Decl { .. } | Node::Typename { .. }) {
            unreachable!("parameter declaration without Decl/Typename");
        }
        set_node_attributes(&mut decl, attributes);
        Ok(decl)
    }

    fn parse_parameter_declaration_body(&mut self) -> PResult<Node> {
        let (mut spec, _, spec_coord) = self.parse_declaration_specifiers(true)?;
        if self.is_inferred_auto(&spec) {
            return Err(self.error(
                "Cannot infer the type of a parameter".to_string(),
                CoordArg::opt(spec_coord),
            ));
        }
        if spec.storage.iter().any(|s| s == "typedef") {
            return Err(self.error(
                "Invalid storage class for a parameter".to_string(),
                CoordArg::opt(spec_coord),
            ));
        }
        if spec.typ.is_empty() {
            spec.typ = vec![Node::IdentifierType {
                names: vec!["int".to_string()],
                coord: spec_coord.clone(),
            }];
        }
        if self.starts_declarator(false)? {
            let (decl, is_named) = self.parse_any_declarator(true, true)?;
            if is_named {
                let decl = decl.expect("named declarator");
                let built = self.build_declarations(
                    &mut spec,
                    vec![DeclInfo {
                        decl: Some(decl),
                        init: None,
                        bitsize: None,
                    }],
                    false,
                )?;
                return Ok(built.into_iter().next().expect("one declaration"));
            }
            return self.build_parameter_declaration(&mut spec, decl, spec_coord);
        }
        let decl = self.parse_abstract_declarator_opt()?;
        self.build_parameter_declaration(&mut spec, decl, spec_coord)
    }

    fn build_parameter_declaration(
        &mut self,
        spec: &mut DeclSpec,
        decl: Option<Node>,
        spec_coord: Option<Coord>,
    ) -> PResult<Node> {
        if spec.typ.len() > 1 {
            if let Node::IdentifierType { names, .. } = &spec.typ[spec.typ.len() - 1] {
                if names.len() == 1 && self.is_type_in_scope(&names[0]) {
                    let built = self.build_declarations(
                        spec,
                        vec![DeclInfo {
                            decl,
                            init: None,
                            bitsize: None,
                        }],
                        false,
                    )?;
                    return Ok(built.into_iter().next().expect("one declaration"));
                }
            }
        }
        self.build_type_name(spec, decl, spec_coord)
    }

    fn parse_identifier_list_opt(&mut self) -> PResult<Option<Node>> {
        if self.peek_kind(1)? == Some(TokenKind::Rparen) {
            return Ok(None);
        }
        Ok(Some(self.parse_identifier_list()?))
    }

    fn parse_identifier_list(&mut self) -> PResult<Node> {
        let first = self.parse_identifier()?;
        let coord = first.coord().cloned();
        let mut params = vec![first];
        while self.accept(TokenKind::Comma)?.is_some() {
            params.push(self.parse_identifier()?);
        }
        Ok(Node::ParamList { params, coord })
    }

    // ------------------------------------------------------------------
    // Abstract declarators.
    // ------------------------------------------------------------------
    fn parse_type_name(&mut self) -> PResult<Node> {
        let spec = self.parse_specifier_qualifier_list()?;
        let decl = self.parse_abstract_declarator_opt()?;
        let coord = match &decl {
            Some(d) => d.coord().cloned(),
            None => spec.typ.first().and_then(|t| t.coord().cloned()),
        };
        self.build_type_name(&spec, decl, coord)
    }

    fn parse_abstract_declarator_opt(&mut self) -> PResult<Option<Node>> {
        if self.peek_kind(1)? == Some(TokenKind::Times) {
            let ptr = self.parse_pointer()?;
            let decl = if self.starts_direct_abstract_declarator()? {
                self.parse_direct_abstract_declarator()?
            } else {
                Node::TypeDecl {
                    declname: None,
                    quals: None,
                    align: None,
                    typ: None,
                    attributes: None,
                    coord: None,
                }
            };
            let ptr = ptr.expect("pointer after TIMES");
            return Ok(Some(type_modify_decl(decl, ptr)));
        }
        if self.starts_direct_abstract_declarator()? {
            return Ok(Some(self.parse_direct_abstract_declarator()?));
        }
        Ok(None)
    }

    fn parse_direct_abstract_declarator(&mut self) -> PResult<Node> {
        let decl = if let Some(lparen_tok) = self.accept(TokenKind::Lparen)? {
            let _ = lparen_tok;
            if self.starts_declaration_idx(None)?
                || matches!(
                    self.peek_kind(1)?,
                    Some(TokenKind::Rparen) | Some(TokenKind::Ellipsis)
                )
            {
                let params = self.parse_parameter_type_list_opt()?;
                self.expect(TokenKind::Rparen)?;
                let coord = self.tok_coord_idx(lparen_tok);
                Node::FuncDecl {
                    args: params.map(Box::new),
                    typ: Some(Box::new(Node::TypeDecl {
                        declname: None,
                        quals: None,
                        align: None,
                        typ: None,
                        attributes: None,
                        coord: None,
                    })),
                    coord: Some(coord),
                    attributes: Some(self.parse_attribute_specifier_sequence()?),
                }
            } else {
                let inner = self.parse_abstract_declarator_opt()?;
                self.expect(TokenKind::Rparen)?;
                inner.expect("abstract declarator in parens")
            }
        } else if self.peek_kind(1)? == Some(TokenKind::Lbracket) {
            self.parse_abstract_array_base()?
        } else {
            return Err(self.error(
                "Invalid abstract declarator".to_string(),
                CoordArg::file(self.filename()),
            ));
        };
        self.parse_decl_suffixes(decl)
    }

    fn parse_parameter_type_list_opt(&mut self) -> PResult<Option<Node>> {
        if self.peek_kind(1)? == Some(TokenKind::Rparen) {
            return Ok(None);
        }
        Ok(Some(self.parse_parameter_type_list()?))
    }

    fn parse_abstract_array_base(&mut self) -> PResult<Node> {
        self.parse_array_decl_common(
            Some(Node::TypeDecl {
                declname: None,
                quals: None,
                align: None,
                typ: None,
                attributes: None,
                coord: None,
            }),
            None,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeclaratorKind {
    Id,
    TypeId,
}

impl Parser {
    // ------------------------------------------------------------------
    // Statements (appended chunk).
    // ------------------------------------------------------------------
    fn parse_statement(&mut self) -> PResult<Node> {
        if self.peek_kind(1)? == Some(TokenKind::AttributeBegin) {
            let attributes = self.parse_attribute_specifier_sequence()?;
            if self.accept(TokenKind::Semi)?.is_some() {
                let coord = attributes.first().and_then(|a| a.coord().cloned());
                return Ok(Node::AttributeDeclaration { attributes, coord });
            }
            let stmt = self.parse_statement()?;
            return self.attribute_statement(stmt, attributes);
        }
        let tok_type = self.peek_kind(1)?;
        match tok_type {
            Some(TokenKind::Case) | Some(TokenKind::Default) => self.parse_labeled_statement(),
            Some(TokenKind::Id) if self.peek_kind(2)? == Some(TokenKind::Colon) => {
                self.parse_labeled_statement()
            }
            Some(TokenKind::Lbrace) => self.parse_compound_statement(true),
            Some(TokenKind::If) | Some(TokenKind::Switch) => self.parse_selection_statement(),
            Some(TokenKind::While) | Some(TokenKind::Do) | Some(TokenKind::For) => {
                self.parse_iteration_statement()
            }
            Some(TokenKind::Goto)
            | Some(TokenKind::Break)
            | Some(TokenKind::Continue)
            | Some(TokenKind::Return) => self.parse_jump_statement(),
            Some(TokenKind::PpPragma) | Some(TokenKind::PragmaTok) => {
                self.parse_pppragma_directive()
            }
            _ => self.parse_expression_statement(),
        }
    }
}

impl Parser {
    fn parse_pragmacomp_or_statement(&mut self) -> PResult<Node> {
        if matches!(
            self.peek_kind(1)?,
            Some(TokenKind::PpPragma) | Some(TokenKind::PragmaTok)
        ) {
            let pragmas = self.parse_pppragma_directive_list()?;
            let coord = pragmas.first().and_then(|p| p.coord().cloned());
            let stmt = self.parse_statement()?;
            let mut items = pragmas;
            items.push(stmt);
            return Ok(Node::Compound {
                block_items: Some(items),
                coord,
            });
        }
        self.parse_statement()
    }

    fn parse_block_item(&mut self) -> PResult<BlockItem> {
        if self.peek_kind(1)? == Some(TokenKind::AttributeBegin) {
            let attributes = self.parse_attribute_specifier_sequence()?;
            if self.accept(TokenKind::Semi)?.is_some() {
                let coord = attributes.first().and_then(|a| a.coord().cloned());
                return Ok(BlockItem::One(Node::AttributeDeclaration {
                    attributes,
                    coord,
                }));
            }
            if self.starts_declaration_idx(None)? {
                let inner = self.parse_declaration()?;
                return Ok(BlockItem::Many(
                    self.attribute_declarations(inner, attributes)?,
                ));
            }
            let stmt = self.parse_statement()?;
            return Ok(BlockItem::One(self.attribute_statement(stmt, attributes)?));
        }
        if self.peek_kind(1)? == Some(TokenKind::StaticAssert) {
            return Ok(BlockItem::One(self.parse_static_assert()?));
        }
        if self.starts_declaration_idx(None)? {
            return Ok(BlockItem::Many(self.parse_declaration()?));
        }
        Ok(BlockItem::One(self.parse_statement()?))
    }

    fn parse_block_item_list(&mut self) -> PResult<Vec<Node>> {
        let mut items: Vec<Node> = Vec::new();
        loop {
            match self.peek_kind(1)? {
                Some(TokenKind::Rbrace) | None => break,
                _ => {}
            }
            match self.parse_block_item()? {
                BlockItem::One(node) => items.push(node),
                BlockItem::Many(mut nodes) => items.append(&mut nodes),
            }
        }
        Ok(items)
    }

    fn parse_compound_statement(&mut self, new_scope: bool) -> PResult<Node> {
        let lbrace_tok = self.expect(TokenKind::Lbrace)?;
        let coord = self.tok_coord_idx(lbrace_tok);
        if new_scope {
            self.push_scope();
        }
        let mut block_items = None;
        if self.accept(TokenKind::Rbrace)?.is_none() {
            block_items = Some(self.parse_block_item_list()?);
            self.expect(TokenKind::Rbrace)?;
        }
        if new_scope {
            self.pop_scope();
        }
        Ok(Node::Compound {
            block_items,
            coord: Some(coord),
        })
    }

    fn parse_labeled_statement(&mut self) -> PResult<Node> {
        match self.peek_kind(1)? {
            Some(TokenKind::Id) => {
                let name_tok = self.advance()?;
                let name = self.tokens[name_tok].value.clone();
                let coord = self.tok_coord_idx(name_tok);
                self.expect(TokenKind::Colon)?;
                let stmt = if self.starts_statement()? {
                    self.parse_pragmacomp_or_statement()?
                } else {
                    Node::EmptyStatement {
                        coord: Some(self.tok_coord_idx(name_tok)),
                    }
                };
                Ok(Node::Label {
                    name,
                    stmt: Box::new(stmt),
                    attributes: None,
                    coord: Some(coord),
                })
            }
            Some(TokenKind::Case) => {
                let case_tok = self.advance()?;
                let coord = self.tok_coord_idx(case_tok);
                let expr = self.parse_constant_expression()?;
                self.expect(TokenKind::Colon)?;
                let stmt = if self.starts_statement()? {
                    self.parse_pragmacomp_or_statement()?
                } else {
                    Node::EmptyStatement {
                        coord: Some(coord.clone()),
                    }
                };
                Ok(Node::Case {
                    expr: Box::new(expr),
                    stmts: vec![stmt],
                    attributes: None,
                    coord: Some(coord),
                })
            }
            Some(TokenKind::Default) => {
                let def_tok = self.advance()?;
                let coord = self.tok_coord_idx(def_tok);
                self.expect(TokenKind::Colon)?;
                let stmt = if self.starts_statement()? {
                    self.parse_pragmacomp_or_statement()?
                } else {
                    Node::EmptyStatement {
                        coord: Some(coord.clone()),
                    }
                };
                Ok(Node::Default {
                    stmts: vec![stmt],
                    attributes: None,
                    coord: Some(coord),
                })
            }
            _ => Err(self.error(
                "Invalid labeled statement".to_string(),
                CoordArg::file(self.filename()),
            )),
        }
    }

    fn parse_selection_statement(&mut self) -> PResult<Node> {
        let tok = self.advance()?;
        let coord = self.tok_coord_idx(tok);
        match self.tokens[tok].kind {
            TokenKind::If => {
                self.expect(TokenKind::Lparen)?;
                let cond = self.parse_expression()?;
                self.expect(TokenKind::Rparen)?;
                let then_stmt = self.parse_pragmacomp_or_statement()?;
                if self.accept(TokenKind::Else)?.is_some() {
                    let else_stmt = self.parse_pragmacomp_or_statement()?;
                    return Ok(Node::If {
                        cond: Box::new(cond),
                        iftrue: Box::new(then_stmt),
                        iffalse: Some(Box::new(else_stmt)),
                        coord: Some(coord),
                    });
                }
                Ok(Node::If {
                    cond: Box::new(cond),
                    iftrue: Box::new(then_stmt),
                    iffalse: None,
                    coord: Some(coord),
                })
            }
            TokenKind::Switch => {
                self.expect(TokenKind::Lparen)?;
                let expr = self.parse_expression()?;
                self.expect(TokenKind::Rparen)?;
                let stmt = self.parse_pragmacomp_or_statement()?;
                let switch = Node::Switch {
                    cond: Box::new(expr),
                    stmt: Box::new(stmt),
                    coord: Some(coord),
                };
                // `fix_switch_cases` runs on genuine `c_ast` objects in the
                // binding converter, exactly once per Switch node.
                Ok(Node::NeedsSwitchFix(Box::new(switch)))
            }
            _ => Err(self.error(
                "Invalid selection statement".to_string(),
                CoordArg::Full(coord),
            )),
        }
    }

    fn parse_iteration_statement(&mut self) -> PResult<Node> {
        let tok = self.advance()?;
        let coord = self.tok_coord_idx(tok);
        match self.tokens[tok].kind {
            TokenKind::While => {
                self.expect(TokenKind::Lparen)?;
                let cond = self.parse_expression()?;
                self.expect(TokenKind::Rparen)?;
                let stmt = self.parse_pragmacomp_or_statement()?;
                Ok(Node::While {
                    cond: Box::new(cond),
                    stmt: Box::new(stmt),
                    coord: Some(coord),
                })
            }
            TokenKind::Do => {
                let stmt = self.parse_pragmacomp_or_statement()?;
                self.expect(TokenKind::While)?;
                self.expect(TokenKind::Lparen)?;
                let cond = self.parse_expression()?;
                self.expect(TokenKind::Rparen)?;
                self.expect(TokenKind::Semi)?;
                Ok(Node::DoWhile {
                    cond: Box::new(cond),
                    stmt: Box::new(stmt),
                    coord: Some(coord),
                })
            }
            TokenKind::For => {
                self.expect(TokenKind::Lparen)?;
                if self.peek_kind(1)? == Some(TokenKind::StaticAssert)
                    || self.starts_declaration_idx(None)?
                {
                    // Names declared here are scoped to the loop (C11 6.8.5p5).
                    self.push_scope();
                    let decls = self.parse_declaration()?;
                    let init = Node::DeclList {
                        decls,
                        coord: Some(coord.clone()),
                    };
                    let cond = self.parse_expression_opt()?;
                    self.expect(TokenKind::Semi)?;
                    let next_expr = self.parse_expression_opt()?;
                    self.expect(TokenKind::Rparen)?;
                    let stmt = self.parse_pragmacomp_or_statement()?;
                    self.pop_scope();
                    return Ok(Node::For {
                        init: Some(Box::new(init)),
                        cond: cond.map(Box::new),
                        next: next_expr.map(Box::new),
                        stmt: Box::new(stmt),
                        coord: Some(coord),
                    });
                }
                let init = self.parse_expression_opt()?;
                self.expect(TokenKind::Semi)?;
                let cond = self.parse_expression_opt()?;
                self.expect(TokenKind::Semi)?;
                let next_expr = self.parse_expression_opt()?;
                self.expect(TokenKind::Rparen)?;
                let stmt = self.parse_pragmacomp_or_statement()?;
                Ok(Node::For {
                    init: init.map(Box::new),
                    cond: cond.map(Box::new),
                    next: next_expr.map(Box::new),
                    stmt: Box::new(stmt),
                    coord: Some(coord),
                })
            }
            _ => Err(self.error(
                "Invalid iteration statement".to_string(),
                CoordArg::Full(coord),
            )),
        }
    }

    fn parse_jump_statement(&mut self) -> PResult<Node> {
        let tok = self.advance()?;
        let coord = self.tok_coord_idx(tok);
        match self.tokens[tok].kind {
            TokenKind::Goto => {
                let name_tok = self.expect(TokenKind::Id)?;
                let name = self.tokens[name_tok].value.clone();
                self.expect(TokenKind::Semi)?;
                Ok(Node::Goto {
                    name,
                    coord: Some(coord),
                })
            }
            TokenKind::Break => {
                self.expect(TokenKind::Semi)?;
                Ok(Node::Break { coord: Some(coord) })
            }
            TokenKind::Continue => {
                self.expect(TokenKind::Semi)?;
                Ok(Node::Continue { coord: Some(coord) })
            }
            TokenKind::Return => {
                if self.accept(TokenKind::Semi)?.is_some() {
                    return Ok(Node::Return {
                        expr: None,
                        coord: Some(coord),
                    });
                }
                let expr = self.parse_expression()?;
                self.expect(TokenKind::Semi)?;
                Ok(Node::Return {
                    expr: Some(Box::new(expr)),
                    coord: Some(coord),
                })
            }
            _ => Err(self.error("Invalid jump statement".to_string(), CoordArg::Full(coord))),
        }
    }

    fn parse_expression_statement(&mut self) -> PResult<Node> {
        let expr = self.parse_expression_opt()?;
        let semi_tok = self.expect(TokenKind::Semi)?;
        match expr {
            None => {
                let coord = self.tok_coord_idx(semi_tok);
                Ok(Node::EmptyStatement { coord: Some(coord) })
            }
            Some(e) => Ok(e),
        }
    }
}

/// A block item is either a single node or a spliced list of nodes
/// (mirrors `_parse_block_item` returning `Node | list[Node]`).
enum BlockItem {
    One(Node),
    Many(Vec<Node>),
}

impl Parser {
    // ------------------------------------------------------------------
    // Expressions.
    // ------------------------------------------------------------------
    fn parse_expression_opt(&mut self) -> PResult<Option<Node>> {
        if self.starts_expression_idx(None)? {
            return Ok(Some(self.parse_expression()?));
        }
        Ok(None)
    }

    fn parse_expression(&mut self) -> PResult<Node> {
        let expr = self.parse_assignment_expression()?;
        if self.accept(TokenKind::Comma)?.is_none() {
            return Ok(expr);
        }
        let coord = expr.coord().cloned();
        let mut exprs = vec![expr, self.parse_assignment_expression()?];
        while self.accept(TokenKind::Comma)?.is_some() {
            exprs.push(self.parse_assignment_expression()?);
        }
        Ok(Node::ExprList { exprs, coord })
    }

    fn parse_assignment_expression(&mut self) -> PResult<Node> {
        let expr = self.parse_conditional_expression()?;
        if self.is_assignment_op()? {
            let op_tok = self.advance()?;
            let op = self.tokens[op_tok].value.clone();
            let coord = expr.coord().cloned();
            let rhs = self.parse_assignment_expression()?;
            return Ok(Node::Assignment {
                op,
                lvalue: Box::new(expr),
                rvalue: Box::new(rhs),
                coord,
            });
        }
        Ok(expr)
    }

    fn parse_conditional_expression(&mut self) -> PResult<Node> {
        let expr = self.parse_binary_expression(0, None)?;
        if self.accept(TokenKind::Condop)?.is_some() {
            let iftrue = self.parse_expression()?;
            self.expect(TokenKind::Colon)?;
            let iffalse = self.parse_conditional_expression()?;
            let coord = expr.coord().cloned();
            return Ok(Node::TernaryOp {
                cond: Box::new(expr),
                iftrue: Box::new(iftrue),
                iffalse: Box::new(iffalse),
                coord,
            });
        }
        Ok(expr)
    }

    fn parse_binary_expression(&mut self, min_prec: u32, lhs: Option<Node>) -> PResult<Node> {
        let mut lhs = match lhs {
            Some(l) => l,
            None => self.parse_cast_expression()?,
        };
        loop {
            let idx = self.peek_idx(1)?;
            let (prec, op) = match idx {
                Some(i) => match binary_precedence(self.tokens[i].kind) {
                    Some(p) => (p, self.tokens[i].value.clone()),
                    None => break,
                },
                None => break,
            };
            if prec < min_prec {
                break;
            }
            self.advance()?;
            let mut rhs = self.parse_cast_expression()?;
            loop {
                let next_prec = match self.peek_idx(1)? {
                    Some(j) => binary_precedence(self.tokens[j].kind),
                    None => None,
                };
                match next_prec {
                    Some(np) if np > prec => {
                        rhs = self.parse_binary_expression(np, Some(rhs))?;
                    }
                    _ => break,
                }
            }
            let coord = lhs.coord().cloned();
            lhs = Node::BinaryOp {
                op,
                left: Box::new(lhs),
                right: Box::new(rhs),
                coord,
            };
        }
        Ok(lhs)
    }

    fn parse_cast_expression(&mut self) -> PResult<Node> {
        if let Some((typ, mark, lparen_coord, _)) = self.try_parse_paren_type_name()? {
            if self.peek_kind(1)? == Some(TokenKind::Lbrace) {
                self.reset(mark);
            } else {
                let expr = self.parse_cast_expression()?;
                return Ok(Node::Cast {
                    to_type: Box::new(typ),
                    expr: Box::new(expr),
                    coord: Some(lparen_coord),
                });
            }
        }
        self.parse_unary_expression()
    }

    fn parse_unary_expression(&mut self) -> PResult<Node> {
        let tok_type = self.peek_kind(1)?;
        if matches!(
            tok_type,
            Some(TokenKind::PlusPlus) | Some(TokenKind::MinusMinus)
        ) {
            let tok = self.advance()?;
            let op = self.tokens[tok].value.clone();
            let expr = self.parse_unary_expression()?;
            let coord = expr.coord().cloned();
            return Ok(Node::UnaryOp {
                op,
                expr: Box::new(expr),
                coord,
            });
        }
        if matches!(
            tok_type,
            Some(TokenKind::And)
                | Some(TokenKind::Times)
                | Some(TokenKind::Plus)
                | Some(TokenKind::Minus)
                | Some(TokenKind::Not)
                | Some(TokenKind::Lnot)
        ) {
            let tok = self.advance()?;
            let op = self.tokens[tok].value.clone();
            let expr = self.parse_cast_expression()?;
            let coord = expr.coord().cloned();
            return Ok(Node::UnaryOp {
                op,
                expr: Box::new(expr),
                coord,
            });
        }
        if tok_type == Some(TokenKind::Sizeof) {
            let tok = self.advance()?;
            let coord = self.tok_coord_idx(tok);
            if let Some((typ, mark, _, _)) = self.try_parse_paren_type_name()? {
                if self.peek_kind(1)? != Some(TokenKind::Lbrace) {
                    let op = self.tokens[tok].value.clone();
                    return Ok(Node::UnaryOp {
                        op,
                        expr: Box::new(typ),
                        coord: Some(coord),
                    });
                }
                // sizeof (type){...} has an expression operand, including
                // any postfix operators following the compound literal.
                self.reset(mark);
            }
            let expr = self.parse_unary_expression()?;
            let op = self.tokens[tok].value.clone();
            return Ok(Node::UnaryOp {
                op,
                expr: Box::new(expr),
                coord: Some(coord),
            });
        }
        if tok_type == Some(TokenKind::Alignof) {
            let tok = self.advance()?;
            let coord = self.tok_coord_idx(tok);
            self.expect(TokenKind::Lparen)?;
            let typ = self.parse_type_name()?;
            self.expect(TokenKind::Rparen)?;
            let op = self.tokens[tok].value.clone();
            return Ok(Node::UnaryOp {
                op,
                expr: Box::new(typ),
                coord: Some(coord),
            });
        }
        self.parse_postfix_expression()
    }

    fn parse_postfix_expression(&mut self) -> PResult<Node> {
        let mut expr =
            if let Some((typ, mark, lparen_coord, storage)) = self.try_parse_paren_type_name()? {
                if self.peek_kind(1)? == Some(TokenKind::Lbrace) {
                    let init = self.parse_initializer()?;
                    Some(Node::CompoundLiteral {
                        typ: Box::new(typ),
                        init: Box::new(init),
                        storage,
                        coord: Some(lparen_coord),
                    })
                } else {
                    self.reset(mark);
                    None
                }
            } else {
                None
            };
        if expr.is_none() {
            expr = Some(self.parse_primary_expression()?);
        }
        let mut expr = expr.expect("postfix operand");
        loop {
            match self.peek_kind(1)? {
                Some(TokenKind::Lbracket) => {
                    self.advance()?;
                    let sub = self.parse_expression()?;
                    self.expect(TokenKind::Rbracket)?;
                    let coord = expr.coord().cloned();
                    expr = Node::ArrayRef {
                        name: Box::new(expr),
                        subscript: Box::new(sub),
                        coord,
                    };
                }
                Some(TokenKind::Lparen) => {
                    self.advance()?;
                    let args = if self.peek_kind(1)? == Some(TokenKind::Rparen) {
                        self.advance()?;
                        None
                    } else {
                        let args = self.parse_argument_expression_list()?;
                        self.expect(TokenKind::Rparen)?;
                        Some(Box::new(args))
                    };
                    let coord = expr.coord().cloned();
                    expr = Node::FuncCall {
                        name: Box::new(expr),
                        args,
                        coord,
                    };
                }
                Some(TokenKind::Period) | Some(TokenKind::Arrow) => {
                    let op_tok = self.advance()?;
                    let op = self.tokens[op_tok].value.clone();
                    let name_tok = self.advance()?;
                    if !matches!(
                        self.tokens[name_tok].kind,
                        TokenKind::Id | TokenKind::TypeId
                    ) {
                        let coord = self.tok_coord_idx(name_tok);
                        return Err(self.error(
                            "Invalid struct reference".to_string(),
                            CoordArg::Full(coord),
                        ));
                    }
                    let fname = self.tokens[name_tok].value.clone();
                    let fcoord = self.tok_coord_idx(name_tok);
                    let field = Node::ID {
                        name: fname,
                        coord: Some(fcoord),
                    };
                    let coord = expr.coord().cloned();
                    expr = Node::StructRef {
                        name: Box::new(expr),
                        op,
                        field: Box::new(field),
                        coord,
                    };
                }
                Some(TokenKind::PlusPlus) | Some(TokenKind::MinusMinus) => {
                    let tok = self.advance()?;
                    let op = format!("p{}", self.tokens[tok].value);
                    let coord = expr.coord().cloned();
                    expr = Node::UnaryOp {
                        op,
                        expr: Box::new(expr),
                        coord,
                    };
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    fn parse_primary_expression(&mut self) -> PResult<Node> {
        let tok_type = self.peek_kind(1)?;
        if tok_type == Some(TokenKind::Id) {
            return self.parse_identifier();
        }
        if matches!(
            tok_type,
            Some(TokenKind::IntConstDec)
                | Some(TokenKind::IntConstOct)
                | Some(TokenKind::IntConstHex)
                | Some(TokenKind::IntConstBin)
                | Some(TokenKind::IntConstChar)
                | Some(TokenKind::FloatConst)
                | Some(TokenKind::HexFloatConst)
                | Some(TokenKind::CharConst)
                | Some(TokenKind::WcharConst)
                | Some(TokenKind::U8CharConst)
                | Some(TokenKind::U16CharConst)
                | Some(TokenKind::U32CharConst)
                | Some(TokenKind::BoolConst)
                | Some(TokenKind::Nullptr)
        ) {
            return self.parse_constant();
        }
        if matches!(
            tok_type,
            Some(TokenKind::StringLiteral)
                | Some(TokenKind::WstringLiteral)
                | Some(TokenKind::U8StringLiteral)
                | Some(TokenKind::U16StringLiteral)
                | Some(TokenKind::U32StringLiteral)
        ) {
            return self.parse_unified_string_literal();
        }
        if tok_type == Some(TokenKind::Lparen) {
            self.advance()?;
            let expr = if self.peek_kind(1)? == Some(TokenKind::Lbrace) {
                // GNU statement expressions participate in all expression
                // operators, including postfix operators.
                self.parse_compound_statement(true)?
            } else {
                self.parse_expression()?
            };
            self.expect(TokenKind::Rparen)?;
            return Ok(expr);
        }
        if tok_type == Some(TokenKind::Generic) {
            return self.parse_generic_selection();
        }
        if tok_type == Some(TokenKind::BuiltinVaArg) {
            let tok = self.advance()?;
            let coord = self.tok_coord_idx(tok);
            let name = self.tokens[tok].value.clone();
            self.expect(TokenKind::Lparen)?;
            let expr = self.parse_assignment_expression()?;
            self.expect(TokenKind::Comma)?;
            let typ = self.parse_type_name()?;
            self.expect(TokenKind::Rparen)?;
            let args = Node::ExprList {
                exprs: vec![expr, typ],
                coord: Some(coord.clone()),
            };
            return Ok(Node::FuncCall {
                name: Box::new(Node::ID {
                    name,
                    coord: Some(coord.clone()),
                }),
                args: Some(Box::new(args)),
                coord: Some(coord),
            });
        }
        if tok_type == Some(TokenKind::Offsetof) {
            let off_tok = self.advance()?;
            let coord = self.tok_coord_idx(off_tok);
            let name = self.tokens[off_tok].value.clone();
            self.expect(TokenKind::Lparen)?;
            let typ = self.parse_type_name()?;
            self.expect(TokenKind::Comma)?;
            let designator = self.parse_offsetof_member_designator()?;
            self.expect(TokenKind::Rparen)?;
            let args = Node::ExprList {
                exprs: vec![typ, designator],
                coord: Some(coord.clone()),
            };
            return Ok(Node::FuncCall {
                name: Box::new(Node::ID {
                    name,
                    coord: Some(coord.clone()),
                }),
                args: Some(Box::new(args)),
                coord: Some(coord),
            });
        }
        Err(self.error(
            "Invalid expression".to_string(),
            CoordArg::file(self.filename()),
        ))
    }

    fn parse_generic_selection(&mut self) -> PResult<Node> {
        let tok = self.expect(TokenKind::Generic)?;
        let coord = self.tok_coord_idx(tok);
        self.expect(TokenKind::Lparen)?;
        let expr = self.parse_assignment_expression()?;
        self.expect(TokenKind::Comma)?;
        let mut associations = vec![self.parse_generic_association()?];
        while self.accept(TokenKind::Comma)?.is_some() {
            associations.push(self.parse_generic_association()?);
        }
        self.expect(TokenKind::Rparen)?;
        Ok(Node::GenericSelection {
            expr: Box::new(expr),
            associations,
            coord: Some(coord),
        })
    }

    fn parse_generic_association(&mut self) -> PResult<Node> {
        let idx = self.peek_idx(1)?;
        let Some(t) = idx else {
            return Err(self.error(
                "At end of input".to_string(),
                CoordArg::file(self.filename()),
            ));
        };
        let coord = self.tok_coord_idx(t);
        let typ = if self.accept(TokenKind::Default)?.is_some() {
            None
        } else {
            Some(Box::new(self.parse_type_name()?))
        };
        self.expect(TokenKind::Colon)?;
        let expr = self.parse_assignment_expression()?;
        Ok(Node::GenericAssociation {
            typ,
            expr: Box::new(expr),
            coord: Some(coord),
        })
    }

    fn parse_offsetof_member_designator(&mut self) -> PResult<Node> {
        let mut node = self.parse_identifier_or_typeid()?;
        loop {
            if self.accept(TokenKind::Period)?.is_some() {
                let field = self.parse_identifier_or_typeid()?;
                let coord = node.coord().cloned();
                node = Node::StructRef {
                    name: Box::new(node),
                    op: ".".to_string(),
                    field: Box::new(field),
                    coord,
                };
                continue;
            }
            if self.accept(TokenKind::Lbracket)?.is_some() {
                let expr = self.parse_expression()?;
                self.expect(TokenKind::Rbracket)?;
                let coord = node.coord().cloned();
                node = Node::ArrayRef {
                    name: Box::new(node),
                    subscript: Box::new(expr),
                    coord,
                };
                continue;
            }
            break;
        }
        Ok(node)
    }

    fn parse_argument_expression_list(&mut self) -> PResult<Node> {
        let expr = self.parse_assignment_expression()?;
        let coord = expr.coord().cloned();
        let mut exprs = vec![expr];
        while self.accept(TokenKind::Comma)?.is_some() {
            exprs.push(self.parse_assignment_expression()?);
        }
        Ok(Node::ExprList { exprs, coord })
    }

    fn parse_constant_expression(&mut self) -> PResult<Node> {
        self.parse_conditional_expression()
    }

    // ------------------------------------------------------------------
    // Terminals.
    // ------------------------------------------------------------------
    fn parse_identifier(&mut self) -> PResult<Node> {
        let tok = self.expect(TokenKind::Id)?;
        let coord = self.tok_coord_idx(tok);
        Ok(Node::ID {
            name: self.tokens[tok].value.clone(),
            coord: Some(coord),
        })
    }

    fn parse_identifier_or_typeid(&mut self) -> PResult<Node> {
        let tok = self.advance()?;
        if !matches!(self.tokens[tok].kind, TokenKind::Id | TokenKind::TypeId) {
            let coord = self.tok_coord_idx(tok);
            return Err(self.error("Expected identifier".to_string(), CoordArg::Full(coord)));
        }
        let coord = self.tok_coord_idx(tok);
        Ok(Node::ID {
            name: self.tokens[tok].value.clone(),
            coord: Some(coord),
        })
    }

    fn parse_constant(&mut self) -> PResult<Node> {
        let tok = self.advance()?;
        let kind = self.tokens[tok].kind;
        let value = self.tokens[tok].value.clone();
        let coord = self.tok_coord_idx(tok);
        if kind == TokenKind::BoolConst {
            return Ok(Node::Constant {
                typ: "bool".to_string(),
                value,
                coord: Some(coord),
            });
        }
        if kind == TokenKind::Nullptr {
            return Ok(Node::Constant {
                typ: "nullptr_t".to_string(),
                value,
                coord: Some(coord),
            });
        }
        if matches!(
            kind,
            TokenKind::IntConstDec
                | TokenKind::IntConstOct
                | TokenKind::IntConstHex
                | TokenKind::IntConstBin
                | TokenKind::IntConstChar
        ) {
            // Integer suffixes are at most three characters (e.g. ull, wbu).
            let tail: String = value
                .chars()
                .rev()
                .take(3)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            let tail_lower = tail.to_lowercase();
            if tail_lower.ends_with("wb") || tail_lower.ends_with("wbu") {
                if !self.dialect_c23 {
                    return Err(self.error(
                        "Bit-precise integer constants require C23".to_string(),
                        CoordArg::Full(coord),
                    ));
                }
                // Classify by suffix, as with other integer constants; retain
                // the spelling without computing the value or its bit width.
                let t = if tail_lower.contains('u') {
                    "unsigned _BitInt"
                } else {
                    "_BitInt"
                };
                return Ok(Node::Constant {
                    typ: t.to_string(),
                    value,
                    coord: Some(coord),
                });
            }
            let mut u_count = 0;
            let mut l_count = 0;
            for ch in value[value.len().saturating_sub(3)..].chars() {
                if ch == 'l' || ch == 'L' {
                    l_count += 1;
                } else if ch == 'u' || ch == 'U' {
                    u_count += 1;
                }
            }
            // Unreachable from lexer output (the suffix matcher is maximal),
            // mirrored for exactness.
            if u_count > 1 {
                unreachable!("Constant cannot have more than one u/U suffix.");
            }
            if l_count > 2 {
                unreachable!("Constant cannot have more than two l/L suffix.");
            }
            let prefix = format!("{}{}", "unsigned ".repeat(u_count), "long ".repeat(l_count));
            return Ok(Node::Constant {
                typ: format!("{prefix}int"),
                value,
                coord: Some(coord),
            });
        }
        if matches!(kind, TokenKind::FloatConst | TokenKind::HexFloatConst) {
            let t = if value.ends_with("df") || value.ends_with("DF") {
                "_Decimal32"
            } else if value.ends_with("dd") || value.ends_with("DD") {
                "_Decimal64"
            } else if value.ends_with("dl") || value.ends_with("DL") {
                "_Decimal128"
            } else if value.ends_with('f') || value.ends_with('F') {
                "float"
            } else if value.ends_with('l') || value.ends_with('L') {
                "long double"
            } else {
                "double"
            };
            if matches!(t, "_Decimal32" | "_Decimal64" | "_Decimal128") && !self.dialect_c23 {
                return Err(self.error(
                    "Decimal floating constants require C23".to_string(),
                    CoordArg::Full(coord),
                ));
            }
            return Ok(Node::Constant {
                typ: t.to_string(),
                value,
                coord: Some(coord),
            });
        }
        if matches!(
            kind,
            TokenKind::CharConst
                | TokenKind::WcharConst
                | TokenKind::U8CharConst
                | TokenKind::U16CharConst
                | TokenKind::U32CharConst
        ) {
            return Ok(Node::Constant {
                typ: "char".to_string(),
                value,
                coord: Some(coord),
            });
        }
        Err(self.error("Invalid constant".to_string(), CoordArg::Full(coord)))
    }

    fn parse_unified_string_literal(&mut self) -> PResult<Node> {
        // Represent the whole run of adjacent literals with one Constant.
        let tok = self.advance()?;
        let first_kind = self.tokens[tok].kind;
        if !matches!(
            first_kind,
            TokenKind::StringLiteral
                | TokenKind::WstringLiteral
                | TokenKind::U8StringLiteral
                | TokenKind::U16StringLiteral
                | TokenKind::U32StringLiteral
        ) {
            let coord = self.tok_coord_idx(tok);
            return Err(self.error("Invalid string literal".to_string(), CoordArg::Full(coord)));
        }
        let first_value = self.tokens[tok].value.clone();
        let first_coord = self.tok_coord_idx(tok);
        // Split off the optional encoding prefix and remove the closing quote.
        let (mut prefix, first_contents) = split_string_literal(&first_value);
        let mut parts = vec![first_contents];
        while matches!(
            self.peek_kind(1)?,
            Some(TokenKind::StringLiteral)
                | Some(TokenKind::WstringLiteral)
                | Some(TokenKind::U8StringLiteral)
                | Some(TokenKind::U16StringLiteral)
                | Some(TokenKind::U32StringLiteral)
        ) {
            let tok2 = self.advance()?;
            let v2 = self.tokens[tok2].value.clone();
            let (next_prefix, next_contents) = split_string_literal(&v2);
            // The first nonempty prefix applies to the whole run, even if it
            // occurs after ordinary literals.
            if !next_prefix.is_empty() && prefix.is_empty() {
                prefix = next_prefix;
            }
            if string_literal_needs_separator(
                parts.last().map(|s| s.as_str()).unwrap_or(""),
                &next_contents,
            ) {
                parts.push(next_contents);
            } else {
                parts.last_mut().expect("one part").push_str(&next_contents);
            }
        }
        let value = format!("{}\"{}\"", prefix, parts.join("\" \""));
        Ok(Node::Constant {
            typ: "string".to_string(),
            value,
            coord: Some(first_coord),
        })
    }

    fn parse_initializer(&mut self) -> PResult<Node> {
        if let Some(lbrace_tok) = self.accept(TokenKind::Lbrace)? {
            if self.accept(TokenKind::Rbrace)?.is_some() {
                let coord = self.tok_coord_idx(lbrace_tok);
                return Ok(Node::InitList {
                    exprs: vec![],
                    coord: Some(coord),
                });
            }
            let init_list = self.parse_initializer_list()?;
            self.accept(TokenKind::Comma)?;
            self.expect(TokenKind::Rbrace)?;
            return Ok(init_list);
        }
        self.parse_assignment_expression()
    }

    fn parse_initializer_list(&mut self) -> PResult<Node> {
        let first = self.parse_initializer_item()?;
        let coord = first.coord().cloned();
        let mut items = vec![first];
        while self.accept(TokenKind::Comma)?.is_some() {
            if self.peek_kind(1)? == Some(TokenKind::Rbrace) {
                break;
            }
            items.push(self.parse_initializer_item()?);
        }
        Ok(Node::InitList {
            exprs: items,
            coord,
        })
    }

    fn parse_initializer_item(&mut self) -> PResult<Node> {
        let mut designation = None;
        if matches!(
            self.peek_kind(1)?,
            Some(TokenKind::Lbracket) | Some(TokenKind::Period)
        ) {
            designation = Some(self.parse_designation()?);
        }
        let init = self.parse_initializer()?;
        match designation {
            Some(d) => Ok(Node::NamedInitializer {
                name: d,
                expr: Box::new(init),
            }),
            None => Ok(init),
        }
    }

    fn parse_designation(&mut self) -> PResult<Vec<Node>> {
        let designators = self.parse_designator_list()?;
        self.expect(TokenKind::Equals)?;
        Ok(designators)
    }

    fn parse_designator_list(&mut self) -> PResult<Vec<Node>> {
        let mut designators = Vec::new();
        while matches!(
            self.peek_kind(1)?,
            Some(TokenKind::Lbracket) | Some(TokenKind::Period)
        ) {
            designators.push(self.parse_designator()?);
        }
        Ok(designators)
    }

    fn parse_designator(&mut self) -> PResult<Node> {
        if let Some(tok) = self.accept(TokenKind::Lbracket)? {
            let expr = self.parse_constant_expression()?;
            self.expect(TokenKind::Rbracket)?;
            let coord = self.tok_coord_idx(tok);
            return Ok(Node::ArrayDesignator {
                expr: Box::new(expr),
                coord: Some(coord),
            });
        }
        if self.accept(TokenKind::Period)?.is_some() {
            return self.parse_identifier_or_typeid();
        }
        Err(self.error(
            "Invalid designator".to_string(),
            CoordArg::file(self.filename()),
        ))
    }
}

/// Split a literal into `(prefix, contents-without-closing-quote)`.
/// The value is `prefix " contents "`: split at the first `"` and drop the
/// final byte (the closing quote).
fn split_string_literal(value: &str) -> (String, String) {
    match value.find('"') {
        Some(i) => (
            value[..i].to_string(),
            value[i + 1..value.len() - 1].to_string(),
        ),
        None => (String::new(), value.to_string()),
    }
}

/// Whether joining two literal bodies would change their C spelling
/// (mirrors `CParser._string_literal_needs_separator`).
fn string_literal_needs_separator(left: &str, right: &str) -> bool {
    if left.is_empty() || right.is_empty() {
        return false;
    }
    // A generated trigraph would be replaced before C string processing.
    let bl: String = left
        .chars()
        .rev()
        .take(2)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    let br: String = right.chars().take(2).collect();
    let boundary = format!("{bl}{br}");
    if ["=", "/", "'", "(", ")", "!", "<", ">", "-"]
        .iter()
        .any(|c| boundary.contains(&format!("??{c}")))
    {
        return true;
    }
    // Only an escape reaching the end of left can absorb characters from
    // right. The last backslash is the only possible start of that escape.
    let escape_start = match left.rfind('\\') {
        Some(i) => i,
        None => return false,
    };
    // An even run of backslashes represents escaped backslashes, not the
    // start of the numeric escape that the following digits might suggest.
    let mut run_start = escape_start;
    while run_start > 0 && left.as_bytes()[run_start - 1] == b'\\' {
        // Byte indexing is safe on the backslash run (all ASCII).
        run_start -= 1;
    }
    if (escape_start - run_start + 1) % 2 == 0 {
        return false;
    }
    let escape = &left[escape_start + 1..];
    if let Some(hex) = escape.strip_prefix('x') {
        // Hex escapes have no digit limit.
        let hex_digits = "0123456789abcdefABCDEF";
        return right
            .chars()
            .next()
            .map(|c| hex_digits.contains(c))
            .unwrap_or(false)
            && hex.chars().all(|c| hex_digits.contains(c));
    }
    // Octal escapes consume at most three digits; universal character names
    // have fixed widths and cannot absorb characters from the next literal.
    let esc_len = escape.chars().count();
    (1..3).contains(&esc_len)
        && escape.chars().all(|c| matches!(c, '0'..='7'))
        && right
            .chars()
            .next()
            .map(|c| matches!(c, '0'..='7'))
            .unwrap_or(false)
}

// ----------------------------------------------------------------------
// Atomic-specifier normalization (mirrors `ast_transforms`).
//
// Applied eagerly inside `_fix_decl_name_type`, exactly where upstream
// calls `fix_atomic_specifiers`: later parser steps (notably the
// `_Atomic` qualifier append in `_parse_atomic_specifier`) observe the
// fixed tree, so deferring would reorder qualifier placement.
// ----------------------------------------------------------------------

/// Mirrors `fix_atomic_specifiers`: fix `_Atomic` wrappers to a fixed
/// point, then match declaration qualifiers to the innermost `TypeDecl`.
fn fix_atomic_specifiers(mut decl: Node) -> PResult<Node> {
    loop {
        let (d, found) = fix_atomic_specifiers_once(decl)?;
        decl = d;
        if !found {
            break;
        }
    }
    // Match the declaration's qualifiers to the innermost TypeDecl, as for
    // ordinary declarations. Bail out (leaving `decl` as is) when the chain
    // does not reach one.
    if !tail_reaches_typedecl(&decl) {
        return Ok(decl);
    }
    let name = decl_name(&decl);
    let tail_quals = typedecl_tail(&decl)
        .typedecl_quals()
        .cloned()
        .unwrap_or_default();
    // Match the declaration's qualifiers to the innermost TypeDecl.
    set_decl_quals(&mut decl, tail_quals);
    let tail = typedecl_tail_mut(&mut decl);
    let Node::TypeDecl { declname, .. } = tail else {
        unreachable!("tail without TypeDecl")
    };
    if declname.is_none() {
        *declname = name;
    }
    Ok(decl)
}

/// Set the qualifier list of a declaration wrapper.
fn set_decl_quals(n: &mut Node, quals: Vec<String>) {
    match n {
        Node::Decl { quals: slot, .. } => *slot = quals,
        Node::Typedef { quals: slot, .. } => *slot = quals,
        Node::Typename { quals: slot, .. } => *slot = quals,
        _ => unreachable!("set_decl_quals on non-declaration"),
    }
}

/// Whether the `.type` chain reaches a `TypeDecl`.
fn tail_reaches_typedecl(n: &Node) -> bool {
    if matches!(n, Node::TypeDecl { .. }) {
        return true;
    }
    match decl_inner(n) {
        Some(inner) => tail_reaches_typedecl(inner),
        None => false,
    }
}

/// Qualifiers of a `TypeDecl` tail.
trait TypedeclQuals {
    fn typedecl_quals(&self) -> Option<&Vec<String>>;
}

impl TypedeclQuals for Node {
    fn typedecl_quals(&self) -> Option<&Vec<String>> {
        match self {
            Node::TypeDecl { quals, .. } => quals.as_ref(),
            _ => None,
        }
    }
}

/// One fix round: find the innermost `_Atomic` wrapper `Typename` in the
/// `.type` chain and splice its contained type into its grandparent.
/// Returns `(decl, false)` when no atomic wrapper is present.
fn fix_atomic_specifiers_once(mut decl: Node) -> PResult<(Node, bool)> {
    let Some(depth) = atomic_wrapper_depth(&decl) else {
        return Ok((decl, false));
    };
    // The wrapper is always nested below a `TypeDecl` parent with a
    // grandparent above it (mirrors the upstream asserts); anything else
    // means no fix applies.
    if depth < 2 || !atomic_parent_is_typedecl(&decl, depth) {
        return Ok((decl, false));
    }
    // Probe everything immutably first.
    let probe = atomic_probe(&decl, depth);
    let Some(probe) = probe else {
        return Ok((decl, false));
    };
    if matches!(probe.inner_kind, AtomicInner::Array | AtomicInner::Func) {
        return Err(ParseError::new(
            "_Atomic cannot be applied to array or function type",
            &CoordArg::opt(probe.atomic_coord),
        ));
    }
    // Declaration specifiers are shared by comma-separated declarators:
    // work on a clone of the contained type.
    let mut typ = probe.inner_clone;
    if typ.coord().is_none() {
        // Preserve the declarator coord for `_Atomic(T)` so `TypeDecl`
        // doesn't lose its location when replacing the wrapper.
        set_node_coord(&mut typ, probe.parent_coord);
    }
    // Qualifiers outside `_Atomic(T)` apply to `T` itself.
    let mut quals = probe.parent_quals;
    quals.extend(typ_quals_of(&typ));
    if !quals.iter().any(|q| q == "_Atomic") {
        quals.push("_Atomic".to_string());
    }
    set_node_quals(&mut typ, quals);
    // Attach into the grandparent.
    {
        let mut cur: &mut Node = &mut decl;
        for _ in 0..depth - 2 {
            cur = decl_inner_mut(cur).expect("atomic chain changed");
        }
        set_inner(cur, typ);
    }
    Ok((decl, true))
}

/// Depth of the innermost `_Atomic`-qualified `Typename` in the `.type`
/// chain (`decl` itself is depth 0), if any. `Shared` positions resolve
/// transparently for the check but do not add depth (upstream has no
/// wrapper there).
fn atomic_wrapper_depth(decl: &Node) -> Option<usize> {
    let mut depth = 1;
    let mut cur = decl_inner(decl)?;
    loop {
        if let Node::Typename { quals, .. } = cur.resolve() {
            if quals.iter().any(|q| q == "_Atomic") {
                return Some(depth);
            }
        }
        cur = decl_inner(cur)?;
        depth += 1;
    }
}

/// Whether the atomic wrapper's parent is a `TypeDecl`.
fn atomic_parent_is_typedecl(decl: &Node, depth: usize) -> bool {
    let mut cur = decl;
    for _ in 0..depth - 1 {
        match decl_inner(cur) {
            Some(inner) => cur = inner,
            None => return false,
        }
    }
    matches!(cur, Node::TypeDecl { .. })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AtomicInner {
    Other,
    Array,
    Func,
}

/// Immutable probe of the surgery site.
struct AtomicProbe {
    inner_kind: AtomicInner,
    inner_clone: Node,
    atomic_coord: Option<Coord>,
    parent_coord: Option<Coord>,
    parent_quals: Vec<String>,
}

fn atomic_probe(decl: &Node, depth: usize) -> Option<AtomicProbe> {
    let mut cur = decl;
    for _ in 0..depth - 2 {
        cur = decl_inner(cur)?;
    }
    let grandparent = cur;
    let _ = grandparent;
    let mut cur = decl;
    for _ in 0..depth - 1 {
        cur = decl_inner(cur)?;
    }
    let Node::TypeDecl {
        quals: parent_quals,
        coord: parent_coord,
        ..
    } = cur
    else {
        return None;
    };
    let node = decl_inner(cur)?.resolve();
    let Node::Typename {
        coord: atomic_coord,
        ..
    } = node
    else {
        return None;
    };
    let inner = decl_inner(node)?;
    let inner_kind = match inner {
        Node::ArrayDecl { .. } => AtomicInner::Array,
        Node::FuncDecl { .. } => AtomicInner::Func,
        _ => AtomicInner::Other,
    };
    Some(AtomicProbe {
        inner_kind,
        inner_clone: inner.clone(),
        atomic_coord: atomic_coord.clone(),
        parent_coord: parent_coord.clone(),
        parent_quals: parent_quals.clone().unwrap_or_default(),
    })
}

/// Qualifier list of a contained type (`[]` when the node carries none).
fn typ_quals_of(n: &Node) -> Vec<String> {
    match n {
        Node::TypeDecl { quals, .. } => quals.clone().unwrap_or_default(),
        Node::PtrDecl { quals, .. } => quals.clone(),
        _ => vec![],
    }
}

fn set_node_quals(n: &mut Node, quals: Vec<String>) {
    match n {
        Node::TypeDecl { quals: slot, .. } => *slot = Some(quals),
        Node::PtrDecl { quals: slot, .. } => *slot = quals,
        _ => unreachable!("atomic inner without quals slot"),
    }
}

fn set_node_coord(n: &mut Node, coord: Option<Coord>) {
    match n {
        Node::TypeDecl { coord: slot, .. } => *slot = coord,
        Node::PtrDecl { coord: slot, .. } => *slot = coord,
        Node::ArrayDecl { coord: slot, .. } => *slot = coord,
        Node::FuncDecl { coord: slot, .. } => *slot = coord,
        _ => unreachable!("atomic inner without coord slot"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str, c23: bool) -> Result<Node, String> {
        Parser::new(c23).parse(text, "t.c").map_err(|e| e.text)
    }

    fn decl_name_of(node: &Node) -> Option<String> {
        decl_name(node)
    }

    #[test]
    fn simple_decl() {
        let ast = parse("int x;", false).unwrap();
        let Node::FileAST { ext } = ast else {
            panic!("expected FileAST");
        };
        assert_eq!(ext.len(), 1);
        assert_eq!(decl_name_of(&ext[0]), Some("x".to_string()));
    }

    #[test]
    fn shared_specifier_identity() {
        // `struct S {...} a, *b;` shares one Struct node across declarators.
        let ast = parse("struct S { int x; } a, *b;", false).unwrap();
        let Node::FileAST { ext } = ast else {
            panic!("expected FileAST");
        };
        assert_eq!(ext.len(), 2);
        fn struct_ptr(n: &Node) -> *const Node {
            let mut cur = n;
            loop {
                // Resolve through `Shared` (transparent for reads).
                if let Node::Struct { .. } = cur.resolve() {
                    return cur.resolve() as *const Node;
                }
                match decl_inner(cur) {
                    Some(inner) => cur = inner,
                    None => panic!("no struct in declarator"),
                }
            }
        }
        assert_eq!(struct_ptr(&ext[0]), struct_ptr(&ext[1]));
    }

    #[test]
    fn atomic_quals_placement() {
        let ast = parse("_Atomic(int) ai;", false).unwrap();
        let Node::FileAST { ext } = ast else {
            panic!("expected FileAST");
        };
        let Node::Decl { quals, name, .. } = &ext[0] else {
            panic!("expected Decl");
        };
        assert_eq!(quals, &vec!["_Atomic".to_string()]);
        assert_eq!(name.as_deref(), Some("ai"));
    }

    #[test]
    fn error_messages_match_upstream() {
        // `_parse_primary_expression` reports with the bare filename,
        // exactly like upstream `self.clex.filename` coordinates.
        assert_eq!(
            parse("int a = ;", false).unwrap_err(),
            "t.c: Invalid expression"
        );
        assert_eq!(
            parse("struct S { int x; }", false).unwrap_err(),
            "t.c: At end of input"
        );
    }

    #[test]
    fn trailing_tokens_rejected() {
        assert_eq!(
            parse("int x; 42", false).unwrap_err(),
            "t.c:1:8: before: 42"
        );
    }
}
