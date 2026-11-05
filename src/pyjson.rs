//! A `json.dumps` work-alike: ensure_ascii, Python's float repr and separators, insertion order
//! (or `sort_keys`). Everything written to a file or a tmux option goes through `dumps`.

use std::fmt::Write;

use serde_json::{Number, Value};

/// Which separators and layout `json.dumps` would use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// `json.dumps(v)`: `", "` between items and `": "` after keys.
    Default,
    /// `json.dumps(v, separators=(",", ":"))`.
    Compact,
    /// `json.dumps(v, indent=2)`: `","` plus a newline between items, `": "` after keys.
    Indent2,
}

/// What `json.dumps(value, ...)` returns for the given style, with `sort_keys` as in Python.
pub fn dumps(value: &Value, style: Style, sort_keys: bool) -> String {
    let layout = Layout {
        item: match style {
            Style::Default => ", ",
            Style::Compact | Style::Indent2 => ",",
        },
        key: if style == Style::Compact { ":" } else { ": " },
        indent: style == Style::Indent2,
        sort_keys,
    };
    let mut out = String::new();
    write_value(&mut out, value, &layout, 0);
    out
}

/// Python's `repr(float)`: the shortest digits that round-trip, in scientific notation when the
/// decimal exponent is below -4 or at least 16, otherwise in fixed notation with a fraction.
pub fn float_repr(f: f64) -> String {
    if f.is_nan() {
        return "nan".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    // Rust's `{:e}` prints the shortest round-trip digits as `d.ddde<exp>`. When two candidates of
    // that length are equally close to `f`, it takes the upper one, while Python rounds half to
    // even. Rendering `f` exactly at that length gives Python's choice; it can only fail to
    // round-trip next to a power of two, where the shortest digits stay.
    let shortest = format!("{f:e}");
    let mantissa_digits = shortest
        .split('e')
        .next()
        .map_or(1, |m| m.chars().filter(char::is_ascii_digit).count());
    let places = mantissa_digits.saturating_sub(1);
    let exact = format!("{f:.places$e}");
    let sci = if exact.parse::<f64>().is_ok_and(|g| g == f) {
        exact
    } else {
        shortest
    };
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let (sign, mantissa) = mantissa
        .strip_prefix('-')
        .map_or(("", mantissa), |m| ("-", m));
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let point = exp + 1; // digits before the decimal point in fixed notation
    if !(-4..16).contains(&exp) {
        let fraction = if digits.len() > 1 {
            format!(".{}", &digits[1..])
        } else {
            String::new()
        };
        let exp_sign = if exp < 0 { '-' } else { '+' };
        return format!(
            "{sign}{}{fraction}e{exp_sign}{:02}",
            &digits[..1],
            exp.abs()
        );
    }
    if point <= 0 {
        return format!(
            "{sign}0.{}{digits}",
            "0".repeat(point.unsigned_abs() as usize)
        );
    }
    let point = point as usize;
    if point >= digits.len() {
        return format!("{sign}{digits}{}.0", "0".repeat(point - digits.len()));
    }
    format!("{sign}{}.{}", &digits[..point], &digits[point..])
}

struct Layout {
    item: &'static str,
    key: &'static str,
    indent: bool,
    sort_keys: bool,
}

fn write_value(out: &mut String, value: &Value, layout: &Layout, depth: usize) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&number_repr(n)),
        Value::String(s) => write_string(out, s),
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(layout.item);
                }
                newline(out, layout, depth + 1);
                write_value(out, item, layout, depth + 1);
            }
            newline(out, layout, depth);
            out.push(']');
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            if layout.sort_keys {
                entries.sort_by(|a, b| a.0.cmp(b.0));
            }
            out.push('{');
            for (i, (key, item)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push_str(layout.item);
                }
                newline(out, layout, depth + 1);
                write_string(out, key);
                out.push_str(layout.key);
                write_value(out, item, layout, depth + 1);
            }
            newline(out, layout, depth);
            out.push('}');
        }
    }
}

fn newline(out: &mut String, layout: &Layout, depth: usize) {
    if layout.indent {
        out.push('\n');
        out.extend(std::iter::repeat_n(' ', depth * 2));
    }
}

fn number_repr(n: &Number) -> String {
    if let Some(i) = n.as_i64() {
        i.to_string()
    } else if let Some(u) = n.as_u64() {
        u.to_string()
    } else {
        float_repr(n.as_f64().unwrap_or(f64::NAN))
    }
}

