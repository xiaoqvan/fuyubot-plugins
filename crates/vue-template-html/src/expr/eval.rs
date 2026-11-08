//! 求值：AST + 作用域 → [`serde_json::Value`]。
//!
//! 语义对齐 JavaScript / Vue，见 [`super`] 的模块文档。两个刻意的取舍：
//!
//! - **JSON 值没有引用身份**：`===` 对数组 / 对象按结构比较（JS 比较的是引用），
//!   这样 `selected === current` 在数据来自 JSON 时才有意义。
//! - **字符串的 `.length` / 下标按 UTF-16 码元算**（与 JS 一致）；孤立代理项在 Rust
//!   字符串里表示不了，取下标时降级成 `U+FFFD`。

use serde_json::{Map, Number, Value};

use super::{BinaryOp, EvalError, Expr, Scope, UnaryOp};
use crate::options::UndefinedBehavior;

/// JS 真值判断：`false` / `0` / `""` / `null` / `undefined` 为假，其余为真。
///
/// 注意空数组 `[]` 与空对象 `{}` 在 JS 里是**真值**，这里保持一致。
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|number| number != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// 对一棵已经解析好的表达式求值。
pub(super) fn eval(
    expr: &Expr,
    scope: &dyn Scope,
    undefined: UndefinedBehavior,
) -> Result<Value, EvalError> {
    match expr {
        Expr::Literal { value, .. } => Ok(value.clone()),
        Expr::Var { name, span } => match scope.get(name) {
            Some(value) => Ok(value.clone()),
            None => match undefined {
                UndefinedBehavior::Lenient => Ok(Value::Null),
                UndefinedBehavior::Error => {
                    Err(EvalError::new(span.start, format!("未定义变量 `{name}`")))
                }
            },
        },
        Expr::Member {
            object,
            property,
            span,
        } => {
            let target = eval(object, scope, undefined)?;
            if target.is_null() {
                return match undefined {
                    UndefinedBehavior::Lenient => Ok(Value::Null),
                    UndefinedBehavior::Error => Err(EvalError::new(
                        span.start,
                        format!("无法读取 null 的成员 `{}`", describe_property(property)),
                    )),
                };
            }
            let key = eval(property, scope, undefined)?;
            Ok(member(&target, &key))
        }
        Expr::Unary { op, operand, span } => {
            let value = eval(operand, scope, undefined)?;
            match op {
                UnaryOp::Not => Ok(Value::Bool(!truthy(&value))),
                UnaryOp::Negate => {
                    let number = number_of(&value, span.start, "一元 `-`")?;
                    finite_value(-number, span.start, "一元 `-`")
                }
                UnaryOp::Plus => {
                    let number = number_of(&value, span.start, "一元 `+`")?;
                    finite_value(number, span.start, "一元 `+`")
                }
            }
        }
        Expr::Binary {
            op,
            left,
            right,
            span,
        } => match op {
            // 短路：右侧在需要时才求值，未定义的变量也就不会报错。
            BinaryOp::And => {
                let left = eval(left, scope, undefined)?;
                if truthy(&left) {
                    eval(right, scope, undefined)
                } else {
                    Ok(left)
                }
            }
            BinaryOp::Or => {
                let left = eval(left, scope, undefined)?;
                if truthy(&left) {
                    Ok(left)
                } else {
                    eval(right, scope, undefined)
                }
            }
            _ => {
                let left = eval(left, scope, undefined)?;
                let right = eval(right, scope, undefined)?;
                binary(*op, &left, &right, span.start)
            }
        },
        Expr::Ternary {
            condition,
            consequent,
            alternate,
            ..
        } => {
            let condition = eval(condition, scope, undefined)?;
            if truthy(&condition) {
                eval(consequent, scope, undefined)
            } else {
                eval(alternate, scope, undefined)
            }
        }
        Expr::Array { items, .. } => {
            let mut values = Vec::with_capacity(items.len());
            for item in items {
                values.push(eval(item, scope, undefined)?);
            }
            Ok(Value::Array(values))
        }
        Expr::Object { entries, .. } => {
            let mut map = Map::new();
            for (key, value) in entries {
                map.insert(key.clone(), eval(value, scope, undefined)?);
            }
            Ok(Value::Object(map))
        }
    }
}

