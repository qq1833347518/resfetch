//! m3u8 解析与下载清单生成。
//!
//! 旧版是调外部 ffmpeg 兜底；新内核自己解析，好处是三端行为一致，
//! 而且能在下载前就把分片数、总时长、加密方式算清楚，给 UI 一个可信的进度分母。

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    pub method: String,
    pub uri: Option<String>,
    pub iv: Option<String>,
}

impl Key {
    pub fn is_encrypted(&self) -> bool {
        !self.method.eq_ignore_ascii_case("NONE")
    }

    /// 本版能解的只有 AES-128（SAMPLE-AES 需要更完整的解密链，明确拒绝而不是硬跑）
    pub fn method_supported(&self) -> bool {
        self.method.eq_ignore_ascii_case("AES-128")
    }

    /// 该分片的 IV：`EXT-X-KEY` 给了 IV 就用它，没给就用媒体序号（RFC 8216 §5.2）
    pub fn iv_for(&self, sequence: u64) -> Option<[u8; 16]> {
        match &self.iv {
            Some(h) => crate::aes::parse_iv(h),
            None => Some(crate::aes::sequence_iv(sequence)),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub duration: f64,
    pub uri: String,
    pub key: Option<Key>,
    /// (起始字节, 长度) —— EXT-X-BYTERANGE
    pub byte_range: Option<(u64, u64)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variant {
    pub bandwidth: u64,
    pub resolution: Option<(u32, u32)>,
    pub codecs: Option<String>,
    pub uri: String,
}

impl Variant {
    pub fn height(&self) -> Option<u32> {
        self.resolution.map(|(_, h)| h)
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Playlist {
    pub is_master: bool,
    pub variants: Vec<Variant>,
    pub segments: Vec<Segment>,
    pub map_uri: Option<String>,
    pub target_duration: u32,
    pub endlist: bool,
    pub total_duration: f64,
    /// 第一片的媒体序号（`EXT-X-MEDIA-SEQUENCE`），没写就是 0；
    /// 没带 IV 的加密分片要靠它推 IV，所以不能丢
    pub media_sequence: u64,
}

impl Playlist {
    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    pub fn encrypted(&self) -> bool {
        self.segments
            .iter()
            .any(|s| s.key.as_ref().map(|k| k.is_encrypted()).unwrap_or(false))
    }

    /// 选码率：给了高度上限就在上限内挑最高码率；没有上限就挑最高。
    pub fn pick_variant(&self, max_height: Option<u32>) -> Option<&Variant> {
        let cap = max_height.unwrap_or(u32::MAX);
        self.variants
            .iter()
            .filter(|v| v.height().map(|h| h <= cap).unwrap_or(true))
            .max_by_key(|v| v.bandwidth)
            .or_else(|| self.variants.iter().max_by_key(|v| v.bandwidth))
    }
}

/// 解析 `KEY=VALUE,KEY="VALUE,WITH,COMMAS"` 形式
fn parse_attrs(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i] == b',' || bytes[i] == b' ') {
            i += 1;
        }
        let key_start = i;
        while i < bytes.len() && bytes[i] != b'=' && bytes[i] != b',' {
            i += 1;
        }
        let key = s[key_start..i].trim().to_ascii_uppercase();
        if i >= bytes.len() || bytes[i] != b'=' {
            continue;
        }
        i += 1;
        let val = if i < bytes.len() && bytes[i] == b'"' {
            i += 1;
            let start = i;
            while i < bytes.len() && bytes[i] != b'"' {
                i += 1;
            }
            let v = s[start..i].to_string();
            i += 1;
            v
        } else {
            let start = i;
            while i < bytes.len() && bytes[i] != b',' {
                i += 1;
            }
            s[start..i].trim().to_string()
        };
        if !key.is_empty() {
            out.push((key, val));
        }
    }
    out
}

fn attr<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn parse_resolution(s: &str) -> Option<(u32, u32)> {
    let mut it = s.split(['x', 'X']);
    let w = it.next()?.trim().parse().ok()?;
    let h = it.next()?.trim().parse().ok()?;
    Some((w, h))
}

/// 相对路径解析（不依赖第三方 url crate）
pub fn resolve(base: &str, rel: &str) -> String {
    let rel = rel.trim();
    if rel.starts_with("http://") || rel.starts_with("https://") {
        return rel.to_string();
    }
    let (scheme, authority) = match base.find("://") {
        Some(p) => {
            let rest = &base[p + 3..];
            let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
            (&base[..p], &rest[..end])
        }
        None => ("https", ""),
    };

    if rel.starts_with("//") {
        return format!("{}:{}", scheme, rel);
    }
    if rel.starts_with('/') {
        return format!("{}://{}{}", scheme, authority, rel);
    }
    if rel.is_empty() {
        return base.to_string();
    }

    let clean = base.split(['?', '#']).next().unwrap_or(base);
    let dir = match clean.rfind('/') {
        Some(p) if p > clean.find("://").map(|q| q + 3).unwrap_or(0) => &clean[..p + 1],
        _ => return rel.to_string(),
    };

    let joined = format!("{}{}", dir, rel);
    normalize_path(&joined)
}

fn normalize_path(url: &str) -> String {
    let (head, path) = match url.find("://") {
        Some(p) => {
            let rest = &url[p + 3..];
            let end = rest.find('/').map(|i| p + 3 + i).unwrap_or(url.len());
            (&url[..end], &url[end..])
        }
        None => ("", url),
    };
    let mut segs: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                segs.pop();
            }
            s => segs.push(s),
        }
    }
    format!("{}/{}", head, segs.join("/"))
}

