//! AST types mirroring `pycparser.c_ast` node kinds 1:1, plus lexer tokens.
//!
//! Every variant carries the same payload (and coordinates) the Python
//! constructor receives, so the binding converts field-for-field into the
//! genuine `c_ast` classes. Coordinates are `Option`: upstream leaves
//! `coord=None` on synthetic nodes (abstract `TypeDecl`s, empty declarators),
//! and the converter maps `None` back to Python `None`.
//!
//! Specifier nodes shared by comma-separated declarators (e.g. one `struct`
//! body for `} a, *b`) are shared in the core too, via [`Node::Shared`]:
//! the converter memoizes them so the Python objects preserve identity
//! (the generator groups definitions by `is`).

/// File/line/column coordinate. Columns are 1-based character columns,
/// exactly as `c_parser.Coord`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coord {
    pub file: String,
    pub line: u32,
    pub col: u32,
}

/// Coordinate argument of a parse failure, mirroring the `coord` parameter
/// of `_parse_error`: usually a full coordinate, sometimes the bare
/// filename string, `"?"`, or `None` (all render verbatim upstream).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoordArg {
    Full(Coord),
    Text(String),
    None,
}

impl CoordArg {
    pub fn file(name: impl Into<String>) -> Self {
        CoordArg::Text(name.into())
    }

    pub fn opt(coord: Option<Coord>) -> Self {
        match coord {
            Some(c) => CoordArg::Full(c),
            None => CoordArg::None,
        }
    }
}

/// Parse failure with the fully rendered message (`f"{coord}: {msg}"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub text: String,
}

impl ParseError {
    pub fn new(msg: impl Into<String>, coord: &CoordArg) -> Self {
        let msg = msg.into();
        let text = match coord {
            CoordArg::Full(c) => format!("{}:{}:{}: {}", c.file, c.line, c.col, msg),
            CoordArg::Text(t) => format!("{t}: {msg}"),
            CoordArg::None => format!("None: {msg}"),
        };
        Self { text }
    }
}

/// Lexer token kinds. [`TokenKind::name`] returns the exact upstream type
/// string (`ID`, `INT_CONST_DEC`, `PLUSPLUS`, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenKind {
    // Keywords (legacy set).
    Auto,
    Break,
    Case,
    Char,
    Const,
    Continue,
    Default,
    Do,
    Double,
    Else,
    Enum,
    Extern,
    Float,
    For,
    Goto,
    If,
    Inline,
    Int,
    Long,
    Register,
    Offsetof,
    Restrict,
    Return,
    Short,
    Signed,
    Sizeof,
    Static,
    Struct,
    Switch,
    Typedef,
    Union,
    Unsigned,
    Void,
    Volatile,
    While,
    BuiltinVaArg,
    Int128,
    Bool,
    Complex,
    Generic,
    Noreturn,
    ThreadLocal,
    StaticAssert,
    Atomic,
    Alignof,
    Alignas,
    PragmaTok,
    // C23-only spellings.
    BoolConst,
    Nullptr,
    Constexpr,
    Typeof,
    TypeofUnqual,
    BitInt,
    Decimal32,
    Decimal64,
    Decimal128,
    Imaginary,
    // Literals.
    IntConstDec,
    IntConstOct,
    IntConstHex,
    IntConstBin,
    IntConstChar,
    FloatConst,
    HexFloatConst,
    CharConst,
    WcharConst,
    U8CharConst,
    U16CharConst,
    U32CharConst,
    StringLiteral,
    WstringLiteral,
    U8StringLiteral,
    U16StringLiteral,
    U32StringLiteral,
    // Operators and punctuation.
    Ellipsis,
    LshiftEqual,
    RshiftEqual,
    PlusPlus,
    MinusMinus,
    Arrow,
    Land,
    Lor,
    Lshift,
    Rshift,
    Le,
    Ge,
    Eq,
    Ne,
    TimesEqual,
    DivEqual,
    ModEqual,
    PlusEqual,
    MinusEqual,
    AndEqual,
    OrEqual,
    XorEqual,
    Equals,
    Plus,
    Minus,
    Times,
    Divide,
    Mod,
    Or,
    And,
    Not,
    Xor,
    Lnot,
    Lt,
    Gt,
    Condop,
    Lparen,
    Rparen,
    Lbracket,
    Rbracket,
    Lbrace,
    Rbrace,
    Comma,
    Period,
    Semi,
    Colon,
    ColonColon,
    // Preprocessor-shaped tokens.
    PpHash,
    PpPragma,
    PpPragmaStr,
    // C23 attribute tokens.
    AttributeBegin,
    AttributeEnd,
    AttributeArguments,
    // Identifiers. The lexer emits only `Id`; the parser promotes to
    // `TypeId` after scope classification (mirrors `_classify_identifier`).
    Id,
    TypeId,
    // Error-rule pseudo-kinds (never surface as tokens; they carry the
    // upstream `_regex_rules` type names for diagnostics).
    UnsupportedCStyleComment,
    UnsupportedCxxStyleComment,
    BadStringLiteral,
    BadConstOct,
    UnmatchedQuote,
    BadCharConst,
}

