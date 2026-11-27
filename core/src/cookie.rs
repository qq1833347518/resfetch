//! Cookie 存储与合并。
//!
//! 这里专门用来根治旧版那个"整文件覆盖导致 65 条 cookie 丢失"的坑：
//! 合并按 (domain, path, name) 三元组，只有同一颗 cookie 才会被新值覆盖，
//! 其它站点的 cookie 一律保留。过期 cookie 会被单独计数并剔除，而不是静默留着。

use crate::error::{Error, Result};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cookie {
    pub domain: String,
    /// 是否对子域生效（Netscape 格式里域名以 `.` 开头即 true）
    pub include_subdomains: bool,
    pub path: String,
    pub secure: bool,
    /// Unix 秒；None 表示会话 cookie
    pub expires: Option<i64>,
    pub name: String,
    pub value: String,
}

impl Cookie {
    pub fn new(domain: &str, path: &str, name: &str, value: &str) -> Self {
        let include_subdomains = domain.starts_with('.');
        Cookie {
            domain: domain.trim_start_matches('.').to_ascii_lowercase(),
            include_subdomains,
            path: if path.is_empty() { "/".to_string() } else { path.to_string() },
            secure: false,
            expires: None,
            name: name.to_string(),
            value: value.to_string(),
        }
    }

    fn key(&self) -> (String, String, String) {
        (
            self.domain.trim_start_matches('.').to_ascii_lowercase(),
            self.path.clone(),
            self.name.clone(),
        )
    }

    pub fn is_expired(&self, now: i64) -> bool {
        matches!(self.expires, Some(t) if t > 0 && t <= now)
    }

    pub fn is_session(&self) -> bool {
        self.expires.is_none() || self.expires == Some(0)
    }
}

// ---------------------------------------------------------------------------
// Netscape cookies.txt 读写（Safari/Chrome/curl 导出的通用格式）
// ---------------------------------------------------------------------------

pub fn parse_netscape(text: &str) -> Result<Vec<Cookie>> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // 注释行，但 #HttpOnly_ 前缀是数据行
        let body = if let Some(rest) = line.strip_prefix("#HttpOnly_") {
            rest
        } else if line.starts_with('#') {
            continue;
        } else {
            line
        };

        let cols: Vec<&str> = body.split('\t').collect();
        if cols.len() < 7 {
            let cols2: Vec<&str> = body.split('\t').filter(|c| !c.is_empty()).collect();
            if cols2.len() < 7 {
                return Err(Error::InvalidCookie(format!("字段不足 7 列：{}", line)));
            }
            out.push(build(&cols2));
            continue;
        }
        out.push(build(&cols));
    }
    Ok(out)
}

fn build(cols: &[&str]) -> Cookie {
    let domain = cols[0].trim();
    let include = cols[1].trim().eq_ignore_ascii_case("TRUE") || domain.starts_with('.');
    let path = cols[2].trim();
    let secure = cols[3].trim().eq_ignore_ascii_case("TRUE");
    let expires = cols[4].trim().parse::<i64>().ok().filter(|t| *t > 0);
    Cookie {
        domain: domain.trim_start_matches('#').trim_start_matches('.').to_ascii_lowercase(),
        include_subdomains: include,
        path: if path.is_empty() { "/".to_string() } else { path.to_string() },
        secure,
        expires,
        name: cols[5].to_string(),
        value: cols[6].to_string(),
    }
}

pub fn to_netscape(cookies: &[Cookie]) -> String {
    let mut s = String::new();
    s.push_str("# Netscape HTTP Cookie File\n");
    s.push_str("# 由 resfetch 生成，可直接给 curl -b 使用\n\n");
    for c in cookies {
        let domain = if c.include_subdomains && !c.domain.starts_with('.') {
            format!(".{}", c.domain)
        } else {
            c.domain.clone()
        };
        let expires = c.expires.unwrap_or(0);
        s.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            domain,
            if c.include_subdomains { "TRUE" } else { "FALSE" },
            c.path,
            if c.secure { "TRUE" } else { "FALSE" },
            expires,
            c.name,
            c.value
        ));
    }
    s
}

