//! REPL app — an interactive shell backed by a small built-in interpreter.
//!
//! Not a full language VM; a compact, no_std tree-walker for a small scripting
//! language with familiar syntax:
//!   - values: int, float, bool, str, list, dict, None
//!   - operators: + - * / // % **, comparisons, `and`/`or`/`not`, indexing
//!   - statements: assignment (incl. `a[i] = v`), `if`/`elif`/`else`, `while`,
//!     `for x in ...`, `def`/`return`, `break`/`continue`/`pass`
//!   - builtins: print, len, range, int, float, str, bool, list, abs
//! Blocks are indentation-based; the REPL collects an indented block until a
//! blank line, like a typical interactive prompt. `help` lists the syntax.
//!
//! Two embedded-safety limits keep a bad script from bricking the UI: a global
//! step budget (so `while True:` can't freeze the cooperative main loop) and a
//! call-depth cap (so runaway recursion can't blow the small device stack).
//!
//! `eval_source`/`eval_line` are pure and are exercised from the serial
//! self-test; the rest is the on-device REPL UI (keyboard in, scrollback out).

use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use embedded_graphics::{pixelcolor::Rgb565, prelude::*};

use crate::hal::keymap;
use crate::theme;

// Guards against a script hanging or crashing the firmware.
const STEP_LIMIT: u64 = 200_000; // statement/iteration budget before we bail
const DEPTH_LIMIT: u32 = 48; // max nested function calls
const RANGE_MAX: i64 = 100_000; // largest list range() will materialise

// ============================ values ============================

/// A user-defined function.
struct FuncDef {
    name: String,
    params: Vec<String>,
    body: Vec<Stmt>,
}

/// A runtime value.
#[derive(Clone)]
enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    List(Vec<Value>),
    Dict(Vec<(Value, Value)>),
    Func(Rc<FuncDef>),
    None,
}

impl Value {
    /// `str()` form — what `print` shows.
    fn display(&self) -> String {
        match self {
            Value::Str(s) => s.clone(),
            other => other.repr(),
        }
    }
    /// `repr()` form — what the REPL echoes (strings quoted, containers nested).
    fn repr(&self) -> String {
        match self {
            Value::Int(n) => n.to_string(),
            Value::Float(f) => fmt_float(*f),
            Value::Bool(b) => if *b { "True".into() } else { "False".into() },
            Value::Str(s) => format!("'{}'", s),
            Value::None => "None".into(),
            Value::Func(f) => format!("<function {}>", f.name),
            Value::List(items) => {
                let mut s = String::from("[");
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&v.repr());
                }
                s.push(']');
                s
            }
            Value::Dict(pairs) => {
                let mut s = String::from("{");
                for (i, (k, v)) in pairs.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&k.repr());
                    s.push_str(": ");
                    s.push_str(&v.repr());
                }
                s.push('}');
                s
            }
        }
    }
    fn truthy(&self) -> bool {
        match self {
            Value::None => false,
            Value::Bool(b) => *b,
            Value::Int(n) => *n != 0,
            Value::Float(f) => *f != 0.0,
            Value::Str(s) => !s.is_empty(),
            Value::List(v) => !v.is_empty(),
            Value::Dict(v) => !v.is_empty(),
            Value::Func(_) => true,
        }
    }
    fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Int(n) => Some(*n as f64),
            Value::Float(f) => Some(*f),
            Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            _ => None,
        }
    }
    fn type_name(&self) -> &'static str {
        match self {
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Bool(_) => "bool",
            Value::Str(_) => "str",
            Value::List(_) => "list",
            Value::Dict(_) => "dict",
            Value::Func(_) => "function",
            Value::None => "NoneType",
        }
    }
}

fn fmt_float(f: f64) -> String {
    if f.is_finite() && f == libm::trunc(f) && (if f < 0.0 { -f } else { f }) < 1e15 {
        format!("{}.0", f as i64)
    } else {
        format!("{}", f)
    }
}

/// Value equality (used by `==`, `in`-style dict lookups and list membership).
fn value_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::None, Value::None) => true,
        (Value::List(x), Value::List(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| value_eq(p, q)),
        _ => match (a.as_f64(), b.as_f64()) {
            (Some(x), Some(y)) => x == y,
            _ => false,
        },
    }
}

// ============================ tokens ============================

#[derive(Clone, PartialEq)]
enum Tok {
    Num(f64, bool), // value, is_int
    Str(String),
    Name(String),
    Plus,
    Minus,
    Star,
    Slash,
    DSlash,
    Percent,
    DStar,
    Assign,
    AddAssign,
    SubAssign,
    EqEq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
    LParen,
    RParen,
    LBrack,
    RBrack,
    LBrace,
    RBrace,
    Comma,
    Colon,
}

