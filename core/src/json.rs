//! 极简 JSON（core 保持零依赖，但需要读平台页面里的内嵌数据）。
//!
//! 只做"读"：解析出 `Value` 树，再用 `get` / `idx` / `find_key` 取值。
//! 不做序列化——写出 JSON 由各模块自己拼（避免为了几个字段引入 writer）。

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Value>),
    Obj(Vec<(String, Value)>),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(kv) => kv.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn idx(&self, i: usize) -> Option<&Value> {
        match self {
            Value::Arr(v) => v.get(i),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Num(n) => Some(*n),
            Value::Str(s) => s.parse().ok(),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        self.as_f64().map(|n| n as i64)
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            Value::Str(s) => Some(s.eq_ignore_ascii_case("true")),
            _ => None,
        }
    }

    pub fn as_arr(&self) -> Option<&[Value]> {
        match self {
            Value::Arr(v) => Some(v),
            _ => None,
        }
    }

    /// 万物皆数组：单对象也包成 1 元素数组，省得每条链路都判一次
    pub fn as_arr_or_single(&self) -> Vec<&Value> {
        match self {
            Value::Arr(v) => v.iter().collect(),
            _ => vec![self],
        }
    }

    /// 按 key 递归找第一处匹配（页面内嵌结构常被包在 `loaderData` 之类的外壳里）
    pub fn find_key(&self, key: &str) -> Option<&Value> {
        if let Value::Obj(kv) = self {
            if let Some((_, v)) = kv.iter().find(|(k, _)| k == key) {
                return Some(v);
            }
        }
        match self {
            Value::Obj(kv) => kv.iter().find_map(|(_, v)| v.find_key(key)),
            Value::Arr(v) => v.iter().find_map(|v| v.find_key(key)),
            _ => None,
        }
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

pub fn parse(text: &str) -> Result<Value> {
    let mut p = Parser { bytes: text.as_bytes(), pos: 0 };
    p.skip_ws();
    p.value()
}

impl<'a> Parser<'a> {
    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len() {
            match self.bytes[self.pos] {
                b' ' | b'\t' | b'\n' | b'\r' => self.pos += 1,
                _ => break,
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn value(&mut self) -> Result<Value> {
        self.skip_ws();
        match self.peek() {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => Ok(Value::Str(self.string()?)),
            Some(b't') => self.word("true", Value::Bool(true)),
            Some(b'f') => self.word("false", Value::Bool(false)),
            Some(b'n') => self.word("null", Value::Null),
            Some(_) => self.number(),
            None => Err(Error::InvalidJson("意外的输入结尾".to_string())),
        }
    }

    fn word(&mut self, w: &str, v: Value) -> Result<Value> {
        if self.bytes[self.pos..].starts_with(w.as_bytes()) {
            self.pos += w.len();
            Ok(v)
        } else {
            Err(Error::InvalidJson(format!("期望字面量 {}", w)))
        }
    }

    fn number(&mut self) -> Result<Value> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if matches!(c, b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E') {
                self.pos += 1;
            } else {
                break;
            }
        }
        let s = std::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|e| Error::InvalidJson(e.to_string()))?;
        s.parse::<f64>()
            .map(Value::Num)
            .map_err(|e| Error::InvalidJson(format!("数字 \"{}\" 无法解析：{}", s, e)))
    }

    fn string(&mut self) -> Result<String> {
        self.pos += 1; // 开引号
        let mut out = String::new();
        loop {
            match self.peek() {
                None => return Err(Error::InvalidJson("字符串未闭合".to_string())),
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.pos += 1;
                    match self.peek() {
                        Some(b'n') => {
                            out.push('\n');
                            self.pos += 1;
                        }
                        Some(b't') => {
                            out.push('\t');
                            self.pos += 1;
                        }
                        Some(b'r') => {
                            out.push('\r');
                            self.pos += 1;
                        }
                        Some(b'b') => {
                            out.push('\u{8}');
                            self.pos += 1;
                        }
                        Some(b'f') => {
                            out.push('\u{c}');
                            self.pos += 1;
                        }
                        Some(b'/') => {
                            out.push('/');
                            self.pos += 1;
                        }
                        Some(b'"') => {
                            out.push('"');
                            self.pos += 1;
                        }
                        Some(b'\\') => {
                            out.push('\\');
                            self.pos += 1;
                        }
                        Some(b'u') => {
                            self.pos += 1;
                            let code = self.hex4()?;
                            // 代理对：一个字符可能由两个 \uXXXX 组成
                            if (0xD800..0xDC00).contains(&code)
                                && self.bytes[self.pos..].starts_with(b"\\u")
                            {
                                self.pos += 2;
                                let low = self.hex4()?;
                                if (0xDC00..0xE000).contains(&low) {
                                    let merged = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                                    out.push(char::from_u32(merged).unwrap_or('\u{fffd}'));
                                    continue;
                                }
                                out.push('\u{fffd}');
                                out.push(char::from_u32(low).unwrap_or('\u{fffd}'));
                                continue;
                            }
                            out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                        }
                        Some(c) => {
                            out.push(c as char);
                            self.pos += 1;
                        }
                        None => return Err(Error::InvalidJson("转义序列未结束".to_string())),
                    }
                }
                Some(c) if c < 0x80 => {
                    self.pos += 1;
                    out.push(c as char);
                }
                Some(_) => {
                    // 多字节 UTF-8 原样搬运，交给标准库校验
                    let start = self.pos;
                    while let Some(x) = self.peek() {
                        if x == b'"' || x == b'\\' {
                            break;
                        }
                        self.pos += 1;
                    }
                    let s = std::str::from_utf8(&self.bytes[start..self.pos])
                        .map_err(|e| Error::InvalidJson(e.to_string()))?;
                    out.push_str(s);
                }
            }
        }
    }

    fn hex4(&mut self) -> Result<u32> {
        let mut hex = String::with_capacity(4);
        for _ in 0..4 {
            match self.peek() {
                Some(c) => {
                    hex.push(c as char);
                    self.pos += 1;
                }
                None => return Ok(0xFFFD),
            }
        }
        Ok(u32::from_str_radix(&hex, 16).unwrap_or(0xFFFD))
    }

    fn array(&mut self) -> Result<Value> {
        self.pos += 1;
        let mut v = Vec::new();
        loop {
            self.skip_ws();
            if self.peek() == Some(b']') {
                self.pos += 1;
                return Ok(Value::Arr(v));
            }
            v.push(self.value()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Value::Arr(v));
                }
                _ => return Err(Error::InvalidJson("数组格式错误".to_string())),
            }
        }
    }

    fn object(&mut self) -> Result<Value> {
        self.pos += 1;
        let mut kv = Vec::new();
        loop {
            self.skip_ws();
            if self.peek() == Some(b'}') {
                self.pos += 1;
                return Ok(Value::Obj(kv));
            }
            let k = self.string()?;
            self.skip_ws();
            if self.peek() != Some(b':') {
                return Err(Error::InvalidJson("对象缺少冒号".to_string()));
            }
            self.pos += 1;
            let v = self.value()?;
            kv.push((k, v));
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Value::Obj(kv));
                }
                _ => return Err(Error::InvalidJson("对象格式错误".to_string())),
            }
        }
    }
}

