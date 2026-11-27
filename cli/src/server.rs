//! 本地 HTTP 服务 + 任务队列 + 三端共用的 Web UI。
//!
//! 这不是"为了看起来像个 App"才加的：桌面壳（macOS/Windows）和安卓壳只要各放一个
//! WebView 指向这里，就得到完全一致的界面，业务逻辑只维护一份。

use resfetch_core::link;
use resfetch_core::naming;
use resfetch_core::plan::{Task, TaskState};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

#[derive(Clone)]
pub struct AppState {
    pub dir: PathBuf,
    pub ui_dir: PathBuf,
    pub tasks: Arc<Mutex<Vec<Task>>>,
    pub cookie_file: Option<PathBuf>,
}

impl AppState {
    pub fn snapshot_json(&self) -> String {
        let tasks = self.tasks.lock().unwrap();
        let items: Vec<String> = tasks.iter().map(|t| t.to_json()).collect();
        format!(
            "{{\"version\":\"{}\",\"dir\":\"{}\",\"tasks\":[{}]}}",
            resfetch_core::VERSION,
            resfetch_core::plan::json_escape(&self.dir.to_string_lossy()),
            items.join(",")
        )
    }

    /// 把粘贴进来的文本变成任务，只收能识别的链接
    pub fn enqueue_text(&self, text: &str) -> Vec<Task> {
        let mut tasks = self.tasks.lock().unwrap();
        let mut added = Vec::new();
        let mut taken: std::collections::HashSet<String> = tasks
            .iter()
            .map(|t| format!("{}/{}", t.dir, t.filename))
            .collect();
        for parsed in link::parse_many(text) {
            if tasks.iter().any(|t| t.canonical == parsed.canonical) {
                continue;
            }
            let mut task = Task::from_link(&parsed, &self.dir.to_string_lossy(), now());
            task.filename = naming::unique(&task.filename, &taken);
            taken.insert(format!("{}/{}", task.dir, task.filename));
            added.push(task.clone());
            tasks.push(task);
        }
        added
    }
}

pub fn run(port: u16, state: AppState) -> Result<(), String> {
    let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| format!("端口 {} 绑定失败：{}", port, e))?;
    println!("resfetch {} 已启动：http://127.0.0.1:{}", resfetch_core::VERSION, port);
    println!("下载目录：{}", state.dir.display());

    spawn_worker(state.clone());

    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let st = state.clone();
                thread::spawn(move || {
                    if let Err(e) = handle(s, st) {
                        eprintln!("连接处理失败：{}", e);
                    }
                });
            }
            Err(e) => eprintln!("接受连接失败：{}", e),
        }
    }
    Ok(())
}

fn handle(mut stream: TcpStream, state: AppState) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let mut reader = BufReader::new(stream.try_clone()?);

    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        return respond(&mut stream, 400, "text/plain; charset=utf-8", "bad request");
    }
    let method = parts[0];
    let target = parts[1];

    let mut content_length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            break;
        }
        if header == "\r\n" || header == "\n" {
            break;
        }
        let lower = header.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
    }

    let mut body = Vec::new();
    if content_length > 0 {
        body.resize(content_length.min(8 * 1024 * 1024), 0);
        reader.read_exact(&mut body)?;
    }

    let path = match target.split_once('?') {
        Some((p, _q)) => p,
        None => target,
    };
    let json = "application/json; charset=utf-8";

    match (method, path) {
        ("GET", "/api/version") => respond(
            &mut stream,
            200,
            json,
            &format!("{{\"version\":\"{}\",\"engine\":\"rust-core\"}}", resfetch_core::VERSION),
        )?,
        ("GET", "/api/tasks") => respond(&mut stream, 200, json, &state.snapshot_json())?,
        ("POST", "/api/tasks") => {
            let text = String::from_utf8_lossy(&body).to_string();
            let added = state.enqueue_text(&text);
            let items: Vec<String> = added.iter().map(|t| t.to_json()).collect();
            respond(&mut stream, 200, json, &format!("{{\"added\":[{}]}}", items.join(",")))?;
        }
        ("POST", "/api/clear") => {
            let mut tasks = state.tasks.lock().unwrap();
            tasks.retain(|t| !t.state.is_terminal());
            respond(&mut stream, 200, json, "{\"ok\":true}")?;
        }
        ("GET", "/") | ("GET", "/index.html") => {
            serve_static(&mut stream, &state.ui_dir, "index.html")?;
        }
        ("GET", p) if p.starts_with("/api/") => respond(&mut stream, 404, json, "{\"error\":\"not found\"}")?,
        ("GET", p) => {
            let name = p.trim_start_matches('/');
            if name.is_empty() || name.contains("..") || name.contains('/') {
                respond(&mut stream, 400, "text/plain; charset=utf-8", "bad path")?;
            } else {
                serve_static(&mut stream, &state.ui_dir, name)?;
            }
        }
        _ => respond(&mut stream, 405, "text/plain; charset=utf-8", "method not allowed")?,
    }
    Ok(())
}

fn serve_static(stream: &mut TcpStream, root: &Path, name: &str) -> std::io::Result<()> {
    let path = root.join(name);
    match std::fs::read(&path) {
        Ok(bytes) => {
            let ctype = match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
                "html" => "text/html; charset=utf-8",
                "js" => "application/javascript; charset=utf-8",
                "css" => "text/css; charset=utf-8",
                "svg" => "image/svg+xml",
                "png" => "image/png",
                "ico" => "image/x-icon",
                _ => "application/octet-stream",
            };
            respond_bytes(stream, 200, ctype, &bytes)
        }
        Err(_) => respond(stream, 404, "text/plain; charset=utf-8", "not found"),
    }
}

