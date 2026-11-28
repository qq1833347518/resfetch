//! 本地 HTTP 服务 + 任务队列 + 三端共用的 Web UI。
//!
//! 这不是"为了看起来像个 App"才加的：桌面壳（macOS/Windows）和安卓壳只要各放一个
//! WebView 指向这里，就得到完全一致的界面，业务逻辑只维护一份。

use resfetch_core::link;
use resfetch_core::naming;
use resfetch_core::plan::{Task, TaskState};
use resfetch_core::store;
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
    /// 任务快照文件：`<下载目录>/.resfetch/tasks.jsonl`
    pub fn state_path(&self) -> PathBuf {
        self.dir.join(store::STATE_DIR).join(store::STATE_FILE)
    }

    /// 整份重写快照（先写 `.tmp` 再原子改名，断电也不会写出半份文件）。
    /// 单个任务几十到几百字节，几百个任务也就几十 KB，不必做增量日志。
    pub fn persist(&self) {
        let text = {
            let tasks = self.tasks.lock().unwrap();
            store::encode(&tasks)
        };
        let path = self.state_path();
        if let Some(parent) = path.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                return;
            }
        }
        let tmp = path.with_extension("jsonl.tmp");
        if std::fs::write(&tmp, text.as_bytes()).is_err() {
            return;
        }
        let _ = std::fs::rename(&tmp, &path);
    }

    /// 启动时读回上次的任务；返回 (恢复条数, 跳过坏行数)。
    /// `running` 归一到 `queued`（重启后接着跑），`done` 保持完成（不重复下）。
    pub fn restore(&self) -> (usize, usize) {
        let path = self.state_path();
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let decoded = store::decode(&text);
        let skipped = decoded.skipped;
        let mut restored = 0usize;
        let mut tasks = self.tasks.lock().unwrap();
        let mut taken: std::collections::HashSet<String> = tasks
            .iter()
            .map(|t| format!("{}/{}", t.dir, t.filename))
            .collect();
        for mut t in decoded.tasks {
            store::normalize_on_load(&mut t);
            if t.state.can_start() {
                // 清掉上次的退避时间，否则重启后要等到未来某个时间点才动
                t.updated = 0;
            }
            t.dir = self.dir.to_string_lossy().to_string();
            t.filename = naming::unique(&t.filename, &taken);
            taken.insert(format!("{}/{}", t.dir, t.filename));
            tasks.push(t);
            restored += 1;
        }
        (restored, skipped)
    }

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
        drop(tasks);
        if !added.is_empty() {
            self.persist();
        }
        added
    }
}

pub fn run(port: u16, state: AppState) -> Result<(), String> {
    let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| format!("端口 {} 绑定失败：{}", port, e))?;
    println!("resfetch {} 已启动：http://127.0.0.1:{}", resfetch_core::VERSION, port);
    println!("下载目录：{}", state.dir.display());

    let (restored, skipped) = state.restore();
    if restored > 0 || skipped > 0 {
        println!(
            "已恢复 {} 个任务{}",
            restored,
            if skipped > 0 {
                format!("（跳过 {} 条坏记录）", skipped)
            } else {
                String::new()
            }
        );
    }

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
            drop(tasks);
            state.persist();
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
            drop(tasks);
            state.persist();
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
        "douyin" => download_douyin(task, &dest, cookie),
        "bilibili" => download_bilibili(task, &dest, cookie),
        "kuaishou" | "xiaohongshu" | "hongguo" => Err(format!(
            "{} 页面的真实地址需要站内接口解析（还没接，见 docs/ROADMAP.md）",
            task.platform
        )),
        other => Err(format!("暂不支持的平台：{}", other)),
    }
}

/// 抖音：先解析页面拿到真实地址，再下载。图集作品落成同名目录 + 序号文件。
fn download_douyin(task: &Task, dest: &Path, cookie: Option<&Path>) -> Result<u64, String> {
    const REFERER: &str = "https://www.douyin.com/";
    let html = crate::http::get_text(&task.canonical, cookie, Some(REFERER))?;
    let resolved = resfetch_core::douyin::parse_page(&html).map_err(|e| e.to_string())?;

    match resolved.media {
        resfetch_core::douyin::Media::Video { url, .. } => {
            fetch_to(&url, dest, cookie, Some(REFERER))
        }
        resfetch_core::douyin::Media::Images(urls) => {
            let dir = dest.with_extension("");
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let mut total = 0;
            for (i, u) in urls.iter().enumerate() {
                let p = dir.join(format!("{:02}.jpg", i + 1));
                total += fetch_to(u, &p, cookie, Some(REFERER))?;
            }
            Ok(total)
        }
    }
}

