//! 递归下降解析器：token 序列 → [`Expr`]。
//!
//! 优先级从低到高（与 [`super`] 的模块文档一致）：
//!
//! ```text
//! ternary → or → and → equality → relational → additive → multiplicative
//!         → unary → postfix → primary
//! ```
//!
//! 每个 AST 节点都带 [`Span`]（相对表达式起点的字节区间），求值期报错与
//! 模板层映射回源码都靠它。

use serde_json::Value;

use super::eval::number_value;
use super::lexer::{self, Token, TokenKind};
use super::{BinaryOp, Expr, ExprError, UnaryOp};
use crate::syntax::Span;

/// 解析一段表达式源码。
pub(super) fn parse(source: &str) -> Result<Expr, ExprError> {
    let tokens = lexer::lex(source)?;
    let mut parser = Parser {
        source,
        tokens,
        pos: 0,
    };
    if matches!(parser.peek_kind(), TokenKind::Eof) {
        return Err(ExprError::new(0, "表达式为空"));
    }
    let expr = parser.parse_ternary()?;
    if !matches!(parser.peek_kind(), TokenKind::Eof) {
        // 赋值、箭头函数之类的「语法上认得、但本引擎不支持」的东西，尽量给
        // 更准确的提示，而不是笼统的「意外的 token」。
        let token = parser.peek();
        let message = match token.kind {
            TokenKind::Assign => "不支持赋值表达式（模板表达式只读）".to_owned(),
            TokenKind::LParen => "不支持函数调用（模板表达式里不能调用函数）".to_owned(),
            _ => format!("表达式末尾有多余内容 `{}`", parser.token_text(token)),
        };
        return Err(ExprError::new(token.start, message));
    }
    Ok(expr)
}