fn respond(stream: &mut TcpStream, status: u16, ctype: &str, body: &str) -> std::io::Result<()> {
    respond_bytes(stream, status, ctype, body.as_bytes())
}

fn respond_bytes(stream: &mut TcpStream, status: u16, ctype: &str, body: &[u8]) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "OK",
    };
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        status, reason, ctype, body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

// ---------------------------------------------------------------------------
// 后台下载线程
// ---------------------------------------------------------------------------

fn spawn_worker(state: AppState) {
    thread::spawn(move || loop {
        let candidate: Option<Task> = {
            let mut tasks = state.tasks.lock().unwrap();
            let now_ts = now();
            match tasks.iter().position(|t| t.state == TaskState::Queued && t.updated <= now_ts) {
                Some(i) => {
                    tasks[i].state = TaskState::Running;
                    tasks[i].attempts += 1;
                    tasks[i].updated = now_ts;
                    Some(tasks[i].clone())
                }
                None => None,
            }
        };

        let Some(task) = candidate else {
            thread::sleep(Duration::from_millis(400));
            continue;
        };

        let outcome = run_task(&state, &task);

        let mut tasks = state.tasks.lock().unwrap();
        if let Some(i) = tasks.iter().position(|t| t.id == task.id && t.created == task.created) {
            match outcome {
                Ok(bytes) => {
                    tasks[i].state = TaskState::Done;
                    tasks[i].bytes_done = bytes;
                    if tasks[i].bytes_total == 0 {
                        tasks[i].bytes_total = bytes;
                    }
                    tasks[i].last_error = None;
                }
                Err(msg) => {
                    tasks[i].last_error = Some(msg);
                    // 重试用指数退避，超过 3 次就停在 failed，等人看，而不是无限刷
                    if tasks[i].attempts >= 3 {
                        tasks[i].state = TaskState::Failed;
                    } else {
                        tasks[i].state = TaskState::Queued;
                        let delay = resfetch_core::plan::backoff_secs(tasks[i].attempts);
                        tasks[i].updated = now() + delay as i64;
                    }
                }
            }
        }
    });
}

fn run_task(state: &AppState, task: &Task) -> Result<u64, String> {
    let dest = state.dir.join(&task.filename);
    let cookie = state.cookie_file.as_deref();
    let referer = Some(task.canonical.as_str());

    if task.kind == "playlist" {
        return download_hls(task, &dest, cookie, referer);
    }

    match task.platform.as_str() {
        "direct" => {
            let head = crate::http::head(&task.canonical, cookie, referer)?;
            if head.status == 404 || head.status == 403 || head.status >= 500 {
                return Err(format!("HTTP {}（可能是签名过期或需要登录）", head.status));
            }
            let r = crate::http::download(&task.canonical, &dest, head.content_length, cookie, referer)?;
            Ok(r.bytes)
        }
        "douyin" | "kuaishou" | "xiaohongshu" | "bilibili" | "hongguo" => Err(format!(
            "{} 页面的真实地址需要站内接口解析（v0.2 计划，见 docs/ROADMAP.md）",
            task.platform
        )),
        other => Err(format!("暂不支持的平台：{}", other)),
    }
}

fn download_hls(task: &Task, dest: &Path, cookie: Option<&Path>, referer: Option<&str>) -> Result<u64, String> {
    let mut playlist_url = task.canonical.clone();
    let mut text = crate::http::get_text(&playlist_url, cookie, referer)?;
    let mut pl = resfetch_core::hls::parse(&text, &playlist_url).map_err(|e| e.to_string())?;

    if pl.is_master {
        let best = pl
            .pick_variant(Some(1080))
            .ok_or_else(|| "主播放列表里没有可用码率".to_string())?
            .uri
            .clone();
        playlist_url = best;
        text = crate::http::get_text(&playlist_url, cookie, referer)?;
        pl = resfetch_core::hls::parse(&text, &playlist_url).map_err(|e| e.to_string())?;
    }

    if pl.encrypted() {
        return Err("分片是 AES-128 加密的，v0.1 只下明文（v0.2 计划做本地解密）".to_string());
    }
    if pl.segments.is_empty() {
        return Err("播放列表里没有分片".to_string());
    }

    let seg_dir = dest.with_extension("segs");
    std::fs::create_dir_all(&seg_dir).map_err(|e| e.to_string())?;
    let mut total: u64 = 0;
    let mut ordered: Vec<PathBuf> = Vec::new();

    for (i, seg) in pl.segments.iter().enumerate() {
        let seg_path = seg_dir.join(format!("{:05}.ts", i + 1));
        let head = crate::http::head(&seg.uri, cookie, referer).unwrap_or_default();
        let r = crate::http::download(&seg.uri, &seg_path, head.content_length, cookie, referer)?;
        total += r.bytes;
        ordered.push(seg_path);
    }

    let mut out = std::fs::File::create(dest).map_err(|e| e.to_string())?;
    for p in &ordered {
        let mut f = std::fs::File::open(p).map_err(|e| e.to_string())?;
        std::io::copy(&mut f, &mut out).map_err(|e| e.to_string())?;
    }
    let _ = std::fs::remove_dir_all(&seg_dir);
    Ok(total)
}
