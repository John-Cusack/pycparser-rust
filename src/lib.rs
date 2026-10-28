//! Python bindings for the pycparser-rust port (PyO3).
//!
//! The extension exposes two primitives; the `pycparser` package wraps
//! them in API-compatible `CLexer`/`CParser` facades:
//!
//! - `parse_ast(text, filename, c23)` parses a translation unit with the
//!   Rust core and converts the tree field-for-field into genuine
//!   `c_ast` objects (imported lazily from the installed `pycparser`
//!   package, so the classes are exactly the shipped ones). Failures raise
//!   `CoreParseError` carrying the already-formatted message; the facade
//!   re-raises it as `c_parser.ParseError`.
//! - `LexerState` drives the Rust lexer one `token()` step at a time so
//!   the facade can invoke `error_func` in the exact upstream order.

use pycparser_rust_core::ast::Node;
use pycparser_rust_core::lexer::{LexEvent, Lexer};
use pycparser_rust_core::parser::Parser;
use pyo3::create_exception;
use pyo3::prelude::*;
use pyo3::types::PyDict;

create_exception!(_core, CoreParseError, pyo3::exceptions::PyException);

/// Parse `text` and return a genuine `c_ast.FileAST`.
#[pyfunction]
fn parse_ast(py: Python<'_>, text: &str, filename: &str, c23: bool) -> PyResult<PyObject> {
    let mut parser = Parser::new(c23);
    match parser.parse(text, filename) {
        Ok(node) => {
            let ctx = Ctx::load(py)?;
            ctx.convert(&node)
        }
        Err(e) => Err(CoreParseError::new_err(e.text)),
    }
}

/// Incremental lexer state backing the `CLexer` facade. Events arrive as
/// tuples: `("token", kind, value, lineno, column, filename)`,
/// `("error", msg, line, column)`, `("end",)`. Error resume positions are
/// applied lazily inside the core, so a raising `error_func` observes the
/// same lexer state as upstream.
#[pyclass]
struct LexerState {
    lexer: Lexer,
}

#[pymethods]
impl LexerState {
    #[new]
    fn new(c23: bool) -> Self {
        Self {
            lexer: Lexer::new(c23),
        }
    }

    fn input(&mut self, text: &str, filename: &str) {
        self.lexer.input(text, filename);
    }

    fn filename(&self) -> String {
        self.lexer.filename().to_string()
    }

    fn next_event(&mut self, py: Python<'_>) -> PyResult<PyObject> {
        match self.lexer.next_event() {
            LexEvent::Token(t) => Ok((
                "token",
                t.kind.name(),
                t.value,
                t.lineno,
                t.column,
                t.filename,
                t.file_gen,
            )
                .into_pyobject(py)?
                .into()),
            LexEvent::Error(e, _) => {
                Ok(("error", e.msg, e.line, e.column).into_pyobject(py)?.into())
            }
            LexEvent::End => Ok(("end",).into_pyobject(py)?.into()),
        }
    }
}

#[pymodule]
fn _core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(parse_ast, m)?)?;
    m.add_class::<LexerState>()?;
    m.add("CoreParseError", m.py().get_type::<CoreParseError>())?;
    Ok(())
}

/// Conversion context: the genuine shipped classes, imported lazily so the
/// extension never participates in the `pycparser` import cycle.
struct Ctx<'py> {
    py: Python<'py>,
    c_ast: Bound<'py, PyModule>,
    transforms: Bound<'py, PyModule>,
    coord_cls: Bound<'py, PyAny>,
    /// Converted objects by shared-subtree address: `Shared` positions
    /// convert once, so comma-separated declarators observe the same
    /// object (mirrors upstream sharing; the generator groups by `is`).
    memo: std::cell::RefCell<std::collections::HashMap<usize, PyObject>>,
    /// Interned filename objects by value (one `parse_ast` call shares
    /// them, mirroring upstream rebinding `self._filename` per generation).
    filenames: std::cell::RefCell<std::collections::HashMap<String, PyObject>>,
}