/// Tokenise one source line (comments after `#` are dropped).
fn lex_line(s: &str) -> Result<Vec<Tok>, String> {
    let b = s.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i];
        if c == b' ' || c == b'\t' || c == b'\r' {
            i += 1;
            continue;
        }
        if c == b'#' {
            break; // rest of line is a comment
        }
        if c.is_ascii_digit() || (c == b'.' && i + 1 < b.len() && b[i + 1].is_ascii_digit()) {
            let start = i;
            let mut is_int = true;
            while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
                if b[i] == b'.' {
                    is_int = false;
                }
                i += 1;
            }
            let txt = &s[start..i];
            if is_int {
                match txt.parse::<i64>() {
                    Ok(n) => out.push(Tok::Num(n as f64, true)),
                    Err(_) => return Err("SyntaxError: bad number".into()),
                }
            } else {
                match txt.parse::<f64>() {
                    Ok(f) => out.push(Tok::Num(f, false)),
                    Err(_) => return Err("SyntaxError: bad number".into()),
                }
            }
            continue;
        }
        if c == b'"' || c == b'\'' {
            let q = c;
            i += 1;
            let start = i;
            while i < b.len() && b[i] != q {
                i += 1;
            }
            if i >= b.len() {
                return Err("SyntaxError: unterminated string".into());
            }
            out.push(Tok::Str(s[start..i].to_string()));
            i += 1;
            continue;
        }
        if c == b'_' || c.is_ascii_alphabetic() {
            let start = i;
            while i < b.len() && (b[i] == b'_' || b[i].is_ascii_alphanumeric()) {
                i += 1;
            }
            out.push(Tok::Name(s[start..i].to_string()));
            continue;
        }
        // two-character operators first
        let two = if i + 1 < b.len() { Some((c, b[i + 1])) } else { None };
        match two {
            Some((b'*', b'*')) => {
                out.push(Tok::DStar);
                i += 2;
                continue;
            }
            Some((b'/', b'/')) => {
                out.push(Tok::DSlash);
                i += 2;
                continue;
            }
            Some((b'=', b'=')) => {
                out.push(Tok::EqEq);
                i += 2;
                continue;
            }
            Some((b'!', b'=')) => {
                out.push(Tok::NotEq);
                i += 2;
                continue;
            }
            Some((b'<', b'=')) => {
                out.push(Tok::Le);
                i += 2;
                continue;
            }
            Some((b'>', b'=')) => {
                out.push(Tok::Ge);
                i += 2;
                continue;
            }
            Some((b'+', b'=')) => {
                out.push(Tok::AddAssign);
                i += 2;
                continue;
            }
            Some((b'-', b'=')) => {
                out.push(Tok::SubAssign);
                i += 2;
                continue;
            }
            _ => {}
        }
        let t = match c {
            b'+' => Tok::Plus,
            b'-' => Tok::Minus,
            b'*' => Tok::Star,
            b'/' => Tok::Slash,
            b'%' => Tok::Percent,
            b'=' => Tok::Assign,
            b'<' => Tok::Lt,
            b'>' => Tok::Gt,
            b'(' => Tok::LParen,
            b')' => Tok::RParen,
            b'[' => Tok::LBrack,
            b']' => Tok::RBrack,
            b'{' => Tok::LBrace,
            b'}' => Tok::RBrace,
            b',' => Tok::Comma,
            b':' => Tok::Colon,
            _ => return Err("SyntaxError: bad character".into()),
        };
        out.push(t);
        i += 1;
    }
    Ok(out)
}

// ============================== AST ==============================

#[derive(Clone, Copy, PartialEq)]
enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    FloorDiv,
    Mod,
    Pow,
}

#[derive(Clone, Copy, PartialEq)]
enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

