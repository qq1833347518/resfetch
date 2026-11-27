//! resfetch 命令行 —— macOS/Linux 直接可用，Windows 上同一份代码编译出 resfetch.exe。
//! 子命令刻意做成"每一条链路都能单独验证"，方便以后安卓壳逐个对接。

mod http;
mod server;

use resfetch_core::plan;
use std::path::{Path, PathBuf};

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
  resfetch hls <m3u8-url> [--max-height N] 解析播放列表，打印码率与分片清单
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
        "hls" => cmd_hls(&args[1..]),
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