fn binary(op: BinaryOp, left: &Value, right: &Value, offset: usize) -> Result<Value, EvalError> {
    match op {
        BinaryOp::Add => add(left, right, offset),
        BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Rem => {
            let left = number_of(left, offset, op_text(op))?;
            let right = number_of(right, offset, op_text(op))?;
            let result = match op {
                BinaryOp::Sub => left - right,
                BinaryOp::Mul => left * right,
                BinaryOp::Div => left / right,
                BinaryOp::Rem => left % right,
                _ => unreachable!("上面的分支已经限定为四则运算"),
            };
            finite_value(result, offset, op_text(op))
        }
        BinaryOp::EqStrict => Ok(Value::Bool(strict_eq(left, right))),
        BinaryOp::NeStrict => Ok(Value::Bool(!strict_eq(left, right))),
        BinaryOp::EqLoose => Ok(Value::Bool(loose_eq(left, right))),
        BinaryOp::NeLoose => Ok(Value::Bool(!loose_eq(left, right))),
        BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => {
            Ok(Value::Bool(compare(op, left, right)))
        }
        BinaryOp::And | BinaryOp::Or => unreachable!("短路运算在 eval 里已经处理"),
    }
}

/// `+`：任一侧是字符串就拼接，否则数值相加。
fn add(left: &Value, right: &Value, offset: usize) -> Result<Value, EvalError> {
    if matches!(left, Value::String(_)) || matches!(right, Value::String(_)) {
        return Ok(Value::String(format!(
            "{}{}",
            to_js_string(left),
            to_js_string(right)
        )));
    }
    let left = number_of(left, offset, "`+`")?;
    let right = number_of(right, offset, "`+`")?;
    finite_value(left + right, offset, "`+`")
}

/// 把值转成数字；转不了返回 `None`（调用方负责报错）。
fn to_number(value: &Value) -> Option<f64> {
    match value {
        Value::Null => Some(0.0),
        Value::Bool(flag) => Some(if *flag { 1.0 } else { 0.0 }),
        Value::Number(number) => number.as_f64(),
        Value::String(text) => string_to_number(text),
        Value::Array(_) | Value::Object(_) => None,
    }
}

/// 字符串 → 数字，尽量贴近 JS 的 `Number(string)`：空白串是 `0`，其余按十进制解析。
fn string_to_number(text: &str) -> Option<f64> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Some(0.0);
    }
    let value: f64 = trimmed.parse().ok()?;
    // Rust 的 `f64::from_str` 接受 `inf` / `NaN`，JS 的 `Number()` 不会把它们当普通
    // 数字字符串；统一当作「转换失败」。
    value.is_finite().then_some(value)
}

fn number_of(value: &Value, offset: usize, what: &str) -> Result<f64, EvalError> {
    to_number(value).ok_or_else(|| {
        EvalError::new(
            offset,
            format!("{what} 的操作数无法转换为数字：{}", describe_value(value)),
        )
    })
}

/// 数字 → JSON 值。
///
/// 整数值存成整数形式（`Number::from(i64)`），这样 `1 + 2` 的结果就是 `3` 而不是
/// `3.0`：插值输出、`serde_json::Value` 比较都更符合直觉，也与 JSON 数据里的整数
/// 表示一致。超出 `i64` 范围的整数值退回 `f64`。
pub(super) fn number_value(value: f64) -> Value {
    if value == 0.0 {
        // JS 的 -0 在模板里没有可观察差异（我们拒绝非有限结果，不会出现 1/-0），
        // 统一成整数 0。
        return Value::Number(Number::from(0));
    }
    // 2^63：`i64` 的上下界，闭开区间避免 `as i64` 在边界上饱和。
    const I64_LIMIT: f64 = 9_223_372_036_854_775_808.0;
    if value.fract() == 0.0 && (-I64_LIMIT..I64_LIMIT).contains(&value) {
        return Value::Number(Number::from(value as i64));
    }
    Value::Number(Number::from_f64(value).expect("有限 f64 一定能转成 JSON 数字"))
}

