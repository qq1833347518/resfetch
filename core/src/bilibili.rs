//! B 站 `wbi` 签名（web 接口的防抓参数）。
//!
//! 机制很简单，但很容易抄错，所以整个流程都在这里：
//! 1. `GET /x/web-interface/nav` 拿到 `data.wbi_img.img_url` / `sub_url`；
//! 2. 取两个 URL 文件名的**主干**（去掉扩展名）拼成 64 字符，按固定表置换，取前 32 位作 `mixin_key`；
//! 3. 请求参数加上 `wts`（秒级时间戳），按 key 升序，值做 URL 编码（并删掉 `!'()*` 这五个字符——
//!    JS 的 `encodeURIComponent` 不编码它们，B 站服务端也是按这个规则校验的），
//!    然后 `w_rid = md5(query + mixin_key)`。
//!
//! 签名只要求"算法对"，不涉及账号权限：没有登录 cookie 一样能签，只是拿不到需要登录的内容。

use crate::error::{Error, Result};
use crate::json;
use crate::md5;

/// 官方实现里的混淆表（64 个下标，前 32 位就是 mixin_key）
const MIXIN_KEY_ENC_TAB: [usize; 64] = [
    46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35, 27, 43, 5, 49, 33, 9, 42, 19, 29,
    28, 14, 39, 12, 38, 41, 13, 37, 48, 7, 16, 24, 55, 40, 61, 26, 17, 0, 1, 60, 51, 30, 4, 22, 25,
    54, 21, 56, 59, 6, 63, 57, 62, 11, 36, 20, 34, 44, 52,
];

/// 从 `.../bfs/wbi/7cd084941338484aae1ad9425b84077c.png` 取 `7cd084941338484aae1ad9425b84077c`
fn key_stem(url: &str) -> Option<String> {
    let name = url.split(['?', '#']).next()?.rsplit('/').next()?;
    let stem = match name.rfind('.') {
        Some(p) => &name[..p],
        None => name,
    };
    if stem.is_empty() {
        None
    } else {
        Some(stem.to_string())
    }
}

/// 两个 key 主干 → 64 字符 → 按表置换取前 32 位
pub fn mixin_key(img_key: &str, sub_key: &str) -> String {
    let raw = format!("{}{}", img_key, sub_key);
    let mut out = String::with_capacity(32);
    for &i in MIXIN_KEY_ENC_TAB.iter().take(32) {
        if let Some(c) = raw.chars().nth(i) {
            out.push(c);
        }
    }
    out
}

/// 从 nav 接口的响应 JSON 里取两个 key 主干
pub fn parse_wbi_keys(nav_json: &str) -> Result<(String, String)> {
    let v = json::parse(nav_json)?;
    let img = v
        .get("data")
        .and_then(|d| d.get("wbi_img"))
        .and_then(|w| w.get("img_url"))
        .and_then(|x| x.as_str())
        .ok_or_else(|| Error::Platform("nav 响应里没有 data.wbi_img.img_url".to_string()))?;
    let sub = v
        .get("data")
        .and_then(|d| d.get("wbi_img"))
        .and_then(|w| w.get("sub_url"))
        .and_then(|x| x.as_str())
        .ok_or_else(|| Error::Platform("nav 响应里没有 data.wbi_img.sub_url".to_string()))?;

    let (img_stem, sub_stem) = (
        key_stem(img)
            .ok_or_else(|| Error::Platform(format!("img_url 取不出主干：{}", img)))?,
        key_stem(sub)
            .ok_or_else(|| Error::Platform(format!("sub_url 取不出主干：{}", sub)))?,
    );
    Ok((img_stem, sub_stem))
}

/// 直接给 nav JSON，返回可直接用的 mixin_key
pub fn mixin_key_from_nav(nav_json: &str) -> Result<String> {
    let (img, sub) = parse_wbi_keys(nav_json)?;
    Ok(mixin_key(&img, &sub))
}

