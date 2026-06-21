// step_guard.rs — Guard expression parser and evaluator (v0.17.0.4.5).
//
// Parses and evaluates simple boolean expressions over WorkflowContext fields.
//
// Supported syntax:
//   context.field OP literal         comparison
//   expr and expr                     boolean and
//   expr or expr                      boolean or
//   not expr                          boolean not
//
// Operator precedence (highest first): not > and > or
//
// Literals: integers (3), floats (3.14), quoted strings ("foo"), booleans (true/false)
// Field refs: context.field_name  →  looks up WorkflowContext.fields["field_name"]

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::step_context::WorkflowContext;

// ── GuardExpr ─────────────────────────────────────────────────────────────────

/// A parsed and validated guard expression.
///
/// Constructed via `GuardExpr::parse(s)` — validates the expression at parse
/// time so execution never fails on syntax. Serializes as a raw string.
///
/// YAML example:
/// ```yaml
/// guard: "context.rework_count < 3"
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct GuardExpr(String);

impl GuardExpr {
    /// Parse and validate a guard expression string.
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err("guard expression cannot be empty".to_string());
        }
        // Validate by tokenizing and attempting a parse.
        let tokens = tokenize(s)?;
        let mut pos = 0;
        parse_or_expr(&tokens, &mut pos)?;
        if pos < tokens.len() {
            return Err(format!(
                "unexpected token '{}' at position {}",
                tokens[pos].display(),
                pos
            ));
        }
        Ok(GuardExpr(s.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Evaluate this guard against the given context.
    ///
    /// Returns `false` on any evaluation error (missing field, type mismatch).
    pub fn evaluate(&self, ctx: &WorkflowContext) -> bool {
        evaluate_guard(self.0.trim(), ctx).unwrap_or_default()
    }
}

// Serialize as a plain string.
impl Serialize for GuardExpr {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

// Deserialize by calling GuardExpr::parse.
impl<'de> Deserialize<'de> for GuardExpr {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        GuardExpr::parse(&s).map_err(serde::de::Error::custom)
    }
}

// ── Token ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Token {
    ContextField(String), // context.field_name
    IntLit(i64),
    FloatLit(f64),
    StrLit(String),
    BoolLit(bool),
    Lt,
    Gt,
    LtEq,
    GtEq,
    Eq,
    NotEq,
    And,
    Or,
    Not,
    LParen,
    RParen,
}

impl Token {
    fn display(&self) -> String {
        match self {
            Token::ContextField(f) => format!("context.{}", f),
            Token::IntLit(n) => n.to_string(),
            Token::FloatLit(f) => f.to_string(),
            Token::StrLit(s) => format!("\"{}\"", s),
            Token::BoolLit(b) => b.to_string(),
            Token::Lt => "<".to_string(),
            Token::Gt => ">".to_string(),
            Token::LtEq => "<=".to_string(),
            Token::GtEq => ">=".to_string(),
            Token::Eq => "==".to_string(),
            Token::NotEq => "!=".to_string(),
            Token::And => "and".to_string(),
            Token::Or => "or".to_string(),
            Token::Not => "not".to_string(),
            Token::LParen => "(".to_string(),
            Token::RParen => ")".to_string(),
        }
    }
}

// ── Tokenizer ─────────────────────────────────────────────────────────────────