/// 数字 → 字符串，按 JS 的 `String(number)` 习惯：整数不带 `.0`。
///
/// 极端量级（`>= 1e21` / `< 1e-6`）下 JS 会改用指数形式，这里仍是十进制；模板里
/// 几乎不会遇到，注释在此说明。
pub(super) fn number_to_string(value: f64) -> String {
    if value == 0.0 {
        "0".to_owned()
    } else if value.fract() == 0.0 && value.abs() < 1e21 {
        format!("{value:.0}")
    } else {
        format!("{value}")
    }
}

/// 字符串拼接时的值 → 字符串。
fn to_js_string(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number
            .as_f64()
            .map(number_to_string)
            .unwrap_or_else(|| number.to_string()),
        Value::String(text) => text.clone(),
        // JS 的 ToPrimitive（`[1,2] + ''` → `"1,2"`）在模板里没有实际用途，
        // 这里退化成 JSON 文本，保证结果可预测。
        Value::Array(_) | Value::Object(_) => value.to_string(),
    }
}

fn strict_eq(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(left), Value::Bool(right)) => left == right,
        // JSON 里整数与浮点可能用不同的内部表示（`1` / `1.0`），按数值比较。
        (Value::Number(left), Value::Number(right)) => left.as_f64() == right.as_f64(),
        (Value::String(left), Value::String(right)) => left == right,
        // JSON 值没有引用身份，数组 / 对象按结构比较（与 JS 的引用比较不同）。
        (Value::Array(_), Value::Array(_)) | (Value::Object(_), Value::Object(_)) => left == right,
        _ => false,
    }
}

/// 最小限度的宽松相等：`null == undefined`、数字与字符串 / 布尔互转。
fn loose_eq(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Null, Value::Null) => true,
        // JS 里 `null == 0` / `null == ""` 都是 false。
        (Value::Null, _) | (_, Value::Null) => false,
        (Value::Bool(left), Value::Bool(right)) => left == right,
        (Value::Number(left), Value::Number(right)) => left.as_f64() == right.as_f64(),
        (Value::String(left), Value::String(right)) => left == right,
        (Value::Bool(flag), other) | (other, Value::Bool(flag)) => {
            let expected = if *flag { 1.0 } else { 0.0 };
            to_number(other).is_some_and(|number| number == expected)
        }
        (Value::Number(number), Value::String(text))
        | (Value::String(text), Value::Number(number)) => {
            string_to_number(text).is_some_and(|parsed| number.as_f64() == Some(parsed))
        }
        (Value::Array(_), Value::Array(_)) | (Value::Object(_), Value::Object(_)) => left == right,
        _ => false,
    }
}

/// `<` / `<=` / `>` / `>=`：同为字符串按字典序，否则数值比较。
fn compare(op: BinaryOp, left: &Value, right: &Value) -> bool {
    let ordering = match (left, right) {
        (Value::String(left), Value::String(right)) => Some(left.cmp(right)),
        _ => match (to_number(left), to_number(right)) {
            (Some(left), Some(right)) => left.partial_cmp(&right),
            _ => None,
        },
    };
    let Some(ordering) = ordering else {
        // JS 里与 NaN 的比较一律为 false。
        return false;
    };
    match op {
        BinaryOp::Lt => ordering.is_lt(),
        BinaryOp::Le => ordering.is_le(),
        BinaryOp::Gt => ordering.is_gt(),
        BinaryOp::Ge => ordering.is_ge(),
        _ => unreachable!("compare 只处理比较运算"),
    }
}

