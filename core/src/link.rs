//! 链接识别与归一化。
//!
//! 这是三端共用的第一道关：不管是 macOS 桌面、Windows 还是安卓分享面板，
//! 用户丢进来的都是同一种"原始链接"（带一堆跟踪参数、短链域名、分享文案）。
//! 归一化只做确定性的事，不改写语义。

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    Douyin,
    Kuaishou,
    Xiaohongshu,
    Bilibili,
    Hongguo,
    /// 直链文件（图片/视频/音频/文档）
    Direct,
    /// m3u8 播放列表
    Hls,
    Unknown,
}

impl Platform {
    pub fn as_str(&self) -> &'static str {
        match self {
            Platform::Douyin => "douyin",
            Platform::Kuaishou => "kuaishou",
            Platform::Xiaohongshu => "xiaohongshu",
            Platform::Bilibili => "bilibili",
            Platform::Hongguo => "hongguo",
            Platform::Direct => "direct",
            Platform::Hls => "hls",
            Platform::Unknown => "unknown",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Platform::Douyin => "抖音",
            Platform::Kuaishou => "快手",
            Platform::Xiaohongshu => "小红书",
            Platform::Bilibili => "哔哩哔哩",
            Platform::Hongguo => "红果短剧",
            Platform::Direct => "直链",
            Platform::Hls => "HLS 流",
            Platform::Unknown => "未知",
        }
    }

    fn from_host(host: &str) -> Option<Platform> {
        let h = host.trim_start_matches("www.");
        if h == "douyin.com" || h.ends_with(".douyin.com") || h == "iesdouyin.com" {
            Some(Platform::Douyin)
        } else if h == "kuaishou.com" || h.ends_with(".kuaishou.com") || h == "gifshow.com" {
            Some(Platform::Kuaishou)
        } else if h == "xiaohongshu.com" || h.ends_with(".xiaohongshu.com") || h == "xhslink.com" {
            Some(Platform::Xiaohongshu)
        } else if h == "bilibili.com" || h.ends_with(".bilibili.com") || h == "b23.tv" {
            Some(Platform::Bilibili)
        } else if h == "hongguoduanju.com" || h.ends_with(".hongguoduanju.com") {
            Some(Platform::Hongguo)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Video,
    Audio,
    Image,
    Playlist,
    Document,
    Unknown,
}

impl Kind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Video => "video",
            Kind::Audio => "audio",
            Kind::Image => "image",
            Kind::Playlist => "playlist",
            Kind::Document => "document",
            Kind::Unknown => "unknown",
        }
    }

    pub fn default_ext(&self) -> Option<&'static str> {
        match self {
            Kind::Video => Some("mp4"),
            Kind::Audio => Some("m4a"),
            Kind::Image => Some("jpg"),
            Kind::Playlist => Some("m3u8"),
            Kind::Document => None,
            Kind::Unknown => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedLink {
    pub original: String,
    pub canonical: String,
    pub platform: Platform,
    pub kind: Kind,
    pub id: Option<String>,
    pub host: String,
    pub path: String,
}

impl ParsedLink {
    /// 给任务列表用的单行摘要，UI 三端共用。
    pub fn summary(&self) -> String {
        match &self.id {
            Some(id) => format!("{} · {}", self.platform.label(), id),
            None => format!("{} · {}", self.platform.label(), self.host),
        }
    }
}

/// 极简 URL 拆分，不引第三方 url crate（核心保持零依赖）。
#[derive(Debug, Clone)]
pub struct UrlParts<'a> {
    pub scheme: &'a str,
    pub host: &'a str,
    pub port: Option<&'a str>,
    pub path: &'a str,
    pub query: &'a str,
    pub fragment: &'a str,
}