fn tokenize(input: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        match chars[i] {
            ' ' | '\t' | '\n' | '\r' => {
                i += 1;
            }
            '(' => {
                tokens.push(Token::LParen);
                i += 1;
            }
            ')' => {
                tokens.push(Token::RParen);
                i += 1;
            }
            '<' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token::LtEq);
                    i += 2;
                } else {
                    tokens.push(Token::Lt);
                    i += 1;
                }
            }
            '>' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token::GtEq);
                    i += 2;
                } else {
                    tokens.push(Token::Gt);
                    i += 1;
                }
            }
            '=' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token::Eq);
                    i += 2;
                } else {
                    return Err(format!(
                        "unexpected '=' at position {} (did you mean '=='?)",
                        i
                    ));
                }
            }
            '!' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(Token::NotEq);
                    i += 2;
                } else {
                    return Err(format!(
                        "unexpected '!' at position {} (did you mean '!='?)",
                        i
                    ));
                }
            }
            '"' => {
                // Quoted string literal.
                i += 1;
                let start = i;
                while i < chars.len() && chars[i] != '"' {
                    i += 1;
                }
                if i >= chars.len() {
                    return Err("unterminated string literal".to_string());
                }
                let s: String = chars[start..i].iter().collect();
                tokens.push(Token::StrLit(s));
                i += 1; // skip closing quote
            }
            c if c.is_ascii_digit()
                || (c == '-' && i + 1 < chars.len() && chars[i + 1].is_ascii_digit()) =>
            {
                let start = i;
                if chars[i] == '-' {
                    i += 1;
                }
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
                let is_float = i < chars.len() && chars[i] == '.';
                if is_float {
                    i += 1;
                    while i < chars.len() && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                    let s: String = chars[start..i].iter().collect();
                    let f: f64 = s.parse().map_err(|_| format!("invalid float '{}'", s))?;
                    tokens.push(Token::FloatLit(f));
                } else {
                    let s: String = chars[start..i].iter().collect();
                    let n: i64 = s.parse().map_err(|_| format!("invalid integer '{}'", s))?;
                    tokens.push(Token::IntLit(n));
                }
            }
            c if c.is_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len()
                    && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '.')
                {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                let token = match word.as_str() {
                    "and" => Token::And,
                    "or" => Token::Or,
                    "not" => Token::Not,
                    "true" => Token::BoolLit(true),
                    "false" => Token::BoolLit(false),
                    s if s.starts_with("context.") => {
                        let field = s["context.".len()..].to_string();
                        if field.is_empty() {
                            return Err("context field name cannot be empty".to_string());
                        }
                        Token::ContextField(field)
                    }
                    other => {
                        return Err(format!(
                            "unknown identifier '{}' (context fields must be prefixed with 'context.')",
                            other
                        ));
                    }
                };
                tokens.push(token);
            }
            other => {
                return Err(format!("unexpected character '{}'", other));
            }
        }
    }

    Ok(tokens)
}

// ── Parser (validation only — returns () on success) ─────────────────────────

fn parse_or_expr(tokens: &[Token], pos: &mut usize) -> Result<(), String> {
    parse_and_expr(tokens, pos)?;
    while *pos < tokens.len() && tokens[*pos] == Token::Or {
        *pos += 1;
        parse_and_expr(tokens, pos)?;
    }
    Ok(())
}

fn parse_and_expr(tokens: &[Token], pos: &mut usize) -> Result<(), String> {
    parse_not_expr(tokens, pos)?;
    while *pos < tokens.len() && tokens[*pos] == Token::And {
        *pos += 1;
        parse_not_expr(tokens, pos)?;
    }
    Ok(())
}

fn parse_not_expr(tokens: &[Token], pos: &mut usize) -> Result<(), String> {
    if *pos < tokens.len() && tokens[*pos] == Token::Not {
        *pos += 1;
        parse_not_expr(tokens, pos)
    } else {
        parse_compare_expr(tokens, pos)
    }
}