/// `ensure_ascii`: everything outside `' '..='~'` is escaped; astral characters become surrogate
/// pairs.
fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(c),
            _ => {
                for unit in c.encode_utf16(&mut [0; 2]) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        serde_json::from_str(r#"{"b": [1, 2.5, null, true], "a": "café 😀 \"q\" \\ /\n\u007f", "z": {"y": 1e16, "x": 1.5e-05, "w": 0.1, "v": 1762318765.1234567, "u": 3.0}}"#).unwrap()
    }

    #[test]
    fn pyjson_matches_python_default() {
        assert_eq!(
            dumps(&sample(), Style::Default, false),
            r#"{"b": [1, 2.5, null, true], "a": "caf\u00e9 \ud83d\ude00 \"q\" \\ /\n\u007f", "z": {"y": 1e+16, "x": 1.5e-05, "w": 0.1, "v": 1762318765.1234567, "u": 3.0}}"#
        );
    }

    #[test]
    fn pyjson_matches_python_sorted() {
        assert_eq!(
            dumps(&sample(), Style::Default, true),
            r#"{"a": "caf\u00e9 \ud83d\ude00 \"q\" \\ /\n\u007f", "b": [1, 2.5, null, true], "z": {"u": 3.0, "v": 1762318765.1234567, "w": 0.1, "x": 1.5e-05, "y": 1e+16}}"#
        );
    }

    #[test]
    fn pyjson_matches_python_compact() {
        assert_eq!(
            dumps(&sample(), Style::Compact, false),
            r#"{"b":[1,2.5,null,true],"a":"caf\u00e9 \ud83d\ude00 \"q\" \\ /\n\u007f","z":{"y":1e+16,"x":1.5e-05,"w":0.1,"v":1762318765.1234567,"u":3.0}}"#
        );
    }

    #[test]
    fn pyjson_matches_python_indent() {
        let v = serde_json::from_str(r#"{"k": [1, {"m": []}], "e": {}, "s": []}"#).unwrap();
        assert_eq!(
            dumps(&v, Style::Indent2, false),
            "{\n  \"k\": [\n    1,\n    {\n      \"m\": []\n    }\n  ],\n  \"e\": {},\n  \"s\": []\n}"
        );
    }

    #[test]
    fn float_repr_matches_python() {
        for (f, s) in [
            (1e16, "1e+16"),
            (1.5e-05, "1.5e-05"),
            (0.1, "0.1"),
            (1762318765.1234567, "1762318765.1234567"),
            (3.0, "3.0"),
            (123456789012345678.0, "1.2345678901234568e+17"),
            (1e-4, "0.0001"),
            (0.0001234, "0.0001234"),
            (-2.5, "-2.5"),
            (1e22, "1e+22"),
            (5e-324, "5e-324"),
        ] {
            assert_eq!(float_repr(f), s);
        }
    }

    /// More `repr(float)` cases from Python 3.15, around the 1e-4 and 1e16 switch points.
    #[test]
    fn float_repr_edges_match_python() {
        for (f, s) in [
            (1e15, "1000000000000000.0"),
            (9007199254740992.0, "9007199254740992.0"),
            (100.0, "100.0"),
            (123456.789, "123456.789"),
            (-0.0, "-0.0"),
            (0.0, "0.0"),
            (5e-05, "5e-05"),
            (1e23, "1e+23"),
            (1e300, "1e+300"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
            // Two shortest candidates are equally close: Python rounds half to even.
            (1059438285926254.0 + 0.25, "1059438285926254.2"),
            (-1425502010969177.0 - 0.25, "-1425502010969177.2"),
            (f64::NAN, "nan"),
            (f64::INFINITY, "inf"),
            (f64::NEG_INFINITY, "-inf"),
        ] {
            assert_eq!(float_repr(f), s);
        }
    }

    /// Control characters, DEL, non-ASCII, line separators and astral characters, from
    /// `json.dumps("\x00\x01\x08\x09\x0a\x0b\x0c\x0d\x1f\x7f\x80 \U0001F600￿\U00010000 ~")`.
    #[test]
    fn strings_escape_like_python_ensure_ascii() {
        let s = Value::String(
            "\u{0}\u{1}\u{8}\u{9}\u{a}\u{b}\u{c}\u{d}\u{1f}\u{7f}\u{80}\u{2028}\u{1F600}\u{ffff}\u{10000} ~"
                .to_string(),
        );
        assert_eq!(
            dumps(&s, Style::Default, false),
            r#""\u0000\u0001\b\t\n\u000b\f\r\u001f\u007f\u0080\u2028\ud83d\ude00\uffff\ud800\udc00 ~""#
        );
    }

    #[test]
    fn empty_containers_and_sorted_indent_match_python() {
        let v = serde_json::from_str(r#"{"b":[{"d":1,"c":[]}],"a":{}}"#).unwrap();
        assert_eq!(
            dumps(&v, Style::Indent2, true),
            "{\n  \"a\": {},\n  \"b\": [\n    {\n      \"c\": [],\n      \"d\": 1\n    }\n  ]\n}"
        );
        for style in [Style::Default, Style::Compact, Style::Indent2] {
            assert_eq!(dumps(&Value::Array(vec![]), style, false), "[]");
            assert_eq!(dumps(&serde_json::json!({}), style, false), "{}");
        }
        assert_eq!(
            dumps(&serde_json::json!([[]]), Style::Indent2, false),
            "[\n  []\n]"
        );
        assert_eq!(
            dumps(
                &serde_json::json!([1, [2, {"a": null}]]),
                Style::Compact,
                false
            ),
            r#"[1,[2,{"a":null}]]"#
        );
    }

    /// Python sorts keys by code point, which is also UTF-8 byte order.
    #[test]
    fn sort_keys_orders_by_code_point() {
        let v = serde_json::from_str(r#"{"é": 1, "z": 2, "Z": 3, "😀": 4, "￿": 5}"#).unwrap();
        assert_eq!(
            dumps(&v, Style::Compact, true),
            r#"{"Z":3,"z":2,"\u00e9":1,"\uffff":5,"\ud83d\ude00":4}"#
        );
    }

    /// serde_json's default float parser can land one ULP off; `float_roundtrip` parses exactly
    /// like Python, so a document read and written back keeps its numbers.
    #[test]
    fn parsed_floats_round_trip_exactly() {
        let text = "[-1.306052388527796e-147, 1059438285926254.2, 1762318765.1234567, 2.2250738585072014e-308]";
        let v: Value = serde_json::from_str(text).unwrap();
        assert_eq!(dumps(&v, Style::Default, false), text);
    }

    #[test]
    fn integers_print_as_integers() {
        let v = serde_json::json!([0, -7, 9223372036854775807i64, 18446744073709551615u64]);
        assert_eq!(
            dumps(&v, Style::Default, false),
            "[0, -7, 9223372036854775807, 18446744073709551615]"
        );
    }
}