pub fn split(url: &str) -> Option<UrlParts<'_>> {
    let trimmed = url.trim();
    let sep = trimmed.find("://")?;
    let scheme = &trimmed[..sep];
    if scheme.is_empty() || !scheme.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.') {
        return None;
    }
    let rest = &trimmed[sep + 3..];
    let end_authority = rest.find(|c| c == '/' || c == '?' || c == '#').unwrap_or(rest.len());
    let authority = &rest[..end_authority];
    let tail = &rest[end_authority..];

    // 去掉 userinfo
    let hostpart = match authority.rfind('@') {
        Some(p) => &authority[p + 1..],
        None => authority,
    };
    if hostpart.is_empty() {
        return None;
    }

    let (host, port) = match hostpart.rfind(':') {
        Some(p) => {
            let maybe_port = &hostpart[p + 1..];
            if !maybe_port.is_empty() && maybe_port.chars().all(|c| c.is_ascii_digit()) {
                (&hostpart[..p], Some(maybe_port))
            } else {
                (hostpart, None)
            }
        }
        None => (hostpart, None),
    };
    if host.is_empty() {
        return None;
    }

    let mut path = tail;
    let mut query = "";
    let mut fragment = "";
    if let Some(p) = path.find('#') {
        fragment = &path[p + 1..];
        path = &path[..p];
    }
    if let Some(p) = path.find('?') {
        query = &path[p + 1..];
        path = &path[..p];
    }
    if path.is_empty() {
        path = "/";
    }

    Some(UrlParts {
        scheme,
        host,
        port,
        path,
        query,
        fragment,
    })
}

/// 去掉跟踪参数：只保留真正决定"资源是哪一条"的参数。
fn is_tracking_param(key: &str) -> bool {
    const PREFIXES: [&str; 8] = ["utm_", "share_", "ug_", "ms_", "tt_", "track_", "spm_", "wfr_"];
    let k = key.to_ascii_lowercase();
    if PREFIXES.iter().any(|p| k.starts_with(p)) {
        return true;
    }
    matches!(
        k.as_str(),
        "from" | "enter_from" | "share_from" | "is_copy_url" | "is_from_webapp" | "social_share"
            | "timestamp" | "utm" | "spm" | "scene" | "refer" | "referer" | "channel"
            | "streamsource" | "stream_source" | "authorid_share" | "loginfree"
            | "previous_page" | "previous_loc" | "share_token" | "sec_uid_share"
    )
}

pub fn canonicalize(url: &str) -> Result<String> {
    let parts = split(url).ok_or_else(|| Error::InvalidUrl(url.to_string()))?;
    let host = parts.host.to_ascii_lowercase();
    let port = match parts.port {
        Some("80") if parts.scheme.eq_ignore_ascii_case("http") => None,
        Some("443") if parts.scheme.eq_ignore_ascii_case("https") => None,
        Some(p) => Some(p),
        None => None,
    };

    let kept: Vec<&str> = parts
        .query
        .split('&')
        .filter(|kv| !kv.is_empty())
        .filter(|kv| {
            let key = kv.split('=').next().unwrap_or("");
            !is_tracking_param(key)
        })
        .collect();

    let mut out = String::new();
    out.push_str(&parts.scheme.to_ascii_lowercase());
    out.push_str("://");
    out.push_str(&host);
    if let Some(p) = port {
        out.push(':');
        out.push_str(p);
    }
    out.push_str(parts.path);
    if !kept.is_empty() {
        out.push('?');
        out.push_str(&kept.join("&"));
    }
    Ok(out)
}

fn ext_of(path: &str) -> Option<String> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let dot = name.rfind('.')?;
    let ext = &name[dot + 1..];
    if ext.is_empty() || ext.len() > 8 {
        return None;
    }
    Some(ext.to_ascii_lowercase())
}

pub fn kind_from_path(path: &str) -> Kind {
    match ext_of(path).as_deref() {
        Some("m3u8") | Some("mpd") => Kind::Playlist,
        Some("mp4") | Some("m4v") | Some("webm") | Some("mov") | Some("mkv") | Some("flv") => Kind::Video,
        Some("mp3") | Some("m4a") | Some("aac") | Some("flac") | Some("wav") | Some("ogg") => Kind::Audio,
        Some("jpg") | Some("jpeg") | Some("png") | Some("webp") | Some("gif") | Some("heic") => Kind::Image,
        Some("pdf") | Some("epub") | Some("txt") | Some("md") => Kind::Document,
        _ => Kind::Unknown,
    }
}