/// B 站用的值编码：保留 `A-Za-z0-9-._~`，`!'()*` 不编码但随后删除，其余按 UTF-8 百分比编码
pub fn encode_value(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.as_bytes() {
        let b = *byte;
        let keep = b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'_' | b'.' | b'~')
            || matches!(b, b'!' | b'\'' | b'(' | b')' | b'*');
        if keep {
            if !matches!(b, b'!' | b'\'' | b'(' | b')' | b'*') {
                out.push(char::from(b));
            }
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

/// 生成完整 query（含 `wts` 与 `w_rid`），可直接拼在 `?` 后面
pub fn sign_query(params: &[(&str, String)], mixin: &str, wts: u64) -> String {
    let mut pairs: Vec<(String, String)> = params
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    pairs.push(("wts".to_string(), wts.to_string()));
    pairs.sort_by(|a, b| a.0.cmp(&b.0));

    let query = pairs
        .iter()
        .map(|(k, v)| format!("{}={}", k, encode_value(v)))
        .collect::<Vec<_>>()
        .join("&");

    let rid = md5::hexdigest(format!("{}{}", query, mixin).as_bytes());
    format!("{}&w_rid={}", query, rid)
}

/// 给定已有 query（不含 w_rid），补 `w_rid`
pub fn sign_existing_query(query: &str, mixin: &str) -> String {
    let rid = md5::hexdigest(format!("{}{}", query, mixin).as_bytes());
    if query.is_empty() {
        format!("w_rid={}", rid)
    } else {
        format!("{}&w_rid={}", query, rid)
    }
}

/// `x/web-interface/view` 里我们关心的字段
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoInfo {
    pub bvid: String,
    pub aid: u64,
    pub cid: u64,
    pub title: String,
}

pub fn parse_view(json_text: &str) -> Result<VideoInfo> {
    let v = json::parse(json_text)?;
    check_code(&v)?;
    let data = v
        .get("data")
        .ok_or_else(|| Error::Platform("view 响应里没有 data".to_string()))?;
    let bvid = data
        .get("bvid")
        .and_then(|x| x.as_str())
        .ok_or_else(|| Error::Platform("view 响应里没有 bvid".to_string()))?;
    // 多 P 视频：data.cid 是第一 P，pages[0].cid 也是；优先 pages 更稳
    let cid = data
        .get("pages")
        .and_then(|p| p.idx(0))
        .and_then(|p| p.get("cid"))
        .and_then(|x| x.as_i64())
        .or_else(|| data.get("cid").and_then(|x| x.as_i64()))
        .ok_or_else(|| Error::Platform("view 响应里没有 cid".to_string()))?;
    Ok(VideoInfo {
        bvid: bvid.to_string(),
        aid: data.get("aid").and_then(|x| x.as_i64()).unwrap_or(0).max(0) as u64,
        cid: cid.max(0) as u64,
        title: data
            .get("title")
            .and_then(|x| x.as_str())
            .unwrap_or(bvid)
            .to_string(),
    })
}

/// 一条可用流（dash 的音/视频，或 durl 的整段）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stream {
    pub quality: u64,
    pub bandwidth: u64,
    pub codec: String,
    pub url: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlayUrls {
    pub videos: Vec<Stream>,
    pub audios: Vec<Stream>,
    /// 非 dash（旧接口/带登录）时给的整段地址
    pub durl: Vec<String>,
}

fn stream_from(v: &json::Value) -> Option<Stream> {
    let url = v
        .get("baseUrl")
        .and_then(|x| x.as_str())
        .or_else(|| v.get("base_url").and_then(|x| x.as_str()))
        .or_else(|| v.get("url").and_then(|x| x.as_str()))?;
    Some(Stream {
        quality: v.get("id").and_then(|x| x.as_i64()).unwrap_or(0).max(0) as u64,
        bandwidth: v.get("bandwidth").and_then(|x| x.as_i64()).unwrap_or(0).max(0) as u64,
        codec: v
            .get("codecs")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        url: url.to_string(),
    })
}

pub fn parse_playurl(json_text: &str) -> Result<PlayUrls> {
    let v = json::parse(json_text)?;
    check_code(&v)?;
    let data = v
        .get("data")
        .ok_or_else(|| Error::Platform("playurl 响应里没有 data".to_string()))?;

    let mut out = PlayUrls::default();
    if let Some(dash) = data.get("dash") {
        for v in dash.get("video").and_then(|x| x.as_arr()).unwrap_or(&[]) {
            if let Some(s) = stream_from(v) {
                out.videos.push(s);
            }
        }
        for a in dash.get("audio").and_then(|x| x.as_arr()).unwrap_or(&[]) {
            if let Some(s) = stream_from(a) {
                out.audios.push(s);
            }
        }
    }
    if let Some(durl) = data.get("durl").and_then(|x| x.as_arr()) {
        for seg in durl {
            if let Some(u) = seg.get("url").and_then(|x| x.as_str()) {
                out.durl.push(u.to_string());
            }
        }
    }
    if out.videos.is_empty() && out.durl.is_empty() {
        return Err(Error::Platform(
            "playurl 既没有 dash 流也没有 durl 分段（可能需要登录 cookie）".to_string(),
        ));
    }
    Ok(out)
}

/// 码率最高的那条（同画质下挑带宽大的）
pub fn best(streams: &[Stream]) -> Option<&Stream> {
    streams.iter().max_by_key(|s| (s.quality, s.bandwidth))
}

fn check_code(v: &json::Value) -> Result<()> {
    let code = v.get("code").and_then(|x| x.as_i64()).unwrap_or(0);
    if code == 0 {
        return Ok(());
    }
    let msg = v.get("message").and_then(|x| x.as_str()).unwrap_or("");
    Err(Error::Platform(format!(
        "B 站接口返回 code={}（{}）{}",
        code,
        msg,
        match code {
            -352 => "：wbi 签名被拒，通常是参数缺失或时间戳偏差太大",
            -101 => "：需要登录 cookie",
            -404 => "：资源不存在或已下架",
            _ => "",
        }
    )))
}

/// `x/player/wbi/playurl` 的参数（不含 wts / w_rid，签完再拼）
pub fn playurl_params(bvid: &str, cid: u64, qn: u64, want_dash: bool) -> Vec<(&'static str, String)> {
    vec![
        ("bvid", bvid.to_string()),
        ("cid", cid.to_string()),
        ("qn", qn.to_string()),
        ("fnval", if want_dash { "16".to_string() } else { "1".to_string() }),
        ("fnver", "0".to_string()),
        ("fourk", "1".to_string()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 官方文档给的示例：两个 key 拼起来置换后应得到 ea1db124...
    #[test]
    fn mixin_key_matches_official_example() {
        let mixin = mixin_key(
            "7cd084941338484aae1ad9425b84077c",
            "4932caff0ff746eab6f01bf08b70ac45",
        );
        assert_eq!(mixin, "ea1db124af3c7062474693fa704f4ff8");
        assert_eq!(mixin.len(), 32);
    }

    #[test]
    fn key_stem_strips_path_and_extension() {
        assert_eq!(
            key_stem("https://i0.hdsbi.com/bfs/wbi/7cd084941338484aae1ad9425b84077c.png?x=1")
                .as_deref(),
            Some("7cd084941338484aae1ad9425b84077c")
        );
        assert_eq!(key_stem("abc"), Some("abc".to_string()));
        assert_eq!(key_stem(""), None);
        assert_eq!(key_stem("/x/.png"), None);
    }

    #[test]
    fn parses_keys_from_nav_json() {
        let nav = r#"{"code":0,"data":{"wbi_img":{"img_url":"https://i0.hdsbi.com/bfs/wbi/7cd084941338484aae1ad9425b84077c.png","sub_url":"https://i0.hdsbi.com/bfs/wbi/4932caff0ff746eab6f01bf08b70ac45.png"}}}"#;
        let mixin = mixin_key_from_nav(nav).unwrap();
        assert_eq!(mixin, "ea1db124af3c7062474693fa704f4ff8");
    }

    #[test]
    fn nav_without_wbi_img_is_an_error() {
        let nav = r#"{"code":-101,"message":"账号未登录","data":{"isLogin":false}}"#;
        let err = mixin_key_from_nav(nav).unwrap_err();
        assert!(err.to_string().contains("wbi_img"), "{}", err);
    }

    #[test]
    fn value_encoding_keeps_unreserved_and_drops_five_chars() {
        assert_eq!(encode_value("hello-world_1.0~"), "hello-world_1.0~");
        assert_eq!(encode_value("a&b=c"), "a%26b%3Dc");
        assert_eq!(encode_value("空格 和/斜杠"), "%E7%A9%BA%E6%A0%BC%20%E5%92%8C%2F%E6%96%9C%E6%9D%A0");
        assert_eq!(encode_value("x!(y)*z"), "xyz");
    }

    #[test]
    fn query_is_sorted_and_carries_wts_and_w_rid() {
        let signed = sign_query(
            &[("foo", "114".to_string()), ("bar", "514".to_string()), ("zab", "1919810".to_string())],
            "ea1db124af3c7062474693fa704f4ff8",
            1702204169,
        );
        assert!(signed.starts_with("bar=514&foo=114&wts=1702204169&zab=1919810&w_rid="));
        let rid = signed.rsplit("w_rid=").next().unwrap();
        assert_eq!(rid.len(), 32);
        assert!(rid.chars().all(|c| c.is_ascii_hexdigit()));
        // 同一个输入必须稳定（签名是纯函数，不含随机数）
        let again = sign_query(
            &[("zab", "1919810".to_string()), ("foo", "114".to_string()), ("bar", "514".to_string())],
            "ea1db124af3c7062474693fa704f4ff8",
            1702204169,
        );
        assert_eq!(signed, again, "参数顺序不影响结果");
    }

    #[test]
    fn parse_view_gets_ids_and_title() {
        let view = r#"{"code":0,"data":{"bvid":"BV1GJ411x7h7","aid":12345,"cid":111,"title":"标题","pages":[{"cid":222}]}}"#;
        let info = parse_view(view).unwrap();
        assert_eq!(info.bvid, "BV1GJ411x7h7");
        assert_eq!(info.aid, 12345);
        assert_eq!(info.cid, 222, "多 P 视频优先 pages[0].cid");
        assert_eq!(info.title, "标题");
    }

    #[test]
    fn parse_playurl_reads_dash_and_picks_best() {
        let body = r#"{"code":0,"data":{"dash":{"video":[
            {"id":64,"bandwidth":1000,"codecs":"avc1","baseUrl":"https://cdn/v64.m4s"},
            {"id":80,"bandwidth":2000,"codecs":"hev1","baseUrl":"https://cdn/v80.m4s"}],
            "audio":[{"id":30280,"bandwidth":300,"codecs":"mp4a","baseUrl":"https://cdn/a.m4s"}]}}}"#;
        let p = parse_playurl(body).unwrap();
        assert_eq!(p.videos.len(), 2);
        assert_eq!(best(&p.videos).unwrap().url, "https://cdn/v80.m4s");
        assert_eq!(p.audios.len(), 1);
    }

    #[test]
    fn parse_playurl_falls_back_to_durl() {
        let body = r#"{"code":0,"data":{"durl":[{"url":"https://cdn/x1.mp4"},{"url":"https://cdn/x2.mp4"}]}}"#;
        let p = parse_playurl(body).unwrap();
        assert_eq!(p.durl.len(), 2);
        assert!(p.videos.is_empty());
    }

    #[test]
    fn negative_code_becomes_actionable_error() {
        let body = r#"{"code":-352,"message":"风控校验失败"}"#;
        let err = parse_playurl(body).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("-352"), "{}", text);
        assert!(text.contains("wbi"), "{}", text);
    }

    #[test]
    fn empty_payload_is_an_error_not_an_empty_result() {
        let body = r#"{"code":0,"data":{}}"#;
        assert!(parse_playurl(body).is_err());
    }

    #[test]
    fn sign_existing_query_appends_rid() {
        let q = "a=1&b=2";
        let signed = sign_existing_query(q, "ea1db124af3c7062474693fa704f4ff8");
        assert!(signed.starts_with("a=1&b=2&w_rid="));
        assert_eq!(signed.len(), q.len() + "&w_rid=".len() + 32);
    }
}
