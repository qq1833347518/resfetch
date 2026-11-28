//! resfetch 命令行 —— macOS/Linux 直接可用，Windows 上同一份代码编译出 resfetch.exe。
//! 子命令刻意做成"每一条链路都能单独验证"，方便以后安卓壳逐个对接。

mod http;
mod server;

use resfetch_core::plan;
use std::path::{Path, PathBuf};

/// B 站流的 `baseUrl` 与 `backupUrl` 互为镜像（同一份内容逐字节相同），
/// 所以主地址失败时换备用继续，`.part` 里已下的部分照样能追加，不用从头再下。
fn download_bili_stream(
    urls: &[String],
    dest: &Path,
    cookie: Option<&Path>,
    referer: &str,
) -> Result<u64, String> {
    let mut last = String::from("没有可用地址");
    for (i, u) in urls.iter().enumerate() {
        match http::download(u, dest, 0, cookie, Some(referer)) {
            Ok(r) => {
                if i > 0 {
                    println!("已切到备用地址 #{}", i);
                }
                return Ok(r.bytes);
            }
            Err(e) => {
                last = format!("地址 #{} 失败：{}", i, e);
                if i + 1 < urls.len() {
                    eprintln!("{}，换备用地址", last);
                }
            }
        }
    }
    Err(last)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = dispatch(&args);
    std::process::exit(code);
}

fn usage() -> i32 {
    eprintln!(
        r#"resfetch {} —— 资源下载器内核 + CLI

用法：
  resfetch version
  resfetch probe <url>...                  识别链接（平台/ID/归一化地址）
  resfetch fetch --url <url> [--dir D]     直接下载单个文件（支持断点续传）
  resfetch resolve <url> [--cookie 文件] [--html 本地快照]
                                           解析站内页面，打印真实媒体地址（当前支持抖音）
  resfetch hls <m3u8-url> [--max-height N] 解析播放列表，打印码率与分片清单
  resfetch bili <BV号|视频链接> [--quality N] [--dir D] [--cookie 文件] [--print]
                                           B 站：wbi 签名取播放地址并下载（--print 只打印）
  resfetch cookies merge --store <文件> --incoming <文件|-> [--purge]
                                           保守合并 cookie（不覆盖其它站点）
  resfetch cookies header --store <文件> --host h --path p
                                           生成可用于 curl -b 的 Cookie 头
  resfetch epub --title T --author A --in <文件>... --out <文件.epub>
  resfetch serve [--port 8787] [--dir 下载目录] [--ui ui目录] [--cookie 文件]
                                           启动本地 Web UI（桌面壳/安卓壳共用）
"#,
        resfetch_core::VERSION
    );
    1
}

fn arg_value(args: &[String], key: &str) -> Option<String> {
    args.iter().position(|a| a == key).and_then(|i| args.get(i + 1)).cloned()
}

fn arg_values(args: &[String], key: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == key {
            if let Some(v) = args.get(i + 1) {
                out.push(v.clone());
                i += 1;
            }
        }
        i += 1;
    }
    out
}

fn dispatch(args: &[String]) -> i32 {
    let Some(cmd) = args.first().map(|s| s.as_str()) else {
        return usage();
    };
    match cmd {
        "version" | "-V" | "--version" => {
            println!("resfetch {}（内核 resfetch-core {}）", resfetch_core::VERSION, resfetch_core::VERSION);
            0
        }
        "probe" => cmd_probe(&args[1..]),
        "fetch" => cmd_fetch(&args[1..]),
        "resolve" => cmd_resolve(&args[1..]),
        "hls" => cmd_hls(&args[1..]),
        "bili" | "bilibili" => cmd_bili(&args[1..]),
        "cookies" => cmd_cookies(&args[1..]),
        "epub" => cmd_epub(&args[1..]),
        "serve" => cmd_serve(&args[1..]),
        _ => usage(),
    }
}