/// 在 `marker` 之后截取一段 id（字母数字、下划线、连字符）。
fn id_after(haystack: &str, marker: &str) -> Option<String> {
    let start = haystack.find(marker)? + marker.len();
    let rest = &haystack[start..];
    let id: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

fn query_param(query: &str, needle: &str) -> Option<String> {
    for kv in query.split('&') {
        let mut it = kv.splitn(2, '=');
        let k = it.next().unwrap_or("");
        let v = it.next().unwrap_or("");
        if k == needle && !v.is_empty() {
            return Some(v.to_string());
        }
    }
    None
}

fn extract_id(platform: Platform, parts: &UrlParts<'_>) -> Option<String> {
    match platform {
        Platform::Douyin => id_after(parts.path, "/video/")
            .or_else(|| id_after(parts.path, "/note/"))
            .or_else(|| id_after(parts.path, "/share/video/"))
            .or_else(|| query_param(parts.query, "modal_id"))
            .or_else(|| query_param(parts.query, "aweme_id")),
        Platform::Kuaishou => id_after(parts.path, "/short-video/").or_else(|| id_after(parts.path, "/f/")),
        Platform::Xiaohongshu => id_after(parts.path, "/explore/")
            .or_else(|| id_after(parts.path, "/discovery/item/"))
            .or_else(|| id_after(parts.path, "/item/")),
        Platform::Bilibili => {
            if let Some(bv) = parts.path.find("BV") {
                let tail = &parts.path[bv..];
                let id: String = tail.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
                if id.len() > 2 {
                    return Some(id);
                }
            }
            id_after(parts.path, "/video/av").map(|n| format!("av{}", n))
        }
        Platform::Hongguo => id_after(parts.path, "/detail/").or_else(|| query_param(parts.query, "id")),
        _ => None,
    }
}

pub fn parse(url: &str) -> Result<ParsedLink> {
    let raw = url.trim();
    if raw.is_empty() {
        return Err(Error::EmptyInput);
    }
    // 分享文案里常带一段"复制打开抖音"的文字，只取其中的 URL。
    let url_only = match raw.find("http") {
        Some(p) if p > 0 => &raw[p..],
        _ => raw,
    };
    let url_only = url_only.split_whitespace().next().unwrap_or(url_only);
    let url_only = url_only.trim_end_matches(|c: char| matches!(c, '，' | '。' | ',' | '.' | ')' | '）'));

    let parts = split(url_only).ok_or_else(|| Error::InvalidUrl(url_only.to_string()))?;
    let scheme = parts.scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(Error::UnsupportedUrl(url_only.to_string()));
    }

    let canonical = canonicalize(url_only)?;
    let cparts = split(&canonical).ok_or_else(|| Error::InvalidUrl(canonical.clone()))?;
    let host = cparts.host.to_ascii_lowercase();

    let kind_by_path = kind_from_path(cparts.path);
    let platform = match Platform::from_host(&host) {
        Some(p) => p,
        None => {
            if kind_by_path == Kind::Playlist {
                Platform::Hls
            } else if kind_by_path != Kind::Unknown {
                Platform::Direct
            } else {
                Platform::Unknown
            }
        }
    };

    let kind = if platform == Platform::Hls {
        Kind::Playlist
    } else if kind_by_path != Kind::Unknown {
        kind_by_path
    } else {
        match platform {
            Platform::Direct | Platform::Hls => Kind::Unknown,
            _ => Kind::Video,
        }
    };

    Ok(ParsedLink {
        original: raw.to_string(),
        id: extract_id(platform, &cparts),
        platform,
        kind,
        host,
        path: cparts.path.to_string(),
        canonical,
    })
}