/// 从文本里抠出一个 `{...}` 对象字面量：按花括号配平，且忽略字符串里的括号。
/// 平台页面里的内嵌数据常常夹在 JS 语句中间，需要这样切出来。
pub fn extract_object(text: &str, marker: &str) -> Option<String> {
    let start_at = text.find(marker)?;
    let bytes = text.as_bytes();
    let mut i = start_at;
    while i < bytes.len() && bytes[i] != b'{' {
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }
    let begin = i;
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escaped = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_str {
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                in_str = false;
            }
        } else {
            match c {
                b'"' => in_str = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(text[begin..=i].to_string());
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// URL 百分号解码（`RENDER_DATA` 里的 JSON 是 encodeURIComponent 过的）
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_structure() {
        let v = parse(r#"{"a":{"b":[1,2,{"c":"x"}]},"n":null,"t":true}"#).unwrap();
        let c = v.get("a").unwrap().get("b").unwrap().idx(2).unwrap();
        assert_eq!(c.get("c").unwrap().as_str(), Some("x"));
        assert_eq!(v.get("n"), Some(&Value::Null));
        assert_eq!(v.get("t").unwrap().as_bool(), Some(true));
    }

    #[test]
    fn handles_escapes_and_unicode() {
        let v = parse(r#"{"s":"a\"b\nc\u4e2d"}"#).unwrap();
        assert_eq!(v.get("s").unwrap().as_str(), Some("a\"b\nc中"));
    }

    #[test]
    fn handles_surrogate_pairs() {
        let v = parse(r#"{"e":"\ud83d\ude00"}"#).unwrap();
        assert_eq!(v.get("e").unwrap().as_str(), Some("😀"));
    }

    #[test]
    fn find_key_walks_objects_and_arrays() {
        let v = parse(r#"{"loaderData":{"x":[{"videoInfoRes":{"item_list":[{"desc":"hi"}]}}]}}"#)
            .unwrap();
        let found = v.find_key("videoInfoRes").unwrap();
        let item = found.get("item_list").unwrap().idx(0).unwrap();
        assert_eq!(item.get("desc").unwrap().as_str(), Some("hi"));
    }

    #[test]
    fn extract_object_respects_strings_and_depth() {
        let text = r#"<script>window._ROUTER_DATA = {"a":{"b":"x}y{"},"c":1};</script>"#;
        let obj = extract_object(text, "_ROUTER_DATA").unwrap();
        assert_eq!(obj, r#"{"a":{"b":"x}y{"},"c":1}"#);
        let v = parse(&obj).unwrap();
        assert_eq!(v.get("a").unwrap().get("b").unwrap().as_str(), Some("x}y{"));
    }

    #[test]
    fn extract_object_missing_marker() {
        assert!(extract_object("nothing here", "_ROUTER_DATA").is_none());
    }

    #[test]
    fn percent_decode_roundtrip() {
        assert_eq!(percent_decode("%7B%22a%22%3A1%7D"), r#"{"a":1}"#);
        assert_eq!(percent_decode("plain"), "plain");
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse("{").is_err());
        assert!(parse("[1,").is_err());
    }
}