fn cmd_probe(args: &[String]) -> i32 {
    let mut targets: Vec<String> = args.iter().filter(|a| !a.starts_with("--")).cloned().collect();
    if targets.is_empty() {
        return usage();
    }
    if targets.len() == 1 {
        let joined = targets.join(" ");
        let parsed = resfetch_core::link::parse_many(&joined);
        if parsed.len() > 1 {
            targets = parsed.iter().map(|p| p.canonical.clone()).collect();
        }
    }

    let mut count = 0;
    for t in targets {
        match resfetch_core::link::parse(&t) {
            Ok(p) => {
                count += 1;
                println!(
                    "{{\"platform\":\"{}\",\"label\":\"{}\",\"kind\":\"{}\",\"id\":{},\"canonical\":\"{}\"}}",
                    p.platform.as_str(),
                    p.platform.label(),
                    p.kind.as_str(),
                    match &p.id {
                        Some(id) => format!("\"{}\"", plan::json_escape(id)),
                        None => "null".to_string(),
                    },
                    plan::json_escape(&p.canonical)
                );
            }
            Err(e) => eprintln!("跳过：{} —— {}", t, e),
        }
    }
    if count == 0 { 2 } else { 0 }
}

/// 解析站内页面，把真实媒体地址打印出来。
/// 两个用途：① 有真实 cookie 时直接给出可下载直链；② 抓不到时把
/// 抖音给的页面（风控页/壳页）原样落到 `--html`，便于离线复现解析问题。
fn cmd_resolve(args: &[String]) -> i32 {
    let Some(url) = arg_value(args, "--url").or_else(|| {
        args.iter().find(|a| a.starts_with("http")).cloned()
    }) else {
        return usage();
    };
    let cookie = arg_value(args, "--cookie").map(PathBuf::from);
    let snapshot = arg_value(args, "--html").map(PathBuf::from);

    let parsed = match resfetch_core::link::parse(&url) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("链接无法识别：{}", e);
            return 2;
        }
    };
    if parsed.platform != resfetch_core::link::Platform::Douyin {
        eprintln!("resolve 目前只接了抖音（{} 还没做，见 docs/ROADMAP.md v0.2）", parsed.platform.as_str());
        return 2;
    }

    let html = match &snapshot {
        Some(p) => match std::fs::read_to_string(p) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("读不了本地快照 {}：{}", p.display(), e);
                return 2;
            }
        },
        None => match http::get_text(&parsed.canonical, cookie.as_deref(), Some("https://www.douyin.com/")) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("抓页面失败：{}", e);
                return 2;
            }
        },
    };

    match resfetch_core::douyin::parse_page(&html) {
        Ok(r) => {
            println!("标题：{}", if r.title.is_empty() { "（无标题）" } else { r.title.as_str() });
            if let Some(a) = &r.author {
                println!("作者：{}", a);
            }
            if !r.id.is_empty() {
                println!("作品：{}", r.id);
            }
            println!("类型：{}", r.media.kind());
            match &r.media {
                resfetch_core::douyin::Media::Video { url, duration_ms, size_bytes } => {
                    if let Some(d) = duration_ms {
                        println!("时长：{:.1}s", *d as f64 / 1000.0);
                    }
                    if let Some(s) = size_bytes {
                        println!("大小：{}", http::human(*s as u64));
                    }
                    println!("直链：{}", url);
                    println!(
                        "{{\"kind\":\"video\",\"id\":\"{}\",\"title\":\"{}\",\"url\":\"{}\"}}",
                        plan::json_escape(&r.id),
                        plan::json_escape(&r.title),
                        plan::json_escape(url)
                    );
                }
                resfetch_core::douyin::Media::Images(urls) => {
                    println!("图片：{} 张", urls.len());
                    for (i, u) in urls.iter().enumerate() {
                        println!("  {}. {}", i + 1, u);
                    }
                }
            }
            0
        }
        Err(e) => {
            eprintln!("解析失败：{}", e);
            eprintln!("提示：抖音对无登录态的裸请求普遍返回验证页；用 --cookie 指定 cookies.txt，");
            eprintln!("      或先用浏览器打开该视频页、把 HTML 存下来，再用 --html 离线解析。");
            3
        }
    }
}

