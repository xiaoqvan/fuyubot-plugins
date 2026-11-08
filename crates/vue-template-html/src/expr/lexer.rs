//! 词法分析：把一段表达式切成带字节区间的 token。
//!
//! 只做「词」这一层的事：数字、字符串、标识符、运算符。语法结构交给
//! [`super::parser`]，语义交给 [`super::eval`]。
//!
//! 所有位置都是**字节偏移**，相对表达式字符串起点——与 [`crate::syntax::Span`]
//! 的口径一致，调用方可以直接加上 `raw_start` 映射回模板源码。

use super::ExprError;

/// 词法单元的种类。
///
/// 带数据的变体（数字 / 字符串 / 标识符）在词法阶段就已经完成转换：
/// 数字保证是有限 `f64`，字符串的转义已经处理完。
#[derive(Debug, Clone, PartialEq)]
pub(super) enum TokenKind {
    /// 数字字面量（已转成 `f64`，且保证有限）。
    Number(f64),
    /// 字符串字面量（转义已经展开）。
    String(String),
    /// 标识符（关键字已经单独归类，不会出现在这里）。
    Ident(String),
    True,
    False,
    Null,
    Undefined,
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Dot,
    Comma,
    Colon,
    Question,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Not,
    /// 单个 `=`；表达式不支持赋值，保留它是为了给出更准确的报错。
    Assign,
    EqEq,
    EqEqEq,
    NotEq,
    NotEqEq,
    Lt,
    Le,
    Gt,
    Ge,
    AndAnd,
    OrOr,
    /// 输入结束。
    Eof,
}

/// 一个词法单元：种类 + 在表达式里的字节区间。
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Token {
    pub(super) kind: TokenKind,
    pub(super) start: usize,
    pub(super) end: usize,
}

/// 把 `source` 切成 token 序列；末尾一定有一个 [`TokenKind::Eof`]。
pub(super) fn lex(source: &str) -> Result<Vec<Token>, ExprError> {
    let mut lexer = Lexer {
        source,
        pos: 0,
        tokens: Vec::new(),
    };
    lexer.run()?;
    Ok(lexer.tokens)
}

struct Lexer<'a> {
    source: &'a str,
    pos: usize,
    tokens: Vec<Token>,
}