impl<'py> Ctx<'py> {
    fn load(py: Python<'py>) -> PyResult<Self> {
        let c_ast = py.import("pycparser.c_ast")?;
        let transforms = py.import("pycparser.ast_transforms")?;
        let parser_mod = py.import("pycparser.c_parser")?;
        let coord_cls = parser_mod.getattr("Coord")?;
        Ok(Self {
            py,
            c_ast,
            transforms,
            coord_cls,
            memo: Default::default(),
            filenames: Default::default(),
        })
    }

    fn coord(&self, coord: &Option<pycparser_rust_core::ast::Coord>) -> PyResult<PyObject> {
        match coord {
            None => Ok(self.py.None()),
            Some(c) => {
                let cached: Option<PyObject> = self
                    .filenames
                    .borrow()
                    .get(&c.file)
                    .map(|o| o.clone_ref(self.py));
                let file = if let Some(obj) = cached {
                    obj
                } else {
                    let obj: PyObject = c.file.clone().into_pyobject(self.py)?.into();
                    self.filenames
                        .borrow_mut()
                        .insert(c.file.clone(), obj.clone_ref(self.py));
                    obj
                };
                Ok(self.coord_cls.call1((file, c.line, c.col))?.into())
            }
        }
    }

    fn list(&self, nodes: &[Node]) -> PyResult<PyObject> {
        let mut out = Vec::with_capacity(nodes.len());
        for n in nodes {
            out.push(self.convert(n)?);
        }
        Ok(out.into_pyobject(self.py)?.into())
    }

    fn opt(&self, node: &Option<Box<Node>>) -> PyResult<PyObject> {
        match node {
            None => Ok(self.py.None()),
            Some(n) => self.convert(n),
        }
    }

    fn opt_list(&self, nodes: &Option<Vec<Node>>) -> PyResult<PyObject> {
        match nodes {
            None => Ok(self.py.None()),
            Some(v) => self.list(v),
        }
    }

    fn strs(&self, ss: &[String]) -> PyObject {
        ss.into_pyobject(self.py).expect("str list").into()
    }

    fn opt_str(&self, s: &Option<String>) -> PyObject {
        match s {
            None => self.py.None(),
            Some(v) => v.into_pyobject(self.py).expect("str").into(),
        }
    }