/// `Cookie: a=1; b=2` 这种请求头形式
pub fn parse_header(domain: &str, path: &str, text: &str) -> Vec<Cookie> {
    let mut out = Vec::new();
    for pair in text.split(';') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let mut it = pair.splitn(2, '=');
        let name = it.next().unwrap_or("").trim();
        let value = it.next().unwrap_or("").trim();
        if !name.is_empty() {
            out.push(Cookie::new(domain, path, name, value));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 极小 JSON 解析器：只为读浏览器导出的 cookie JSON，不引 serde
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(kv) => kv.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
    fn as_num(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            Json::Str(s) => s.parse().ok(),
            _ => None,
        }
    }
    fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            Json::Str(s) => Some(s.eq_ignore_ascii_case("true")),
            _ => None,
        }
    }
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> P<'a> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }
    fn value(&mut self) -> std::result::Result<Json, String> {
        self.ws();
        match self.peek() {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b't') | Some(b'f') => {
                let word = if self.peek() == Some(b't') { "true" } else { "false" };
                self.expect_word(word)?;
                Ok(Json::Bool(word == "true"))
            }
            Some(b'n') => {
                self.expect_word("null")?;
                Ok(Json::Null)
            }
            Some(_) => self.number(),
            None => Err("意外的输入结尾".to_string()),
        }
    }
    fn expect_word(&mut self, w: &str) -> std::result::Result<(), String> {
        if self.b[self.i..].starts_with(w.as_bytes()) {
            self.i += w.len();
            Ok(())
        } else {
            Err(format!("期望 {}", w))
        }
    }
    fn number(&mut self) -> std::result::Result<Json, String> {
        let start = self.i;
        while let Some(c) = self.peek() {
            if matches!(c, b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E') {
                self.i += 1;
            } else {
                break;
            }
        }
        let s = std::str::from_utf8(&self.b[start..self.i]).map_err(|e| e.to_string())?;
        s.parse::<f64>().map(Json::Num).map_err(|e| e.to_string())
    }
    fn string(&mut self) -> std::result::Result<String, String> {
        self.i += 1; // 开引号
        let mut out = String::new();
        loop {
            match self.peek() {
                None => return Err("字符串未闭合".to_string()),
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.i += 1;
                    match self.peek() {
                        Some(b'n') => {
                            out.push('\n');
                            self.i += 1;
                        }
                        Some(b't') => {
                            out.push('\t');
                            self.i += 1;
                        }
                        Some(b'r') => {
                            out.push('\r');
                            self.i += 1;
                        }
                        Some(b'u') => {
                            self.i += 1;
                            let mut hex = String::new();
                            for _ in 0..4 {
                                if let Some(c) = self.peek() {
                                    hex.push(c as char);
                                    self.i += 1;
                                }
                            }
                            let code = u32::from_str_radix(&hex, 16).unwrap_or(0xFFFD);
                            out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                        }
                        Some(c) => {
                            out.push(c as char);
                            self.i += 1;
                        }
                        None => return Err("转义序列未结束".to_string()),
                    }
                }
                Some(c) => {
                    self.i += 1;
                    if c < 0x80 {
                        out.push(c as char);
                    } else {
                        self.i -= 1;
                        let start = self.i;
                        while let Some(x) = self.peek() {
                            if x == b'"' || x == b'\\' {
                                break;
                            }
                            self.i += 1;
                        }
                        let s = std::str::from_utf8(&self.b[start..self.i]).map_err(|e| e.to_string())?;
                        out.push_str(s);
                    }
                }
            }
        }
    }
    fn array(&mut self) -> std::result::Result<Json, String> {
        self.i += 1;
        let mut v = Vec::new();
        loop {
            self.ws();
            if self.peek() == Some(b']') {
                self.i += 1;
                return Ok(Json::Arr(v));
            }
            v.push(self.value()?);
            self.ws();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    return Ok(Json::Arr(v));
                }
                _ => return Err("数组格式错误".to_string()),
            }
        }
    }
    fn object(&mut self) -> std::result::Result<Json, String> {
        self.i += 1;
        let mut kv = Vec::new();
        loop {
            self.ws();
            if self.peek() == Some(b'}') {
                self.i += 1;
                return Ok(Json::Obj(kv));
            }
            let k = self.string()?;
            self.ws();
            if self.peek() != Some(b':') {
                return Err("对象缺少冒号".to_string());
            }
            self.i += 1;
            let v = self.value()?;
            kv.push((k, v));
            self.ws();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Json::Obj(kv));
                }
                _ => return Err("对象格式错误".to_string()),
            }
        }
    }
}