/// 单文件下载 + 状态码检查。直链有时效，过期就明确报状态码，让重试重新解析。
fn fetch_to(url: &str, dest: &Path, cookie: Option<&Path>, referer: Option<&str>) -> Result<u64, String> {
    let head = crate::http::head(url, cookie, referer)?;
    if head.status == 403 || head.status == 404 || head.status >= 500 {
        return Err(format!("取媒体直链返回 HTTP {}（直链有效期内才可下，重试会重新解析）", head.status));
    }
    let r = crate::http::download(url, dest, head.content_length, cookie, referer)?;
    Ok(r.bytes)
}

// ---------------------------------------------------------------------------
// B 站：wbi 签名 → playurl → 音视频分离下载（有 ffmpeg 就合并）
// ---------------------------------------------------------------------------

/// 与 CLI 里同一套规则：接受裸 BV 号或带 BV 号的链接（b23.tv 短链在链接层已归一化）。
fn bvid_of(input: &str) -> Option<String> {
    if input.len() == 12 && input.starts_with("BV") {
        return Some(input.to_string());
    }
    let pos = input.find("BV")?;
    let bvid: String = input[pos..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
    if bvid.len() == 12 {
        Some(bvid)
    } else {
        None
    }
}

/// 下载目录下的兄弟文件：`a.mp4` + "audio.m4s" → `a.mp4.audio.m4s`
fn sibling(dest: &Path, suffix: &str) -> PathBuf {
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "download".to_string());
    dest.with_file_name(format!("{}.{}", name, suffix))
}

fn try_merge(video: &Path, audio: &Path, out: &Path) -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-y")
        .arg("-i")
        .arg(video)
        .arg("-i")
        .arg(audio)
        .arg("-c")
        .arg("copy")
        .arg(out)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// B 站媒体 CDN 不支持 HEAD（`curl -I` 直接 404），所以不探测长度，直接续传下。
