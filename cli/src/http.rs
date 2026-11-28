//! 用系统 `curl` 做网络层。
//!
//! 为什么不引 reqwest/rustls？因为内核要求三端零依赖编译：macOS 自带 curl，
//! Windows 10+ 自带 curl.exe，安卓可以换成 JNI 里的 OkHttp，接口签名保持一致即可。
//! 这样"网络"就是可替换的外设，而不是把 TLS 栈焊死在核心里。

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
(KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

#[derive(Debug, Clone, Default)]
pub struct HeadInfo {
    pub status: u16,
    pub content_length: u64,
    pub content_type: String,
    pub effective_url: String,
}

fn curl_base() -> Command {
    let mut c = Command::new("curl");
    c.arg("-sS").arg("--compressed").arg("-A").arg(UA);
    c
}

/// HEAD 探测：跟随重定向，取最后一组响应头。
pub fn head(url: &str, cookie_file: Option<&Path>, referer: Option<&str>) -> Result<HeadInfo, String> {
    let mut cmd = curl_base();
    cmd.arg("-I").arg("-L").arg("--max-time").arg("20");
    if let Some(c) = cookie_file {
        cmd.arg("-b").arg(c);
    }
    if let Some(r) = referer {
        cmd.arg("-e").arg(r);
    }
    cmd.arg("-w").arg("__CODE__%{http_code}__URL__%{url_effective}");
    cmd.arg(url);

    let out = cmd.output().map_err(|e| format!("调不动 curl：{}", e))?;
    if !out.status.success() && out.stdout.is_empty() {
        return Err(format!("curl 失败：{}", String::from_utf8_lossy(&out.stderr)));
    }
    let text = String::from_utf8_lossy(&out.stdout);

    let mut info = HeadInfo::default();
    if let Some(p) = text.rfind("__CODE__") {
        let code_start = p + "__CODE__".len();
        info.status = text[code_start..]
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
    }
    if let Some(p) = text.rfind("__URL__") {
        info.effective_url = text[p + "__URL__".len()..].trim().to_string();
    }
    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            if let Ok(n) = v.trim().parse::<u64>() {
                info.content_length = n;
            }
        }
        if let Some(v) = lower.strip_prefix("content-type:") {
            info.content_type = v.split(';').next().unwrap_or("").trim().to_string();
        }
    }
    Ok(info)
}

/// 取文本（m3u8、接口 JSON）
pub fn get_text(url: &str, cookie_file: Option<&Path>, referer: Option<&str>) -> Result<String, String> {
    let mut cmd = curl_base();
    cmd.arg("-L").arg("--max-time").arg("20").arg("-f");
    if let Some(c) = cookie_file {
        cmd.arg("-b").arg(c);
    }
    if let Some(r) = referer {
        cmd.arg("-e").arg(r);
    }
    cmd.arg(url);
    let out = cmd.output().map_err(|e| format!("调不动 curl：{}", e))?;
    if !out.status.success() {
        let code = out.status.code().unwrap_or(-1);
        return Err(format!(
            "HTTP/curl 失败（退出码 {}）：{}",
            code,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// 取二进制到内存（AES-128 的密钥就 16 字节，不必落临时文件）
pub fn get_bytes(url: &str, cookie_file: Option<&Path>, referer: Option<&str>) -> Result<Vec<u8>, String> {
    let mut cmd = curl_base();
    cmd.arg("-L").arg("-f").arg("--max-time").arg("30");
    if let Some(c) = cookie_file {
        cmd.arg("-b").arg(c);
    }
    if let Some(r) = referer {
        cmd.arg("-e").arg(r);
    }
    cmd.arg(url);

    let out = cmd.output().map_err(|e| format!("调不动 curl：{}", e))?;
    if !out.status.success() {
        return Err(format!(
            "取 {} 失败：{}",
            url,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

pub struct DownloadResult {
    pub bytes: u64,
    pub elapsed: Duration,
}

/// 断点续传下载：先写 `.part`，完成后再改名，中断了下次能接着下。
pub fn download(
    url: &str,
    dest: &Path,
    expected_total: u64,
    cookie_file: Option<&Path>,
    referer: Option<&str>,
) -> Result<DownloadResult, String> {
    let part = dest.with_extension(format!(
        "{}part",
        dest.extension().map(|e| format!("{}.", e.to_string_lossy())).unwrap_or_default()
    ));

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let mut cmd = curl_base();
    cmd.arg("-L").arg("-f").arg("--retry").arg("2").arg("--retry-delay").arg("1");
    cmd.arg("-C").arg("-"); // 续传
    cmd.arg("-o").arg(&part);
    if let Some(c) = cookie_file {
        cmd.arg("-b").arg(c);
    }
    if let Some(r) = referer {
        cmd.arg("-e").arg(r);
    }
    cmd.arg(url);

    let started = Instant::now();
    let mut child = cmd
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("启动 curl 失败：{}", e))?;

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return Err(format!("下载失败，curl 退出码 {:?}", status.code()));
                }
                break;
            }
            Ok(None) => {
                if let Ok(meta) = std::fs::metadata(&part) {
                    let done = meta.len();
                    if expected_total > 0 {
                        let pct = (done as f64 / expected_total as f64 * 100.0).min(100.0);
                        eprint!("\r  {:.1}%  {} / {}  ", pct, human(done), human(expected_total));
                    } else {
                        eprint!("\r  {} ", human(done));
                    }
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    eprintln!();

    let bytes = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    std::fs::rename(&part, dest).map_err(|e| format!("改名失败：{}", e))?;
    Ok(DownloadResult { bytes, elapsed: started.elapsed() })
}

pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{:.1} {}", v, UNITS[i])
    }
}