fn parse_json(text: &str) -> std::result::Result<Json, String> {
    let mut p = P { b: text.as_bytes(), i: 0 };
    p.value()
}

/// 解析浏览器扩展 / EditThisCookie 导出的 JSON 数组
pub fn parse_json_cookies(text: &str, default_domain: &str) -> Result<Vec<Cookie>> {
    let root = parse_json(text).map_err(Error::InvalidCookie)?;
    let arr = match root {
        Json::Arr(a) => a,
        other @ Json::Obj(_) => vec![other],
        _ => return Err(Error::InvalidCookie("顶层不是数组".to_string())),
    };
    let mut out = Vec::new();
    for item in arr {
        let name = match item.get("name").and_then(|v| v.as_str()) {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => continue,
        };
        let value = item.get("value").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let domain = item
            .get("domain")
            .and_then(|v| v.as_str())
            .filter(|d| !d.is_empty())
            .unwrap_or(default_domain)
            .to_string();
        let path = item.get("path").and_then(|v| v.as_str()).unwrap_or("/").to_string();
        let secure = item.get("secure").and_then(|v| v.as_bool()).unwrap_or(false);
        let expires = item
            .get("expirationDate")
            .or_else(|| item.get("expires"))
            .and_then(|v| v.as_num())
            .map(|n| n as i64)
            .filter(|n| *n > 0);
        out.push(Cookie {
            domain: domain.trim_start_matches('.').to_ascii_lowercase(),
            include_subdomains: domain.starts_with('.')
                || item.get("hostOnly").and_then(|v| v.as_bool()) == Some(false),
            path,
            secure,
            expires,
            name,
            value,
        });
    }
    Ok(out)
}

/// 自动识别格式：含制表符→Netscape；以 [ 或 { 开头→JSON；否则按 Cookie 请求头处理。
pub fn parse_any(text: &str, host_hint: &str) -> Result<Vec<Cookie>> {
    let t = text.trim();
    if t.starts_with('[') || t.starts_with('{') {
        return parse_json_cookies(t, host_hint);
    }
    if t.contains('\t') {
        return parse_netscape(t);
    }
    if t.contains('=') {
        return Ok(parse_header(host_hint, "/", t));
    }
    Err(Error::InvalidCookie("无法识别的 cookie 格式".to_string()))
}

// ---------------------------------------------------------------------------
// 合并：这是防丢的关键
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MergeReport {
    pub added: usize,
    pub updated: usize,
    pub kept: usize,
    pub expired_dropped: usize,
    pub total: usize,
}