fn cmd_fetch(args: &[String]) -> i32 {
    let Some(url) = arg_value(args, "--url").or_else(|| args.iter().find(|a| a.starts_with("http")).cloned()) else {
        return usage();
    };
    let dir = PathBuf::from(arg_value(args, "--dir").unwrap_or_else(|| ".".to_string()));
    let cookie = arg_value(args, "--cookie").map(PathBuf::from);

    let parsed = match resfetch_core::link::parse(&url) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("链接无法识别：{}", e);
            return 2;
        }
    };
    let filename = arg_value(args, "--name").unwrap_or_else(|| {
        resfetch_core::naming::from_url_and_title(&parsed.canonical, None, parsed.kind.default_ext())
    });
    let dest = dir.join(&filename);

    let total = match http::head(&parsed.canonical, cookie.as_deref(), None) {
        Ok(h) => {
            println!(
                "目标 {} · HTTP {} · {} · {}",
                dest.display(),
                h.status,
                http::human(h.content_length),
                h.content_type
            );
            h.content_length
        }
        Err(e) => {
            eprintln!("探测失败（继续试着下）：{}", e);
            0
        }
    };

    match http::download(&parsed.canonical, &dest, total, cookie.as_deref(), None) {
        Ok(r) => {
            println!("完成：{}（{} 字节，用时 {:.1}s）", dest.display(), r.bytes, r.elapsed.as_secs_f64());
            0
        }
        Err(e) => {
            eprintln!("失败：{}", e);
            3
        }
    }
}

fn cmd_hls(args: &[String]) -> i32 {
    let Some(url) = args.iter().find(|a| a.starts_with("http")).cloned() else {
        return usage();
    };
    let max_h: Option<u32> = arg_value(args, "--max-height").and_then(|v| v.parse().ok());
    let cookie = arg_value(args, "--cookie").map(PathBuf::from);

    let mut text = match http::get_text(&url, cookie.as_deref(), None) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("拉取播放列表失败：{}", e);
            return 3;
        }
    };
    let mut pl = match resfetch_core::hls::parse(&text, &url) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("解析失败：{}", e);
            return 2;
        }
    };

    if pl.is_master {
        let picked = pl.pick_variant(max_h).map(|v| (v.bandwidth, v.resolution, v.uri.clone()));
        println!("主播放列表：{} 个码率", pl.variants.len());
        for v in &pl.variants {
            println!(
                "  {:>8} kbps  {}  {}",
                v.bandwidth / 1000,
                match v.resolution {
                    Some((w, h)) => format!("{}x{}", w, h),
                    None => "未知分辨率".to_string(),
                },
                v.uri
            );
        }
        let Some((bw, res, uri)) = picked else {
            eprintln!("没有可用码率");
            return 2;
        };
        println!(
            "选中：{} kbps {}（{}）",
            bw / 1000,
            res.map(|(w, h)| format!("{}x{}", w, h)).unwrap_or_else(|| "-".into()),
            uri
        );
        text = match http::get_text(&uri, cookie.as_deref(), None) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("拉取子播放列表失败：{}", e);
                return 3;
            }
        };
        pl = match resfetch_core::hls::parse(&text, &uri) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("解析子播放列表失败：{}", e);
                return 2;
            }
        };
    }

    println!(
        "分片 {} 个 · 总时长 {:.1}s · 目标片长 {}s · 加密 {} · 结束标记 {}",
        pl.segment_count(),
        pl.total_duration,
        pl.target_duration,
        if pl.encrypted() { "是" } else { "否" },
        pl.endlist
    );
    0
}

