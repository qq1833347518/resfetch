//! 分级日志、脱敏与去重。
//!
//! 三端共用一套日志口径：级别可调、敏感信息一律打码、同一条错误 5 秒内只写一次。
//! 这些都是旧版在实际使用里被逼出来的规则（日志里出现过明文 cookie 和手机号）。

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Trace = 0,
    Debug = 1,
    Info = 2,
    Warn = 3,
    Error = 4,
}

impl Level {
    pub fn parse(s: &str) -> Option<Level> {
        match s.trim().to_ascii_lowercase().as_str() {
            "trace" => Some(Level::Trace),
            "debug" => Some(Level::Debug),
            "info" => Some(Level::Info),
            "warn" | "warning" => Some(Level::Warn),
            "error" => Some(Level::Error),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Level::Trace => "TRC",
            Level::Debug => "DBG",
            Level::Info => "INF",
            Level::Warn => "WRN",
            Level::Error => "ERR",
        }
    }
}

/// 把 Unix 秒转成 UTC 的 `YYYY-MM-DD HH:MM:SS`（不引 chrono）。
pub fn format_timestamp(epoch: i64) -> String {
    let days = epoch.div_euclid(86_400);
    let secs = epoch.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        y,
        m,
        d,
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// Howard Hinnant 的 days→civil 算法（1970-01-01 起算）
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn redact_phone(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_digit() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            let run: String = chars[start..i].iter().collect();
            let is_cn_mobile =
                run.len() == 11 && run.starts_with('1') && matches!(run.as_bytes()[1], b'3'..=b'9');
            if is_cn_mobile {
                out.push_str(&format!("{}****{}", &run[..3], &run[7..]));
            } else {
                out.push_str(&run);
            }
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn redact_emails(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(at) = rest.find('@') {
        let before = &rest[..at];
        let local_start = before
            .rfind(|c: char| !(c.is_alphanumeric() || c == '.' || c == '_' || c == '-' || c == '+'))
            .map(|p| p + 1)
            .unwrap_or(0);
        let local = &before[local_start..];
        if local.len() >= 2 && !local.contains(' ') {
            out.push_str(&before[..local_start]);
            let keep: usize = local.chars().take(2).map(|c| c.len_utf8()).sum();
            out.push_str(&format!("{}***", &local[..keep]));
            rest = &rest[at..];
            let domain_end = rest
                .find(|c: char| c.is_whitespace() || c == ',' || c == ';')
                .unwrap_or(rest.len());
            out.push_str(&rest[..domain_end]);
            rest = &rest[domain_end..];
            continue;
        }
        out.push_str(&rest[..at + 1]);
        rest = &rest[at + 1..];
    }
    out.push_str(rest);
    out
}

/// 敏感信息脱敏。规则都是"宁可多打码"：
/// - `Cookie:` / `Set-Cookie:` 整行值
/// - `token=` / `sign=` / `sessionid=` / `secret=` 等参数值
/// - `Authorization: Bearer ...`
/// - 中国大陆手机号、邮箱
/// - 本机用户目录（日志外发时不泄露用户名）
pub fn redact(input: &str) -> String {
    let mut lines: Vec<String> = Vec::new();

    for line in input.split('\n') {
        let mut l = line.to_string();

        for header in ["cookie", "set-cookie", "authorization", "x-token"] {
            if let Some(pos) = l.to_ascii_lowercase().find(&format!("{}:", header)) {
                let val_start = pos + header.len() + 1;
                l.truncate(val_start);
                l.push_str(" <已脱敏>");
                break;
            }
        }

        let mut out = String::with_capacity(l.len());
        let mut cursor = 0usize;
        let lower = l.to_ascii_lowercase();
        let keys = [
            "token=", "access_token=", "sessionid=", "session_id=", "sign=", "sig=", "secret=",
            "ticket=", "password=", "passwd=", "authorization=", "cookie=", "xsec_token=",
            "verify=",
        ];
        while cursor < l.len() {
            let hit = keys
                .iter()
                .filter_map(|k| lower[cursor..].find(k).map(|p| (cursor + p, k.len())))
                .min();
            match hit {
                Some((p, klen)) => {
                    out.push_str(&l[cursor..p]);
                    out.push_str(&l[p..p + klen]);
                    out.push_str("***");
                    let val_start = p + klen;
                    let val_end = l[val_start..]
                        .find(|c: char| c == '&' || c == ' ' || c == ';' || c == '"' || c == '\'')
                        .map(|q| val_start + q)
                        .unwrap_or(l.len());
                    cursor = val_end;
                }
                None => {
                    out.push_str(&l[cursor..]);
                    break;
                }
            }
        }
        l = out;

        l = redact_emails(&l);
        l = redact_phone(&l);

        if let Some(pos) = l.find("/Users/") {
            let rest = &l[pos + 7..];
            let end = rest.find('/').map(|q| pos + 7 + q).unwrap_or(l.len());
            let user = &l[pos + 7..end];
            if !user.is_empty() && user != "Shared" {
                l = format!("{}/Users/***{}", &l[..pos], &l[end..]);
            }
        }

        lines.push(l);
    }

    lines.join("\n")
}

/// 日志器：只管级别过滤，真正的写在各个外壳里（桌面写文件、安卓写 logcat）
#[derive(Debug, Clone)]
pub struct Logger {
    pub level: Level,
}

impl Logger {
    pub fn new(level: Level) -> Self {
        Logger { level }
    }

    pub fn enabled(&self, level: Level) -> bool {
        level >= self.level
    }

    pub fn line(&self, level: Level, target: &str, msg: &str, now: i64) -> Option<String> {
        if !self.enabled(level) {
            return None;
        }
        let text = format!("{} [{}] {}: {}", format_timestamp(now), level.as_str(), target, msg);
        let mut text = redact(&text);
        const MAX: usize = 2000;
        if text.chars().count() > MAX {
            let cut: String = text.chars().take(MAX).collect();
            text = format!("{}…（已截断）", cut);
        }
        Some(text)
    }
}

/// 同一 key 在窗口期内只放行一次，用于"同一个拦截原因每秒刷屏"的场景。
#[derive(Debug, Default)]
pub struct RateLimiter {
    window: u64,
    last: HashMap<String, u64>,
}

impl RateLimiter {
    pub fn new(window_secs: u64) -> Self {
        RateLimiter { window: window_secs, last: HashMap::new() }
    }

    pub fn allow(&mut self, key: &str, now: u64) -> bool {
        match self.last.get(key) {
            Some(prev) if now.saturating_sub(*prev) < self.window => false,
            _ => {
                self.last.insert(key.to_string(), now);
                true
            }
        }
    }

    pub fn window(&self) -> u64 {
        self.window
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_matches_known_epoch() {
        assert_eq!(format_timestamp(1_700_000_000), "2023-11-14 22:13:20");
        assert_eq!(format_timestamp(0), "1970-01-01 00:00:00");
        assert_eq!(format_timestamp(1_800_000_000).len(), 19);
    }

    #[test]
    fn cookie_header_is_fully_masked() {
        let r = redact("Cookie: sessionid=abc123; uid=42\n其他行保留");
        assert!(r.starts_with("Cookie: <已脱敏>"));
        assert!(r.contains("其他行保留"));
        assert!(!r.contains("abc123"));
    }

    #[test]
    fn url_params_are_masked() {
        let r = redact("GET https://x.com/api?sign=abcdef&token=zzz&page=2");
        assert!(r.contains("sign=***"));
        assert!(r.contains("token=***"));
        assert!(!r.contains("abcdef"));
        assert!(!r.contains("zzz"));
        assert!(r.contains("page=2"), "非敏感参数必须保留，否则排障就没信息了");
    }

    #[test]
    fn phone_and_email_masked() {
        let r = redact("账号 13812345678 邮箱 zhangsan@example.com");
        assert!(r.contains("138****5678"));
        assert!(!r.contains("12345678"));
        assert!(r.contains("zh***@example.com"));
        assert!(!r.contains("zhangsan@"));
    }

    #[test]
    fn home_path_masked() {
        let r = redact("写入 /Users/demo/DouyinDL/app.log 成功");
        assert_eq!(r, "写入 /Users/***/DouyinDL/app.log 成功");
    }

    #[test]
    fn level_filter() {
        let logger = Logger::new(Level::Warn);
        assert!(logger.line(Level::Error, "net", "失败", 1_800_000_000).is_some());
        assert!(logger.line(Level::Info, "net", "开始", 1_800_000_000).is_none());
    }

    #[test]
    fn logger_truncates_long_lines() {
        let logger = Logger::new(Level::Info);
        let line = logger.line(Level::Info, "net", &"x".repeat(5000), 1_800_000_000).unwrap();
        assert!(line.ends_with("…（已截断）"));
        assert!(line.chars().count() < 2100);
    }

    #[test]
    fn rate_limiter_blocks_repeats_within_window() {
        let mut rl = RateLimiter::new(5);
        assert!(rl.allow("bitbrowser://cc/", 100));
        assert!(!rl.allow("bitbrowser://cc/", 103));
        assert!(rl.allow("bitbrowser://cc/", 105));
        assert!(rl.allow("snssdk://x", 103));
        assert_eq!(rl.window(), 5);
    }

    #[test]
    fn level_parse_and_ordering() {
        assert_eq!(Level::parse("WARN"), Some(Level::Warn));
        assert_eq!(Level::parse("nope"), None);
        assert!(Level::Error > Level::Warn);
        assert_eq!(Level::Info.as_str(), "INF");
    }
}