/// 保守合并：新值覆盖同键旧值，不同键一律保留；过期项剔除并计数。
pub fn merge(
    existing: &[Cookie],
    incoming: &[Cookie],
    now: i64,
    purge_expired: bool,
) -> (Vec<Cookie>, MergeReport) {
    let mut report = MergeReport::default();
    let mut table: HashMap<(String, String, String), Cookie> = HashMap::new();
    let mut order: Vec<(String, String, String)> = Vec::new();

    for c in existing {
        if purge_expired && c.is_expired(now) {
            report.expired_dropped += 1;
            continue;
        }
        let k = c.key();
        if !table.contains_key(&k) {
            order.push(k.clone());
        }
        table.insert(k, c.clone());
    }

    for c in incoming {
        if c.is_expired(now) {
            report.expired_dropped += 1;
            continue;
        }
        let k = c.key();
        let had = table.contains_key(&k);
        if had {
            report.updated += 1;
        } else {
            report.added += 1;
            order.push(k.clone());
        }
        table.insert(k, c.clone());
    }

    report.kept = order.len().saturating_sub(report.added);
    let out: Vec<Cookie> = order.into_iter().filter_map(|k| table.remove(&k)).collect();
    report.total = out.len();
    (out, report)
}

// ---------------------------------------------------------------------------
// 发送时挑选 cookie
// ---------------------------------------------------------------------------

fn domain_matches(c: &Cookie, host: &str) -> bool {
    let host = host.trim_start_matches('.').to_ascii_lowercase();
    let d = c.domain.trim_start_matches('.').to_ascii_lowercase();
    if host == d {
        return true;
    }
    c.include_subdomains && host.ends_with(&format!(".{}", d))
}

fn path_matches(c: &Cookie, req_path: &str) -> bool {
    if c.path == "/" || c.path.is_empty() {
        return true;
    }
    if req_path == c.path {
        return true;
    }
    req_path.starts_with(&c.path)
        && (c.path.ends_with('/') || req_path.as_bytes().get(c.path.len()) == Some(&b'/'))
}