fn cmd_cookies(args: &[String]) -> i32 {
    let Some(sub) = args.first().map(|s| s.as_str()) else {
        return usage();
    };
    match sub {
        "merge" => {
            let Some(store) = arg_value(args, "--store") else {
                return usage();
            };
            let Some(incoming) = arg_value(args, "--incoming") else {
                return usage();
            };
            let now = server::now();

            let existing = if Path::new(&store).exists() {
                match std::fs::read_to_string(&store) {
                    Ok(t) => resfetch_core::cookie::parse_any(&t, "").unwrap_or_default(),
                    Err(e) => {
                        eprintln!("读不到现有 cookie 库：{}", e);
                        return 3;
                    }
                }
            } else {
                Vec::new()
            };

            let incoming_text = if incoming == "-" {
                let mut s = String::new();
                use std::io::Read;
                if std::io::stdin().read_to_string(&mut s).is_err() {
                    eprintln!("读标准输入失败");
                    return 3;
                }
                s
            } else {
                match std::fs::read_to_string(&incoming) {
                    Ok(t) => t,
                    Err(e) => {
                        eprintln!("读不到待导入文件：{}", e);
                        return 3;
                    }
                }
            };
            let incoming_cookies = match resfetch_core::cookie::parse_any(&incoming_text, "") {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("解析失败：{}", e);
                    return 2;
                }
            };

            let purge = args.iter().any(|a| a == "--purge");
            let (merged, report) = resfetch_core::cookie::merge(&existing, &incoming_cookies, now, purge);
            if let Some(parent) = Path::new(&store).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(e) = std::fs::write(&store, resfetch_core::cookie::to_netscape(&merged)) {
                eprintln!("写回失败：{}", e);
                return 3;
            }
            println!(
                "{{\"added\":{},\"updated\":{},\"kept\":{},\"expired_dropped\":{},\"total\":{}}}",
                report.added, report.updated, report.kept, report.expired_dropped, report.total
            );
            0
        }
        "header" => {
            let Some(store) = arg_value(args, "--store") else {
                return usage();
            };
            let host = arg_value(args, "--host").unwrap_or_default();
            let path = arg_value(args, "--path").unwrap_or_else(|| "/".to_string());
            let text = std::fs::read_to_string(&store).unwrap_or_default();
            let cookies = resfetch_core::cookie::parse_any(&text, &host).unwrap_or_default();
            println!(
                "{}",
                resfetch_core::cookie::header_value(&cookies, &host, &path, server::now())
            );
            0
        }
        _ => usage(),
    }
}

fn cmd_epub(args: &[String]) -> i32 {
    let title = arg_value(args, "--title").unwrap_or_else(|| "未命名".to_string());
    let author = arg_value(args, "--author").unwrap_or_else(|| "resfetch".to_string());
    let Some(out) = arg_value(args, "--out") else {
        return usage();
    };
    let inputs = arg_values(args, "--in");
    if inputs.is_empty() {
        return usage();
    }

    let mut chapters: Vec<resfetch_core::epub::Chapter> = Vec::new();
    for f in &inputs {
        let text = match std::fs::read_to_string(f) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("读不到 {}：{}", f, e);
                return 3;
            }
        };
        // 按 Markdown 标题切章；没有标题就整篇一章
        let mut cur_title: Option<String> = None;
        let mut cur_body = String::new();
        for line in text.lines() {
            if let Some(h) = line.strip_prefix("## ").or_else(|| line.strip_prefix("# ")) {
                if let Some(t) = cur_title.take() {
                    chapters.push(resfetch_core::epub::Chapter {
                        title: t,
                        html: resfetch_core::epub::text_to_html(&cur_body),
                    });
                    cur_body.clear();
                }
                cur_title = Some(h.trim().to_string());
            } else {
                cur_body.push_str(line);
                cur_body.push('\n');
            }
        }
        let fallback = Path::new(f)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "正文".into());
        if cur_title.is_none() && cur_body.trim().is_empty() {
            continue;
        }
        chapters.push(resfetch_core::epub::Chapter {
            title: cur_title.unwrap_or(fallback),
            html: resfetch_core::epub::text_to_html(&cur_body),
        });
    }

    match resfetch_core::epub::build_epub(&title, &author, "zh-CN", &format!("resfetch-{}", title), &chapters) {
        Ok(bytes) => match std::fs::write(&out, &bytes) {
            Ok(_) => {
                println!("已生成 {}（{} 章，{} 字节）", out, chapters.len(), bytes.len());
                0
            }
            Err(e) => {
                eprintln!("写文件失败：{}", e);
                3
            }
        },
        Err(e) => {
            eprintln!("打包失败：{}", e);
            2
        }
    }
}

fn cmd_serve(args: &[String]) -> i32 {
    let port: u16 = arg_value(args, "--port").and_then(|v| v.parse().ok()).unwrap_or(8787);
    let dir = PathBuf::from(arg_value(args, "--dir").unwrap_or_else(|| ".".to_string()));
    let ui = PathBuf::from(arg_value(args, "--ui").unwrap_or_else(|| "ui".to_string()));
    let cookie = arg_value(args, "--cookie").map(PathBuf::from);

    if !ui.exists() {
        eprintln!("UI 目录不存在：{}（请用 --ui 指向仓库里的 ui/）", ui.display());
        return 2;
    }
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("下载目录不可用：{}", e);
        return 3;
    }

    let state = server::AppState {
        dir,
        ui_dir: ui,
        tasks: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        cookie_file: cookie,
    };

    match server::run(port, state) {
        Ok(_) => 0,
        Err(e) => {
            eprintln!("{}", e);
            3
        }
    }
}

