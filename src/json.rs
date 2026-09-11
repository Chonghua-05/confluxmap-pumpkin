//! A minimal JSON reader and writer for the two files this plugin persists.
//!
//! Both persisted documents - the server instance id and the shared-waypoint
//! catalog - are written by this plugin and read back by it, and both mirror a
//! file shape the reference companion defines. That makes a full JSON library
//! unnecessary: what is needed is a strict parser for the subset those two files
//! use, plus correct string escaping on the way out.
//!
//! Strictness is deliberate. A shared-waypoint document that does not match the
//! expected shape is quarantined rather than half-read, and the caller decides
//! what "quarantined" means. So the parser rejects trailing input, unquoted
//! keys, `NaN`/`Infinity`, and anything past `MAX_DEPTH` instead of guessing.
//!
//! Numbers keep their literal text: a revision or an epoch timestamp must
//! round-trip through `i64` without a detour through a double.

/// Deepest nesting the parser accepts. Both documents nest two levels.
const MAX_DEPTH: usize = 32;

/// A parsed JSON value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// `null`
    Null,
    /// `true` / `false`
    Bool(bool),
    /// A number, kept as its literal text so integers survive exactly.
    Number(String),
    /// A string with escapes already resolved.
    String(String),
    /// An array.
    Array(Vec<Value>),
    /// An object, in document order.
    Object(Vec<(String, Value)>),
}

impl Value {
    /// The value of `key`, when this is an object that has it.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(entries) => entries
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    /// The value as a string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(value) => Some(value),
            _ => None,
        }
    }

    /// The value as an integer, when it was written without a fraction.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Number(text) => text.parse().ok(),
            _ => None,
        }
    }

    /// The value as a double.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Number(text) => text.parse().ok(),
            _ => None,
        }
    }

    /// The value as an array.
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }

}

/// Why a document was rejected. The text is for a log line, not for a client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonError(pub String);

impl JsonError {
    fn new(message: impl Into<String>) -> Self {
        JsonError(message.into())
    }
}

