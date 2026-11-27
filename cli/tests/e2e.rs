//! 端到端测试：真的把二进制拉起来，走 HTTP API 建任务，验证队列与 JSON 结构。
//! 不联网、不依赖真实账号，所以能进 CI。

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct ServerGuard {
    child: Child,
    port: u16,
    _dir: PathBuf,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn tmp_dir(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("resfetch-e2e-{}-{}", tag, std::process::id()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn start_server(port: u16, dir: &Path) -> ServerGuard {
    let bin = env!("CARGO_BIN_EXE_resfetch");
    let ui = Path::new(env!("CARGO_MANIFEST_DIR")).join("../ui");
    let child = Command::new(bin)
        .args([
            "serve",
            "--port",
            &port.to_string(),
            "--dir",
            dir.to_str().unwrap(),
            "--ui",
            ui.to_str().unwrap(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("启动 resfetch serve 失败");

    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    ServerGuard { child, port, _dir: dir.to_path_buf() }
}

fn request(port: u16, method: &str, path: &str, body: &[u8]) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("连不上本地服务");
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let head = format!(
        "{} {} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        method,
        path,
        body.len()
    );
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
    let mut out = String::new();
    stream.read_to_string(&mut out).expect("读响应失败");
    out
}

fn body_of(resp: &str) -> &str {
    resp.split("\r\n\r\n").nth(1).unwrap_or("")
}

#[test]
fn serves_ui_and_json_api() {
    let dir = tmp_dir("api");
    let guard = start_server(8811, &dir);

    let version = request(guard.port, "GET", "/api/version", b"");
    assert!(version.starts_with("HTTP/1.1 200"), "版本接口应 200：{}", version);
    assert!(body_of(&version).contains("\"version\""));

    let index = request(guard.port, "GET", "/", b"");
    assert!(index.starts_with("HTTP/1.1 200"));
    assert!(body_of(&index).contains("resfetch"), "首页应返回 UI");

    let style = request(guard.port, "GET", "/style.css", b"");
    assert!(style.starts_with("HTTP/1.1 200"));
    assert!(body_of(&style).contains("--accent"));

    let missing = request(guard.port, "GET", "/../etc/passwd", b"");
    assert!(missing.starts_with("HTTP/1.1 400") || missing.starts_with("HTTP/1.1 404"));
}

#[test]
fn post_creates_tasks_and_dedupes() {
    let dir = tmp_dir("tasks");
    let guard = start_server(8812, &dir);

    let text = "https://cdn.example.com/a/ep1.m3u8\n\
                https://www.douyin.com/video/7301234567890123456?utm_source=x\n\
                https://www.douyin.com/video/7301234567890123456?utm_source=y\n\
                这不是链接";
    let resp = request(guard.port, "POST", "/api/tasks", text.as_bytes());
    assert!(resp.starts_with("HTTP/1.1 200"), "{}", resp);
    assert!(body_of(&resp).contains("\"added\""));

    let list = request(guard.port, "GET", "/api/tasks", b"");
    let body = body_of(&list);
    assert_eq!(body.matches("\"platform\":\"douyin\"").count(), 1, "重复链接应去重：{}", body);
    assert!(body.contains("\"platform\":\"hls\""));
    assert!(body.contains("\"state\":\"queued\""));

    let again = request(guard.port, "POST", "/api/tasks", text.as_bytes());
    assert!(body_of(&again).contains("\"added\":[]"), "{}", again);
}

#[test]
fn failed_download_is_recorded_not_crashed() {
    let dir = tmp_dir("fail");
    let guard = start_server(8813, &dir);

    // 127.0.0.1 上一个没人监听的端口，curl 必然失败
    request(guard.port, "POST", "/api/tasks", b"http://127.0.0.1:1/nope.mp4");

    let deadline = Instant::now() + Duration::from_secs(25);
    let mut last = String::new();
    while Instant::now() < deadline {
        last = body_of(&request(guard.port, "GET", "/api/tasks", b"")).to_string();
        if last.contains("\"state\":\"failed\"") || last.contains("\"state\":\"done\"") {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(
        last.contains("\"last_error\":\"") || last.contains("\"state\":\"failed\""),
        "失败必须被记录为 last_error，而不是进程崩掉：{}",
        last
    );
    assert!(request(guard.port, "GET", "/api/version", b"").starts_with("HTTP/1.1 200"));
}