// ---------------------------------------------------------------------------
// B 站：wbi 签名 → playurl → 下载
// ---------------------------------------------------------------------------

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 从 "BV1xx..." 或 "https://www.bilibili.com/video/BV1xx..." 里取出 12 位 BV 号
fn extract_bvid(input: &str) -> Option<String> {
    if input.len() == 12 && input.starts_with("BV") {
        return Some(input.to_string());
    }
    let pos = input.find("BV")?;
    let bvid: String = input[pos..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect();
    if bvid.len() == 12 {
        Some(bvid)
    } else {
        None
    }
}

fn cmd_bili(args: &[String]) -> i32 {
    let Some(target) = args.iter().find(|a| !a.starts_with("--")).cloned() else {
        return usage();
    };
    let dir = PathBuf::from(arg_value(args, "--dir").unwrap_or_else(|| ".".to_string()));
    let cookie = arg_value(args, "--cookie").map(PathBuf::from);
    let qn: u64 = arg_value(args, "--quality").and_then(|v| v.parse().ok()).unwrap_or(80);
    let print_only = args.iter().any(|a| a == "--print");
    let referer = "https://www.bilibili.com/";

    // b23.tv 短链先跟随重定向拿真实地址
    let mut input = target.clone();
    if input.contains("b23.tv") {
        match http::head(&input, cookie.as_deref(), Some(referer)) {
            Ok(h) if !h.effective_url.is_empty() => input = h.effective_url,
            _ => {
                eprintln!("短链解析失败，请直接贴 https://www.bilibili.com/video/BV... 形式的地址");
                return 3;
            }
        }
    }
    let Some(bvid) = extract_bvid(&input) else {
        eprintln!("认不出 BV 号：{}", input);
        return 2;
    };

    // 1) 视频信息（标题 + cid）
    let view_url = format!("https://api.bilibili.com/x/web-interface/view?bvid={}", bvid);
    let info = match http::get_text(&view_url, cookie.as_deref(), Some(referer))
        .map_err(|e| e.to_string())
        .and_then(|t| resfetch_core::bilibili::parse_view(&t).map_err(|e| e.to_string()))
    {
        Ok(i) => i,
        Err(e) => {
            eprintln!("取视频信息失败：{}", e);
            return 3;
        }
    };
    println!("{}  {}（cid {}）", info.bvid, info.title, info.cid);

    // 2) nav 取两个 key → mixin_key
    let nav = match http::get_text("https://api.bilibili.com/x/web-interface/nav", cookie.as_deref(), Some(referer)) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("取 wbi key 失败：{}", e);
            return 3;
        }
    };
    let mixin = match resfetch_core::bilibili::mixin_key_from_nav(&nav) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("解析 wbi key 失败：{}", e);
            return 3;
        }
    };

    // 3) 签名 playurl（dash 优先，拿得到分离的音轨）
    let params = resfetch_core::bilibili::playurl_params(&info.bvid, info.cid, qn, true);
    let query = resfetch_core::bilibili::sign_query(&params, &mixin, now_secs());
    let play_url = format!("https://api.bilibili.com/x/player/wbi/playurl?{}", query);
    let body = match http::get_text(&play_url, cookie.as_deref(), Some(referer)) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("请求 playurl 失败：{}", e);
            return 3;
        }
    };
    let urls = match resfetch_core::bilibili::parse_playurl(&body) {
        Ok(u) => u,
        Err(e) => {
            eprintln!("解析 playurl 失败：{}", e);
            return 3;
        }
    };

    for v in &urls.videos {
        println!("  视频 id={} bandwidth={} codec={}", v.quality, v.bandwidth, v.codec);
    }
    for a in &urls.audios {
        println!("  音频 id={} bandwidth={} codec={}", a.quality, a.bandwidth, a.codec);
    }
    for (i, u) in urls.durl.iter().enumerate() {
        println!("  分段 {}: {}", i + 1, u);
    }

    let base_name = resfetch_core::naming::from_url_and_title(
        &format!("https://www.bilibili.com/video/{}", info.bvid),
        Some(&info.title),
        Some("mp4"),
    );

    if print_only {
        if let Some(v) = resfetch_core::bilibili::best(&urls.videos) {
            println!("最优视频：{}", v.url);
            for b in &v.backup {
                println!("  备用：{}", b);
            }
        }
        if let Some(a) = resfetch_core::bilibili::best(&urls.audios) {
            println!("最优音频：{}", a.url);
            for b in &a.backup {
                println!("  备用：{}", b);
            }
        }
        for (i, u) in urls.durl.iter().enumerate() {
            println!("durl[{}]：{}", i, u);
        }
        return 0;
    }

    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("下载目录不可用：{}", e);
        return 3;
    }

    // 4) 下载：durl 直接落成文件；dash 音视频分开下，有 ffmpeg 就合并
    if !urls.durl.is_empty() {
        let mut all = Vec::new();
        for (i, u) in urls.durl.iter().enumerate() {
            let part = dir.join(format!("{}.part{}.mp4", base_name, i + 1));
            match http::download(u, &part, 0, cookie.as_deref(), Some(referer)) {
                Ok(r) => {
                    println!("分段 {} 完成：{}", i + 1, http::human(r.bytes));
                    all.push(part);
                }
                Err(e) => {
                    eprintln!("分段 {} 下载失败：{}", i + 1, e);
                    return 3;
                }
            }
        }
        let out = dir.join(&base_name);
        let mut f = match std::fs::File::create(&out) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("创建输出文件失败：{}", e);
                return 3;
            }
        };
        for p in &all {
            if let Ok(mut src) = std::fs::File::open(p) {
                let _ = std::io::copy(&mut src, &mut f);
            }
            let _ = std::fs::remove_file(p);
        }
        println!("已保存：{}", out.display());
        return 0;
    }

    let vstream = resfetch_core::bilibili::best(&urls.videos).map(|s| s.urls());
    let astream = resfetch_core::bilibili::best(&urls.audios).map(|s| s.urls());
    let Some(vurls) = vstream else {
        eprintln!("没有可用视频流");
        return 3;
    };

    let stem = base_name.trim_end_matches(".mp4").to_string();
    let vpath = dir.join(format!("{}.video.m4s", stem));
    let apath = dir.join(format!("{}.audio.m4s", stem));
    match download_bili_stream(&vurls, &vpath, cookie.as_deref(), referer) {
        Ok(r) => println!("视频流完成：{}", http::human(r)),
        Err(e) => {
            eprintln!("视频流下载失败：{}", e);
            return 3;
        }
    }
    if let Some(aurls) = astream {
        match download_bili_stream(&aurls, &apath, cookie.as_deref(), referer) {
            Ok(r) => println!("音频流完成：{}", http::human(r)),
            Err(e) => {
                eprintln!("音频流下载失败：{}", e);
                return 3;
            }
        }
    }

    // 有 ffmpeg 就合并成 mp4，没有就把两段留着并说清楚
    let out = dir.join(&base_name);
    let merged = apath.exists()
        && std::process::Command::new("ffmpeg")
            .arg("-y")
            .arg("-i")
            .arg(&vpath)
            .arg("-i")
            .arg(&apath)
            .arg("-c")
            .arg("copy")
            .arg(&out)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

    if merged {
        let _ = std::fs::remove_file(&vpath);
        let _ = std::fs::remove_file(&apath);
        println!("已合并并保存：{}", out.display());
    } else {
        println!(
            "音视频已分离保存（本机没有 ffmpeg 或合并失败）：\n  {}\n  {}",
            vpath.display(),
            apath.display()
        );
        println!("装好 ffmpeg 后可用：ffmpeg -i <video> -i <audio> -c copy out.mp4");
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_bvid_accepts_bare_and_url_forms() {
        assert_eq!(extract_bvid("BV1GJ411x7h7").as_deref(), Some("BV1GJ411x7h7"));
        assert_eq!(
            extract_bvid("https://www.bilibili.com/video/BV1GJ411x7h7/?p=1").as_deref(),
            Some("BV1GJ411x7h7")
        );
        assert_eq!(extract_bvid("https://example.com/abc"), None);
    }
}