/// Parses one complete JSON document, rejecting trailing content.
pub fn parse(text: &str) -> Result<Value, JsonError> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        pos: 0,
    };
    parser.skip_whitespace();
    let value = parser.value(0)?;
    parser.skip_whitespace();
    if parser.pos != parser.bytes.len() {
        return Err(JsonError::new(format!(
            "trailing content at byte {}",
            parser.pos
        )));
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn take(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.pos += 1;
        Some(byte)
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn literal(&mut self, word: &str) -> bool {
        if self.bytes[self.pos..].starts_with(word.as_bytes()) {
            self.pos += word.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, JsonError> {
        if depth > MAX_DEPTH {
            return Err(JsonError::new("nesting is too deep"));
        }
        match self.peek() {
            None => Err(JsonError::new("unexpected end of input")),
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => Ok(Value::String(self.string()?)),
            Some(b't') if self.literal("true") => Ok(Value::Bool(true)),
            Some(b'f') if self.literal("false") => Ok(Value::Bool(false)),
            Some(b'n') if self.literal("null") => Ok(Value::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(other) => Err(JsonError::new(format!(
                "unexpected byte {:?} at {}",
                other as char, self.pos
            ))),
        }
    }

    fn object(&mut self, depth: usize) -> Result<Value, JsonError> {
        self.take();
        let mut entries = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.take();
            return Ok(Value::Object(entries));
        }
        loop {
            self.skip_whitespace();
            if self.peek() != Some(b'"') {
                return Err(JsonError::new("object key must be a string"));
            }
            let key = self.string()?;
            self.skip_whitespace();
            if self.take() != Some(b':') {
                return Err(JsonError::new("expected `:` after an object key"));
            }
            self.skip_whitespace();
            let value = self.value(depth + 1)?;
            entries.push((key, value));
            self.skip_whitespace();
            match self.take() {
                Some(b',') => {}
                Some(b'}') => return Ok(Value::Object(entries)),
                _ => return Err(JsonError::new("expected `,` or `}`")),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Value, JsonError> {
        self.take();
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.take();
            return Ok(Value::Array(items));
        }
        loop {
            self.skip_whitespace();
            items.push(self.value(depth + 1)?);
            self.skip_whitespace();
            match self.take() {
                Some(b',') => {}
                Some(b']') => return Ok(Value::Array(items)),
                _ => return Err(JsonError::new("expected `,` or `]`")),
            }
        }
    }

    fn number(&mut self) -> Result<Value, JsonError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        let text = core::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|_| JsonError::new("number is not valid UTF-8"))?;
        if text.is_empty() || text == "-" || text.parse::<f64>().is_err() {
            return Err(JsonError::new(format!("`{text}` is not a number")));
        }
        Ok(Value::Number(text.to_string()))
    }

    fn string(&mut self) -> Result<String, JsonError> {
        debug_assert_eq!(self.peek(), Some(b'"'));
        self.take();
        let mut out = String::new();
        loop {
            let byte = self.take().ok_or_else(|| JsonError::new("unterminated string"))?;
            match byte {
                b'"' => return Ok(out),
                b'\\' => {
                    let escape = self
                        .take()
                        .ok_or_else(|| JsonError::new("unterminated escape"))?;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => out.push(self.code_unit()?),
                        _ => return Err(JsonError::new("unknown string escape")),
                    }
                }
                // Bytes below 0x20 must be escaped; raw UTF-8 passes through.
                byte if byte < 0x20 => {
                    return Err(JsonError::new("unescaped control character in a string"));
                }
                byte => {
                    // Re-read the whole UTF-8 sequence this byte starts.
                    let start = self.pos - 1;
                    if byte >= 0x80 {
                        let width = match byte {
                            0xc2..=0xdf => 2,
                            0xe0..=0xef => 3,
                            0xf0..=0xf4 => 4,
                            _ => return Err(JsonError::new("invalid UTF-8 in a string")),
                        };
                        self.pos = start + width;
                    }
                    let slice = self
                        .bytes
                        .get(start..self.pos)
                        .ok_or_else(|| JsonError::new("invalid UTF-8 in a string"))?;
                    out.push_str(
                        core::str::from_utf8(slice)
                            .map_err(|_| JsonError::new("invalid UTF-8 in a string"))?,
                    );
                }
            }
        }
    }

    /// Reads the four hex digits of a `\uXXXX` escape, combining a surrogate
    /// pair when one follows.
    fn code_unit(&mut self) -> Result<char, JsonError> {
        let first = self.hex4()?;
        if (0xd800..0xdc00).contains(&first) {
            if self.take() != Some(b'\\') || self.take() != Some(b'u') {
                return Err(JsonError::new("lone high surrogate in a string"));
            }
            let second = self.hex4()?;
            if !(0xdc00..0xe000).contains(&second) {
                return Err(JsonError::new("lone high surrogate in a string"));
            }
            let combined = 0x1_0000 + ((first - 0xd800) << 10) + (second - 0xdc00);
            char::from_u32(combined).ok_or_else(|| JsonError::new("invalid code point"))
        } else {
            char::from_u32(first).ok_or_else(|| JsonError::new("invalid code point"))
        }
    }

    fn hex4(&mut self) -> Result<u32, JsonError> {
        let mut value = 0u32;
        for _ in 0..4 {
            let digit = self
                .take()
                .ok_or_else(|| JsonError::new("truncated \\u escape"))?;
            let nibble = match digit {
                b'0'..=b'9' => u32::from(digit - b'0'),
                b'a'..=b'f' => u32::from(digit - b'a') + 10,
                b'A'..=b'F' => u32::from(digit - b'A') + 10,
                _ => return Err(JsonError::new("non-hex digit in a \\u escape")),
            };
            value = (value << 4) | nibble;
        }
        Ok(value)
    }
}

/// Escapes `value` for use inside a JSON string literal.
pub fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ch if (ch as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", ch as u32));
            }
            ch => out.push(ch),
        }
    }
    out
}