fn parse_compare_expr(tokens: &[Token], pos: &mut usize) -> Result<(), String> {
    parse_value(tokens, pos)?;
    if *pos < tokens.len() {
        match &tokens[*pos] {
            Token::Lt | Token::Gt | Token::LtEq | Token::GtEq | Token::Eq | Token::NotEq => {
                *pos += 1;
                parse_value(tokens, pos)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn parse_value(tokens: &[Token], pos: &mut usize) -> Result<(), String> {
    if *pos >= tokens.len() {
        return Err("expected value but found end of expression".to_string());
    }
    match &tokens[*pos] {
        Token::ContextField(_)
        | Token::IntLit(_)
        | Token::FloatLit(_)
        | Token::StrLit(_)
        | Token::BoolLit(_) => {
            *pos += 1;
            Ok(())
        }
        Token::LParen => {
            *pos += 1;
            parse_or_expr(tokens, pos)?;
            if *pos >= tokens.len() || tokens[*pos] != Token::RParen {
                return Err("expected ')' to close parenthesized expression".to_string());
            }
            *pos += 1;
            Ok(())
        }
        other => Err(format!("expected value, got '{}'", other.display())),
    }
}

// ── Evaluator ─────────────────────────────────────────────────────────────────

/// Evaluated value during guard expression evaluation.
#[derive(Debug, Clone)]
enum EvalValue {
    Number(f64),
    Str(String),
    Bool(bool),
    Null,
}

impl EvalValue {
    fn as_bool(&self) -> bool {
        match self {
            EvalValue::Bool(b) => *b,
            EvalValue::Number(n) => *n != 0.0,
            EvalValue::Str(s) => !s.is_empty(),
            EvalValue::Null => false,
        }
    }
}

fn evaluate_guard(expr: &str, ctx: &WorkflowContext) -> Result<bool, String> {
    let tokens = tokenize(expr)?;
    let mut pos = 0;
    let result = eval_or_expr(&tokens, &mut pos, ctx)?;
    Ok(result.as_bool())
}

fn eval_or_expr(
    tokens: &[Token],
    pos: &mut usize,
    ctx: &WorkflowContext,
) -> Result<EvalValue, String> {
    let mut left = eval_and_expr(tokens, pos, ctx)?;
    while *pos < tokens.len() && tokens[*pos] == Token::Or {
        *pos += 1;
        let right = eval_and_expr(tokens, pos, ctx)?;
        left = EvalValue::Bool(left.as_bool() || right.as_bool());
    }
    Ok(left)
}

fn eval_and_expr(
    tokens: &[Token],
    pos: &mut usize,
    ctx: &WorkflowContext,
) -> Result<EvalValue, String> {
    let mut left = eval_not_expr(tokens, pos, ctx)?;
    while *pos < tokens.len() && tokens[*pos] == Token::And {
        *pos += 1;
        let right = eval_not_expr(tokens, pos, ctx)?;
        left = EvalValue::Bool(left.as_bool() && right.as_bool());
    }
    Ok(left)
}

fn eval_not_expr(
    tokens: &[Token],
    pos: &mut usize,
    ctx: &WorkflowContext,
) -> Result<EvalValue, String> {
    if *pos < tokens.len() && tokens[*pos] == Token::Not {
        *pos += 1;
        let v = eval_not_expr(tokens, pos, ctx)?;
        Ok(EvalValue::Bool(!v.as_bool()))
    } else {
        eval_compare_expr(tokens, pos, ctx)
    }
}

fn eval_compare_expr(
    tokens: &[Token],
    pos: &mut usize,
    ctx: &WorkflowContext,
) -> Result<EvalValue, String> {
    let left = eval_value(tokens, pos, ctx)?;

    if *pos >= tokens.len() {
        return Ok(left);
    }

    let op = match &tokens[*pos] {
        Token::Lt => "<",
        Token::Gt => ">",
        Token::LtEq => "<=",
        Token::GtEq => ">=",
        Token::Eq => "==",
        Token::NotEq => "!=",
        _ => return Ok(left),
    };
    *pos += 1;

    let right = eval_value(tokens, pos, ctx)?;

    let result = compare_values(&left, op, &right)?;
    Ok(EvalValue::Bool(result))
}

fn compare_values(left: &EvalValue, op: &str, right: &EvalValue) -> Result<bool, String> {
    match (left, right) {
        (EvalValue::Number(l), EvalValue::Number(r)) => Ok(match op {
            "<" => l < r,
            ">" => l > r,
            "<=" => l <= r,
            ">=" => l >= r,
            "==" => (l - r).abs() < f64::EPSILON,
            "!=" => (l - r).abs() >= f64::EPSILON,
            _ => return Err(format!("unknown op '{}'", op)),
        }),
        (EvalValue::Str(l), EvalValue::Str(r)) => Ok(match op {
            "==" => l == r,
            "!=" => l != r,
            "<" => l < r,
            ">" => l > r,
            "<=" => l <= r,
            ">=" => l >= r,
            _ => return Err(format!("unknown op '{}'", op)),
        }),
        (EvalValue::Bool(l), EvalValue::Bool(r)) => Ok(match op {
            "==" => l == r,
            "!=" => l != r,
            _ => {
                return Err(format!(
                    "operator '{}' is not supported for boolean values",
                    op
                ))
            }
        }),
        (EvalValue::Null, _) | (_, EvalValue::Null) => Ok(match op {
            "==" => false,
            "!=" => true,
            _ => false,
        }),
        _ => Err(format!(
            "cannot compare {:?} and {:?} with '{}'",
            left, right, op
        )),
    }
}

fn eval_value(
    tokens: &[Token],
    pos: &mut usize,
    ctx: &WorkflowContext,
) -> Result<EvalValue, String> {
    if *pos >= tokens.len() {
        return Err("expected value but reached end of expression".to_string());
    }

    match &tokens[*pos].clone() {
        Token::ContextField(field) => {
            *pos += 1;
            match ctx.get(field) {
                Some(serde_json::Value::Number(n)) => {
                    Ok(EvalValue::Number(n.as_f64().unwrap_or(0.0)))
                }
                Some(serde_json::Value::String(s)) => Ok(EvalValue::Str(s.clone())),
                Some(serde_json::Value::Bool(b)) => Ok(EvalValue::Bool(*b)),
                Some(serde_json::Value::Null) | None => Ok(EvalValue::Null),
                Some(other) => Ok(EvalValue::Str(other.to_string())),
            }
        }
        Token::IntLit(n) => {
            let val = *n as f64;
            *pos += 1;
            Ok(EvalValue::Number(val))
        }
        Token::FloatLit(f) => {
            let val = *f;
            *pos += 1;
            Ok(EvalValue::Number(val))
        }
        Token::StrLit(s) => {
            let val = s.clone();
            *pos += 1;
            Ok(EvalValue::Str(val))
        }
        Token::BoolLit(b) => {
            let val = *b;
            *pos += 1;
            Ok(EvalValue::Bool(val))
        }
        Token::LParen => {
            *pos += 1;
            let v = eval_or_expr(tokens, pos, ctx)?;
            if *pos >= tokens.len() || tokens[*pos] != Token::RParen {
                return Err("expected ')'".to_string());
            }
            *pos += 1;
            Ok(v)
        }
        other => Err(format!("expected value, got '{}'", other.display())),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with(pairs: &[(&str, serde_json::Value)]) -> WorkflowContext {
        let mut ctx = WorkflowContext::new();
        for (k, v) in pairs {
            ctx.fields.insert(k.to_string(), v.clone());
        }
        ctx
    }

    // ── parse ────────────────────────────────────────────────────────────────

    #[test]
    fn parse_rejects_empty() {
        assert!(GuardExpr::parse("").is_err());
        assert!(GuardExpr::parse("   ").is_err());
    }

    #[test]
    fn parse_valid_numeric_comparison() {
        assert!(GuardExpr::parse("context.rework_count < 3").is_ok());
        assert!(GuardExpr::parse("context.rework_count >= 3").is_ok());
        assert!(GuardExpr::parse("context.count <= 10").is_ok());
    }

    #[test]
    fn parse_valid_string_comparison() {
        assert!(GuardExpr::parse(r#"context.status == "approved""#).is_ok());
        assert!(GuardExpr::parse(r#"context.status != "denied""#).is_ok());
    }

    #[test]
    fn parse_valid_bool_comparison() {
        assert!(GuardExpr::parse("context.approved == true").is_ok());
        assert!(GuardExpr::parse("context.enabled != false").is_ok());
    }

    #[test]
    fn parse_valid_and_or() {
        assert!(GuardExpr::parse("context.count < 5 and context.enabled == true").is_ok());
        assert!(GuardExpr::parse("context.a == true or context.b == false").is_ok());
    }

    #[test]
    fn parse_valid_not() {
        assert!(GuardExpr::parse("not context.disabled == true").is_ok());
    }

    #[test]
    fn parse_valid_parentheses() {
        assert!(GuardExpr::parse("(context.a < 3) and (context.b >= 1)").is_ok());
    }

    #[test]
    fn parse_invalid_bare_identifier() {
        assert!(GuardExpr::parse("foo < 3").is_err());
    }

    #[test]
    fn parse_invalid_single_equals() {
        assert!(GuardExpr::parse("context.count = 3").is_err());
    }

    // ── evaluate numeric ─────────────────────────────────────────────────────

    #[test]
    fn evaluate_numeric_less_than_true() {
        let ctx = ctx_with(&[("rework_count", serde_json::json!(2))]);
        let g = GuardExpr::parse("context.rework_count < 3").unwrap();
        assert!(g.evaluate(&ctx));
    }

    #[test]
    fn evaluate_numeric_less_than_false_at_boundary() {
        let ctx = ctx_with(&[("rework_count", serde_json::json!(3))]);
        let g = GuardExpr::parse("context.rework_count < 3").unwrap();
        assert!(!g.evaluate(&ctx));
    }

    #[test]
    fn evaluate_numeric_gte_at_boundary() {
        let ctx = ctx_with(&[("rework_count", serde_json::json!(3))]);
        let g = GuardExpr::parse("context.rework_count >= 3").unwrap();
        assert!(g.evaluate(&ctx));
    }

    #[test]
    fn evaluate_numeric_eq() {
        let ctx = ctx_with(&[("n", serde_json::json!(5))]);
        assert!(GuardExpr::parse("context.n == 5").unwrap().evaluate(&ctx));
        assert!(!GuardExpr::parse("context.n == 6").unwrap().evaluate(&ctx));
    }

    // ── evaluate string ──────────────────────────────────────────────────────

    #[test]
    fn evaluate_string_eq() {
        let ctx = ctx_with(&[("status", serde_json::json!("approved"))]);
        let g = GuardExpr::parse(r#"context.status == "approved""#).unwrap();
        assert!(g.evaluate(&ctx));
        let g2 = GuardExpr::parse(r#"context.status == "denied""#).unwrap();
        assert!(!g2.evaluate(&ctx));
    }

    #[test]
    fn evaluate_string_neq() {
        let ctx = ctx_with(&[("status", serde_json::json!("pending"))]);
        let g = GuardExpr::parse(r#"context.status != "approved""#).unwrap();
        assert!(g.evaluate(&ctx));
    }

    // ── evaluate boolean ─────────────────────────────────────────────────────

    #[test]
    fn evaluate_bool_eq_true() {
        let ctx = ctx_with(&[("approved", serde_json::json!(true))]);
        assert!(GuardExpr::parse("context.approved == true")
            .unwrap()
            .evaluate(&ctx));
        assert!(!GuardExpr::parse("context.approved == false")
            .unwrap()
            .evaluate(&ctx));
    }

    // ── evaluate combinators ─────────────────────────────────────────────────

    #[test]
    fn evaluate_and_both_true() {
        let ctx = ctx_with(&[
            ("count", serde_json::json!(2)),
            ("enabled", serde_json::json!(true)),
        ]);
        let g = GuardExpr::parse("context.count < 5 and context.enabled == true").unwrap();
        assert!(g.evaluate(&ctx));
    }

    #[test]
    fn evaluate_and_one_false() {
        let ctx = ctx_with(&[
            ("count", serde_json::json!(10)),
            ("enabled", serde_json::json!(true)),
        ]);
        let g = GuardExpr::parse("context.count < 5 and context.enabled == true").unwrap();
        assert!(!g.evaluate(&ctx));
    }

    #[test]
    fn evaluate_or_one_true() {
        let ctx = ctx_with(&[
            ("a", serde_json::json!(true)),
            ("b", serde_json::json!(false)),
        ]);
        let g = GuardExpr::parse("context.a == true or context.b == true").unwrap();
        assert!(g.evaluate(&ctx));
    }

    #[test]
    fn evaluate_not() {
        let ctx = ctx_with(&[("disabled", serde_json::json!(false))]);
        let g = GuardExpr::parse("not context.disabled == true").unwrap();
        assert!(g.evaluate(&ctx));
    }

    // ── missing field ────────────────────────────────────────────────────────

    #[test]
    fn evaluate_missing_field_returns_false() {
        let ctx = WorkflowContext::new();
        let g = GuardExpr::parse("context.nonexistent < 3").unwrap();
        // Missing field → Null, comparison with number → false
        assert!(!g.evaluate(&ctx));
    }

    // ── serde ────────────────────────────────────────────────────────────────

    #[test]
    fn guard_expr_serde_round_trip() {
        let g = GuardExpr::parse("context.rework_count < 3").unwrap();
        let json = serde_json::to_string(&g).unwrap();
        assert_eq!(json, r#""context.rework_count < 3""#);
        let restored: GuardExpr = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.as_str(), "context.rework_count < 3");
    }

    #[test]
    fn guard_expr_deserialize_invalid_fails() {
        let result: Result<GuardExpr, _> = serde_json::from_str(r#""""#);
        assert!(result.is_err());
    }
}