struct Parser<'a> {
    source: &'a str,
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> &Token {
        // lexer 保证至少有一个 Eof，且 advance 不会越过它。
        &self.tokens[self.pos.min(self.tokens.len().saturating_sub(1))]
    }

    fn peek_kind(&self) -> &TokenKind {
        &self.peek().kind
    }

    fn advance(&mut self) {
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
    }

    fn previous_end(&self) -> usize {
        self.tokens[self.pos.saturating_sub(1)].end
    }

    fn eat(&mut self, kind: TokenKind) -> bool {
        if *self.peek_kind() == kind {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, kind: TokenKind, message: &str) -> Result<(), ExprError> {
        if *self.peek_kind() == kind {
            self.advance();
            Ok(())
        } else {
            Err(self.error_here(message))
        }
    }

    fn error_here(&self, message: impl Into<String>) -> ExprError {
        ExprError::new(self.peek().start, message)
    }

    fn token_text(&self, token: &Token) -> &str {
        &self.source[token.start..token.end]
    }

    fn unexpected(&self) -> ExprError {
        let token = self.peek();
        if matches!(token.kind, TokenKind::Eof) {
            ExprError::new(token.start, "表达式意外结束")
        } else {
            ExprError::new(
                token.start,
                format!("意外的 token `{}`", self.token_text(token)),
            )
        }
    }

    /// 左结合二元运算的统一循环：`left (op left)*`。
    fn parse_left_assoc(
        &mut self,
        ops: &[(TokenKind, BinaryOp)],
        mut next: impl FnMut(&mut Self) -> Result<Expr, ExprError>,
    ) -> Result<Expr, ExprError> {
        let mut left = next(self)?;
        loop {
            let op = ops
                .iter()
                .find_map(|(kind, op)| (kind == self.peek_kind()).then_some(*op));
            let Some(op) = op else {
                break;
            };
            self.advance();
            let right = next(self)?;
            let span = span_of(&left).cover(span_of(&right));
            left = Expr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
                span,
            };
        }
        Ok(left)
    }

    /// `ternary := or ('?' expr ':' ternary)?`，右结合。
    fn parse_ternary(&mut self) -> Result<Expr, ExprError> {
        let condition = self.parse_or()?;
        if !self.eat(TokenKind::Question) {
            return Ok(condition);
        }
        let consequent = self.parse_ternary()?;
        self.expect(TokenKind::Colon, "三元表达式缺少 `:`")?;
        let alternate = self.parse_ternary()?;
        let span = span_of(&condition).cover(span_of(&alternate));
        Ok(Expr::Ternary {
            condition: Box::new(condition),
            consequent: Box::new(consequent),
            alternate: Box::new(alternate),
            span,
        })
    }

    fn parse_or(&mut self) -> Result<Expr, ExprError> {
        self.parse_left_assoc(&[(TokenKind::OrOr, BinaryOp::Or)], |parser| {
            parser.parse_and()
        })
    }

    fn parse_and(&mut self) -> Result<Expr, ExprError> {
        self.parse_left_assoc(&[(TokenKind::AndAnd, BinaryOp::And)], |parser| {
            parser.parse_equality()
        })
    }

    fn parse_equality(&mut self) -> Result<Expr, ExprError> {
        self.parse_left_assoc(
            &[
                (TokenKind::EqEqEq, BinaryOp::EqStrict),
                (TokenKind::NotEqEq, BinaryOp::NeStrict),
                (TokenKind::EqEq, BinaryOp::EqLoose),
                (TokenKind::NotEq, BinaryOp::NeLoose),
            ],
            |parser| parser.parse_relational(),
        )
    }

    fn parse_relational(&mut self) -> Result<Expr, ExprError> {
        self.parse_left_assoc(
            &[
                (TokenKind::Lt, BinaryOp::Lt),
                (TokenKind::Le, BinaryOp::Le),
                (TokenKind::Gt, BinaryOp::Gt),
                (TokenKind::Ge, BinaryOp::Ge),
            ],
            |parser| parser.parse_additive(),
        )
    }

    fn parse_additive(&mut self) -> Result<Expr, ExprError> {
        self.parse_left_assoc(
            &[
                (TokenKind::Plus, BinaryOp::Add),
                (TokenKind::Minus, BinaryOp::Sub),
            ],
            |parser| parser.parse_multiplicative(),
        )
    }

    fn parse_multiplicative(&mut self) -> Result<Expr, ExprError> {
        self.parse_left_assoc(
            &[
                (TokenKind::Star, BinaryOp::Mul),
                (TokenKind::Slash, BinaryOp::Div),
                (TokenKind::Percent, BinaryOp::Rem),
            ],
            |parser| parser.parse_unary(),
        )
    }

    fn parse_unary(&mut self) -> Result<Expr, ExprError> {
        let token = self.peek().clone();
        let op = match token.kind {
            TokenKind::Not => Some(UnaryOp::Not),
            TokenKind::Minus => Some(UnaryOp::Negate),
            TokenKind::Plus => Some(UnaryOp::Plus),
            _ => None,
        };
        if let Some(op) = op {
            self.advance();
            let operand = self.parse_unary()?;
            let span = Span::new(token.start, span_of(&operand).end);
            return Ok(Expr::Unary {
                op,
                operand: Box::new(operand),
                span,
            });
        }
        self.parse_postfix()
    }

    /// `postfix := primary ('.' IDENT | '[' expr ']')*`
    fn parse_postfix(&mut self) -> Result<Expr, ExprError> {
        let mut expr = self.parse_primary()?;
        loop {
            if self.eat(TokenKind::Dot) {
                let property_token = self.peek().clone();
                let name = match &property_token.kind {
                    TokenKind::Ident(name) => name.clone(),
                    kind => match keyword_name(kind) {
                        Some(name) => name.to_owned(),
                        None => {
                            return Err(ExprError::new(property_token.start, "`.` 后需要属性名"));
                        }
                    },
                };
                self.advance();
                let property = Expr::Literal {
                    value: Value::String(name),
                    span: Span::new(property_token.start, property_token.end),
                };
                let span = Span::new(span_of(&expr).start, property_token.end);
                expr = Expr::Member {
                    object: Box::new(expr),
                    property: Box::new(property),
                    span,
                };
            } else if self.eat(TokenKind::LBracket) {
                let index = self.parse_ternary()?;
                self.expect(TokenKind::RBracket, "下标访问缺少 `]`")?;
                let span = Span::new(span_of(&expr).start, self.previous_end());
                expr = Expr::Member {
                    object: Box::new(expr),
                    property: Box::new(index),
                    span,
                };
            } else {
                break;
            }
        }
        if matches!(self.peek_kind(), TokenKind::LParen) {
            return Err(self.error_here("不支持函数调用（模板表达式里不能调用函数）"));
        }
        Ok(expr)
    }

    fn parse_primary(&mut self) -> Result<Expr, ExprError> {
        let token = self.peek().clone();
        let span = Span::new(token.start, token.end);
        match token.kind {
            TokenKind::Number(value) => {
                self.advance();
                Ok(Expr::Literal {
                    value: number_value(value),
                    span,
                })
            }
            TokenKind::String(value) => {
                self.advance();
                Ok(Expr::Literal {
                    value: Value::String(value),
                    span,
                })
            }
            TokenKind::True => {
                self.advance();
                Ok(Expr::Literal {
                    value: Value::Bool(true),
                    span,
                })
            }
            TokenKind::False => {
                self.advance();
                Ok(Expr::Literal {
                    value: Value::Bool(false),
                    span,
                })
            }
            TokenKind::Null | TokenKind::Undefined => {
                self.advance();
                Ok(Expr::Literal {
                    value: Value::Null,
                    span,
                })
            }
            TokenKind::Ident(name) => {
                self.advance();
                if name == "new" {
                    return Err(ExprError::new(token.start, "不支持 `new` 表达式"));
                }
                if matches!(self.peek_kind(), TokenKind::LParen) {
                    return Err(ExprError::new(
                        token.start,
                        "不支持函数调用（模板表达式里不能调用函数）",
                    ));
                }
                Ok(Expr::Var { name, span })
            }
            TokenKind::LParen => {
                self.advance();
                let inner = self.parse_ternary()?;
                self.expect(TokenKind::RParen, "括号未闭合，缺少 `)`")?;
                Ok(inner)
            }
            TokenKind::LBracket => self.parse_array(),
            TokenKind::LBrace => self.parse_object(),
            TokenKind::Assign => Err(ExprError::new(
                token.start,
                "不支持赋值表达式（模板表达式只读）",
            )),
            TokenKind::Slash => Err(ExprError::new(token.start, "不支持正则字面量")),
            TokenKind::Eof => Err(ExprError::new(token.start, "表达式意外结束")),
            _ => Err(self.unexpected()),
        }
    }

    /// `'[' (expr (',' expr)* ','?)? ']'`
    fn parse_array(&mut self) -> Result<Expr, ExprError> {
        let open = self.peek().clone();
        self.advance();
        let mut items = Vec::new();
        if !self.eat(TokenKind::RBracket) {
            loop {
                items.push(self.parse_ternary()?);
                if self.eat(TokenKind::Comma) {
                    if self.eat(TokenKind::RBracket) {
                        break;
                    }
                } else {
                    self.expect(TokenKind::RBracket, "数组字面量缺少 `]`")?;
                    break;
                }
            }
        }
        Ok(Expr::Array {
            items,
            span: Span::new(open.start, self.previous_end()),
        })
    }

    /// `'{' (key ':' expr | IDENT) (',' ...)* ','? '}'`
    ///
    /// 键支持标识符、字符串、数字；简写 `{ color }` 等价于 `{ color: color }`。
    /// 不支持计算键 `{ [k]: v }`。
    fn parse_object(&mut self) -> Result<Expr, ExprError> {
        let open = self.peek().clone();
        self.advance();
        let mut entries = Vec::new();
        if !self.eat(TokenKind::RBrace) {
            loop {
                let key_token = self.peek().clone();
                let (key, shorthand) = match &key_token.kind {
                    TokenKind::Ident(name) => {
                        self.advance();
                        (name.clone(), true)
                    }
                    TokenKind::String(value) => {
                        self.advance();
                        (value.clone(), false)
                    }
                    TokenKind::Number(value) => {
                        self.advance();
                        (super::eval::number_to_string(*value), false)
                    }
                    kind => match keyword_name(kind) {
                        Some(name) => {
                            self.advance();
                            (name.to_owned(), false)
                        }
                        None => {
                            return Err(ExprError::new(
                                key_token.start,
                                "对象字面量的键必须是标识符、字符串或数字（不支持计算键）",
                            ));
                        }
                    },
                };
                let value = if self.eat(TokenKind::Colon) {
                    self.parse_ternary()?
                } else if shorthand {
                    Expr::Var {
                        name: key.clone(),
                        span: Span::new(key_token.start, key_token.end),
                    }
                } else {
                    return Err(self.error_here("对象字面量缺少 `:`"));
                };
                entries.push((key, value));
                if self.eat(TokenKind::Comma) {
                    if self.eat(TokenKind::RBrace) {
                        break;
                    }
                } else {
                    self.expect(TokenKind::RBrace, "对象字面量缺少 `}`")?;
                    break;
                }
            }
        }
        Ok(Expr::Object {
            entries,
            span: Span::new(open.start, self.previous_end()),
        })
    }
}

/// 关键字也能当属性名（`obj.true`、`{ null: 1 }`——JS 里合法）。
fn keyword_name(kind: &TokenKind) -> Option<&'static str> {
    match kind {
        TokenKind::True => Some("true"),
        TokenKind::False => Some("false"),
        TokenKind::Null => Some("null"),
        TokenKind::Undefined => Some("undefined"),
        _ => None,
    }
}

fn span_of(expr: &Expr) -> Span {
    match expr {
        Expr::Literal { span, .. }
        | Expr::Var { span, .. }
        | Expr::Member { span, .. }
        | Expr::Unary { span, .. }
        | Expr::Binary { span, .. }
        | Expr::Ternary { span, .. }
        | Expr::Array { span, .. }
        | Expr::Object { span, .. } => *span,
    }
}