impl Lexer<'_> {
    fn run(&mut self) -> Result<(), ExprError> {
        loop {
            self.skip_whitespace();
            let start = self.pos;
            let Some(ch) = self.peek() else {
                self.tokens.push(Token {
                    kind: TokenKind::Eof,
                    start,
                    end: start,
                });
                return Ok(());
            };
            let kind = match ch {
                '(' => {
                    self.bump();
                    TokenKind::LParen
                }
                ')' => {
                    self.bump();
                    TokenKind::RParen
                }
                '[' => {
                    self.bump();
                    TokenKind::LBracket
                }
                ']' => {
                    self.bump();
                    TokenKind::RBracket
                }
                '{' => {
                    self.bump();
                    TokenKind::LBrace
                }
                '}' => {
                    self.bump();
                    TokenKind::RBrace
                }
                ',' => {
                    self.bump();
                    TokenKind::Comma
                }
                ':' => {
                    self.bump();
                    TokenKind::Colon
                }
                '?' => {
                    self.bump();
                    // 只在紧邻时判定，避免把 `a ? .5 : 1` 误认成可选链。
                    if self.peek() == Some('.') {
                        return Err(ExprError::new(start, "不支持可选链（`?.`）"));
                    }
                    if self.peek() == Some('?') {
                        return Err(ExprError::new(start, "不支持空值合并运算符（`??`）"));
                    }
                    TokenKind::Question
                }
                '+' => {
                    if self.starts_with("++") {
                        return Err(ExprError::new(start, "不支持自增运算符（`++`）"));
                    }
                    self.bump();
                    TokenKind::Plus
                }
                '-' => {
                    if self.starts_with("--") {
                        return Err(ExprError::new(start, "不支持自减运算符（`--`）"));
                    }
                    self.bump();
                    TokenKind::Minus
                }
                '*' => {
                    if self.starts_with("**") {
                        return Err(ExprError::new(start, "不支持幂运算符（`**`）"));
                    }
                    self.bump();
                    TokenKind::Star
                }
                '/' => {
                    self.bump();
                    TokenKind::Slash
                }
                '%' => {
                    self.bump();
                    TokenKind::Percent
                }
                '!' => {
                    self.bump();
                    if self.starts_with("==") {
                        self.bump();
                        self.bump();
                        TokenKind::NotEqEq
                    } else if self.peek() == Some('=') {
                        self.bump();
                        TokenKind::NotEq
                    } else {
                        TokenKind::Not
                    }
                }
                '=' => {
                    self.bump();
                    if self.starts_with("==") {
                        self.bump();
                        self.bump();
                        TokenKind::EqEqEq
                    } else if self.peek() == Some('=') {
                        self.bump();
                        TokenKind::EqEq
                    } else if self.peek() == Some('>') {
                        return Err(ExprError::new(start, "不支持箭头函数（`=>`）"));
                    } else {
                        TokenKind::Assign
                    }
                }
                '<' => {
                    self.bump();
                    if self.peek() == Some('=') {
                        self.bump();
                        TokenKind::Le
                    } else {
                        TokenKind::Lt
                    }
                }
                '>' => {
                    self.bump();
                    if self.peek() == Some('=') {
                        self.bump();
                        TokenKind::Ge
                    } else {
                        TokenKind::Gt
                    }
                }
                '&' => {
                    if self.starts_with("&&") {
                        self.bump();
                        self.bump();
                        TokenKind::AndAnd
                    } else {
                        return Err(ExprError::new(start, "不支持按位与运算（`&`）"));
                    }
                }
                '|' => {
                    if self.starts_with("||") {
                        self.bump();
                        self.bump();
                        TokenKind::OrOr
                    } else {
                        return Err(ExprError::new(
                            start,
                            "不支持按位或运算 / 过滤器语法（`|`）",
                        ));
                    }
                }
                '^' | '~' => {
                    return Err(ExprError::new(start, "不支持按位运算"));
                }
                ';' => {
                    return Err(ExprError::new(start, "不支持语句分隔符（`;`）"));
                }
                '`' => {
                    return Err(ExprError::new(start, "不支持模板字符串（反引号）"));
                }
                '\'' | '"' => self.string(ch, start)?,
                '.' => {
                    if self.peek_next().is_some_and(|next| next.is_ascii_digit()) {
                        self.number(start)?
                    } else {
                        self.bump();
                        TokenKind::Dot
                    }
                }
                '0'..='9' => self.number(start)?,
                ch if is_ident_start(ch) => self.identifier(start),
                ch => {
                    return Err(ExprError::new(
                        start,
                        format!("无法识别的字符 `{ch}`（标识符只支持 ASCII 字母、数字、`_`、`$`）"),
                    ));
                }
            };
            let end = self.pos;
            self.tokens.push(Token { kind, start, end });
        }
    }

    fn skip_whitespace(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.bump();
        }
    }

    fn peek(&self) -> Option<char> {
        self.source[self.pos..].chars().next()
    }

    fn peek_next(&self) -> Option<char> {
        self.source[self.pos..].chars().nth(1)
    }

    fn bump(&mut self) {
        if let Some(ch) = self.peek() {
            self.pos += ch.len_utf8();
        }
    }

    fn starts_with(&self, text: &str) -> bool {
        self.source[self.pos..].starts_with(text)
    }

    /// 数字：`123`、`1.5`、`.5`、`1e3`、`1.5E-2`。
    ///
    /// 不接受 `1.` 这种「小数点后没有数字」的写法（JS 允许，但模板里几乎不会出现，
    /// 明确报错比含糊地留下一个 `.` 更好）。
    fn number(&mut self, start: usize) -> Result<TokenKind, ExprError> {
        if self.peek() == Some('.') {
            self.bump();
            self.digits();
        } else {
            self.digits();
            if self.peek() == Some('.') && self.peek_next().is_some_and(|ch| ch.is_ascii_digit()) {
                self.bump();
                self.digits();
            }
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            self.bump();
            if matches!(self.peek(), Some('+' | '-')) {
                self.bump();
            }
            if !self.peek().is_some_and(|ch| ch.is_ascii_digit()) {
                return Err(ExprError::new(self.pos, "数字字面量的指数部分缺少数字"));
            }
            self.digits();
        }
        let text = &self.source[start..self.pos];
        let value: f64 = text
            .parse()
            .map_err(|_| ExprError::new(start, format!("无法解析数字字面量 `{text}`")))?;
        if !value.is_finite() {
            return Err(ExprError::new(
                start,
                format!("数字字面量 `{text}` 超出 f64 可表示范围"),
            ));
        }
        Ok(TokenKind::Number(value))
    }

    fn digits(&mut self) {
        while self.peek().is_some_and(|ch| ch.is_ascii_digit()) {
            self.bump();
        }
    }

    fn identifier(&mut self, start: usize) -> TokenKind {
        while self.peek().is_some_and(is_ident_continue) {
            self.bump();
        }
        let text = &self.source[start..self.pos];
        match text {
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "null" => TokenKind::Null,
            "undefined" => TokenKind::Undefined,
            _ => TokenKind::Ident(text.to_owned()),
        }
    }

    /// 字符串字面量：单 / 双引号，支持 `\n \r \t \b \f \\ \' \" \/ \uXXXX`。
    ///
    /// 代理对：`\uD83D\uDE00` 会合成一个码点（😀）。**单独**的代理项在 Rust 的
    /// `String` 里表示不了，统一降级成 `U+FFFD`（JS 允许孤立代理项，这里不保留）。
    fn string(&mut self, quote: char, start: usize) -> Result<TokenKind, ExprError> {
        self.bump();
        let mut out = String::new();
        loop {
            let Some(ch) = self.peek() else {
                return Err(ExprError::new(start, "字符串字面量未闭合"));
            };
            if ch == quote {
                self.bump();
                return Ok(TokenKind::String(out));
            }
            if ch != '\\' {
                out.push(ch);
                self.bump();
                continue;
            }
            let escape_start = self.pos;
            self.bump();
            let Some(escaped) = self.peek() else {
                return Err(ExprError::new(start, "字符串字面量未闭合"));
            };
            self.bump();
            match escaped {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'b' => out.push('\u{0008}'),
                'f' => out.push('\u{000C}'),
                '\\' => out.push('\\'),
                '\'' => out.push('\''),
                '"' => out.push('"'),
                '/' => out.push('/'),
                'u' => {
                    let first = self.hex4(escape_start)?;
                    let code = if (0xD800..=0xDBFF).contains(&first) {
                        match self.try_low_surrogate() {
                            Some(second) => {
                                // `\u` + 4 位十六进制，全是 ASCII，直接按字节跳过。
                                for _ in 0..6 {
                                    self.bump();
                                }
                                0x1_0000 + ((first - 0xD800) << 10) + (second - 0xDC00)
                            }
                            None => 0xFFFD,
                        }
                    } else if (0xDC00..=0xDFFF).contains(&first) {
                        0xFFFD
                    } else {
                        first
                    };
                    out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                }
                other => {
                    return Err(ExprError::new(
                        escape_start,
                        format!("不支持的转义序列 `\\{other}`"),
                    ));
                }
            }
        }
    }

    /// 读 4 位十六进制数字（`\u` 后面那四位）。
    fn hex4(&mut self, escape_start: usize) -> Result<u32, ExprError> {
        let mut value = 0u32;
        for _ in 0..4 {
            let Some(ch) = self.peek() else {
                return Err(ExprError::new(
                    escape_start,
                    "`\\u` 转义缺少 4 位十六进制数字",
                ));
            };
            let Some(digit) = ch.to_digit(16) else {
                return Err(ExprError::new(
                    escape_start,
                    "`\\u` 转义需要 4 位十六进制数字",
                ));
            };
            value = value * 16 + digit;
            self.bump();
        }
        Ok(value)
    }

    /// 如果当前位置正好是 `\uDC00`–`\uDFFF`（低代理项），返回它的码点，不前进。
    fn try_low_surrogate(&self) -> Option<u32> {
        let rest = &self.source.as_bytes()[self.pos..];
        if rest.len() < 6 || rest[0] != b'\\' || rest[1] != b'u' {
            return None;
        }
        let mut value = 0u32;
        for &byte in &rest[2..6] {
            let digit = (byte as char).to_digit(16)?;
            value = value * 16 + digit;
        }
        (0xDC00..=0xDFFF).contains(&value).then_some(value)
    }
}

fn is_ident_start(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_' || ch == '$'
}

fn is_ident_continue(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || ch == '$'
}
