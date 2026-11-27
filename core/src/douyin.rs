//! 抖音页面解析。
//!
//! 抖音不存在"拿到链接就能拼出下载地址"的公开规律，页面本身会内嵌一份 JSON。
//! 这里的职责很窄：**给定一份 HTML，抠出里面的媒体信息**。
//!
//! - 不联网、不带 cookie、不算签名 —— 抓页面是外壳的事（`cli/src/http.rs`）。
//! - 解析不出来时给出**能指导下一步**的错误：风控页和"页面改版"是两种完全不同的
//!   处置方式，笼统报"失败"会让人白折腾。
//!
//! 已知的内嵌形态（按优先级尝试）：
//! 1. `window._ROUTER_DATA = {...}`（分享页 / web 页主流形态）
//! 2. `<script id="RENDER_DATA" type="application/json">%7B...`（老版页面，URL 编码）
//! 3. `<script id="__NEXT_DATA__">`（改版兜底）

use crate::error::{Error, Result};
use crate::json::{self, Value};

#[derive(Debug, Clone, PartialEq)]
pub enum Media {
    /// 直链视频。`url` 已尽量取到无水印地址（`playwm` → `play`）
    Video {
        url: String,
        duration_ms: Option<i64>,
        size_bytes: Option<i64>,
    },
    /// 图文（图集）
    Images(Vec<String>),
}

