//! Python's text forms of numbers and JSON, for the frozen-file check of `ruslan_v2_1.py`.
//!
//! v2.1 is registered on top of v2 by the sha256 of `json.dumps(json.load(f), sort_keys=True)`.
//! Reproducing that hash needs Python's exact output: `repr(float)` for every float, `", "` and
//! `": "` as separators, sorted keys and ASCII-only strings.

use std::fmt::Write as _;

use serde_json::Value;
use sha2::{Digest, Sha256};

/// `repr(x)` of a Python float: the shortest digits that read back as `x`, in fixed notation when
/// the decimal exponent is in -4..16 (with `.0` for a whole number), else as `d[.ddd]e±XX`.
pub fn py_float_repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".to_string();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0.0" } else { "0.0" }.to_string();
    }
    // Rust prints the shortest round-trip digits, as Python's repr does: "1.25e0", "-5e-1".
    let sci = format!("{x:e}");
    let (mantissa, exp) = sci.split_once('e').expect("{:e} always has an exponent");
    let exp: i32 = exp.parse().expect("{:e} exponent is an integer");
    let (sign, mantissa) = match mantissa.strip_prefix('-') {
        Some(m) => ("-", m),
        None => ("", mantissa),
    };
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let n = digits.len() as i32;
    let decpt = exp + 1; // the decimal point sits after this many digits
    let mut out = String::from(sign);
    if !(-4..16).contains(&exp) {
        out.push_str(&digits[..1]);
        if n > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let _ = write!(out, "e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs());
    } else if decpt <= 0 {
        out.push_str("0.");
        out.extend(std::iter::repeat_n('0', (-decpt) as usize));
        out.push_str(&digits);
    } else if decpt < n {
        out.push_str(&digits[..decpt as usize]);
        out.push('.');
        out.push_str(&digits[decpt as usize..]);
    } else {
        out.push_str(&digits);
        out.extend(std::iter::repeat_n('0', (decpt - n) as usize));
        out.push_str(".0");
    }
    out
}

/// A string as `json.dumps` writes it with `ensure_ascii=True`: everything outside the printable
/// ASCII range escaped, as `\uXXXX` in lower case (a surrogate pair beyond the BMP).
fn push_string(out: &mut String, s: &str) {
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
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
        }
    }
    out.push('"');
}

fn push_value(out: &mut String, v: &Value) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                let _ = write!(out, "{i}");
            } else if let Some(u) = n.as_u64() {
                let _ = write!(out, "{u}");
            } else {
                let f = n.as_f64().expect("a JSON number is an integer or a float");
                out.push_str(&py_float_repr(f));
            }
        }
        Value::String(s) => push_string(out, s),
        Value::Array(items) => {
            out.push('[');
            for (k, item) in items.iter().enumerate() {
                if k > 0 {
                    out.push_str(", ");
                }
                push_value(out, item);
            }
            out.push(']');
        }
        Value::Object(map) => {
            // Sorted here, whatever order the map keeps: another crate can switch on serde_json's
            // `preserve_order`. Code-point order, which is Python's str order and UTF-8 byte order.
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (k, key) in keys.into_iter().enumerate() {
                if k > 0 {
                    out.push_str(", ");
                }
                push_string(out, key);
                out.push_str(": ");
                push_value(out, &map[key]);
            }
            out.push('}');
        }
    }
}

/// `json.dumps(v, sort_keys=True)`.
pub fn canonical_json(v: &Value) -> String {
    let mut out = String::new();
    push_value(&mut out, v);
    out
}

/// The hex sha256 of the canonical form of a JSON text, as `ruslan_v2_1.check_base` computes it.
pub fn canonical_sha256(json_text: &str) -> Result<String, serde_json::Error> {
    let v: Value = serde_json::from_str(json_text)?;
    let digest = Sha256::digest(canonical_json(&v).as_bytes());
    let mut hex = String::with_capacity(64);
    for b in digest {
        let _ = write!(hex, "{b:02x}");
    }
    Ok(hex)
}