impl TokenKind {
    pub fn name(self) -> &'static str {
        match self {
            TokenKind::Auto => "AUTO",
            TokenKind::Break => "BREAK",
            TokenKind::Case => "CASE",
            TokenKind::Char => "CHAR",
            TokenKind::Const => "CONST",
            TokenKind::Continue => "CONTINUE",
            TokenKind::Default => "DEFAULT",
            TokenKind::Do => "DO",
            TokenKind::Double => "DOUBLE",
            TokenKind::Else => "ELSE",
            TokenKind::Enum => "ENUM",
            TokenKind::Extern => "EXTERN",
            TokenKind::Float => "FLOAT",
            TokenKind::For => "FOR",
            TokenKind::Goto => "GOTO",
            TokenKind::If => "IF",
            TokenKind::Inline => "INLINE",
            TokenKind::Int => "INT",
            TokenKind::Long => "LONG",
            TokenKind::Register => "REGISTER",
            TokenKind::Offsetof => "OFFSETOF",
            TokenKind::Restrict => "RESTRICT",
            TokenKind::Return => "RETURN",
            TokenKind::Short => "SHORT",
            TokenKind::Signed => "SIGNED",
            TokenKind::Sizeof => "SIZEOF",
            TokenKind::Static => "STATIC",
            TokenKind::Struct => "STRUCT",
            TokenKind::Switch => "SWITCH",
            TokenKind::Typedef => "TYPEDEF",
            TokenKind::Union => "UNION",
            TokenKind::Unsigned => "UNSIGNED",
            TokenKind::Void => "VOID",
            TokenKind::Volatile => "VOLATILE",
            TokenKind::While => "WHILE",
            TokenKind::BuiltinVaArg => "__BUILTIN_VA_ARG",
            TokenKind::Int128 => "__INT128",
            TokenKind::Bool => "_BOOL",
            TokenKind::Complex => "_COMPLEX",
            TokenKind::Generic => "_GENERIC",
            TokenKind::Noreturn => "_NORETURN",
            TokenKind::ThreadLocal => "_THREAD_LOCAL",
            TokenKind::StaticAssert => "_STATIC_ASSERT",
            TokenKind::Atomic => "_ATOMIC",
            TokenKind::Alignof => "_ALIGNOF",
            TokenKind::Alignas => "_ALIGNAS",
            TokenKind::PragmaTok => "_PRAGMA",
            TokenKind::BoolConst => "BOOL_CONST",
            TokenKind::Nullptr => "NULLPTR",
            TokenKind::Constexpr => "CONSTEXPR",
            TokenKind::Typeof => "TYPEOF",
            TokenKind::TypeofUnqual => "TYPEOF_UNQUAL",
            TokenKind::BitInt => "_BITINT",
            TokenKind::Decimal32 => "_DECIMAL32",
            TokenKind::Decimal64 => "_DECIMAL64",
            TokenKind::Decimal128 => "_DECIMAL128",
            TokenKind::Imaginary => "_IMAGINARY",
            TokenKind::IntConstDec => "INT_CONST_DEC",
            TokenKind::IntConstOct => "INT_CONST_OCT",
            TokenKind::IntConstHex => "INT_CONST_HEX",
            TokenKind::IntConstBin => "INT_CONST_BIN",
            TokenKind::IntConstChar => "INT_CONST_CHAR",
            TokenKind::FloatConst => "FLOAT_CONST",
            TokenKind::HexFloatConst => "HEX_FLOAT_CONST",
            TokenKind::CharConst => "CHAR_CONST",
            TokenKind::WcharConst => "WCHAR_CONST",
            TokenKind::U8CharConst => "U8CHAR_CONST",
            TokenKind::U16CharConst => "U16CHAR_CONST",
            TokenKind::U32CharConst => "U32CHAR_CONST",
            TokenKind::StringLiteral => "STRING_LITERAL",
            TokenKind::WstringLiteral => "WSTRING_LITERAL",
            TokenKind::U8StringLiteral => "U8STRING_LITERAL",
            TokenKind::U16StringLiteral => "U16STRING_LITERAL",
            TokenKind::U32StringLiteral => "U32STRING_LITERAL",
            TokenKind::Ellipsis => "ELLIPSIS",
            TokenKind::LshiftEqual => "LSHIFTEQUAL",
            TokenKind::RshiftEqual => "RSHIFTEQUAL",
            TokenKind::PlusPlus => "PLUSPLUS",
            TokenKind::MinusMinus => "MINUSMINUS",
            TokenKind::Arrow => "ARROW",
            TokenKind::Land => "LAND",
            TokenKind::Lor => "LOR",
            TokenKind::Lshift => "LSHIFT",
            TokenKind::Rshift => "RSHIFT",
            TokenKind::Le => "LE",
            TokenKind::Ge => "GE",
            TokenKind::Eq => "EQ",
            TokenKind::Ne => "NE",
            TokenKind::TimesEqual => "TIMESEQUAL",
            TokenKind::DivEqual => "DIVEQUAL",
            TokenKind::ModEqual => "MODEQUAL",
            TokenKind::PlusEqual => "PLUSEQUAL",
            TokenKind::MinusEqual => "MINUSEQUAL",
            TokenKind::AndEqual => "ANDEQUAL",
            TokenKind::OrEqual => "OREQUAL",
            TokenKind::XorEqual => "XOREQUAL",
            TokenKind::Equals => "EQUALS",
            TokenKind::Plus => "PLUS",
            TokenKind::Minus => "MINUS",
            TokenKind::Times => "TIMES",
            TokenKind::Divide => "DIVIDE",
            TokenKind::Mod => "MOD",
            TokenKind::Or => "OR",
            TokenKind::And => "AND",
            TokenKind::Not => "NOT",
            TokenKind::Xor => "XOR",
            TokenKind::Lnot => "LNOT",
            TokenKind::Lt => "LT",
            TokenKind::Gt => "GT",
            TokenKind::Condop => "CONDOP",
            TokenKind::Lparen => "LPAREN",
            TokenKind::Rparen => "RPAREN",
            TokenKind::Lbracket => "LBRACKET",
            TokenKind::Rbracket => "RBRACKET",
            TokenKind::Lbrace => "LBRACE",
            TokenKind::Rbrace => "RBRACE",
            TokenKind::Comma => "COMMA",
            TokenKind::Period => "PERIOD",
            TokenKind::Semi => "SEMI",
            TokenKind::Colon => "COLON",
            TokenKind::ColonColon => "COLONCOLON",
            TokenKind::PpHash => "PPHASH",
            TokenKind::PpPragma => "PPPRAGMA",
            TokenKind::PpPragmaStr => "PPPRAGMASTR",
            TokenKind::AttributeBegin => "ATTRIBUTE_BEGIN",
            TokenKind::AttributeEnd => "ATTRIBUTE_END",
            TokenKind::AttributeArguments => "ATTRIBUTE_ARGUMENTS",
            TokenKind::Id => "ID",
            TokenKind::TypeId => "TYPEID",
            TokenKind::UnsupportedCStyleComment => "UNSUPPORTED_C_STYLE_COMMENT",
            TokenKind::UnsupportedCxxStyleComment => "UNSUPPORTED_CXX_STYLE_COMMENT",
            TokenKind::BadStringLiteral => "BAD_STRING_LITERAL",
            TokenKind::BadConstOct => "BAD_CONST_OCT",
            TokenKind::UnmatchedQuote => "UNMATCHED_QUOTE",
            TokenKind::BadCharConst => "BAD_CHAR_CONST",
        }
    }
}

