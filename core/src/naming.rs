//! 文件名清洗与去重。
//!
//! 三端共用一条规则：macOS 允许的字符 Windows 不一定允许，安卓上又有存储限制。
//! 这里按"最严格的一份"（Windows + FAT/exFAT 兼容）来清洗，任何平台都不会写失败。

use std::collections::HashSet;

/// Windows 保留设备名，带扩展名也照样非法。
const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// 单个文件名建议上限：255 字节是多数文件系统的硬限制，留出后缀 "(1)" 的余量。
pub const SAFE_NAME_BYTES: usize = 180;

fn is_illegal(c: char) -> bool {
    matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || (c as u32) < 0x20 || c == '\u{7f}'
}

fn split_ext(name: &str) -> (String, String) {
    match name.rfind('.') {
        Some(p) if p > 0 && name[p + 1..].chars().all(|c| c.is_ascii_alphanumeric()) && name[p + 1..].len() <= 10 => {
            (name[..p].to_string(), name[p..].to_string())
        }
        _ => (name.to_string(), String::new()),
    }
}

/// 按字节截断但保证落在字符边界上。
fn truncate_bytes(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = 0;
    for (idx, ch) in s.char_indices() {
        let next = idx + ch.len_utf8();
        if next > max_bytes {
            break;
        }
        end = next;
    }
    &s[..end]
}

/// 清洗成各平台都能写出的文件名（不含目录）。
pub fn sanitize(input: &str, max_bytes: usize) -> String {
    let mut cleaned = String::with_capacity(input.len());
    for ch in input.chars() {
        if is_illegal(ch) {
            cleaned.push('_');
        } else if ch == '\u{feff}' || ch == '\u{200b}' {
            // BOM / 零宽空格，直接丢掉
        } else {
            cleaned.push(ch);
        }
    }

    // 连续空白压成一个空格，去掉首尾空白
    let mut squeezed = String::with_capacity(cleaned.len());
    let mut last_space = false;
    for ch in cleaned.chars() {
        if ch.is_whitespace() {
            if !last_space {
                squeezed.push(' ');
                last_space = true;
            }
        } else {
            squeezed.push(ch);
            last_space = false;
        }
    }
    let mut name = squeezed.trim().to_string();

    // Windows 不允许结尾的点或空格
    while name.ends_with('.') || name.ends_with(' ') {
        name.pop();
    }
    // 前导点会变成隐藏文件，换掉
    while name.starts_with('.') {
        name.replace_range(0..1, "_");
    }

    if name.is_empty() {
        return "未命名".to_string();
    }

    // 保留扩展名做长度截断，避免截掉 ".mp4" 变成无后缀文件
    let (base, ext) = split_ext(&name);
    let budget = max_bytes.saturating_sub(ext.len());
    let base_trunc = truncate_bytes(&base, budget.max(1)).trim_end_matches(['.', ' ']).to_string();
    let mut out = format!("{}{}", if base_trunc.is_empty() { "未命名".to_string() } else { base_trunc }, ext);

    // 保留名检查（CON、NUL 之类）
    let (stem, _) = split_ext(&out);
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(&stem)) {
        out = format!("_{}", out);
    }
    out
}

/// 在已占用集合里拿到一个不冲突的名字：`标题.mp4` → `标题 (1).mp4`
pub fn unique(candidate: &str, taken: &HashSet<String>) -> String {
    if !taken.contains(candidate) {
        return candidate.to_string();
    }
    let (base, ext) = split_ext(candidate);
    for n in 1..10_000u32 {
        let probe = format!("{} ({}){}", base, n, ext);
        if !taken.contains(&probe) {
            return probe;
        }
    }
    format!("{}-{}{}", base, 9999, ext)
}

/// 从 URL 与标题推出落盘文件名。
pub fn from_url_and_title(url: &str, title: Option<&str>, default_ext: Option<&str>) -> String {
    let url_name = url
        .split('?')
        .next()
        .unwrap_or(url)
        .rsplit('/')
        .next()
        .unwrap_or("");

    let raw = match title {
        Some(t) if !t.trim().is_empty() => t.trim().to_string(),
        _ => {
            if url_name.is_empty() || url_name == "/" {
                "未命名".to_string()
            } else {
                url_name.to_string()
            }
        }
    };

    let mut name = sanitize(&raw, SAFE_NAME_BYTES);
    let has_ext = split_ext(&name).1.len() > 1;
    if !has_ext {
        if let Some(ext) = default_ext {
            name.push('.');
            name.push_str(ext);
        }
    }
    // 直链本身带扩展名时以 URL 为准（标题常常没有后缀）
    let url_ext = url_name.rsplit('.').next().unwrap_or("");
    if !has_ext && url_name.contains('.') && url_ext.len() <= 10 && !url_ext.is_empty() {
        let ext = url_ext.to_ascii_lowercase();
        if !name.to_ascii_lowercase().ends_with(&format!(".{ext}")) {
            name.push('.');
            name.push_str(&ext);
        }
    }
    sanitize(&name, SAFE_NAME_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn illegal_chars_replaced() {
        assert_eq!(sanitize("a/b\\c:d*e?f\"g<h>i|j", 180), "a_b_c_d_e_f_g_h_i_j");
    }

    #[test]
    fn control_chars_removed() {
        assert_eq!(sanitize("标题\u{0000}带\u{0007}控制符", 180), "标题_带_控制符");
    }

    #[test]
    fn trailing_dots_and_spaces_stripped() {
        assert_eq!(sanitize("  hello.mp4.  ", 180), "hello.mp4");
    }

    #[test]
    fn reserved_windows_names_prefixed() {
        assert_eq!(sanitize("CON", 180), "_CON");
        assert_eq!(sanitize("nul.mp4", 180), "_nul.mp4");
    }

    #[test]
    fn empty_becomes_placeholder() {
        assert_eq!(sanitize("   ", 180), "未命名");
        assert_eq!(sanitize("...", 180), "未命名");
    }

    #[test]
    fn long_name_keeps_extension() {
        let name = sanitize(&format!("{}.mp4", "长".repeat(200)), SAFE_NAME_BYTES);
        assert!(name.ends_with(".mp4"));
        assert!(name.len() <= SAFE_NAME_BYTES + 8);
    }

    #[test]
    fn truncation_is_char_boundary_safe() {
        let out = sanitize(&"😀".repeat(100), 40);
        assert!(out.chars().all(|c| c == '😀'));
        assert!(out.len() <= 40);
    }

    #[test]
    fn unique_generates_counter() {
        let mut taken = HashSet::new();
        taken.insert("视频.mp4".to_string());
        assert_eq!(unique("视频.mp4", &taken), "视频 (1).mp4");
        taken.insert("视频 (1).mp4".to_string());
        assert_eq!(unique("视频.mp4", &taken), "视频 (2).mp4");
        assert_eq!(unique("别的.mp4", &taken), "别的.mp4");
    }

    #[test]
    fn filename_from_title_and_url() {
        assert_eq!(
            from_url_and_title("https://x.com/a/730123.mp4?sign=1", Some("第一集：开端"), None),
            "第一集：开端.mp4"
        );
        assert_eq!(from_url_and_title("https://x.com/a/730123", None, Some("mp4")), "730123.mp4");
    }
}