pub fn parse(text: &str, base: &str) -> Result<Playlist> {
    let mut pl = Playlist::default();
    let mut pending_variant: Option<(u64, Option<(u32, u32)>, Option<String>)> = None;
    let mut pending_duration: Option<f64> = None;
    let mut pending_range: Option<(u64, u64)> = None;
    let mut current_key: Option<Key> = None;
    let mut has_content = false;
    let mut media_sequence: u64 = 0;

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        has_content = true;

        if let Some(rest) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let attrs = parse_attrs(rest);
            let bw = attr(&attrs, "BANDWIDTH").and_then(|v| v.parse().ok()).unwrap_or(0);
            let res = attr(&attrs, "RESOLUTION").and_then(parse_resolution);
            let codecs = attr(&attrs, "CODECS").map(|s| s.to_string());
            pending_variant = Some((bw, res, codecs));
            pl.is_master = true;
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            let d = rest.split(',').next().unwrap_or("").trim();
            pending_duration = d.parse::<f64>().ok();
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-KEY:") {
            let attrs = parse_attrs(rest);
            let method = attr(&attrs, "METHOD").unwrap_or("NONE").to_string();
            let uri = attr(&attrs, "URI").map(|u| resolve(base, u));
            let iv = attr(&attrs, "IV").map(|v| v.to_string());
            current_key = Some(Key { method, uri, iv });
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-MAP:") {
            let attrs = parse_attrs(rest);
            pl.map_uri = attr(&attrs, "URI").map(|u| resolve(base, u));
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            let mut it = rest.split('@');
            let len = it.next().unwrap_or("").trim().parse::<u64>().unwrap_or(0);
            let off = it.next().and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(0);
            pending_range = Some((off, len));
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            media_sequence = rest.trim().parse().unwrap_or(0);
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
            pl.target_duration = rest.trim().parse().unwrap_or(0);
            continue;
        }
        if line == "#EXT-X-ENDLIST" {
            pl.endlist = true;
            continue;
        }
        if line.starts_with('#') {
            continue;
        }

        let uri = resolve(base, line);
        if let Some((bw, res, codecs)) = pending_variant.take() {
            pl.variants.push(Variant { bandwidth: bw, resolution: res, codecs, uri });
            continue;
        }
        if let Some(dur) = pending_duration.take() {
            pl.segments.push(Segment {
                duration: dur,
                uri,
                key: current_key.clone(),
                byte_range: pending_range.take(),
            });
            pl.total_duration += dur;
            continue;
        }
        pending_range = None;
    }

    pl.media_sequence = media_sequence;

    if !has_content {
        return Err(Error::InvalidPlaylist("播放列表为空".to_string()));
    }
    if pl.segments.is_empty() && pl.variants.is_empty() {
        return Err(Error::InvalidPlaylist("既没有分片也没有码率变体".to_string()));
    }
    if !pl.variants.is_empty() {
        pl.is_master = true;
    }
    Ok(pl)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "https://cdn.example.com/hls/ep1/index.m3u8?sign=abc";

    #[test]
    fn parse_master_playlist() {
        let text = "#EXTM3U\n\
            #EXT-X-STREAM-INF:BANDWIDTH=1280000,RESOLUTION=1280x720,CODECS=\"avc1.4d401f,mp4a.40.2\"\n\
            720p/index.m3u8\n\
            #EXT-X-STREAM-INF:BANDWIDTH=2560000,RESOLUTION=1920x1080\n\
            1080p/index.m3u8\n\
            #EXT-X-STREAM-INF:BANDWIDTH=640000,RESOLUTION=640x360\n\
            https://cdn.example.com/hls/ep1/360p/index.m3u8\n";
        let pl = parse(text, BASE).unwrap();
        assert!(pl.is_master);
        assert_eq!(pl.variants.len(), 3);
        assert_eq!(pl.variants[0].resolution, Some((1280, 720)));
        assert_eq!(pl.variants[0].codecs.as_deref(), Some("avc1.4d401f,mp4a.40.2"));
        assert_eq!(pl.variants[0].uri, "https://cdn.example.com/hls/ep1/720p/index.m3u8");
        assert_eq!(pl.variants[2].bandwidth, 640000);
    }

    #[test]
    fn pick_variant_respects_height_cap() {
        let text = "#EXTM3U\n\
            #EXT-X-STREAM-INF:BANDWIDTH=2560000,RESOLUTION=1920x1080\na.m3u8\n\
            #EXT-X-STREAM-INF:BANDWIDTH=1280000,RESOLUTION=1280x720\nb.m3u8\n\
            #EXT-X-STREAM-INF:BANDWIDTH=640000,RESOLUTION=640x360\nc.m3u8\n";
        let pl = parse(text, BASE).unwrap();
        assert_eq!(pl.pick_variant(Some(720)).unwrap().bandwidth, 1280000);
        assert_eq!(pl.pick_variant(Some(480)).unwrap().bandwidth, 640000);
        assert_eq!(pl.pick_variant(None).unwrap().bandwidth, 2560000);
    }

    #[test]
    fn parse_media_playlist_with_key_and_map() {
        let text = "#EXTM3U\n\
            #EXT-X-TARGETDURATION:6\n\
            #EXT-X-MAP:URI=\"init.mp4\"\n\
            #EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\",IV=0x0102\n\
            #EXTINF:5.5,\n\
            seg0.m4s\n\
            #EXTINF:5.5,\n\
            seg1.m4s\n\
            #EXT-X-ENDLIST\n";
        let pl = parse(text, BASE).unwrap();
        assert!(!pl.is_master);
        assert_eq!(pl.segment_count(), 2);
        assert!((pl.total_duration - 11.0).abs() < 1e-9);
        assert!(pl.endlist);
        assert!(pl.encrypted());
        assert_eq!(
            pl.segments[0].key.as_ref().unwrap().uri.as_deref(),
            Some("https://cdn.example.com/hls/ep1/key.bin")
        );
        assert_eq!(pl.map_uri.as_deref(), Some("https://cdn.example.com/hls/ep1/init.mp4"));
    }

    #[test]
    fn media_sequence_and_iv_fallbacks() {
        let text = "#EXTM3U\n\
            #EXT-X-MEDIA-SEQUENCE:7\n\
            #EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"\n\
            #EXTINF:2,\nseg0.bin\n\
            #EXTINF:2,\nseg1.bin\n";
        let pl = parse(text, BASE).unwrap();
        assert_eq!(pl.media_sequence, 7);
        let k = pl.segments[0].key.as_ref().unwrap();
        // 没有 IV 时按媒体序号推：第 0 片用 7，第 1 片用 8
        assert_eq!(k.iv_for(pl.media_sequence), Some(crate::aes::sequence_iv(7)));
        assert_eq!(k.iv_for(pl.media_sequence + 1), Some(crate::aes::sequence_iv(8)));
        assert!(k.method_supported());
    }

    #[test]
    fn sample_aes_is_reported_unsupported() {
        let text = "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"k\"\n#EXTINF:2,\ns.bin\n";
        let pl = parse(text, BASE).unwrap();
        let k = pl.segments[0].key.as_ref().unwrap();
        assert!(k.is_encrypted());
        assert!(!k.method_supported());
    }

    #[test]
    fn byterange_segments() {
        let text = "#EXTM3U\n#EXTINF:4,\n#EXT-X-BYTERANGE:1000@0\nmain.mp4\n#EXTINF:4,\n#EXT-X-BYTERANGE:900@1000\nmain.mp4\n";
        let pl = parse(text, BASE).unwrap();
        assert_eq!(pl.segments[0].byte_range, Some((0, 1000)));
        assert_eq!(pl.segments[1].byte_range, Some((1000, 900)));
        assert_eq!(pl.segments[0].uri, "https://cdn.example.com/hls/ep1/main.mp4");
    }

    #[test]
    fn resolve_relative_and_parent() {
        assert_eq!(resolve(BASE, "seg0.m4s"), "https://cdn.example.com/hls/ep1/seg0.m4s");
        assert_eq!(resolve(BASE, "../seg/seg0.m4s"), "https://cdn.example.com/hls/seg/seg0.m4s");
        assert_eq!(resolve(BASE, "/abs/x.m3u8"), "https://cdn.example.com/abs/x.m3u8");
        assert_eq!(resolve(BASE, "//other.com/x.m3u8"), "https://other.com/x.m3u8");
        assert_eq!(resolve(BASE, "http://a.com/b.m3u8"), "http://a.com/b.m3u8");
    }

    #[test]
    fn attrs_with_quoted_commas() {
        let attrs = parse_attrs("BANDWIDTH=1,CODECS=\"a,b\",RESOLUTION=1x2");
        assert_eq!(attr(&attrs, "CODECS"), Some("a,b"));
        assert_eq!(attr(&attrs, "RESOLUTION"), Some("1x2"));
    }

    #[test]
    fn rejects_empty_and_garbage() {
        assert!(parse("", BASE).is_err());
        assert!(parse("#EXTM3U\n# just comments\n", BASE).is_err());
    }
}
