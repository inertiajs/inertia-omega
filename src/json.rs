//! Serializing page data for the browser.

use serde::Serialize;
use serde_json::{Map, Value};

/// Serialize a value as JSON that is safe to embed in an HTML `<script>`
/// element. Forward slashes and HTML-significant characters are escaped, so
/// a string like `</script>` can't end the element early.
pub fn html_safe_json(value: &impl Serialize) -> String {
    let json = serde_json::to_string(value).unwrap_or_else(|_| "null".to_owned());
    let mut escaped = String::with_capacity(json.len());

    for c in json.chars() {
        match c {
            '/' => escaped.push_str("\\/"),
            '<' => escaped.push_str("\\u003c"),
            '>' => escaped.push_str("\\u003e"),
            '&' => escaped.push_str("\\u0026"),
            '\u{2028}' => escaped.push_str("\\u2028"),
            '\u{2029}' => escaped.push_str("\\u2029"),
            c => escaped.push(c),
        }
    }

    escaped
}

/// The key a big integer is transported under.
const BIG_INTEGER_KEY: &str = "$bigint";

/// The largest integer JavaScript represents without losing precision.
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Wrap integers outside JavaScript's safe integer range in a marker object
/// so the client can revive them as native `BigInt` values.
pub fn encode_big_integers(value: Value) -> Value {
    match value {
        Value::Number(number) => {
            let unsafe_integer = match (number.as_u64(), number.as_i64()) {
                (Some(unsigned), _) => unsigned > MAX_SAFE_INTEGER,
                (None, Some(signed)) => signed.unsigned_abs() > MAX_SAFE_INTEGER,
                _ => false,
            };

            if unsafe_integer {
                Value::Object(Map::from_iter([(
                    BIG_INTEGER_KEY.to_owned(),
                    Value::String(number.to_string()),
                )]))
            } else {
                Value::Number(number)
            }
        }
        Value::Array(items) => Value::Array(items.into_iter().map(encode_big_integers).collect()),
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .map(|(key, value)| (key, encode_big_integers(value)))
                .collect(),
        ),
        value => value,
    }
}

/// Turn [`encode_big_integers`]'s markers back into integers.
pub(crate) fn decode_big_integers(value: Value) -> Value {
    match value {
        Value::Object(object) => match object.get(BIG_INTEGER_KEY).and_then(Value::as_str) {
            Some(digits) if object.len() == 1 => digits
                .parse::<u64>()
                .map(Value::from)
                .or_else(|_| digits.parse::<i64>().map(Value::from))
                .unwrap_or(Value::Object(object)),
            _ => Value::Object(
                object
                    .into_iter()
                    .map(|(key, value)| (key, decode_big_integers(value)))
                    .collect(),
            ),
        },
        Value::Array(items) => Value::Array(items.into_iter().map(decode_big_integers).collect()),
        value => value,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn escapes_html_in_json() {
        let json = html_safe_json(&json!({ "html": "</script><b>&" }));

        assert!(!json.contains("</script>"));
        assert_eq!(
            serde_json::from_str::<Value>(&json).unwrap(),
            json!({ "html": "</script><b>&" })
        );
    }

    #[test]
    fn wraps_unsafe_integers() {
        let value = encode_big_integers(json!({ "id": u64::MAX, "small": 1, "negative": i64::MIN }));

        assert_eq!(value["id"], json!({ "$bigint": "18446744073709551615" }));
        assert_eq!(value["small"], 1);
        assert_eq!(value["negative"], json!({ "$bigint": "-9223372036854775808" }));
        assert_eq!(
            decode_big_integers(value),
            json!({ "id": u64::MAX, "small": 1, "negative": i64::MIN })
        );
    }
}
