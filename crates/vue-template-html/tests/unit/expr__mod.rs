//! 表达式引擎的单元测试。
//!
//! 覆盖词法 / 语法 / 求值三层：字面量、优先级、结合性、短路、类型转换、
//! 成员访问、数组 / 对象字面量、两种未定义策略，以及不支持语法的报错位置。

use serde_json::{Value, json};

use super::{Expr, ExprError, Scope, truthy};
use crate::options::UndefinedBehavior;
use crate::syntax::Span;

/// 用 JSON 对象实现一个最简单的 [`Scope`]。
struct DataScope(serde_json::Map<String, Value>);

impl DataScope {
    fn new(data: Value) -> Self {
        match data {
            Value::Object(map) => Self(map),
            _ => Self(serde_json::Map::new()),
        }
    }
}

impl Scope for DataScope {
    fn get(&self, name: &str) -> Option<&Value> {
        self.0.get(name)
    }
}

fn eval_with(
    source: &str,
    data: Value,
    undefined: UndefinedBehavior,
) -> Result<Value, super::EvalError> {
    let expr =
        super::parse(source).unwrap_or_else(|error| panic!("解析 {source:?} 失败：{error:?}"));
    let scope = DataScope::new(data);
    super::eval(&expr, &scope, undefined)
}

fn eval_lenient(source: &str, data: Value) -> Value {
    eval_with(source, data, UndefinedBehavior::Lenient)
        .unwrap_or_else(|error| panic!("求值 {source:?} 失败：{error:?}"))
}

fn eval_error(source: &str) -> super::EvalError {
    match eval_with(source, json!({}), UndefinedBehavior::Lenient) {
        Ok(value) => panic!("期望 {source:?} 求值失败，实际得到 {value:?}"),
        Err(error) => error,
    }
}

fn parse_error(source: &str) -> ExprError {
    match super::parse(source) {
        Ok(expr) => panic!("期望 {source:?} 解析失败，实际得到 {expr:?}"),
        Err(error) => error,
    }
}

// ---------------------------------------------------------------------------
// 字面量
// ---------------------------------------------------------------------------

#[test]
fn evaluates_number_literals() {
    assert_eq!(eval_lenient("42", json!({})), json!(42));
    assert_eq!(eval_lenient("1.5", json!({})), json!(1.5));
    assert_eq!(eval_lenient(".5", json!({})), json!(0.5));
    assert_eq!(eval_lenient("1e3", json!({})), json!(1000));
    assert_eq!(eval_lenient("1.5E-2", json!({})), json!(0.015));
}

#[test]
fn evaluates_string_and_boolean_literals() {
    assert_eq!(eval_lenient("'hello'", json!({})), json!("hello"));
    assert_eq!(eval_lenient("\"hello\"", json!({})), json!("hello"));
    assert_eq!(eval_lenient("true", json!({})), json!(true));
    assert_eq!(eval_lenient("false", json!({})), json!(false));
}

#[test]
fn undefined_literal_is_null() {
    assert_eq!(eval_lenient("null", json!({})), Value::Null);
    assert_eq!(eval_lenient("undefined", json!({})), Value::Null);
    assert_eq!(eval_lenient("null === undefined", json!({})), json!(true));
}