    fn call(
        &self,
        cls: &str,
        args: Vec<PyObject>,
        kwargs: Vec<(&'static str, PyObject)>,
    ) -> PyResult<PyObject> {
        let cls = self.c_ast.getattr(cls)?;
        let kwargs_dict = if kwargs.is_empty() {
            None
        } else {
            let d = PyDict::new(self.py);
            for (k, v) in kwargs {
                d.set_item(k, v)?;
            }
            Some(d)
        };
        let args_tup = pyo3::types::PyTuple::new(self.py, args)?;
        Ok(cls.call(args_tup, kwargs_dict.as_ref())?.into())
    }

    fn with_attrs(
        &self,
        attrs: &Option<Vec<Node>>,
        mut kwargs: Vec<(&'static str, PyObject)>,
    ) -> PyResult<Vec<(&'static str, PyObject)>> {
        if let Some(v) = attrs {
            kwargs.push(("attributes", self.list(v)?));
        }
        Ok(kwargs)
    }

    fn convert(&self, node: &Node) -> PyResult<PyObject> {
        match node {
            Node::Shared(rc) => {
                let key = std::rc::Rc::as_ptr(rc) as usize;
                if let Some(obj) = self.memo.borrow().get(&key).map(|o| o.clone_ref(self.py)) {
                    return Ok(obj);
                }
                let obj = self.convert(rc)?;
                self.memo.borrow_mut().insert(key, obj.clone_ref(self.py));
                Ok(obj)
            }
            Node::Alignas { alignment, coord } => self.call(
                "Alignas",
                vec![self.convert(alignment)?, self.coord(coord)?],
                vec![],
            ),
            Node::ArrayDecl {
                typ,
                dim,
                dim_quals,
                attributes,
                coord,
            } => {
                let kwargs = self.with_attrs(attributes, vec![])?;
                self.call(
                    "ArrayDecl",
                    vec![
                        self.opt(typ)?,
                        self.opt(dim)?,
                        self.strs(dim_quals),
                        self.coord(coord)?,
                    ],
                    kwargs,
                )
            }
            Node::ArrayDesignator { expr, coord } => self.call(
                "ArrayDesignator",
                vec![self.convert(expr)?, self.coord(coord)?],
                vec![],
            ),
            Node::ArrayRef {
                name,
                subscript,
                coord,
            } => self.call(
                "ArrayRef",
                vec![
                    self.convert(name)?,
                    self.convert(subscript)?,
                    self.coord(coord)?,
                ],
                vec![],
            ),
            Node::Assignment {
                op,
                lvalue,
                rvalue,
                coord,
            } => self.call(
                "Assignment",
                vec![
                    op.into_pyobject(self.py)?.into(),
                    self.convert(lvalue)?,
                    self.convert(rvalue)?,
                    self.coord(coord)?,
                ],
                vec![],
            ),
            Node::Attribute {
                name,
                namespace,
                arguments,
                coord,
            } => self.call(
                "Attribute",
                vec![
                    name.into_pyobject(self.py)?.into(),
                    self.opt_str(namespace),
                    self.opt_str(arguments),
                    self.coord(coord)?,
                ],
                vec![],
            ),
            Node::AttributeDeclaration { attributes, coord } => self.call(
                "AttributeDeclaration",
                vec![self.list(attributes)?, self.coord(coord)?],
                vec![],
            ),
            Node::AttributeSpecifier { attributes, coord } => self.call(
                "AttributeSpecifier",
                vec![self.list(attributes)?, self.coord(coord)?],
                vec![],
            ),
            Node::AttributedStatement {
                attributes,
                stmt,
                coord,
            } => self.call(
                "AttributedStatement",
                vec![
                    self.list(attributes)?,
                    self.convert(stmt)?,
                    self.coord(coord)?,
                ],
                vec![],
            ),
            Node::AttributedType {
                typ,
                attributes,
                coord,
            } => self.call(
                "AttributedType",
                vec![
                    self.convert(typ)?,
                    self.list(attributes)?,
                    self.coord(coord)?,
                ],
                vec![],
            ),
            Node::BinaryOp {
                op,
                left,
                right,
                coord,
            } => self.call(
                "BinaryOp",
                vec![
                    op.into_pyobject(self.py)?.into(),
                    self.convert(left)?,
                    self.convert(right)?,
                    self.coord(coord)?,
                ],
                vec![],
            ),
            Node::BitIntType {
                width,
                signedness,
                coord,
            } => self.call(
                "BitIntType",
                vec![
                    self.convert(width)?,
                    self.opt_str(signedness),
                    self.coord(coord)?,
                ],
                vec![],
            ),
            Node::Break { coord } => self.call("Break", vec![self.coord(coord)?], vec![]),
            Node::Case {
                expr,
                stmts,
                attributes,
                coord,
            } => {
                let kwargs = self.with_attrs(attributes, vec![])?;
                self.call(
                    "Case",
                    vec![self.convert(expr)?, self.list(stmts)?, self.coord(coord)?],
                    kwargs,
                )
            }
            Node::Cast {
                to_type,
                expr,
                coord,
            } => self.call(
                "Cast",
                vec![
                    self.convert(to_type)?,
                    self.convert(expr)?,
                    self.coord(coord)?,
                ],
                vec![],
            ),
            Node::Compound { block_items, coord } => self.call(
                "Compound",
                vec![self.opt_list(block_items)?, self.coord(coord)?],
                vec![],
            ),
            Node::CompoundLiteral {
                typ,
                init,
                storage,
                coord,
            } => self.call(
                "CompoundLiteral",
                vec![
                    self.convert(typ)?,
                    self.convert(init)?,
                    self.strs(storage),
                    self.coord(coord)?,
                ],
                vec![],
            ),
            Node::Constant { typ, value, coord } => self.call(
                "Constant",
                vec![
                    typ.into_pyobject(self.py)?.into(),
                    value.into_pyobject(self.py)?.into(),
                    self.coord(coord)?,
                ],
                vec![],
            ),
            Node::Continue { coord } => self.call("Continue", vec![self.coord(coord)?], vec![]),
            Node::Decl {
                name,
                quals,
                align,
                storage,
                funcspec,
                typ,
                init,
                bitsize,
                attributes,
                coord,
            } => {
                let kwargs = self.with_attrs(attributes, vec![])?;
                self.call(
                    "Decl",
                    vec![
                        self.opt_str(name),
                        self.strs(quals),
                        self.list(align)?,
                        self.strs(storage),
                        self.strs(funcspec),
                        self.convert(typ)?,
                        self.opt(init)?,
                        self.opt(bitsize)?,
                        self.coord(coord)?,
                    ],
                    kwargs,
                )
            }
            Node::DeclList { decls, coord } => self.call(
                "DeclList",
                vec![self.list(decls)?, self.coord(coord)?],
                vec![],
            ),
            Node::Default {
                stmts,
                attributes,
                coord,
            } => {
                let kwargs = self.with_attrs(attributes, vec![])?;
                self.call(
                    "Default",
                    vec![self.list(stmts)?, self.coord(coord)?],
                    kwargs,
                )
            }
            Node::DoWhile { cond, stmt, coord } => self.call(
                "DoWhile",
                vec![self.convert(cond)?, self.convert(stmt)?, self.coord(coord)?],
                vec![],
            ),
            Node::EllipsisParam { coord } => {
                self.call("EllipsisParam", vec![self.coord(coord)?], vec![])
            }
            Node::EmptyStatement { coord } => {
                self.call("EmptyStatement", vec![self.coord(coord)?], vec![])
            }
            Node::Enum {
                name,
                values,
                underlying_type,
                attributes,
                coord,
            } => {
                let kwargs = self.with_attrs(attributes, vec![])?;
                self.call(
                    "Enum",
                    vec![
                        self.opt_str(name),
                        self.opt(values)?,
                        self.opt(underlying_type)?,
                        self.coord(coord)?,
                    ],
                    kwargs,
                )
            }
            Node::Enumerator {
                name,
                value,
                attributes,
                coord,
            } => {
                let kwargs = self.with_attrs(attributes, vec![])?;
                self.call(
                    "Enumerator",
                    vec![
                        name.into_pyobject(self.py)?.into(),
                        self.opt(value)?,
                        self.coord(coord)?,
                    ],
                    kwargs,
                )
            }
            Node::EnumeratorList { enumerators, coord } => self.call(
                "EnumeratorList",
                vec![self.list(enumerators)?, self.coord(coord)?],
                vec![],
            ),
            Node::ExprList { exprs, coord } => self.call(
                "ExprList",
                vec![self.list(exprs)?, self.coord(coord)?],
                vec![],
            ),
            Node::FileAST { ext } => self.call("FileAST", vec![self.list(ext)?], vec![]),
            Node::For {
                init,
                cond,
                next,
                stmt,
                coord,
            } => self.call(
                "For",
                vec![
                    self.opt(init)?,
                    self.opt(cond)?,
                    self.opt(next)?,
                    self.convert(stmt)?,
                    self.coord(coord)?,
                ],
                vec![],
            ),
            Node::FuncCall { name, args, coord } => self.call(
                "FuncCall",
                vec![self.convert(name)?, self.opt(args)?, self.coord(coord)?],
                vec![],
            ),
            Node::FuncDecl {
                args,
                typ,
                attributes,
                coord,
            } => {
                let kwargs = self.with_attrs(attributes, vec![])?;
                self.call(
                    "FuncDecl",
                    vec![self.opt(args)?, self.opt(typ)?, self.coord(coord)?],
                    kwargs,
                )
            }
            Node::FuncDef {
                decl,
                param_decls,
                body,
                coord,
            } => self.call(
                "FuncDef",
                vec![
                    self.convert(decl)?,
                    self.opt_list(param_decls)?,
                    self.convert(body)?,
                    self.coord(coord)?,
                ],
                vec![],
            ),
            Node::GenericAssociation { typ, expr, coord } => self.call(
                "GenericAssociation",
                vec![self.opt(typ)?, self.convert(expr)?, self.coord(coord)?],
                vec![],
            ),
            Node::GenericSelection {
                expr,
                associations,
                coord,
            } => self.call(
                "GenericSelection",
                vec![
                    self.convert(expr)?,
                    self.list(associations)?,
                    self.coord(coord)?,
                ],
                vec![],
            ),
            Node::Goto { name, coord } => self.call(
                "Goto",
                vec![name.into_pyobject(self.py)?.into(), self.coord(coord)?],
                vec![],
            ),
            Node::ID { name, coord } => self.call(
                "ID",
                vec![name.into_pyobject(self.py)?.into(), self.coord(coord)?],
                vec![],
            ),
            Node::IdentifierType { names, coord } => self.call(
                "IdentifierType",
                vec![self.strs(names), self.coord(coord)?],
                vec![],
            ),
            Node::If {
                cond,
                iftrue,
                iffalse,
                coord,
            } => self.call(
                "If",
                vec![
                    self.convert(cond)?,
                    self.convert(iftrue)?,
                    self.opt(iffalse)?,
                    self.coord(coord)?,
                ],
                vec![],
            ),
            Node::InferredType { coord } => {
                self.call("InferredType", vec![self.coord(coord)?], vec![])
            }
            Node::InitList { exprs, coord } => self.call(
                "InitList",
                vec![self.list(exprs)?, self.coord(coord)?],
                vec![],
            ),
            Node::Label {
                name,
                stmt,
                attributes,
                coord,
            } => {
                let kwargs = self.with_attrs(attributes, vec![])?;
                self.call(
                    "Label",
                    vec![
                        name.into_pyobject(self.py)?.into(),
                        self.convert(stmt)?,
                        self.coord(coord)?,
                    ],
                    kwargs,
                )
            }
            Node::NamedInitializer { name, expr } => self.call(
                "NamedInitializer",
                vec![self.list(name)?, self.convert(expr)?],
                vec![],
            ),
            Node::ParamList { params, coord } => self.call(
                "ParamList",
                vec![self.list(params)?, self.coord(coord)?],
                vec![],
            ),
            Node::Pragma { string, coord } => {
                let s = match string {
                    pycparser_rust_core::ast::PragmaString::Text(t) => {
                        t.into_pyobject(self.py)?.into()
                    }
                    pycparser_rust_core::ast::PragmaString::Literal(n) => self.convert(n)?,
                };
                self.call("Pragma", vec![s, self.coord(coord)?], vec![])
            }
            Node::PtrDecl {
                quals,
                typ,
                attributes,
                coord,
            } => {
                let kwargs = self.with_attrs(attributes, vec![])?;
                self.call(
                    "PtrDecl",
                    vec![self.strs(quals), self.opt(typ)?, self.coord(coord)?],
                    kwargs,
                )
            }
            Node::Return { expr, coord } => {
                self.call("Return", vec![self.opt(expr)?, self.coord(coord)?], vec![])
            }
            Node::StaticAssert {
                cond,
                message,
                coord,
            } => self.call(
                "StaticAssert",
                vec![self.convert(cond)?, self.opt(message)?, self.coord(coord)?],
                vec![],
            ),
            Node::Struct {
                name,
                decls,
                attributes,
                coord,
            } => {
                let kwargs = self.with_attrs(attributes, vec![])?;
                self.call(
                    "Struct",
                    vec![
                        self.opt_str(name),
                        self.opt_list(decls)?,
                        self.coord(coord)?,
                    ],
                    kwargs,
                )
            }
            Node::StructRef {
                name,
                op,
                field,
                coord,
            } => self.call(
                "StructRef",
                vec![
                    self.convert(name)?,
                    op.into_pyobject(self.py)?.into(),
                    self.convert(field)?,
                    self.coord(coord)?,
                ],
                vec![],
            ),
            Node::Switch { cond, stmt, coord } => self.call(
                "Switch",
                vec![self.convert(cond)?, self.convert(stmt)?, self.coord(coord)?],
                vec![],
            ),
            Node::TernaryOp {
                cond,
                iftrue,
                iffalse,
                coord,
            } => self.call(
                "TernaryOp",
                vec![
                    self.convert(cond)?,
                    self.convert(iftrue)?,
                    self.convert(iffalse)?,
                    self.coord(coord)?,
                ],
                vec![],
            ),
            Node::TypeDecl {
                declname,
                quals,
                align,
                typ,
                attributes,
                coord,
            } => {
                let kwargs = self.with_attrs(attributes, vec![])?;
                self.call(
                    "TypeDecl",
                    vec![
                        self.opt_str(declname),
                        match quals {
                            None => self.py.None(),
                            Some(q) => self.strs(q),
                        },
                        match align {
                            None => self.py.None(),
                            Some(a) => self.list(a)?,
                        },
                        self.opt(typ)?,
                        self.coord(coord)?,
                    ],
                    kwargs,
                )
            }
            Node::TypeOf { op, expr, coord } => self.call(
                "TypeOf",
                vec![
                    op.into_pyobject(self.py)?.into(),
                    self.convert(expr)?,
                    self.coord(coord)?,
                ],
                vec![],
            ),
            Node::Typedef {
                name,
                quals,
                storage,
                typ,
                attributes,
                coord,
            } => {
                let kwargs = self.with_attrs(attributes, vec![])?;
                self.call(
                    "Typedef",
                    vec![
                        self.opt_str(name),
                        self.strs(quals),
                        self.strs(storage),
                        self.convert(typ)?,
                        self.coord(coord)?,
                    ],
                    kwargs,
                )
            }
            Node::Typename {
                name,
                quals,
                align,
                typ,
                attributes,
                coord,
            } => {
                let kwargs = self.with_attrs(attributes, vec![])?;
                self.call(
                    "Typename",
                    vec![
                        self.opt_str(name),
                        self.strs(quals),
                        match align {
                            None => self.py.None(),
                            Some(a) => self.list(a)?,
                        },
                        self.convert(typ)?,
                        self.coord(coord)?,
                    ],
                    kwargs,
                )
            }
            Node::UnaryOp { op, expr, coord } => self.call(
                "UnaryOp",
                vec![
                    op.into_pyobject(self.py)?.into(),
                    self.convert(expr)?,
                    self.coord(coord)?,
                ],
                vec![],
            ),
            Node::Union {
                name,
                decls,
                attributes,
                coord,
            } => {
                let kwargs = self.with_attrs(attributes, vec![])?;
                self.call(
                    "Union",
                    vec![
                        self.opt_str(name),
                        self.opt_list(decls)?,
                        self.coord(coord)?,
                    ],
                    kwargs,
                )
            }
            Node::While { cond, stmt, coord } => self.call(
                "While",
                vec![self.convert(cond)?, self.convert(stmt)?, self.coord(coord)?],
                vec![],
            ),
            Node::NeedsSwitchFix(inner) => {
                let obj = self.convert(inner)?;
                let fixed = self.transforms.getattr("fix_switch_cases")?.call1((obj,))?;
                Ok(fixed.into())
            }
        }
    }
}