/// Formats a double so it round-trips: integral values keep a `.0`, so a file
/// written by the reference implementation and one written here are both read
/// back as the same `f64`.
pub fn number(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 1e15 {
        return format!("{value:.1}");
    }
    format!("{value}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_shape_the_waypoint_file_uses() {
        let value = parse(
            r#"{"schemaVersion":2,"revision":3,"ownerInstanceId":null,"waypoints":[
                 {"id":"a","x":1.5,"colorArgb":-1,"type":"NORMAL","nested":{"k":true}}]}"#,
        )
        .expect("valid document");
        assert_eq!(value.get("schemaVersion").and_then(Value::as_i64), Some(2));
        assert_eq!(value.get("ownerInstanceId"), Some(&Value::Null));
        let list = value.get("waypoints").and_then(Value::as_array).expect("array");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].get("x").and_then(Value::as_f64), Some(1.5));
        assert_eq!(list[0].get("colorArgb").and_then(Value::as_i64), Some(-1));
        assert_eq!(
            list[0].get("nested").and_then(|n| n.get("k")),
            Some(&Value::Bool(true))
        );
    }

    #[test]
    fn rejects_what_a_strict_reader_should_reject() {
        assert!(parse("").is_err());
        assert!(parse("{}{}").is_err(), "trailing content");
        assert!(parse("{a:1}").is_err(), "unquoted key");
        assert!(parse(r#"{"a":NaN}"#).is_err());
        assert!(parse(r#"{"a":}"#).is_err());
        assert!(parse(r#"{"a":"unterminated}"#).is_err());
        assert!(parse(r#"{"a":"\q"}"#).is_err());
        assert!(parse(r#"["\ud800"]"#).is_err(), "lone surrogate");
    }

    #[test]
    fn resolves_escapes_including_surrogate_pairs() {
        let value = parse(r#"{"s":"a\n\"b\u00a7\u4e00\ud83d\ude00"}"#).expect("valid");
        assert_eq!(value.get("s").and_then(Value::as_str), Some("a\n\"b\u{a7}\u{4e00}\u{1f600}"));
    }

    #[test]
    fn accepts_the_literals_and_empty_containers() {
        let value = parse(r#"{"o":{},"a":[],"t":true,"f":false,"n":null,"neg":-12}"#).expect("valid");
        assert_eq!(value.get("o").and_then(Value::as_array), None);
        assert_eq!(value.get("neg").and_then(Value::as_i64), Some(-12));
        assert_eq!(value.get("t"), Some(&Value::Bool(true)));
    }

    #[test]
    fn escaping_round_trips_every_awkward_character() {
        let original = "quote\" backslash\\ newline\n tab\t \u{1} chinese\u{4e00} emoji\u{1f600}";
        let document = format!(r#"{{"s":"{}"}}"#, escape(original));
        let parsed = parse(&document).expect("valid");
        assert_eq!(parsed.get("s").and_then(Value::as_str), Some(original));
    }

    #[test]
    fn numbers_keep_their_integer_form_and_doubles_keep_a_fraction() {
        assert_eq!(number(1.0), "1.0");
        assert_eq!(number(-0.5), "-0.5");
        assert_eq!(number(12_345.0), "12345.0");
        let document = format!(r#"{{"rev":{},"x":{}}}"#, 1_789_288_297_874_145_099i64, number(64.0));
        let parsed = parse(&document).expect("valid");
        assert_eq!(
            parsed.get("rev").and_then(Value::as_i64),
            Some(1_789_288_297_874_145_099)
        );
        assert_eq!(parsed.get("x").and_then(Value::as_f64), Some(64.0));
    }
}