#[test]
fn string_escapes() {
    assert_eq!(eval_lenient(r#""a\nb""#, json!({})), json!("a\nb"));
    assert_eq!(eval_lenient(r"'\r\t'", json!({})), json!("\r\t"));
    assert_eq!(
        eval_lenient(r"'\b\f'", json!({})),
        json!("\u{0008}\u{000C}")
    );
    assert_eq!(eval_lenient(r"'\\'", json!({})), json!("\\"));
    assert_eq!(eval_lenient(r"'\''", json!({})), json!("'"));
    assert_eq!(eval_lenient(r#"'\"'"#, json!({})), json!("\""));
    assert_eq!(eval_lenient(r"'\/'", json!({})), json!("/"));
}

#[test]
fn string_unicode_escapes() {
    assert_eq!(eval_lenient(r"'\u0041'", json!({})), json!("A"));
    assert_eq!(eval_lenient(r"'\u4F60\u597D'", json!({})), json!("你好"));
    // 代理对合成一个码点。
    assert_eq!(eval_lenient(r"'\uD83D\uDE00'", json!({})), json!("😀"));
    // 孤立代理项无法存进 Rust 字符串，降级成 U+FFFD。
    assert_eq!(eval_lenient(r"'\uD800'", json!({})), json!("\u{FFFD}"));
}

// ---------------------------------------------------------------------------
// 优先级与结合性
// ---------------------------------------------------------------------------

#[test]
fn multiplication_binds_tighter_than_addition() {
    assert_eq!(eval_lenient("1 + 2 * 3", json!({})), json!(7));
    assert_eq!(eval_lenient("2 * 3 + 1", json!({})), json!(7));
    assert_eq!(eval_lenient("2 + 8 / 4", json!({})), json!(4));
}

#[test]
fn parentheses_override_precedence() {
    assert_eq!(eval_lenient("(1 + 2) * 3", json!({})), json!(9));
    assert_eq!(eval_lenient("((1))", json!({})), json!(1));
    assert_eq!(eval_lenient("2 * (3 + 4)", json!({})), json!(14));
}

#[test]
fn relational_binds_tighter_than_equality() {
    assert_eq!(eval_lenient("1 + 2 < 4", json!({})), json!(true));
    assert_eq!(eval_lenient("1 < 2 === true", json!({})), json!(true));
    assert_eq!(eval_lenient("1 > 2 === false", json!({})), json!(true));
}

#[test]
fn and_binds_tighter_than_or() {
    assert_eq!(
        eval_lenient("true || false && false", json!({})),
        json!(true)
    );
    assert_eq!(
        eval_lenient("false && false || true", json!({})),
        json!(true)
    );
    assert_eq!(
        eval_lenient("false && (false || true)", json!({})),
        json!(false)
    );
}

#[test]
fn ternary_has_lowest_precedence() {
    assert_eq!(eval_lenient("1 || 0 ? 'a' : 'b'", json!({})), json!("a"));
    assert_eq!(eval_lenient("false ? 1 + 2 : 3 + 4", json!({})), json!(7));
}

#[test]
fn binary_operators_are_left_associative() {
    assert_eq!(eval_lenient("1 - 2 - 3", json!({})), json!(-4));
    assert_eq!(eval_lenient("2 * 3 % 4", json!({})), json!(2));
    assert_eq!(eval_lenient("8 / 4 / 2", json!({})), json!(1));
}

#[test]
fn unary_binds_tighter_than_binary() {
    assert_eq!(eval_lenient("-2 * 3", json!({})), json!(-6));
    assert_eq!(eval_lenient("!true || true", json!({})), json!(true));
    assert_eq!(eval_lenient("!false && true", json!({})), json!(true));
}

// ---------------------------------------------------------------------------
// 三元
// ---------------------------------------------------------------------------

#[test]
fn ternary_picks_branch_by_truthiness() {
    assert_eq!(eval_lenient("true ? 1 : 2", json!({})), json!(1));
    assert_eq!(eval_lenient("false ? 1 : 2", json!({})), json!(2));
    assert_eq!(eval_lenient("0 ? 'a' : 'b'", json!({})), json!("b"));
    assert_eq!(eval_lenient("'' ? 'a' : 'b'", json!({})), json!("b"));
    assert_eq!(eval_lenient("null ? 1 : 2", json!({})), json!(2));
}

#[test]
fn ternary_is_right_associative() {
    assert_eq!(
        eval_lenient("false ? 1 : true ? 2 : 3", json!({})),
        json!(2)
    );
    assert_eq!(
        eval_lenient("true ? false ? 1 : 2 : 3", json!({})),
        json!(2)
    );
}

#[test]
fn question_mark_followed_by_space_and_dot_is_still_ternary() {
    // `? .5` 不是可选链，不能被词法层误判。
    assert_eq!(eval_lenient("true ? .5 : 1", json!({})), json!(0.5));
}

// ---------------------------------------------------------------------------
// 逻辑运算：返回值本身 + 短路
// ---------------------------------------------------------------------------

#[test]
fn and_or_return_operands() {
    assert_eq!(eval_lenient("0 && 'x'", json!({})), json!(0));
    assert_eq!(eval_lenient("1 && 'x'", json!({})), json!("x"));
    assert_eq!(eval_lenient("'a' || 'b'", json!({})), json!("a"));
    assert_eq!(eval_lenient("'' || 'b'", json!({})), json!("b"));
    assert_eq!(eval_lenient("null || 0 || 'c'", json!({})), json!("c"));
    assert_eq!(eval_lenient("'yes' && 'no'", json!({})), json!("no"));
}

#[test]
fn short_circuit_skips_undefined_variable() {
    assert_eq!(
        eval_with("false && missing", json!({}), UndefinedBehavior::Error).unwrap(),
        json!(false)
    );
    assert_eq!(
        eval_with("true || missing", json!({}), UndefinedBehavior::Error).unwrap(),
        json!(true)
    );
}

#[test]
fn short_circuit_still_errors_when_right_is_evaluated() {
    assert!(eval_with("true && missing", json!({}), UndefinedBehavior::Error).is_err());
    assert!(eval_with("false || missing", json!({}), UndefinedBehavior::Error).is_err());
}

// ---------------------------------------------------------------------------
// 算术与字符串拼接
// ---------------------------------------------------------------------------

#[test]
fn adds_numbers() {
    assert_eq!(eval_lenient("1 + 2", json!({})), json!(3));
    assert_eq!(eval_lenient("1.5 + 2.5", json!({})), json!(4));
}

#[test]
fn concatenates_strings() {
    assert_eq!(eval_lenient("'a' + 'b'", json!({})), json!("ab"));
    assert_eq!(eval_lenient("'a' + ''", json!({})), json!("a"));
}

#[test]
fn string_on_either_side_forces_concatenation() {
    assert_eq!(eval_lenient("1 + 'px'", json!({})), json!("1px"));
    assert_eq!(eval_lenient("'px' + 1", json!({})), json!("px1"));
    assert_eq!(eval_lenient("1.5 + 'x'", json!({})), json!("1.5x"));
    assert_eq!(eval_lenient("'n=' + null", json!({})), json!("n=null"));
    assert_eq!(eval_lenient("'b=' + true", json!({})), json!("b=true"));
}

#[test]
fn arithmetic_coerces_operands_to_numbers() {
    assert_eq!(eval_lenient("'5' - 2", json!({})), json!(3));
    assert_eq!(eval_lenient("'5' * '2'", json!({})), json!(10));
    assert_eq!(eval_lenient("true + 1", json!({})), json!(2));
    assert_eq!(eval_lenient("null + 1", json!({})), json!(1));
}

#[test]
fn division_and_remainder() {
    assert_eq!(eval_lenient("7 / 2", json!({})), json!(3.5));
    assert_eq!(eval_lenient("7 % 4", json!({})), json!(3));
    assert_eq!(eval_lenient("-7 % 4", json!({})), json!(-3));
}

#[test]
fn division_by_zero_is_an_error() {
    let error = eval_error("1 / 0");
    assert!(error.message.contains("有限数值"), "{error:?}");
    assert!(error.message.contains("除零"), "{error:?}");
    assert!(error.message.contains("无穷"), "{error:?}");
    assert_eq!(error.offset, 0);
}

#[test]
fn nan_result_is_an_error() {
    let error = eval_error("0 / 0");
    assert!(error.message.contains("NaN"), "{error:?}");
    let error = eval_error("1 % 0");
    assert!(error.message.contains("NaN"), "{error:?}");
}

#[test]
fn overflow_is_an_error() {
    let error = eval_error("1e308 * 10");
    assert!(error.message.contains("有限数值"), "{error:?}");
    assert!(error.message.contains("无穷"), "{error:?}");
}

#[test]
fn unparseable_operand_is_an_error() {
    let error = eval_error("'abc' - 1");
    assert!(error.message.contains("无法转换为数字"), "{error:?}");
    let error = eval_error("[] + 1");
    assert!(error.message.contains("数组"), "{error:?}");
}

// ---------------------------------------------------------------------------
// 一元
// ---------------------------------------------------------------------------

#[test]
fn unary_negate_and_plus() {
    assert_eq!(eval_lenient("-5", json!({})), json!(-5));
    assert_eq!(eval_lenient("-'5'", json!({})), json!(-5));
    assert_eq!(eval_lenient("+true", json!({})), json!(1));
    assert_eq!(eval_lenient("+null", json!({})), json!(0));
    assert_eq!(eval_lenient("+-3", json!({})), json!(-3));
}

#[test]
fn unary_not() {
    assert_eq!(eval_lenient("!0", json!({})), json!(true));
    assert_eq!(eval_lenient("!''", json!({})), json!(true));
    assert_eq!(eval_lenient("!!'x'", json!({})), json!(true));
    assert_eq!(eval_lenient("![]", json!({})), json!(false));
}

// ---------------------------------------------------------------------------
// 相等
// ---------------------------------------------------------------------------

#[test]
fn strict_equality_checks_types() {
    assert_eq!(eval_lenient("1 === 1", json!({})), json!(true));
    assert_eq!(eval_lenient("1 === '1'", json!({})), json!(false));
    assert_eq!(eval_lenient("1 !== '1'", json!({})), json!(true));
    assert_eq!(eval_lenient("null === null", json!({})), json!(true));
    assert_eq!(eval_lenient("null === undefined", json!({})), json!(true));
    assert_eq!(eval_lenient("true === 1", json!({})), json!(false));
}

#[test]
fn strict_equality_compares_json_numbers_by_value() {
    // 数据里的整数与表达式里的浮点表示应当相等（JS 里数字只有一种类型）。
    assert_eq!(
        eval_lenient("count === 1", json!({ "count": 1 })),
        json!(true)
    );
    assert_eq!(
        eval_lenient("count === 1.0", json!({ "count": 1 })),
        json!(true)
    );
}

#[test]
fn loose_equality_converts_primitives() {
    assert_eq!(eval_lenient("null == undefined", json!({})), json!(true));
    assert_eq!(eval_lenient("null == 0", json!({})), json!(false));
    assert_eq!(eval_lenient("'1' == 1", json!({})), json!(true));
    assert_eq!(eval_lenient("true == 1", json!({})), json!(true));
    assert_eq!(eval_lenient("false == ''", json!({})), json!(true));
    assert_eq!(eval_lenient("'a' == 1", json!({})), json!(false));
    assert_eq!(eval_lenient("1 != 2", json!({})), json!(true));
}

// ---------------------------------------------------------------------------
// 比较
// ---------------------------------------------------------------------------

#[test]
fn compares_numbers() {
    assert_eq!(eval_lenient("1 < 2", json!({})), json!(true));
    assert_eq!(eval_lenient("2 <= 2", json!({})), json!(true));
    assert_eq!(eval_lenient("3 > 2", json!({})), json!(true));
    assert_eq!(eval_lenient("3 >= 4", json!({})), json!(false));
    assert_eq!(eval_lenient("null < 1", json!({})), json!(true));
}

#[test]
fn compares_strings_lexicographically() {
    assert_eq!(eval_lenient("'a' < 'b'", json!({})), json!(true));
    assert_eq!(eval_lenient("'abc' <= 'abc'", json!({})), json!(true));
    assert_eq!(eval_lenient("'10' < '9'", json!({})), json!(true));
}

#[test]
fn compares_mixed_types_numerically() {
    assert_eq!(eval_lenient("'10' < 9", json!({})), json!(false));
    assert_eq!(eval_lenient("'10' > 9", json!({})), json!(true));
    // 无法转成数字时与 NaN 的比较一律为 false。
    assert_eq!(eval_lenient("'abc' < 1", json!({})), json!(false));
    assert_eq!(eval_lenient("'abc' >= 1", json!({})), json!(false));
}

// ---------------------------------------------------------------------------
// 成员 / 下标访问
// ---------------------------------------------------------------------------

fn sample_data() -> Value {
    json!({
        "user": { "name": "Alice", "profile": { "city": "Paris" } },
        "items": [{ "name": "A" }, { "name": "B" }],
        "i": 1,
        "text": "hello",
        "empty": {},
        "nil": null
    })
}

#[test]
fn reads_object_members() {
    assert_eq!(eval_lenient("user.name", sample_data()), json!("Alice"));
    assert_eq!(
        eval_lenient("user.profile.city", sample_data()),
        json!("Paris")
    );
}

#[test]
fn reads_array_index_and_nested_members() {
    assert_eq!(eval_lenient("items[0].name", sample_data()), json!("A"));
    assert_eq!(eval_lenient("items[i].name", sample_data()), json!("B"));
    assert_eq!(eval_lenient("items['1'].name", sample_data()), json!("B"));
}

#[test]
fn reads_length_of_array_and_string() {
    assert_eq!(eval_lenient("items.length", sample_data()), json!(2));
    assert_eq!(eval_lenient("text.length", sample_data()), json!(5));
}

#[test]
fn string_length_counts_utf16_code_units() {
    // JS 里 `'😀'.length` 是 2（代理对占两个码元）。
    assert_eq!(eval_lenient(r"'\uD83D\uDE00'.length", json!({})), json!(2));
}

#[test]
fn reads_string_index() {
    assert_eq!(eval_lenient("text[1]", sample_data()), json!("e"));
    assert_eq!(eval_lenient("text[99]", sample_data()), Value::Null);
}

#[test]
fn out_of_bounds_index_is_null() {
    assert_eq!(eval_lenient("items[9]", sample_data()), Value::Null);
    assert_eq!(eval_lenient("items[-1]", sample_data()), Value::Null);
    assert_eq!(eval_lenient("items[1.5]", sample_data()), Value::Null);
}

#[test]
fn missing_object_key_is_null() {
    assert_eq!(eval_lenient("user.missing", sample_data()), Value::Null);
    assert_eq!(eval_lenient("empty.anything", sample_data()), Value::Null);
}

#[test]
fn null_member_is_lenient_by_default() {
    assert_eq!(eval_lenient("nil.name", sample_data()), Value::Null);
}

#[test]
fn null_member_errors_in_strict_mode() {
    let error = eval_with("nil.name", sample_data(), UndefinedBehavior::Error).unwrap_err();
    assert!(error.message.contains("null"), "{error:?}");
    assert_eq!(error.offset, 0);
}

// ---------------------------------------------------------------------------
// 变量与未定义策略
// ---------------------------------------------------------------------------

#[test]
fn looks_up_variables_from_scope() {
    let data = json!({ "name": "Fuyu", "count": 3 });
    assert_eq!(eval_lenient("name", data.clone()), json!("Fuyu"));
    assert_eq!(eval_lenient("count + 1", data), json!(4));
}

#[test]
fn missing_variable_is_null_when_lenient() {
    assert_eq!(eval_lenient("missing", json!({})), Value::Null);
    assert_eq!(
        eval_lenient("missing || 'fallback'", json!({})),
        json!("fallback")
    );
}

#[test]
fn missing_variable_errors_when_strict() {
    let error = eval_with("missing", json!({}), UndefinedBehavior::Error).unwrap_err();
    assert!(error.message.contains("未定义变量"), "{error:?}");
    assert!(error.message.contains("missing"), "{error:?}");
    assert_eq!(error.offset, 0);
}

#[test]
fn undefined_literal_behaves_like_null() {
    assert_eq!(eval_lenient("undefined + 1", json!({})), json!(1));
    assert_eq!(eval_lenient("undefined.length", json!({})), Value::Null);
}

// ---------------------------------------------------------------------------
// 数组 / 对象字面量
// ---------------------------------------------------------------------------

#[test]
fn array_literals() {
    assert_eq!(eval_lenient("[]", json!({})), json!([]));
    assert_eq!(eval_lenient("[1, 2, 3]", json!({})), json!([1, 2, 3]));
    assert_eq!(eval_lenient("[1, [2, 3]]", json!({})), json!([1, [2, 3]]));
    assert_eq!(
        eval_lenient("['a', cls]", json!({ "cls": "active" })),
        json!(["a", "active"])
    );
}

#[test]
fn array_allows_trailing_comma() {
    assert_eq!(eval_lenient("[1, 2, 3,]", json!({})), json!([1, 2, 3]));
}

#[test]
fn object_literals() {
    assert_eq!(eval_lenient("{}", json!({})), json!({}));
    assert_eq!(
        eval_lenient("{color: c}", json!({ "c": "red" })),
        json!({ "color": "red" })
    );
    assert_eq!(
        eval_lenient("{color: c, size: 12}", json!({ "c": "red" })),
        json!({ "color": "red", "size": 12 })
    );
}

#[test]
fn object_shorthand() {
    assert_eq!(
        eval_lenient("{ color }", json!({ "color": "red" })),
        json!({ "color": "red" })
    );
    assert_eq!(
        eval_lenient("{ color, size }", json!({ "color": "red", "size": 12 })),
        json!({ "color": "red", "size": 12 })
    );
}

#[test]
fn object_allows_trailing_comma() {
    assert_eq!(
        eval_lenient("{ color: 'red', }", json!({})),
        json!({ "color": "red" })
    );
}

#[test]
fn object_keys_can_be_strings_or_numbers() {
    assert_eq!(
        eval_lenient("{'line-height': 1.5, 2: 'two'}", json!({})),
        json!({ "line-height": 1.5, "2": "two" })
    );
}

#[test]
fn keyword_object_keys_and_members_are_allowed() {
    // JS 里关键字可以当属性名（`{true: 1}`、`obj.null` 都合法）。
    assert_eq!(
        eval_lenient("{true: 1, null: 2}", json!({})),
        json!({ "true": 1, "null": 2 })
    );
    assert_eq!(eval_lenient("{true: 1}.true", json!({})), json!(1));
}

#[test]
fn nested_object_literal_can_be_accessed() {
    assert_eq!(eval_lenient("{a: {b: [1, 2]}}.a.b[1]", json!({})), json!(2));
}

// ---------------------------------------------------------------------------
// `{{ }}` 里的常见形状
// ---------------------------------------------------------------------------

#[test]
fn common_template_expression_shapes() {
    let data = json!({
        "index": 0,
        "user": { "name": "Alice" },
        "items": [{ "name": "A" }],
        "size": 12,
        "flag": true
    });
    assert_eq!(eval_lenient("index + 1", data.clone()), json!(1));
    assert_eq!(eval_lenient("user.name", data.clone()), json!("Alice"));
    assert_eq!(eval_lenient("items[0].name", data.clone()), json!("A"));
    assert_eq!(eval_lenient("size + 'px'", data.clone()), json!("12px"));
    assert_eq!(eval_lenient("flag ? 'a' : 'b'", data), json!("a"));
}

#[test]
fn class_and_style_literal_shapes() {
    let data = json!({ "cls": "active", "c": "red", "size": 12 });
    assert_eq!(
        eval_lenient("['base', cls]", data.clone()),
        json!(["base", "active"])
    );
    assert_eq!(
        eval_lenient("{ color: c, fontSize: size + 'px' }", data),
        json!({ "color": "red", "fontSize": "12px" })
    );
}

// ---------------------------------------------------------------------------
// 真值
// ---------------------------------------------------------------------------

#[test]
fn truthy_values() {
    assert!(truthy(&json!(true)));
    assert!(truthy(&json!(1)));
    assert!(truthy(&json!(-1)));
    assert!(truthy(&json!("0")));
    assert!(truthy(&json!([])));
    assert!(truthy(&json!({})));
}

#[test]
fn falsy_values() {
    assert!(!truthy(&Value::Null));
    assert!(!truthy(&json!(false)));
    assert!(!truthy(&json!(0)));
    assert!(!truthy(&json!(0.0)));
    assert!(!truthy(&json!("")));
}

// ---------------------------------------------------------------------------
// AST 位置
// ---------------------------------------------------------------------------

#[test]
fn ast_spans_are_byte_offsets() {
    assert_eq!(
        super::parse("user.name").unwrap(),
        Expr::Member {
            object: Box::new(Expr::Var {
                name: "user".to_owned(),
                span: Span::new(0, 4),
            }),
            property: Box::new(Expr::Literal {
                value: Value::String("name".to_owned()),
                span: Span::new(5, 9),
            }),
            span: Span::new(0, 9),
        }
    );
    assert_eq!(
        super::parse(" 1 ").unwrap(),
        Expr::Literal {
            value: json!(1),
            span: Span::new(1, 2),
        }
    );
}

// ---------------------------------------------------------------------------
// 语法错误：消息 + 位置
// ---------------------------------------------------------------------------

#[test]
fn empty_expression_is_an_error() {
    let error = parse_error("");
    assert_eq!(error.offset, 0);
    assert!(error.message.contains("表达式为空"), "{error:?}");
}

#[test]
fn unterminated_string_reports_its_start() {
    let error = parse_error("'abc");
    assert_eq!(error.offset, 0);
    assert!(error.message.contains("未闭合"), "{error:?}");
}

#[test]
fn trailing_tokens_report_their_position() {
    let error = parse_error("1 2");
    assert_eq!(error.offset, 2);
    assert!(error.message.contains("多余内容"), "{error:?}");
}

#[test]
fn unexpected_eof_reports_end_position() {
    let error = parse_error("1 +");
    assert_eq!(error.offset, 3);
    assert!(error.message.contains("意外结束"), "{error:?}");
}

#[test]
fn missing_closing_paren() {
    let error = parse_error("(1 + 2");
    assert_eq!(error.offset, 6);
    assert!(error.message.contains("括号未闭合"), "{error:?}");
}

#[test]
fn function_call_is_unsupported() {
    let error = parse_error("foo(1)");
    assert_eq!(error.offset, 0);
    assert!(error.message.contains("不支持函数调用"), "{error:?}");
}

#[test]
fn method_call_is_unsupported() {
    let error = parse_error("user.name()");
    assert_eq!(error.offset, 9);
    assert!(error.message.contains("不支持函数调用"), "{error:?}");
}

#[test]
fn assignment_is_unsupported() {
    let error = parse_error("a = 1");
    assert_eq!(error.offset, 2);
    assert!(error.message.contains("赋值"), "{error:?}");
}

#[test]
fn arrow_function_is_unsupported() {
    let error = parse_error("x => x");
    assert_eq!(error.offset, 2);
    assert!(error.message.contains("箭头函数"), "{error:?}");
}

#[test]
fn optional_chain_is_unsupported() {
    let error = parse_error("user?.name");
    assert_eq!(error.offset, 4);
    assert!(error.message.contains("可选链"), "{error:?}");
}

#[test]
fn template_string_is_unsupported() {
    let error = parse_error("`abc`");
    assert_eq!(error.offset, 0);
    assert!(error.message.contains("模板字符串"), "{error:?}");
}

#[test]
fn new_expression_is_unsupported() {
    let error = parse_error("new Foo()");
    assert_eq!(error.offset, 0);
    assert!(error.message.contains("new"), "{error:?}");
}

#[test]
fn computed_object_key_is_unsupported() {
    let error = parse_error("{[k]: 1}");
    assert_eq!(error.offset, 1);
    assert!(error.message.contains("计算键"), "{error:?}");
}

#[test]
fn invalid_escape_reports_the_backslash() {
    let error = parse_error(r"'\x'");
    assert_eq!(error.offset, 1);
    assert!(error.message.contains("转义"), "{error:?}");
}

#[test]
fn number_literal_overflow_is_an_error() {
    let error = parse_error("1e400");
    assert_eq!(error.offset, 0);
    assert!(error.message.contains("超出"), "{error:?}");
}

#[test]
fn bitwise_operators_are_unsupported() {
    let error = parse_error("a & b");
    assert_eq!(error.offset, 2);
    assert!(error.message.contains("按位"), "{error:?}");
}

#[test]
fn nullish_coalescing_is_unsupported() {
    let error = parse_error("a ?? b");
    assert_eq!(error.offset, 2);
    assert!(error.message.contains("空值合并"), "{error:?}");
}

#[test]
fn regex_literal_is_unsupported() {
    let error = parse_error("/abc/");
    assert_eq!(error.offset, 0);
    assert!(error.message.contains("正则"), "{error:?}");
}