/// 从一批混杂文本里挑出所有能识别的链接（分享面板粘贴多行时用）。
pub fn parse_many(text: &str) -> Vec<ParsedLink> {
    let mut out: Vec<ParsedLink> = Vec::new();
    for token in text.split(|c: char| c.is_whitespace()) {
        let t = token.trim();
        if t.starts_with("http") {
            if let Ok(p) = parse(t) {
                if !out.iter().any(|x| x.canonical == p.canonical) {
                    out.push(p);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn douyin_with_tracking_params() {
        let p = parse("https://www.douyin.com/video/7301234567890123456?previous_page=app_code_link&utm_source=copy").unwrap();
        assert_eq!(p.platform, Platform::Douyin);
        assert_eq!(p.id.as_deref(), Some("7301234567890123456"));
        assert!(!p.canonical.contains("utm_source"));
        assert!(!p.canonical.contains("previous_page"));
        assert_eq!(p.canonical, "https://www.douyin.com/video/7301234567890123456");
    }

    #[test]
    fn douyin_share_text_with_chinese() {
        let p = parse("7.68 复制打开抖音，看看【某某】的作品 https://v.douyin.com/iRNBho6u/ 复制此链接").unwrap();
        assert_eq!(p.platform, Platform::Douyin);
        assert_eq!(p.host, "v.douyin.com");
    }

    #[test]
    fn kuaishou_short_video() {
        let p = parse("https://www.kuaishou.com/short-video/3xf8abcd1234?authorId=3x1&streamSource=find").unwrap();
        assert_eq!(p.platform, Platform::Kuaishou);
        assert_eq!(p.id.as_deref(), Some("3xf8abcd1234"));
        assert!(!p.canonical.contains("streamSource"));
    }

    #[test]
    fn xiaohongshu_note() {
        let p = parse("https://www.xiaohongshu.com/explore/65f0a1b2c3d4e5f60718293a?xsec_token=AB").unwrap();
        assert_eq!(p.platform, Platform::Xiaohongshu);
        assert_eq!(p.id.as_deref(), Some("65f0a1b2c3d4e5f60718293a"));
    }

    #[test]
    fn bilibili_bv_id() {
        let p = parse("https://www.bilibili.com/video/BV1GJ411x7h7/?spm_id_from=333.999").unwrap();
        assert_eq!(p.platform, Platform::Bilibili);
        assert_eq!(p.id.as_deref(), Some("BV1GJ411x7h7"));
        assert!(!p.canonical.contains("spm_id_from"));
    }

    #[test]
    fn hls_playlist_detected() {
        let p = parse("https://cdn.example.com/path/ep1/index.m3u8?sign=abc").unwrap();
        assert_eq!(p.platform, Platform::Hls);
        assert_eq!(p.kind, Kind::Playlist);
    }

    #[test]
    fn direct_image() {
        let p = parse("https://p3-sign.douyinpic.com/obj/x.jpeg?x-expires=1").unwrap();
        assert_eq!(p.platform, Platform::Direct);
        assert_eq!(p.kind, Kind::Image);
    }

    #[test]
    fn reject_bad_scheme() {
        assert!(matches!(parse("ftp://a.com/x"), Err(Error::UnsupportedUrl(_))));
        assert!(matches!(parse(""), Err(Error::EmptyInput)));
        assert!(matches!(parse("不是链接"), Err(Error::InvalidUrl(_))));
    }

    #[test]
    fn default_ports_removed() {
        assert_eq!(canonicalize("HTTPS://Example.COM:443/a?b=1").unwrap(), "https://example.com/a?b=1");
        assert_eq!(canonicalize("http://example.com:80/a").unwrap(), "http://example.com/a");
        assert_eq!(canonicalize("https://example.com:8443/a").unwrap(), "https://example.com:8443/a");
    }

    #[test]
    fn parse_many_dedup() {
        let text = "https://www.douyin.com/video/123\nhttps://www.douyin.com/video/123?utm_source=x\nhttps://www.kuaishou.com/short-video/abc";
        let list = parse_many(text);
        assert_eq!(list.len(), 2);
    }
}