impl Media {
    pub fn kind(&self) -> &'static str {
        match self {
            Media::Video { .. } => "video",
            Media::Images(_) => "images",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub id: String,
    pub title: String,
    pub author: Option<String>,
    pub media: Media,
}

/// 页面里出现了这些标记，说明抖音没给数据，给的是风控/验证页。
/// 注意别把 `captcha` 当标记：SSR 骨架里几乎每个页面都带验证码配置，
/// 那只是"需要时能弹"，不代表当下就是验证页。
const CHALLENGE_MARKERS: [&str; 2] = ["_$jsvmprt", "verify-center"];

pub fn looks_like_challenge(html: &str) -> bool {
    CHALLENGE_MARKERS.iter().any(|m| html.contains(m))
}

pub fn parse_page(html: &str) -> Result<Resolved> {
    // 2026 年起抖音分享页的 SSR 数据里已经没有 videoInfoRes（播放地址改由前端请求 + 签名获取），
    // 所以先按内嵌 JSON 走，失败再看这是不是一份"浏览器渲染后导出"的 DOM 快照。
    if let Some(r) = parse_rendered_dom(html) {
        return Ok(r);
    }

    let data = embedded_json(html).ok_or_else(|| {
        if looks_like_challenge(html) {
            Error::Platform(
                "抖音返回的是风控验证页而不是视频页；裸请求在多数线路都会被拦，\
                 需要带有效的登录 cookie（cookies.txt / Cookie 头）重试"
                    .to_string(),
            )
        } else {
            Error::Platform(
                "页面里没有内嵌媒体数据：抖音已改成前端请求 + 签名取地址。两条不绕签名可行的路：\
                 ① 在已登录的浏览器/壳里打开该视频，导出渲染后的 DOM 再用 --html 解析；\
                 ② 直接从播放器里复制直链，用 fetch 下载"
                    .to_string(),
            )
        }
    })?;

    let info = data
        .find_key("videoInfoRes")
        .or_else(|| data.find_key("aweme_detail"))
        .ok_or_else(|| {
            Error::Platform(
                "页面里没有 videoInfoRes / aweme_detail：抖音已改成前端请求 + 签名取地址，\
                 静态抓到的只是骨架。两条不绕签名的路：① 在已登录的浏览器/壳里打开该视频，\
                 导出渲染后的 DOM 再解析；② 从播放器里复制直链，用 fetch 直接下"
                    .to_string(),
            )
        })?;

    let item = info
        .get("item_list")
        .and_then(|l| l.idx(0))
        .or_else(|| info.get("aweme_detail"))
        .unwrap_or(info);

    let id = item.get("aweme_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let title = item.get("desc").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let author = item
        .get("author")
        .and_then(|a| a.get("nickname"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // 图集优先：带 images 且没有可用视频的是图文作品
    if let Some(images) = item.get("images").and_then(|v| v.as_arr()) {
        let urls: Vec<String> = images
            .iter()
            .filter_map(|img| best_url(img.get("url_list")))
            .collect();
        if !urls.is_empty() {
            return Ok(Resolved { id, title, author, media: Media::Images(urls) });
        }
    }

    let video = item.get("video").ok_or_else(|| {
        Error::Platform("作品数据里没有 video 字段（可能是图文或作品已删除）".to_string())
    })?;

    let url = ["play_addr", "play_addr_h264", "download_addr"]
        .iter()
        .filter_map(|k| video.get(k).and_then(|p| p.get("url_list")))
        .find_map(|list| best_url(Some(list)))
        .ok_or_else(|| Error::Platform("没有可用的播放地址（play_addr 为空）".to_string()))?;

    let duration_ms = video.get("duration").and_then(|v| v.as_i64());
    let size_bytes = video
        .get("play_addr")
        .and_then(|p| p.get("data_size"))
        .and_then(|v| v.as_i64());

    Ok(Resolved { id, title, author, media: Media::Video { url, duration_ms, size_bytes } })
}

/// 从"渲染后的 DOM 快照"里取播放地址。
///
/// 抖音网页版把真实地址交给 `<video src>` 或 MSE，这里只认前一种（直链 mp4），
/// 因为那正是可被 curl 直接拉取、且**不需要伪造签名**的东西。
/// 顺带把标题（`<title>`）和作品 ID（SSR 里的 lastPath/itemId）捡回来。
pub fn parse_rendered_dom(html: &str) -> Option<Resolved> {
    let url = first_video_src(html)?;
    let id = rendered_id(html).unwrap_or_default();
    let title = tag_text(html, "<title>").map(|t| t.trim().to_string()).unwrap_or_default();
    Some(Resolved {
        id,
        title,
        author: None,
        media: Media::Video { url, duration_ms: None, size_bytes: None },
    })
}

/// 扫描所有 `<video ...>`，取第一个非 blob: 的 src
fn first_video_src(html: &str) -> Option<String> {
    let mut from = 0;
    while let Some(at) = html[from..].find("<video") {
        let start = from + at;
        let tag_end = html[start..].find('>').map(|e| start + e)?;
        let tag = &html[start..=tag_end];
        if let Some(v) = attr_value(tag, "src") {
            if v.starts_with("http") && !v.contains(".m3u8") {
                return Some(v);
            }
        }
        from = tag_end + 1;
    }
    None
}

fn attr_value(tag: &str, name: &str) -> Option<String> {
    let pat = format!("{}=\"", name);
    let at = tag.find(&pat)?;
    let rest = &tag[at + pat.len()..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn tag_text(html: &str, open: &str) -> Option<String> {
    let at = html.find(open)?;
    let rest = &html[at + open.len()..];
    let end = rest.find(if open.starts_with("<title") { "</title>" } else { "</" })?;
    Some(rest[..end].to_string())
}

/// 从 SSR 骨架里取作品 ID：`"itemId":"7300..."` 或 `"lastPath":"7300..."`
fn rendered_id(html: &str) -> Option<String> {
    for key in ["\"itemId\":\"", "\"lastPath\":\""] {
        if let Some(at) = html.find(key) {
            let rest = &html[at + key.len()..];
            let end = rest.find('"')?;
            let id: String = rest[..end].chars().filter(|c| c.is_ascii_digit()).collect();
            if !id.is_empty() {
                return Some(id);
            }
        }
    }
    None
}

/// 从 `url_list` 里挑无水印直链。
///
/// 抖音的习惯是列表里既有 `.../playwm/...`（带水印）也有 `.../play/...`（无水印），
/// 但顺序不保证。策略：优先取已经不含 `playwm` 的；全是 `playwm` 时把路径段换掉
/// ——这是公开的、非对抗性的地址改写，不需要签名。
fn best_url(list: Option<&Value>) -> Option<String> {
    let arr = list?.as_arr()?;
    let strs: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
    if strs.is_empty() {
        return None;
    }
    if let Some(clean) = strs.iter().find(|u| !u.contains("playwm")) {
        return Some((*clean).to_string());
    }
    Some(strs[0].replace("/playwm/", "/play/"))
}

/// 依次尝试三种内嵌形态，返回解析后的 JSON
fn embedded_json(html: &str) -> Option<Value> {
    if let Some(raw) = json::extract_object(html, "_ROUTER_DATA") {
        if let Ok(v) = json::parse(&raw) {
            return Some(v);
        }
    }
    for id in ["RENDER_DATA", "__NEXT_DATA__"] {
        if let Some(raw) = script_payload(html, id) {
            let decoded = json::percent_decode(&raw);
            for candidate in [decoded.as_str(), raw.trim()] {
                if let Ok(v) = json::parse(candidate) {
                    return Some(v);
                }
            }
        }
    }
    None
}

/// 取 `<script id="XXX" ...>内联内容</script>`
fn script_payload(html: &str, id: &str) -> Option<String> {
    let key = format!("id=\"{}\"", id);
    let at = html.find(&key)?;
    let open_end = html[at..].find('>')? + at;
    let close = html[open_end..].find("</script>")? + open_end;
    Some(html[open_end + 1..close].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_with(items: &str) -> String {
        format!(
            r#"<html><body><script>window._ROUTER_DATA = {{"loaderData":{{"video_(id)/page":{{"videoInfoRes":{{"item_list":[{items}]}}}}}},"errors":null}};</script></body></html>"#
        )
    }

    #[test]
    fn parses_video_and_prefers_nowatermark() {
        let html = fixture_with(
            r#"{"aweme_id":"7300000000000000001","desc":"测试标题 #话题","author":{"nickname":"某个作者"},
               "video":{"duration":15000,"play_addr":{"data_size":2048000,"url_list":[
                 "https://aweme.snssdk.com/aweme/v1/playwm/?video_id=v01",
                 "https://aweme.snssdk.com/aweme/v1/play/?video_id=v01"]}}}"#,
        );
        let r = parse_page(&html).unwrap();
        assert_eq!(r.id, "7300000000000000001");
        assert_eq!(r.title, "测试标题 #话题");
        assert_eq!(r.author.as_deref(), Some("某个作者"));
        match r.media {
            Media::Video { url, duration_ms, size_bytes } => {
                assert!(url.contains("/play/"), "应取无水印直链：{url}");
                assert_eq!(duration_ms, Some(15000));
                assert_eq!(size_bytes, Some(2048000));
            }
            other => panic!("期望视频，得到 {:?}", other),
        }
    }

    #[test]
    fn rewrites_playwm_when_it_is_the_only_choice() {
        let html = fixture_with(
            r#"{"aweme_id":"1","desc":"d","video":{"play_addr":{"url_list":[
               "https://aweme.snssdk.com/aweme/v1/playwm/?video_id=v01"]}}}"#,
        );
        match parse_page(&html).unwrap().media {
            Media::Video { url, .. } => assert!(url.contains("/play/"), "应改写成无水印：{url}"),
            other => panic!("期望视频，得到 {:?}", other),
        }
    }

    #[test]
    fn parses_image_post() {
        let html = fixture_with(
            r#"{"aweme_id":"2","desc":"图文","images":[
                 {"url_list":["https://p3.douyinpic.com/a.jpeg"]},
                 {"url_list":["https://p3.douyinpic.com/b.jpeg"]}]}"#,
        );
        match parse_page(&html).unwrap().media {
            Media::Images(urls) => assert_eq!(urls.len(), 2),
            other => panic!("期望图集，得到 {:?}", other),
        }
    }