/// 成员 / 下标访问。
fn member(target: &Value, key: &Value) -> Value {
    match target {
        Value::String(text) => {
            if is_length(key) {
                // JS 的 `length` 数的是 UTF-16 码元（emoji 算 2）。
                return number_value(text.encode_utf16().count() as f64);
            }
            index_of(key)
                .and_then(|index| text.encode_utf16().nth(index))
                .map_or(Value::Null, |unit| {
                    // 孤立代理项在 Rust 字符串里表示不了，取下标时用 U+FFFD 代替。
                    match char::from_u32(u32::from(unit)) {
                        Some(ch) => Value::String(ch.to_string()),
                        None => Value::String("\u{FFFD}".to_owned()),
                    }
                })
        }
        Value::Array(items) => {
            if is_length(key) {
                return number_value(items.len() as f64);
            }
            index_of(key)
                .and_then(|index| items.get(index).cloned())
                .unwrap_or(Value::Null)
        }
        Value::Object(map) => property_key(key)
            .and_then(|key| map.get(&key).cloned())
            .unwrap_or(Value::Null),
        // 数字 / 布尔没有可访问的属性（JS 里返回 undefined）。
        _ => Value::Null,
    }
}

fn is_length(key: &Value) -> bool {
    matches!(key, Value::String(text) if text == "length")
}

/// 数组 / 字符串下标：只接受非负整数（数字或规范十进制字符串）。
fn index_of(key: &Value) -> Option<usize> {
    match key {
        Value::Number(number) => {
            let value = number.as_f64()?;
            if value >= 0.0 && value.fract() == 0.0 && value <= usize::MAX as f64 {
                Some(value as usize)
            } else {
                None
            }
        }
        Value::String(text) => {
            let index: usize = text.parse().ok()?;
            // `arr["01"]` 在 JS 里是普通属性（返回 undefined），不是下标。
            (index.to_string() == *text).then_some(index)
        }
        _ => None,
    }
}

/// 对象属性名：JS 的 `ToPropertyKey` 只对原始值有意义。
fn property_key(key: &Value) -> Option<String> {
    match key {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => number.as_f64().map(number_to_string),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Null => Some("null".to_owned()),
        Value::Array(_) | Value::Object(_) => None,
    }
}

fn describe_property(property: &Expr) -> String {
    match property {
        Expr::Literal {
            value: Value::String(name),
            ..
        } => name.clone(),
        _ => "…".to_owned(),
    }
}

fn describe_value(value: &Value) -> String {
    match value {
        Value::String(text) => format!("字符串 {text:?}"),
        Value::Array(_) => "数组".to_owned(),
        Value::Object(_) => "对象".to_owned(),
        other => other.to_string(),
    }
}

fn op_text(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Add => "`+`",
        BinaryOp::Sub => "`-`",
        BinaryOp::Mul => "`*`",
        BinaryOp::Div => "`/`",
        BinaryOp::Rem => "`%`",
        BinaryOp::EqStrict => "`===`",
        BinaryOp::NeStrict => "`!==`",
        BinaryOp::EqLoose => "`==`",
        BinaryOp::NeLoose => "`!=`",
        BinaryOp::Lt => "`<`",
        BinaryOp::Le => "`<=`",
        BinaryOp::Gt => "`>`",
        BinaryOp::Ge => "`>=`",
        BinaryOp::And => "`&&`",
        BinaryOp::Or => "`||`",
    }
}

/// 有限性检查：`0/0`、溢出、除零得到的 `inf` 都算错误。
fn finite_value(value: f64, offset: usize, what: &str) -> Result<Value, EvalError> {
    if value.is_finite() {
        return Ok(number_value(value));
    }
    let detail = if value.is_nan() {
        "NaN（例如 0/0）"
    } else if value.is_sign_positive() {
        "正无穷大（除零或溢出）"
    } else {
        "负无穷大（除零或溢出）"
    };
    Err(EvalError::new(
        offset,
        format!("{what} 的运算结果不是有限数值：{detail}"),
    ))
}