enum Expr {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    NoneLit,
    Var(String),
    List(Vec<Expr>),
    Dict(Vec<(Expr, Expr)>),
    Neg(Box<Expr>),
    Not(Box<Expr>),
    Bin(BinOp, Box<Expr>, Box<Expr>),
    Cmp(CmpOp, Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Index(Box<Expr>, Box<Expr>),
    Call(Box<Expr>, Vec<Expr>),
}

enum Stmt {
    Expr(Expr),
    Assign(String, Expr),
    AugAssign(String, BinOp, Expr),
    SetItem(String, Vec<Expr>, Expr), // base[idx][idx] = value
    If(Expr, Vec<Stmt>, Vec<Stmt>),
    While(Expr, Vec<Stmt>),
    For(String, Expr, Vec<Stmt>),
    Def(Rc<FuncDef>),
    Return(Option<Expr>),
    Break,
    Continue,
    Pass,
}

// ============================= parser =============================

struct Line {
    indent: usize,
    toks: Vec<Tok>,
}

/// Parse a (possibly multi-line) source block into statements.
fn parse(src: &str) -> Result<Vec<Stmt>, String> {
    let mut lines: Vec<Line> = Vec::new();
    for raw in src.split('\n') {
        let indent = raw.len() - raw.trim_start().len();
        let toks = lex_line(raw)?;
        if toks.is_empty() {
            continue; // blank or comment-only line
        }
        lines.push(Line { indent, toks });
    }
    if lines.is_empty() {
        return Ok(Vec::new());
    }
    let mut i = 0;
    let base = lines[0].indent;
    let stmts = parse_suite(&lines, &mut i, base)?;
    if i != lines.len() {
        return Err("IndentationError".into());
    }
    Ok(stmts)
}

fn parse_suite(lines: &[Line], i: &mut usize, indent: usize) -> Result<Vec<Stmt>, String> {
    let mut out = Vec::new();
    while *i < lines.len() {
        let li = lines[*i].indent;
        if li < indent {
            break;
        }
        if li > indent {
            return Err("IndentationError: unexpected indent".into());
        }
        out.push(parse_stmt(lines, i, indent)?);
    }
    Ok(out)
}

/// The keyword starting a line, if it is a Name token.
fn kw(line: &Line) -> Option<&str> {
    match line.toks.first() {
        Some(Tok::Name(n)) => Some(n.as_str()),
        _ => None,
    }
}

fn parse_block_after(lines: &[Line], i: &mut usize, header_indent: usize) -> Result<Vec<Stmt>, String> {
    *i += 1; // consume the header line
    if *i >= lines.len() || lines[*i].indent <= header_indent {
        return Err("IndentationError: expected an indented block".into());
    }
    let body_indent = lines[*i].indent;
    parse_suite(lines, i, body_indent)
}

fn parse_stmt(lines: &[Line], i: &mut usize, indent: usize) -> Result<Stmt, String> {
    match kw(&lines[*i]) {
        Some("if") => parse_if(lines, i, indent),
        Some("while") => {
            let toks = &lines[*i].toks;
            let cond = parse_header_expr(&toks[1..])?;
            let body = parse_block_after(lines, i, indent)?;
            Ok(Stmt::While(cond, body))
        }
        Some("for") => {
            let toks = &lines[*i].toks;
            // for NAME in EXPR :
            let var = match toks.get(1) {
                Some(Tok::Name(n)) => n.clone(),
                _ => return Err("SyntaxError: expected name after 'for'".into()),
            };
            if toks.get(2) != Some(&Tok::Name("in".into())) {
                return Err("SyntaxError: expected 'in'".into());
            }
            let iter = parse_header_expr(&toks[3..])?;
            let body = parse_block_after(lines, i, indent)?;
            Ok(Stmt::For(var, iter, body))
        }
        Some("def") => {
            let toks = &lines[*i].toks;
            let name = match toks.get(1) {
                Some(Tok::Name(n)) => n.clone(),
                _ => return Err("SyntaxError: expected function name".into()),
            };
            if toks.get(2) != Some(&Tok::LParen) {
                return Err("SyntaxError: expected '('".into());
            }
            // params up to ')'
            let mut params = Vec::new();
            let mut j = 3;
            while j < toks.len() && toks[j] != Tok::RParen {
                match &toks[j] {
                    Tok::Name(n) => params.push(n.clone()),
                    Tok::Comma => {}
                    _ => return Err("SyntaxError: bad parameter".into()),
                }
                j += 1;
            }
            // expect ')' ':'
            if toks.get(j) != Some(&Tok::RParen) || toks.get(j + 1) != Some(&Tok::Colon) || j + 2 != toks.len() {
                return Err("SyntaxError: bad def header".into());
            }
            let body = parse_block_after(lines, i, indent)?;
            Ok(Stmt::Def(Rc::new(FuncDef { name, params, body })))
        }
        Some("return") => {
            let toks = lines[*i].toks.clone();
            *i += 1;
            if toks.len() == 1 {
                Ok(Stmt::Return(None))
            } else {
                Ok(Stmt::Return(Some(parse_expr_all(&toks[1..])?)))
            }
        }
        Some("break") => {
            simple_keyword_only(&lines[*i], "break")?;
            *i += 1;
            Ok(Stmt::Break)
        }
        Some("continue") => {
            simple_keyword_only(&lines[*i], "continue")?;
            *i += 1;
            Ok(Stmt::Continue)
        }
        Some("pass") => {
            simple_keyword_only(&lines[*i], "pass")?;
            *i += 1;
            Ok(Stmt::Pass)
        }
        _ => {
            let toks = lines[*i].toks.clone();
            *i += 1;
            parse_simple(&toks)
        }
    }
}

fn simple_keyword_only(line: &Line, name: &str) -> Result<(), String> {
    if line.toks.len() == 1 {
        Ok(())
    } else {
        Err(format!("SyntaxError: '{}' takes no arguments", name))
    }
}

fn parse_if(lines: &[Line], i: &mut usize, indent: usize) -> Result<Stmt, String> {
    let toks = &lines[*i].toks;
    let cond = parse_header_expr(&toks[1..])?;
    let body = parse_block_after(lines, i, indent)?;
    // optional elif / else at the same indent
    let mut orelse: Vec<Stmt> = Vec::new();
    if *i < lines.len() && lines[*i].indent == indent {
        match kw(&lines[*i]) {
            Some("elif") => {
                // recurse: treat the elif as a nested if in the else branch
                orelse.push(parse_if(lines, i, indent)?);
            }
            Some("else") => {
                if lines[*i].toks.len() != 2 || lines[*i].toks.get(1) != Some(&Tok::Colon) {
                    return Err("SyntaxError: bad else".into());
                }
                orelse = parse_block_after(lines, i, indent)?;
            }
            _ => {}
        }
    }
    Ok(Stmt::If(cond, body, orelse))
}

// `elif EXPR :` reuses parse_if but with the leading keyword being "elif".
// parse_if reads toks[1..] as the condition up to ':', which works for both.

/// Parse a compound-statement header expression: tokens ending in a trailing ':'.
fn parse_header_expr(toks: &[Tok]) -> Result<Expr, String> {
    if toks.last() != Some(&Tok::Colon) {
        return Err("SyntaxError: expected ':'".into());
    }
    parse_expr_all(&toks[..toks.len() - 1])
}

/// Parse a simple (single-line) statement: assignment, aug-assignment,
/// item assignment, or a bare expression.
fn parse_simple(toks: &[Tok]) -> Result<Stmt, String> {
    if toks.is_empty() {
        return Ok(Stmt::Pass);
    }
    // find a top-level '=', '+=' or '-=' (bracket depth 0)
    let mut depth = 0i32;
    let mut assign_at: Option<usize> = None;
    let mut aug: Option<BinOp> = None;
    for (k, t) in toks.iter().enumerate() {
        match t {
            Tok::LParen | Tok::LBrack | Tok::LBrace => depth += 1,
            Tok::RParen | Tok::RBrack | Tok::RBrace => depth -= 1,
            Tok::Assign if depth == 0 => {
                assign_at = Some(k);
                break;
            }
            Tok::AddAssign if depth == 0 => {
                assign_at = Some(k);
                aug = Some(BinOp::Add);
                break;
            }
            Tok::SubAssign if depth == 0 => {
                assign_at = Some(k);
                aug = Some(BinOp::Sub);
                break;
            }
            _ => {}
        }
    }
    let Some(eq) = assign_at else {
        return Ok(Stmt::Expr(parse_expr_all(toks)?));
    };
    let target = &toks[..eq];
    let value = parse_expr_all(&toks[eq + 1..])?;
    // target: NAME  or  NAME[expr][expr]...
    let Some(Tok::Name(base)) = target.first() else {
        return Err("SyntaxError: cannot assign to this target".into());
    };
    if target.len() == 1 {
        return Ok(match aug {
            Some(op) => Stmt::AugAssign(base.clone(), op, value),
            None => Stmt::Assign(base.clone(), value),
        });
    }
    // subscript target: collect each [ ... ] index expression
    let idxs = parse_subscript_chain(&target[1..])?;
    if aug.is_some() {
        return Err("SyntaxError: augmented item assignment not supported".into());
    }
    Ok(Stmt::SetItem(base.clone(), idxs, value))
}

/// Split `[a][b][c]` token runs into the inner index expressions.
fn parse_subscript_chain(mut toks: &[Tok]) -> Result<Vec<Expr>, String> {
    let mut out = Vec::new();
    while !toks.is_empty() {
        if toks[0] != Tok::LBrack {
            return Err("SyntaxError: bad assignment target".into());
        }
        // find matching ']'
        let mut depth = 0i32;
        let mut end = None;
        for (k, t) in toks.iter().enumerate() {
            match t {
                Tok::LBrack => depth += 1,
                Tok::RBrack => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(k);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(e) = end else { return Err("SyntaxError: missing ']'".into()) };
        out.push(parse_expr_all(&toks[1..e])?);
        toks = &toks[e + 1..];
    }
    Ok(out)
}

/// Parse a full expression from a token slice, erroring on trailing tokens.
fn parse_expr_all(toks: &[Tok]) -> Result<Expr, String> {
    let mut p = ExprParser { t: toks, i: 0 };
    let e = p.expr()?;
    if p.i != toks.len() {
        return Err("SyntaxError".into());
    }
    Ok(e)
}

struct ExprParser<'a> {
    t: &'a [Tok],
    i: usize,
}
impl<'a> ExprParser<'a> {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }
    fn is_name(&self, w: &str) -> bool {
        matches!(self.peek(), Some(Tok::Name(n)) if n == w)
    }
    fn expr(&mut self) -> Result<Expr, String> {
        self.or_expr()
    }
    fn or_expr(&mut self) -> Result<Expr, String> {
        let mut l = self.and_expr()?;
        while self.is_name("or") {
            self.i += 1;
            let r = self.and_expr()?;
            l = Expr::Or(Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn and_expr(&mut self) -> Result<Expr, String> {
        let mut l = self.not_expr()?;
        while self.is_name("and") {
            self.i += 1;
            let r = self.not_expr()?;
            l = Expr::And(Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn not_expr(&mut self) -> Result<Expr, String> {
        if self.is_name("not") {
            self.i += 1;
            return Ok(Expr::Not(Box::new(self.not_expr()?)));
        }
        self.cmp()
    }
    fn cmp(&mut self) -> Result<Expr, String> {
        let l = self.add()?;
        let op = match self.peek() {
            Some(Tok::EqEq) => CmpOp::Eq,
            Some(Tok::NotEq) => CmpOp::Ne,
            Some(Tok::Lt) => CmpOp::Lt,
            Some(Tok::Le) => CmpOp::Le,
            Some(Tok::Gt) => CmpOp::Gt,
            Some(Tok::Ge) => CmpOp::Ge,
            _ => return Ok(l),
        };
        self.i += 1;
        let r = self.add()?;
        Ok(Expr::Cmp(op, Box::new(l), Box::new(r)))
    }
    fn add(&mut self) -> Result<Expr, String> {
        let mut l = self.mul()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Plus) => BinOp::Add,
                Some(Tok::Minus) => BinOp::Sub,
                _ => break,
            };
            self.i += 1;
            let r = self.mul()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn mul(&mut self) -> Result<Expr, String> {
        let mut l = self.unary()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Star) => BinOp::Mul,
                Some(Tok::Slash) => BinOp::Div,
                Some(Tok::DSlash) => BinOp::FloorDiv,
                Some(Tok::Percent) => BinOp::Mod,
                _ => break,
            };
            self.i += 1;
            let r = self.unary()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn unary(&mut self) -> Result<Expr, String> {
        if let Some(Tok::Minus) = self.peek() {
            self.i += 1;
            return Ok(Expr::Neg(Box::new(self.unary()?)));
        }
        self.power()
    }
    fn power(&mut self) -> Result<Expr, String> {
        let base = self.postfix()?;
        if let Some(Tok::DStar) = self.peek() {
            self.i += 1;
            let exp = self.unary()?; // right-associative
            return Ok(Expr::Bin(BinOp::Pow, Box::new(base), Box::new(exp)));
        }
        Ok(base)
    }
    fn postfix(&mut self) -> Result<Expr, String> {
        let mut e = self.atom()?;
        loop {
            match self.peek() {
                Some(Tok::LBrack) => {
                    self.i += 1;
                    let idx = self.expr()?;
                    self.expect(Tok::RBrack, "]")?;
                    e = Expr::Index(Box::new(e), Box::new(idx));
                }
                Some(Tok::LParen) => {
                    self.i += 1;
                    let args = self.arg_list(Tok::RParen)?;
                    e = Expr::Call(Box::new(e), args);
                }
                _ => break,
            }
        }
        Ok(e)
    }
    fn arg_list(&mut self, close: Tok) -> Result<Vec<Expr>, String> {
        let mut args = Vec::new();
        if self.peek() == Some(&close) {
            self.i += 1;
            return Ok(args);
        }
        loop {
            args.push(self.expr()?);
            match self.peek() {
                Some(Tok::Comma) => {
                    self.i += 1;
                    if self.peek() == Some(&close) {
                        self.i += 1; // trailing comma
                        break;
                    }
                }
                _ => {
                    self.expect(close, ") or ]")?;
                    break;
                }
            }
        }
        Ok(args)
    }
    fn atom(&mut self) -> Result<Expr, String> {
        match self.peek() {
            Some(Tok::Num(v, is_int)) => {
                let e = if *is_int { Expr::Int(*v as i64) } else { Expr::Float(*v) };
                self.i += 1;
                Ok(e)
            }
            Some(Tok::Str(s)) => {
                let e = Expr::Str(s.clone());
                self.i += 1;
                Ok(e)
            }
            Some(Tok::Name(n)) => {
                let e = match n.as_str() {
                    "True" => Expr::Bool(true),
                    "False" => Expr::Bool(false),
                    "None" => Expr::NoneLit,
                    _ => Expr::Var(n.clone()),
                };
                self.i += 1;
                Ok(e)
            }
            Some(Tok::LParen) => {
                self.i += 1;
                let e = self.expr()?;
                self.expect(Tok::RParen, ")")?;
                Ok(e)
            }
            Some(Tok::LBrack) => {
                self.i += 1;
                let items = self.arg_list(Tok::RBrack)?;
                Ok(Expr::List(items))
            }
            Some(Tok::LBrace) => {
                self.i += 1;
                self.dict_lit()
            }
            _ => Err("SyntaxError: expected a value".into()),
        }
    }
    fn dict_lit(&mut self) -> Result<Expr, String> {
        let mut pairs = Vec::new();
        if self.peek() == Some(&Tok::RBrace) {
            self.i += 1;
            return Ok(Expr::Dict(pairs));
        }
        loop {
            let k = self.expr()?;
            self.expect(Tok::Colon, ":")?;
            let v = self.expr()?;
            pairs.push((k, v));
            match self.peek() {
                Some(Tok::Comma) => {
                    self.i += 1;
                    if self.peek() == Some(&Tok::RBrace) {
                        self.i += 1;
                        break;
                    }
                }
                _ => {
                    self.expect(Tok::RBrace, "}")?;
                    break;
                }
            }
        }
        Ok(Expr::Dict(pairs))
    }
    fn expect(&mut self, t: Tok, what: &str) -> Result<(), String> {
        if self.peek() == Some(&t) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("SyntaxError: expected '{}'", what))
        }
    }
}

// =========================== interpreter ===========================

/// A name -> value scope.
pub struct Env {
    vars: Vec<(String, Value)>,
}
impl Env {
    pub fn new() -> Self {
        Env { vars: Vec::new() }
    }
    fn get(&self, name: &str) -> Option<Value> {
        self.vars.iter().rev().find(|(n, _)| n == name).map(|(_, v)| v.clone())
    }
    fn set(&mut self, name: &str, v: Value) {
        if let Some(slot) = self.vars.iter_mut().find(|(n, _)| n == name) {
            slot.1 = v;
        } else {
            self.vars.push((name.to_string(), v));
        }
    }
}

enum Flow {
    Normal,
    Return(Value),
    Break,
    Continue,
}

struct Vm<'g> {
    global: &'g mut Env,
    locals: Vec<Env>, // function call frames (top = current)
    steps: u64,
    depth: u32,
    out: Vec<String>,
}

impl<'g> Vm<'g> {
    fn tick(&mut self) -> Result<(), String> {
        self.steps += 1;
        if self.steps > STEP_LIMIT {
            Err("RuntimeError: step limit (possible infinite loop)".into())
        } else {
            Ok(())
        }
    }

    fn get(&self, name: &str) -> Option<Value> {
        if let Some(f) = self.locals.last() {
            if let Some(v) = f.get(name) {
                return Some(v);
            }
        }
        self.global.get(name)
    }

    fn set(&mut self, name: &str, v: Value) {
        if let Some(f) = self.locals.last_mut() {
            f.set(name, v);
        } else {
            self.global.set(name, v);
        }
    }

    /// `&mut` to a variable's slot, for in-place item assignment.
    fn slot_mut(&mut self, name: &str) -> Option<&mut Value> {
        let in_local = self.locals.last().map_or(false, |f| f.vars.iter().any(|(n, _)| n == name));
        if in_local {
            let f = self.locals.last_mut().unwrap();
            return f.vars.iter_mut().find(|(n, _)| n == name).map(|s| &mut s.1);
        }
        self.global.vars.iter_mut().find(|(n, _)| n == name).map(|s| &mut s.1)
    }

    fn exec_block(&mut self, body: &[Stmt]) -> Result<Flow, String> {
        for s in body {
            match self.exec(s)? {
                Flow::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal)
    }

    fn exec(&mut self, s: &Stmt) -> Result<Flow, String> {
        self.tick()?;
        match s {
            Stmt::Pass => Ok(Flow::Normal),
            Stmt::Expr(e) => {
                self.eval(e)?;
                Ok(Flow::Normal)
            }
            Stmt::Assign(name, e) => {
                let v = self.eval(e)?;
                self.set(name, v);
                Ok(Flow::Normal)
            }
            Stmt::AugAssign(name, op, e) => {
                let cur = self.get(name).ok_or_else(|| format!("NameError: {}", name))?;
                let rhs = self.eval(e)?;
                let v = binop(*op, &cur, &rhs)?;
                self.set(name, v);
                Ok(Flow::Normal)
            }
            Stmt::SetItem(base, idx_exprs, val_expr) => {
                let mut idxs = Vec::with_capacity(idx_exprs.len());
                for ie in idx_exprs {
                    idxs.push(self.eval(ie)?);
                }
                let val = self.eval(val_expr)?;
                let slot = self.slot_mut(base).ok_or_else(|| format!("NameError: {}", base))?;
                set_item(slot, &idxs, val)?;
                Ok(Flow::Normal)
            }
            Stmt::If(cond, body, orelse) => {
                if self.eval(cond)?.truthy() {
                    self.exec_block(body)
                } else {
                    self.exec_block(orelse)
                }
            }
            Stmt::While(cond, body) => {
                while self.eval(cond)?.truthy() {
                    self.tick()?;
                    match self.exec_block(body)? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        _ => {}
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::For(var, iter_e, body) => {
                let seq = self.eval(iter_e)?;
                let items = iter_values(&seq)?;
                for item in items {
                    self.tick()?;
                    self.set(var, item);
                    match self.exec_block(body)? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        _ => {}
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::Def(fd) => {
                self.set(&fd.name, Value::Func(fd.clone()));
                Ok(Flow::Normal)
            }
            Stmt::Return(opt) => {
                let v = match opt {
                    Some(e) => self.eval(e)?,
                    None => Value::None,
                };
                Ok(Flow::Return(v))
            }
            Stmt::Break => Ok(Flow::Break),
            Stmt::Continue => Ok(Flow::Continue),
        }
    }

    fn eval(&mut self, e: &Expr) -> Result<Value, String> {
        match e {
            Expr::Int(n) => Ok(Value::Int(*n)),
            Expr::Float(f) => Ok(Value::Float(*f)),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::Str(s) => Ok(Value::Str(s.clone())),
            Expr::NoneLit => Ok(Value::None),
            Expr::Var(name) => self.get(name).ok_or_else(|| format!("NameError: {}", name)),
            Expr::List(items) => {
                let mut v = Vec::with_capacity(items.len());
                for it in items {
                    v.push(self.eval(it)?);
                }
                Ok(Value::List(v))
            }
            Expr::Dict(pairs) => {
                let mut d = Vec::with_capacity(pairs.len());
                for (k, val) in pairs {
                    let kv = self.eval(k)?;
                    let vv = self.eval(val)?;
                    if let Some(slot) = d.iter_mut().find(|(ek, _): &&mut (Value, Value)| value_eq(ek, &kv)) {
                        slot.1 = vv;
                    } else {
                        d.push((kv, vv));
                    }
                }
                Ok(Value::Dict(d))
            }
            Expr::Neg(inner) => match self.eval(inner)? {
                Value::Int(n) => Ok(Value::Int(-n)),
                Value::Float(f) => Ok(Value::Float(-f)),
                Value::Bool(b) => Ok(Value::Int(if b { -1 } else { 0 })),
                v => Err(format!("TypeError: bad operand for -: {}", v.type_name())),
            },
            Expr::Not(inner) => Ok(Value::Bool(!self.eval(inner)?.truthy())),
            Expr::And(l, r) => {
                let lv = self.eval(l)?;
                if lv.truthy() {
                    self.eval(r)
                } else {
                    Ok(lv)
                }
            }
            Expr::Or(l, r) => {
                let lv = self.eval(l)?;
                if lv.truthy() {
                    Ok(lv)
                } else {
                    self.eval(r)
                }
            }
            Expr::Cmp(op, l, r) => {
                let a = self.eval(l)?;
                let b = self.eval(r)?;
                Ok(Value::Bool(compare(*op, &a, &b)?))
            }
            Expr::Bin(op, l, r) => {
                let a = self.eval(l)?;
                let b = self.eval(r)?;
                binop(*op, &a, &b)
            }
            Expr::Index(o, idx) => {
                let base = self.eval(o)?;
                let i = self.eval(idx)?;
                index_get(&base, &i)
            }
            Expr::Call(callee, args) => {
                // builtins are referenced by bare name and aren't first-class values
                if let Expr::Var(name) = &**callee {
                    if is_builtin(name) {
                        let mut vals = Vec::with_capacity(args.len());
                        for a in args {
                            vals.push(self.eval(a)?);
                        }
                        return self.call_builtin(name, vals);
                    }
                }
                let f = self.eval(callee)?;
                let Value::Func(fd) = f else {
                    return Err(format!("TypeError: '{}' is not callable", f.type_name()));
                };
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(self.eval(a)?);
                }
                self.call_func(fd, vals)
            }
        }
    }

    fn call_func(&mut self, fd: Rc<FuncDef>, args: Vec<Value>) -> Result<Value, String> {
        if args.len() != fd.params.len() {
            return Err(format!("TypeError: {}() takes {} args, got {}", fd.name, fd.params.len(), args.len()));
        }
        if self.depth + 1 > DEPTH_LIMIT {
            return Err("RecursionError: too deep".into());
        }
        let mut frame = Env::new();
        for (p, v) in fd.params.iter().zip(args.into_iter()) {
            frame.set(p, v);
        }
        self.depth += 1;
        self.locals.push(frame);
        let r = self.exec_block(&fd.body);
        self.locals.pop();
        self.depth -= 1;
        match r? {
            Flow::Return(v) => Ok(v),
            _ => Ok(Value::None),
        }
    }

    fn call_builtin(&mut self, name: &str, args: Vec<Value>) -> Result<Value, String> {
        match name {
            "print" => {
                let mut line = String::new();
                for (i, v) in args.iter().enumerate() {
                    if i > 0 {
                        line.push(' ');
                    }
                    line.push_str(&v.display());
                }
                self.out.push(line);
                Ok(Value::None)
            }
            "len" => {
                let a = arg1(&args)?;
                match a {
                    Value::Str(s) => Ok(Value::Int(s.chars().count() as i64)),
                    Value::List(v) => Ok(Value::Int(v.len() as i64)),
                    Value::Dict(d) => Ok(Value::Int(d.len() as i64)),
                    _ => Err(format!("TypeError: object of type '{}' has no len()", a.type_name())),
                }
            }
            "range" => builtin_range(&args),
            "abs" => {
                let a = arg1(&args)?;
                match a {
                    Value::Int(n) => Ok(Value::Int(n.abs())),
                    Value::Float(f) => Ok(Value::Float(if *f < 0.0 { -f } else { *f })),
                    _ => Err("TypeError: abs() needs a number".into()),
                }
            }
            "int" => {
                let a = arg1(&args)?;
                match a {
                    Value::Int(n) => Ok(Value::Int(*n)),
                    Value::Bool(b) => Ok(Value::Int(if *b { 1 } else { 0 })),
                    Value::Float(f) => Ok(Value::Int(libm::trunc(*f) as i64)),
                    Value::Str(s) => s.trim().parse::<i64>().map(Value::Int).map_err(|_| "ValueError: int()".into()),
                    _ => Err("TypeError: int()".into()),
                }
            }
            "float" => {
                let a = arg1(&args)?;
                if let Some(f) = a.as_f64() {
                    Ok(Value::Float(f))
                } else if let Value::Str(s) = a {
                    s.trim().parse::<f64>().map(Value::Float).map_err(|_| "ValueError: float()".into())
                } else {
                    Err("TypeError: float()".into())
                }
            }
            "str" => Ok(Value::Str(arg1(&args)?.display())),
            "bool" => Ok(Value::Bool(arg1(&args)?.truthy())),
            "list" => {
                let a = arg1(&args)?;
                Ok(Value::List(iter_values(a)?))
            }
            _ => Err(format!("NameError: {}", name)),
        }
    }
}

fn is_builtin(name: &str) -> bool {
    matches!(name, "print" | "len" | "range" | "abs" | "int" | "float" | "str" | "bool" | "list")
}

fn arg1(args: &[Value]) -> Result<&Value, String> {
    match args {
        [a] => Ok(a),
        _ => Err("TypeError: expected 1 argument".into()),
    }
}

fn builtin_range(args: &[Value]) -> Result<Value, String> {
    let int = |v: &Value| -> Result<i64, String> {
        match v {
            Value::Int(n) => Ok(*n),
            Value::Bool(b) => Ok(if *b { 1 } else { 0 }),
            _ => Err("TypeError: range() needs integers".into()),
        }
    };
    let (start, stop, step) = match args {
        [b] => (0, int(b)?, 1),
        [a, b] => (int(a)?, int(b)?, 1),
        [a, b, c] => {
            let s = int(c)?;
            if s == 0 {
                return Err("ValueError: range() step cannot be zero".into());
            }
            (int(a)?, int(b)?, s)
        }
        _ => return Err("TypeError: range() takes 1-3 arguments".into()),
    };
    let mut out = Vec::new();
    let mut i = start;
    while (step > 0 && i < stop) || (step < 0 && i > stop) {
        if out.len() as i64 >= RANGE_MAX {
            return Err("ValueError: range too large".into());
        }
        out.push(Value::Int(i));
        i += step;
    }
    Ok(Value::List(out))
}

/// Values a `for` loop / `list()` iterates over.
fn iter_values(v: &Value) -> Result<Vec<Value>, String> {
    match v {
        Value::List(items) => Ok(items.clone()),
        Value::Str(s) => Ok(s.chars().map(|c| Value::Str(c.to_string())).collect()),
        Value::Dict(d) => Ok(d.iter().map(|(k, _)| k.clone()).collect()),
        _ => Err(format!("TypeError: '{}' is not iterable", v.type_name())),
    }
}

fn index_get(base: &Value, idx: &Value) -> Result<Value, String> {
    match base {
        Value::List(items) => {
            let i = norm_index(idx, items.len())?;
            Ok(items[i].clone())
        }
        Value::Str(s) => {
            let chars: Vec<char> = s.chars().collect();
            let i = norm_index(idx, chars.len())?;
            Ok(Value::Str(chars[i].to_string()))
        }
        Value::Dict(d) => d
            .iter()
            .find(|(k, _)| value_eq(k, idx))
            .map(|(_, v)| v.clone())
            .ok_or_else(|| format!("KeyError: {}", idx.repr())),
        _ => Err(format!("TypeError: '{}' is not subscriptable", base.type_name())),
    }
}

/// Resolve a (possibly negative) sequence index into 0..len.
fn norm_index(idx: &Value, len: usize) -> Result<usize, String> {
    let n = match idx {
        Value::Int(n) => *n,
        Value::Bool(b) => {
            if *b {
                1
            } else {
                0
            }
        }
        _ => return Err("TypeError: index must be an integer".into()),
    };
    let i = if n < 0 { n + len as i64 } else { n };
    if i < 0 || i >= len as i64 {
        Err("IndexError: out of range".into())
    } else {
        Ok(i as usize)
    }
}

/// `target[idx][idx]... = val`, mutating in place.
fn set_item(target: &mut Value, idxs: &[Value], val: Value) -> Result<(), String> {
    if idxs.is_empty() {
        return Err("SyntaxError: no index".into());
    }
    if idxs.len() == 1 {
        return set_one(target, &idxs[0], val);
    }
    let next = get_item_mut(target, &idxs[0])?;
    set_item(next, &idxs[1..], val)
}

fn set_one(target: &mut Value, idx: &Value, val: Value) -> Result<(), String> {
    match target {
        Value::List(items) => {
            let i = norm_index(idx, items.len())?;
            items[i] = val;
            Ok(())
        }
        Value::Dict(d) => {
            if let Some(slot) = d.iter_mut().find(|(k, _)| value_eq(k, idx)) {
                slot.1 = val;
            } else {
                d.push((idx.clone(), val));
            }
            Ok(())
        }
        _ => Err(format!("TypeError: '{}' does not support item assignment", target.type_name())),
    }
}

fn get_item_mut<'a>(target: &'a mut Value, idx: &Value) -> Result<&'a mut Value, String> {
    match target {
        Value::List(items) => {
            let i = norm_index(idx, items.len())?;
            Ok(&mut items[i])
        }
        Value::Dict(d) => d
            .iter_mut()
            .find(|(k, _)| value_eq(k, idx))
            .map(|s| &mut s.1)
            .ok_or_else(|| format!("KeyError: {}", idx.repr())),
        _ => Err(format!("TypeError: '{}' is not subscriptable", target.type_name())),
    }
}

fn compare(op: CmpOp, a: &Value, b: &Value) -> Result<bool, String> {
    if let CmpOp::Eq = op {
        return Ok(value_eq(a, b));
    }
    if let CmpOp::Ne = op {
        return Ok(!value_eq(a, b));
    }
    // ordering: numbers or strings
    let ord = match (a, b) {
        (Value::Str(x), Value::Str(y)) => {
            if x < y {
                -1
            } else if x > y {
                1
            } else {
                0
            }
        }
        _ => match (a.as_f64(), b.as_f64()) {
            (Some(x), Some(y)) => {
                if x < y {
                    -1
                } else if x > y {
                    1
                } else {
                    0
                }
            }
            _ => return Err(format!("TypeError: cannot compare {} and {}", a.type_name(), b.type_name())),
        },
    };
    Ok(match op {
        CmpOp::Lt => ord < 0,
        CmpOp::Le => ord <= 0,
        CmpOp::Gt => ord > 0,
        CmpOp::Ge => ord >= 0,
        _ => unreachable!(),
    })
}

fn binop(op: BinOp, a: &Value, b: &Value) -> Result<Value, String> {
    // `+` also concatenates strings and lists
    if let BinOp::Add = op {
        match (a, b) {
            (Value::Str(x), Value::Str(y)) => {
                let mut s = x.clone();
                s.push_str(y);
                return Ok(Value::Str(s));
            }
            (Value::List(x), Value::List(y)) => {
                let mut v = x.clone();
                v.extend(y.iter().cloned());
                return Ok(Value::List(v));
            }
            _ => {}
        }
    }
    // `str * int` / `list * int` repetition
    if let BinOp::Mul = op {
        if let (Value::Str(s), Value::Int(n)) | (Value::Int(n), Value::Str(s)) = (a, b) {
            let n = (*n).max(0) as usize;
            return Ok(Value::Str(s.repeat(n)));
        }
        if let (Value::List(l), Value::Int(n)) | (Value::Int(n), Value::List(l)) = (a, b) {
            let n = (*n).max(0) as usize;
            let mut v = Vec::with_capacity(l.len() * n);
            for _ in 0..n {
                v.extend(l.iter().cloned());
            }
            return Ok(Value::List(v));
        }
    }
    let (af, bf) = match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => (x, y),
        _ => return Err(format!("TypeError: unsupported operands: {} and {}", a.type_name(), b.type_name())),
    };
    let both_int = matches!(a, Value::Int(_) | Value::Bool(_)) && matches!(b, Value::Int(_) | Value::Bool(_));
    match op {
        BinOp::Add => Ok(num(af + bf, both_int)),
        BinOp::Sub => Ok(num(af - bf, both_int)),
        BinOp::Mul => Ok(num(af * bf, both_int)),
        BinOp::Div => {
            if bf == 0.0 {
                Err("ZeroDivisionError".into())
            } else {
                Ok(Value::Float(af / bf))
            }
        }
        BinOp::FloorDiv => {
            if bf == 0.0 {
                Err("ZeroDivisionError".into())
            } else {
                Ok(num(libm::floor(af / bf), both_int))
            }
        }
        BinOp::Mod => {
            if bf == 0.0 {
                Err("ZeroDivisionError".into())
            } else {
                Ok(num(af - bf * libm::floor(af / bf), both_int))
            }
        }
        BinOp::Pow => {
            if both_int && bf >= 0.0 {
                if let Some(r) = (af as i64).checked_pow(bf as u32) {
                    return Ok(Value::Int(r));
                }
            }
            Ok(Value::Float(libm::pow(af, bf)))
        }
    }
}

fn num(f: f64, int: bool) -> Value {
    if int {
        Value::Int(f as i64)
    } else {
        Value::Float(f)
    }
}

/// `help` output — a terse cheat-sheet sized to fit the 8 visible rows on screen
/// at once (longer output scrolls and only the tail shows). ~39 cols wide.
fn help_lines() -> Vec<String> {
    [
        "help: Aa=caps, indent block, blank=run",
        "types: int float bool str list dict",
        "math: + - * / // % **",
        "cmp: == != < <= > >=  and or not",
        "assign: x=e  x+=e  a[i]=e",
        "flow: if/elif/else  while  for x in s",
        "func: def f(): return  break continue",
        "fns: print len range int float str abs",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Run a (multi-line) source block; returns the display line(s) it produces.
/// Pure — used by both the REPL UI and the serial self-test.
pub fn eval_source(src: &str, env: &mut Env) -> Vec<String> {
    let trimmed = src.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if trimmed == "help" || trimmed == "help()" {
        return help_lines();
    }
    let stmts = match parse(src) {
        Ok(s) => s,
        Err(e) => return alloc::vec![e],
    };
    let mut vm = Vm { global: env, locals: Vec::new(), steps: 0, depth: 0, out: Vec::new() };
    // A lone top-level expression echoes its repr, like a typical REPL prompt.
    if stmts.len() == 1 {
        if let Stmt::Expr(e) = &stmts[0] {
            match vm.eval(e) {
                Ok(v) => {
                    if !matches!(v, Value::None) {
                        vm.out.push(v.repr());
                    }
                }
                Err(err) => vm.out.push(err),
            }
            return vm.out;
        }
    }
    if let Err(err) = vm.exec_block(&stmts) {
        vm.out.push(err);
    }
    vm.out
}

// ============================== REPL UI ==============================

const MAX_INPUT: usize = 48;
const MAX_HISTORY: usize = 80;
const VIS_ROWS: usize = 8; // scrollback lines shown above the input
const ROW_H: i32 = 11;

pub struct Repl {
    input: String,
    history: Vec<String>,
    pending: Vec<String>, // accumulated lines of an unfinished indented block
    env: Env,
    caps: bool, // "Aa" caps toggle (the keyboard has no hardware caps-lock)
}

impl Repl {
    pub fn new() -> Self {
        let mut history = Vec::new();
        history.push("REPL - a small scripting shell".into());
        history.push("type 'help' for commands".into());
        Repl { input: String::new(), history, pending: Vec::new(), env: Env::new(), caps: false }
    }

    pub fn enter<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        self.draw(d, true);
    }

    /// Flip the caps state (driven by the "Aa" key) and refresh the indicator.
    pub fn toggle_caps<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D) {
        self.caps = !self.caps;
        self.draw(d, false);
    }

    pub fn on_key<D: DrawTarget<Color = Rgb565>>(&mut self, rc: (u8, u8), d: &mut D) {
        if rc == crate::K_ENTER {
            let line = core::mem::take(&mut self.input);
            let prompt = if self.pending.is_empty() { ">>>" } else { "..." };
            self.push(format!("{} {}", prompt, line));
            if self.pending.is_empty() {
                if line.trim().is_empty() {
                    // nothing to do
                } else if starts_block(&line) {
                    self.pending.push(line);
                } else {
                    self.run(&line);
                }
            } else if line.trim().is_empty() {
                // blank line ends the block -> run it
                let src = self.pending.join("\n");
                self.pending.clear();
                self.run(&src);
            } else {
                self.pending.push(line);
            }
            self.draw(d, false);
            return;
        }
        if rc == keymap::K_BKSP {
            self.input.pop();
            self.draw(d, false);
            return;
        }
        if let Some(b) = keymap::ch_shift(rc.0, rc.1, self.caps) {
            if self.input.len() < MAX_INPUT {
                self.input.push(b as char);
                self.draw(d, false);
            }
        }
    }

    fn run(&mut self, src: &str) {
        for out in eval_source(src, &mut self.env) {
            self.push(out);
        }
    }

    fn push(&mut self, line: String) {
        self.history.push(line);
        if self.history.len() > MAX_HISTORY {
            self.history.drain(0..self.history.len() - MAX_HISTORY);
        }
    }

    pub fn draw<D: DrawTarget<Color = Rgb565>>(&mut self, d: &mut D, clear: bool) {
        if clear {
            theme::clear(d);
        }
        theme::topbar(d, "REPL");
        theme::fill(d, 0, 20, theme::W as u32, (theme::HINT_Y - 22) as u32, theme::BG);

        let start = self.history.len().saturating_sub(VIS_ROWS);
        for (i, line) in self.history[start..].iter().enumerate() {
            let y = 22 + i as i32 * ROW_H;
            theme::text(d, clip(line), theme::PAD, y, theme::BODY_FONT, theme::MUTED);
        }
        // current input line: ">>>" normally, "..." while inside a block
        let lead = if self.pending.is_empty() { ">>>" } else { "..." };
        let prompt = format!("{} {}_", lead, self.input);
        let y = 22 + VIS_ROWS as i32 * ROW_H;
        theme::text(d, clip(&prompt), theme::PAD, y, theme::BODY_FONT, theme::accent());

        // "Aa ABC/abc" advertises the caps toggle and shows its current state.
        let hint = format!("Aa {}   ENTER run   help   ESC menu", if self.caps { "ABC" } else { "abc" });
        theme::hint(d, &hint);
    }
}

/// True if a line opens an indented block (its code ends with ':').
fn starts_block(line: &str) -> bool {
    // strip a trailing comment, then look at the last non-space char
    let code = match line.split('#').next() {
        Some(c) => c.trim_end(),
        None => line.trim_end(),
    };
    code.ends_with(':')
}

/// Clip a display line to the screen width (40 chars at 6px on a 240px panel).
fn clip(s: &str) -> &str {
    const MAX: usize = 39;
    if s.len() <= MAX {
        s
    } else {
        // byte-truncate is safe here: all REPL text is ASCII
        &s[..MAX]
    }
}