/// 生成 `name=value; name2=value2`，按路径长度倒序（更具体的优先，符合 RFC 6265）
pub fn header_value(cookies: &[Cookie], host: &str, req_path: &str, now: i64) -> String {
    let mut matched: Vec<&Cookie> = cookies
        .iter()
        .filter(|c| domain_matches(c, host) && path_matches(c, req_path) && !c.is_expired(now))
        .collect();
    matched.sort_by(|a, b| b.path.len().cmp(&a.path.len()).then(a.name.cmp(&b.name)));
    matched
        .iter()
        .map(|c| format!("{}={}", c.name, c.value))
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    #[test]
    fn netscape_roundtrip() {
        let text = "# Netscape HTTP Cookie File\n.douyin.com\tTRUE\t/\tFALSE\t1900000000\tsessionid\tabc123\n";
        let cookies = parse_netscape(text).unwrap();
        assert_eq!(cookies.len(), 1);
        assert_eq!(cookies[0].domain, "douyin.com");
        assert!(cookies[0].include_subdomains);
        let out = to_netscape(&cookies);
        let back = parse_netscape(&out).unwrap();
        assert_eq!(back, cookies);
    }

    #[test]
    fn http_only_prefix_is_data() {
        let text = "#HttpOnly_.kuaishou.com\tTRUE\t/\tTRUE\t1900000000\tkw_session\txyz\n";
        let cookies = parse_netscape(text).unwrap();
        assert_eq!(cookies[0].domain, "kuaishou.com");
        assert!(cookies[0].secure);
    }

    /// 旧版最致命的回归：导入新 cookie 时把其它站点整文件覆盖掉，丢了 65 条。
    #[test]
    fn merge_never_drops_other_sites() {
        let mut existing = Vec::new();
        for i in 0..65 {
            let domain = ["douyin.com", "kuaishou.com", "hongguoduanju.com"][i % 3];
            existing.push(Cookie::new(domain, "/", &format!("k{}", i), &format!("v{}", i)));
        }
        let incoming = vec![Cookie::new("douyin.com", "/", "sessionid", "new_value")];
        let (merged, report) = merge(&existing, &incoming, NOW, true);
        assert_eq!(merged.len(), 66, "只应新增 1 条，不能丢掉原有 65 条");
        assert_eq!(report.added, 1);
        assert_eq!(report.kept, 65);
        assert_eq!(report.updated, 0);
    }

    #[test]
    fn merge_same_key_overrides() {
        let existing = vec![Cookie::new("douyin.com", "/", "sessionid", "old")];
        let incoming = vec![Cookie::new("douyin.com", "/", "sessionid", "new")];
        let (merged, report) = merge(&existing, &incoming, NOW, true);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].value, "new");
        assert_eq!(report.updated, 1);
        assert_eq!(report.added, 0);
    }

    #[test]
    fn expired_cookies_are_dropped_and_counted() {
        let mut old = Cookie::new("douyin.com", "/", "dead", "x");
        old.expires = Some(NOW - 10);
        let mut fresh = Cookie::new("douyin.com", "/", "alive", "y");
        fresh.expires = Some(NOW + 100);
        let (merged, report) = merge(&[old], &[fresh], NOW, true);
        assert_eq!(merged.len(), 1);
        assert_eq!(report.expired_dropped, 1);
    }

    #[test]
    fn session_cookie_kept() {
        let c = Cookie::new("douyin.com", "/", "s", "v");
        assert!(c.is_session());
        assert!(!c.is_expired(NOW));
    }

    #[test]
    fn header_uses_most_specific_path_first() {
        let a = Cookie::new(".douyin.com", "/", "root", "1");
        let b = Cookie::new(".douyin.com", "/passport/", "deep", "2");
        let h = header_value(&[a, b], "www.douyin.com", "/passport/web/", NOW);
        assert_eq!(h, "deep=2; root=1");
    }

    #[test]
    fn header_respects_subdomain_flag() {
        let strict = Cookie::new("douyin.com", "/", "a", "1");
        let wild = Cookie {
            domain: "douyin.com".to_string(),
            include_subdomains: true,
            ..Cookie::new("douyin.com", "/", "b", "2")
        };
        assert_eq!(header_value(&[strict], "www.douyin.com", "/", NOW), "");
        assert_eq!(header_value(&[wild.clone()], "www.douyin.com", "/", NOW), "b=2");
        assert_eq!(header_value(&[wild], "douyin.com", "/", NOW), "b=2");
    }

    #[test]
    fn json_import() {
        let text = r#"[
          {"domain":".kuaishou.com","expirationDate":1900000000,"hostOnly":false,"name":"kw_session","path":"/","secure":true,"value":"abc"},
          {"domain":"www.kuaishou.com","name":"did","path":"/","value":"web_1"}
        ]"#;
        let cookies = parse_json_cookies(text, "kuaishou.com").unwrap();
        assert_eq!(cookies.len(), 2);
        assert_eq!(cookies[0].name, "kw_session");
        assert!(cookies[0].secure);
        assert!(cookies[0].include_subdomains);
        assert_eq!(cookies[1].expires, None);
    }

    #[test]
    fn header_format_parsed() {
        let c = parse_header("douyin.com", "/", "a=1; b=2;  c=3 ");
        assert_eq!(c.len(), 3);
        assert_eq!(c[2].name, "c");
    }

    #[test]
    fn parse_any_sniffs_format() {
        let json = r#"[{"name":"x","value":"1","domain":"a.com","path":"/"}]"#;
        assert_eq!(parse_any(json, "a.com").unwrap().len(), 1);
        let ns = "a.com\tTRUE\t/\tFALSE\t1900000000\tx\t1\n";
        assert_eq!(parse_any(ns, "a.com").unwrap().len(), 1);
        assert_eq!(parse_any("x=1; y=2", "a.com").unwrap().len(), 2);
        assert!(parse_any("乱码", "a.com").is_err());
    }

    #[test]
    fn json_parser_handles_escapes_and_unicode() {
        let text = r#"[{"name":"emoji","value":"a\"b\\c\u4e2d"}]"#;
        let c = parse_json_cookies(text, "a.com").unwrap();
        assert_eq!(c[0].value, "a\"b\\c中");
    }
}