/// 同一份流的 `baseUrl` 与 `backupUrl` 指向镜像，内容逐字节相同，所以换地址后
/// `.part` 里的已下数据可以继续追加，不必从头再下。
fn fetch_bili_stream(urls: &[String], dest: &Path, cookie: Option<&Path>, referer: &str) -> Result<u64, String> {
    let mut last = String::from("没有可用地址");
    for (i, u) in urls.iter().enumerate() {
        match crate::http::download(u, dest, 0, cookie, Some(referer)) {
            Ok(r) => {
                if i > 0 {
                    eprintln!("已切到备用地址 #{}", i);
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

fn download_bilibili(task: &Task, dest: &Path, cookie: Option<&Path>) -> Result<u64, String> {
    const REFERER: &str = "https://www.bilibili.com/";
    let bvid = bvid_of(&task.canonical).ok_or_else(|| format!("认不出 BV 号：{}", task.canonical))?;

    // 1) 标题与 cid（多 P 用 pages[0]）
    let view = crate::http::get_text(
        &format!("https://api.bilibili.com/x/web-interface/view?bvid={}", bvid),
        cookie,
        Some(REFERER),
    )?;
    let info = resfetch_core::bilibili::parse_view(&view).map_err(|e| e.to_string())?;

    // 2) wbi key
    let nav = crate::http::get_text("https://api.bilibili.com/x/web-interface/nav", cookie, Some(REFERER))?;
    let mixin = resfetch_core::bilibili::mixin_key_from_nav(&nav).map_err(|e| e.to_string())?;

    // 3) 签名后的 playurl（fnval=16 取 dash，音量画质分开挑）
    let query = resfetch_core::bilibili::sign_query(
        &resfetch_core::bilibili::playurl_params(&info.bvid, info.cid, 80, true),
        &mixin,
        now() as u64,
    );
    let body = crate::http::get_text(
        &format!("https://api.bilibili.com/x/player/wbi/playurl?{}", query),
        cookie,
        Some(REFERER),
    )?;
    let urls = resfetch_core::bilibili::parse_playurl(&body).map_err(|e| e.to_string())?;

    // 老接口给的整段地址：逐段下完按顺序拼成一个文件
    if !urls.durl.is_empty() {
        let mut total = 0u64;
        let mut segs: Vec<PathBuf> = Vec::new();
        for (i, u) in urls.durl.iter().enumerate() {
            let p = sibling(dest, &format!("durl{}", i + 1));
            total += fetch_bili_stream(std::slice::from_ref(u), &p, cookie, REFERER)?;
            segs.push(p);
        }
        let mut out = std::fs::File::create(dest).map_err(|e| e.to_string())?;
        for p in &segs {
            let mut f = std::fs::File::open(p).map_err(|e| e.to_string())?;
            std::io::copy(&mut f, &mut out).map_err(|e| e.to_string())?;
            let _ = std::fs::remove_file(p);
        }
        let n = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(total);
        return Ok(n);
    }

    let vurls = resfetch_core::bilibili::best(&urls.videos)
        .map(|s| s.urls())
        .ok_or_else(|| "playurl 里没有可用视频流".to_string())?;
    let aurls = resfetch_core::bilibili::best(&urls.audios).map(|s| s.urls());

    let vpath = sibling(dest, "video.m4s");
    let vbytes = fetch_bili_stream(&vurls, &vpath, cookie, REFERER)?;

    if let Some(aurls) = aurls {
        let apath = sibling(dest, "audio.m4s");
        let abytes = fetch_bili_stream(&aurls, &apath, cookie, REFERER)?;
        if try_merge(&vpath, &apath, dest) {
            let n = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(vbytes + abytes);
            let _ = std::fs::remove_file(&vpath);
            let _ = std::fs::remove_file(&apath);
            return Ok(n);
        }
        // 没装 ffmpeg：视频落到目标文件，音频留在旁边，不报失败（下次装了再合也行）
        std::fs::rename(&vpath, dest).map_err(|e| e.to_string())?;
        return Ok(vbytes + abytes);
    }

    std::fs::rename(&vpath, dest).map_err(|e| e.to_string())?;
    Ok(vbytes)
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

    if let Some(method) = pl
        .segments
        .iter()
        .filter_map(|s| s.key.as_ref())
        .filter(|k| k.is_encrypted() && !k.method_supported())
        .map(|k| k.method.clone())
        .next()
    {
        return Err(format!(
            "分片是 {} 加密的，本版只支持 AES-128（SAMPLE-AES 需要完整解密链，不做硬跑）",
            method
        ));
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

    // 加密分片：下完之后逐片就地解密，再做拼接
    if pl.encrypted() {
        let mut keys: std::collections::HashMap<String, [u8; 16]> = std::collections::HashMap::new();
        for (i, seg) in pl.segments.iter().enumerate() {
            let Some(k) = &seg.key else { continue };
            if !k.is_encrypted() {
                continue;
            }
            let uri = k
                .uri
                .as_deref()
                .ok_or_else(|| "EXT-X-KEY 缺少 URI，无法取密钥".to_string())?;
            let key = match keys.get(uri) {
                Some(k) => *k,
                None => {
                    let bytes = crate::http::get_bytes(uri, cookie, referer)?;
                    let key: [u8; 16] = bytes.as_slice().try_into().map_err(|_| {
                        format!("AES-128 密钥应为 16 字节，{} 返回了 {} 字节", uri, bytes.len())
                    })?;
                    keys.insert(uri.to_string(), key);
                    key
                }
            };
            let iv = k
                .iv_for(pl.media_sequence + i as u64)
                .ok_or_else(|| format!("无法解析 IV：{:?}", k.iv))?;
            let path = seg_dir.join(format!("{:05}.ts", i + 1));
            let cipher = std::fs::read(&path).map_err(|e| e.to_string())?;
            let plain =
                resfetch_core::aes::cbc_decrypt(&key, &iv, &cipher).map_err(|e| e.to_string())?;
            std::fs::write(&path, &plain).map_err(|e| e.to_string())?;
            total = total - cipher.len() as u64 + plain.len() as u64;
        }
    }

    let mut out = std::fs::File::create(dest).map_err(|e| e.to_string())?;
    for p in &ordered {
        let mut f = std::fs::File::open(p).map_err(|e| e.to_string())?;
        std::io::copy(&mut f, &mut out).map_err(|e| e.to_string())?;
    }
    let _ = std::fs::remove_dir_all(&seg_dir);
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bvid_of_accepts_bare_and_url_forms() {
        assert_eq!(bvid_of("BV1GJ411x7h7").as_deref(), Some("BV1GJ411x7h7"));
        assert_eq!(
            bvid_of("https://www.bilibili.com/video/BV1GJ411x7h7/?p=1").as_deref(),
            Some("BV1GJ411x7h7")
        );
        assert_eq!(bvid_of("https://example.com/abc"), None);
    }

    #[test]
    fn sibling_keeps_full_name() {
        assert_eq!(
            sibling(Path::new("/tmp/a.mp4"), "audio.m4s"),
            Path::new("/tmp/a.mp4.audio.m4s")
        );
    }
}