/// A lexed token: kind plus source spelling and coordinates. `file_gen`
/// identifies the filename generation (bumped on `input` and `#line`
/// reassignment) so hosts can share string objects exactly like upstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawToken {
    pub kind: TokenKind,
    pub value: String,
    pub lineno: u32,
    pub column: u32,
    pub filename: String,
    pub file_gen: u64,
}

/// A recoverable lexing diagnostic (delivered to `error_func`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexError {
    pub msg: String,
    pub line: u32,
    pub column: u32,
}

/// Attribute groups attached to a node (`None` = constructor default,
/// `Some` = explicitly passed, possibly empty — mirrors upstream).
pub type OptAttrs = Option<Vec<Node>>;

/// Alignment specifiers are `Alignas` nodes.
pub type AlignList = Vec<Node>;

/// AST node. Variants mirror `c_ast` classes field-for-field; coordinates
/// are optional exactly where upstream passes `coord=None`.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Alignas {
        alignment: Box<Node>,
        coord: Option<Coord>,
    },
    ArrayDecl {
        typ: Option<Box<Node>>,
        dim: Option<Box<Node>>,
        dim_quals: Vec<String>,
        attributes: OptAttrs,
        coord: Option<Coord>,
    },
    ArrayDesignator {
        expr: Box<Node>,
        coord: Option<Coord>,
    },
    ArrayRef {
        name: Box<Node>,
        subscript: Box<Node>,
        coord: Option<Coord>,
    },
    Assignment {
        op: String,
        lvalue: Box<Node>,
        rvalue: Box<Node>,
        coord: Option<Coord>,
    },
    Attribute {
        name: String,
        namespace: Option<String>,
        arguments: Option<String>,
        coord: Option<Coord>,
    },
    AttributeDeclaration {
        attributes: Vec<Node>,
        coord: Option<Coord>,
    },
    AttributeSpecifier {
        attributes: Vec<Node>,
        coord: Option<Coord>,
    },
    AttributedStatement {
        attributes: Vec<Node>,
        stmt: Box<Node>,
        coord: Option<Coord>,
    },
    AttributedType {
        typ: Box<Node>,
        attributes: Vec<Node>,
        coord: Option<Coord>,
    },
    BinaryOp {
        op: String,
        left: Box<Node>,
        right: Box<Node>,
        coord: Option<Coord>,
    },
    BitIntType {
        width: Box<Node>,
        signedness: Option<String>,
        coord: Option<Coord>,
    },
    Break {
        coord: Option<Coord>,
    },
    Case {
        expr: Box<Node>,
        stmts: Vec<Node>,
        attributes: OptAttrs,
        coord: Option<Coord>,
    },
    Cast {
        to_type: Box<Node>,
        expr: Box<Node>,
        coord: Option<Coord>,
    },
    Compound {
        block_items: Option<Vec<Node>>,
        coord: Option<Coord>,
    },
    CompoundLiteral {
        typ: Box<Node>,
        init: Box<Node>,
        storage: Vec<String>,
        coord: Option<Coord>,
    },
    Constant {
        typ: String,
        value: String,
        coord: Option<Coord>,
    },
    Continue {
        coord: Option<Coord>,
    },
    Decl {
        name: Option<String>,
        quals: Vec<String>,
        align: Vec<Node>,
        storage: Vec<String>,
        funcspec: Vec<String>,
        typ: Box<Node>,
        init: Option<Box<Node>>,
        bitsize: Option<Box<Node>>,
        attributes: OptAttrs,
        coord: Option<Coord>,
    },
    DeclList {
        decls: Vec<Node>,
        coord: Option<Coord>,
    },
    Default {
        stmts: Vec<Node>,
        attributes: OptAttrs,
        coord: Option<Coord>,
    },
    DoWhile {
        cond: Box<Node>,
        stmt: Box<Node>,
        coord: Option<Coord>,
    },
    EllipsisParam {
        coord: Option<Coord>,
    },
    EmptyStatement {
        coord: Option<Coord>,
    },
    Enum {
        name: Option<String>,
        values: Option<Box<Node>>,
        underlying_type: Option<Box<Node>>,
        attributes: OptAttrs,
        coord: Option<Coord>,
    },
    Enumerator {
        name: String,
        value: Option<Box<Node>>,
        attributes: OptAttrs,
        coord: Option<Coord>,
    },
    EnumeratorList {
        enumerators: Vec<Node>,
        coord: Option<Coord>,
    },
    ExprList {
        exprs: Vec<Node>,
        coord: Option<Coord>,
    },
    FileAST {
        ext: Vec<Node>,
    },
    For {
        init: Option<Box<Node>>,
        cond: Option<Box<Node>>,
        next: Option<Box<Node>>,
        stmt: Box<Node>,
        coord: Option<Coord>,
    },
    FuncCall {
        name: Box<Node>,
        args: Option<Box<Node>>,
        coord: Option<Coord>,
    },
    FuncDecl {
        args: Option<Box<Node>>,
        typ: Option<Box<Node>>,
        attributes: OptAttrs,
        coord: Option<Coord>,
    },
    FuncDef {
        decl: Box<Node>,
        param_decls: Option<Vec<Node>>,
        body: Box<Node>,
        coord: Option<Coord>,
    },
    GenericAssociation {
        typ: Option<Box<Node>>,
        expr: Box<Node>,
        coord: Option<Coord>,
    },
    GenericSelection {
        expr: Box<Node>,
        associations: Vec<Node>,
        coord: Option<Coord>,
    },
    Goto {
        name: String,
        coord: Option<Coord>,
    },
    ID {
        name: String,
        coord: Option<Coord>,
    },
    IdentifierType {
        names: Vec<String>,
        coord: Option<Coord>,
    },
    If {
        cond: Box<Node>,
        iftrue: Box<Node>,
        iffalse: Option<Box<Node>>,
        coord: Option<Coord>,
    },
    InferredType {
        coord: Option<Coord>,
    },
    InitList {
        exprs: Vec<Node>,
        coord: Option<Coord>,
    },
    Label {
        name: String,
        stmt: Box<Node>,
        attributes: OptAttrs,
        coord: Option<Coord>,
    },
    /// `name` is the designator list; upstream leaves `coord=None`.
    NamedInitializer {
        name: Vec<Node>,
        expr: Box<Node>,
    },
    ParamList {
        params: Vec<Node>,
        coord: Option<Coord>,
    },
    Pragma {
        string: PragmaString,
        coord: Option<Coord>,
    },
    PtrDecl {
        quals: Vec<String>,
        typ: Option<Box<Node>>,
        attributes: OptAttrs,
        coord: Option<Coord>,
    },
    Return {
        expr: Option<Box<Node>>,
        coord: Option<Coord>,
    },
    StaticAssert {
        cond: Box<Node>,
        message: Option<Box<Node>>,
        coord: Option<Coord>,
    },
    Struct {
        name: Option<String>,
        decls: Option<Vec<Node>>,
        attributes: OptAttrs,
        coord: Option<Coord>,
    },
    StructRef {
        name: Box<Node>,
        op: String,
        field: Box<Node>,
        coord: Option<Coord>,
    },
    Switch {
        cond: Box<Node>,
        stmt: Box<Node>,
        coord: Option<Coord>,
    },
    TernaryOp {
        cond: Box<Node>,
        iftrue: Box<Node>,
        iffalse: Box<Node>,
        coord: Option<Coord>,
    },
    TypeDecl {
        declname: Option<String>,
        quals: Option<Vec<String>>,
        align: Option<Vec<Node>>,
        typ: Option<Box<Node>>,
        attributes: OptAttrs,
        coord: Option<Coord>,
    },
    TypeOf {
        op: String,
        expr: Box<Node>,
        coord: Option<Coord>,
    },
    Typedef {
        name: Option<String>,
        quals: Vec<String>,
        storage: Vec<String>,
        typ: Box<Node>,
        attributes: OptAttrs,
        coord: Option<Coord>,
    },
    Typename {
        name: Option<String>,
        quals: Vec<String>,
        align: Option<Vec<Node>>,
        typ: Box<Node>,
        attributes: OptAttrs,
        coord: Option<Coord>,
    },
    UnaryOp {
        op: String,
        expr: Box<Node>,
        coord: Option<Coord>,
    },
    Union {
        name: Option<String>,
        decls: Option<Vec<Node>>,
        attributes: OptAttrs,
        coord: Option<Coord>,
    },
    While {
        cond: Box<Node>,
        stmt: Box<Node>,
        coord: Option<Coord>,
    },
    /// Internal marker: the Python converter applies `fix_switch_cases`
    /// to the wrapped node (mirrors the `_parse_selection_statement`
    /// call site). Never crosses the binding boundary.
    NeedsSwitchFix(Box<Node>),
    /// Shared specifier subtree (one `Rc` per spec element per declaration
    /// batch). Reads resolve transparently; the converter memoizes by `Rc`
    /// address so converted objects preserve identity. Mutations never go
    /// through a `Shared` node (shared subtrees are never mutated in place;
    /// surgery clones before writing).
    Shared(std::rc::Rc<Node>),
}