    #[test]
    fn parses_render_data_variant() {
        let payload = r#"{"loaderData":{"video_(id)/page":{"videoInfoRes":{"item_list":[{"aweme_id":"9","desc":"old-style","video":{"play_addr":{"url_list":["https://x/play/?video_id=v"]}}}]}}}}"#;
        let encoded = percent_encode(payload);
        let html = format!(
            r#"<html><script id="RENDER_DATA" type="application/json">{encoded}</script></html>"#
        );
        let r = parse_page(&html).unwrap();
        assert_eq!(r.title, "old-style");
        assert_eq!(r.id, "9");
    }

    #[test]
    fn challenge_page_gives_actionable_error() {
        let html = r#"<html><script>var glb;(glb="undefined"==typeof window?global:window)._$jsvmprt={};(function(){})()</script></html>"#;
        let msg = parse_page(html).unwrap_err().to_string();
        assert!(msg.contains("风控"), "错误应说明是风控页：{msg}");
        assert!(msg.contains("cookie"), "错误应指引带 cookie：{msg}");
    }

    #[test]
    fn empty_shell_page_gives_different_error() {
        let msg = parse_page("<html><body></body></html>").unwrap_err().to_string();
        assert!(msg.contains("内嵌媒体数据"), "{}", msg);
    }

    #[test]
    fn missing_play_addr_is_explicit() {
        let msg = parse_page(&fixture_with(r#"{"aweme_id":"3","desc":"x","video":{}}"#))
            .unwrap_err()
            .to_string();
        assert!(msg.contains("play_addr"), "{}", msg);
    }

    #[test]
    fn parses_rendered_dom_video_src() {
        // 一份"浏览器渲染后导出"的 DOM：SSR 里没有播放数据，只有 <video src>
        let html = r#"<html><head><title>某个视频 - 抖音</title></head><body>
            <div id="douyin-header"></div>
            <video class="xgplayer-video" src="https://v3-web.douyinvod.com/play/?video_id=v0300&ratio=1080p" preload="auto"></video>
            <script>window._ROUTER_DATA = {"loaderData":{"video_(id)\u002Fpage":{"itemId":"7300000000000000009","lastPath":"7300000000000000009"}}}</script>
        </body></html>"#;
        let r = parse_page(html).unwrap();
        assert_eq!(r.id, "7300000000000000009");
        assert_eq!(r.title, "某个视频 - 抖音");
        match r.media {
            Media::Video { url, .. } => assert!(url.contains("douyinvod.com"), "{url}"),
            other => panic!("期望视频，得到 {:?}", other),
        }
    }

    #[test]
    fn ignores_blob_and_m3u8_in_video_tag() {
        let html = r#"<html><video src="blob:https://www.douyin.com/abc"></video>
            <video src="https://x/y.m3u8"></video></html>"#;
        assert!(parse_rendered_dom(html).is_none());
    }

    #[test]
    fn challenge_detection_ignores_plain_captcha_config() {
        // 普通 SSR 骨架里带验证码配置，不该被误判成验证页
        assert!(!looks_like_challenge(r#"{"captcha":{"enable":true}}"#));
        assert!(looks_like_challenge(r#"<div class="verify-center"></div>"#));
    }

    /// 测试用百分号编码：按字节编码，非 ASCII 也不会编错
    fn percent_encode(s: &str) -> String {
        let mut out = String::new();
        for b in s.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(b as char)
                }
                _ => out.push_str(&format!("%{:02X}", b)),
            }
        }
        out
    }
}
