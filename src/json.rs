//! Just enough JSON writing for the output lines. Nothing here parses JSON.

use std::fmt::Display;
use std::io::{self, Write};

/// Writes one line to stdout, ignoring a closed pipe.
pub fn print(line: &str) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{line}");
}

pub fn string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub fn numbers<T: Display>(values: &[T]) -> String {
    let items: Vec<String> = values.iter().map(|v| v.to_string()).collect();
    format!("[{}]", items.join(","))
}

/// Builds one object, keys in insertion order.
pub struct Object(String);

impl Object {
    pub fn new() -> Self {
        Object(String::from("{"))
    }

    fn key(&mut self, key: &str) {
        if self.0.len() > 1 {
            self.0.push(',');
        }
        self.0.push_str(&string(key));
        self.0.push(':');
    }

    pub fn str(mut self, key: &str, value: &str) -> Self {
        self.key(key);
        self.0.push_str(&string(value));
        self
    }

    pub fn num<T: Display>(mut self, key: &str, value: T) -> Self {
        self.key(key);
        self.0.push_str(&value.to_string());
        self
    }

    pub fn bool(mut self, key: &str, value: bool) -> Self {
        self.key(key);
        self.0.push_str(if value { "true" } else { "false" });
        self
    }

    pub fn raw(mut self, key: &str, value: &str) -> Self {
        self.key(key);
        self.0.push_str(value);
        self
    }

    pub fn finish(mut self) -> String {
        self.0.push('}');
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_strings() {
        assert_eq!(string("a\"b\\c\nd\u{1}"), r#""a\"b\\c\nd\u0001""#);
    }

    #[test]
    fn builds_objects() {
        let o = Object::new()
            .str("name", "x")
            .num("port", 5173)
            .bool("running", true)
            .raw("ports", &numbers(&[1, 2]))
            .finish();
        assert_eq!(
            o,
            r#"{"name":"x","port":5173,"running":true,"ports":[1,2]}"#
        );
    }
}