/// A `#pragma` payload: plain text or a `_Pragma` string constant node.
#[derive(Debug, Clone, PartialEq)]
pub enum PragmaString {
    Text(String),
    Literal(Box<Node>),
}

impl Node {
    /// Coordinate of this node, when it has one.
    pub fn coord(&self) -> Option<&Coord> {
        match self {
            Node::FileAST { .. } | Node::NamedInitializer { .. } => None,
            Node::NeedsSwitchFix(inner) => inner.coord(),
            Node::Shared(rc) => rc.coord(),
            Node::Alignas { coord, .. }
            | Node::ArrayDecl { coord, .. }
            | Node::ArrayDesignator { coord, .. }
            | Node::ArrayRef { coord, .. }
            | Node::Assignment { coord, .. }
            | Node::Attribute { coord, .. }
            | Node::AttributeDeclaration { coord, .. }
            | Node::AttributeSpecifier { coord, .. }
            | Node::AttributedStatement { coord, .. }
            | Node::AttributedType { coord, .. }
            | Node::BinaryOp { coord, .. }
            | Node::BitIntType { coord, .. }
            | Node::Break { coord }
            | Node::Case { coord, .. }
            | Node::Cast { coord, .. }
            | Node::Compound { coord, .. }
            | Node::CompoundLiteral { coord, .. }
            | Node::Constant { coord, .. }
            | Node::Continue { coord }
            | Node::Decl { coord, .. }
            | Node::DeclList { coord, .. }
            | Node::Default { coord, .. }
            | Node::DoWhile { coord, .. }
            | Node::EllipsisParam { coord }
            | Node::EmptyStatement { coord }
            | Node::Enum { coord, .. }
            | Node::Enumerator { coord, .. }
            | Node::EnumeratorList { coord, .. }
            | Node::ExprList { coord, .. }
            | Node::For { coord, .. }
            | Node::FuncCall { coord, .. }
            | Node::FuncDecl { coord, .. }
            | Node::FuncDef { coord, .. }
            | Node::GenericAssociation { coord, .. }
            | Node::GenericSelection { coord, .. }
            | Node::Goto { coord, .. }
            | Node::ID { coord, .. }
            | Node::IdentifierType { coord, .. }
            | Node::If { coord, .. }
            | Node::InferredType { coord }
            | Node::InitList { coord, .. }
            | Node::Label { coord, .. }
            | Node::ParamList { coord, .. }
            | Node::Pragma { coord, .. }
            | Node::PtrDecl { coord, .. }
            | Node::Return { coord, .. }
            | Node::StaticAssert { coord, .. }
            | Node::Struct { coord, .. }
            | Node::StructRef { coord, .. }
            | Node::Switch { coord, .. }
            | Node::TernaryOp { coord, .. }
            | Node::TypeDecl { coord, .. }
            | Node::TypeOf { coord, .. }
            | Node::Typedef { coord, .. }
            | Node::Typename { coord, .. }
            | Node::UnaryOp { coord, .. }
            | Node::Union { coord, .. }
            | Node::While { coord, .. } => coord.as_ref(),
        }
    }
}

impl Node {
    /// Resolve through `Shared` proxies (transparent for reads).
    pub fn resolve(&self) -> &Node {
        let mut cur = self;
        while let Node::Shared(rc) = cur {
            cur = rc;
        }
        cur
    }
}
