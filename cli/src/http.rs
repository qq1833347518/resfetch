//! 用系统 `curl` 做网络层。
//!
//! 为什么不引 reqwest/rustls？因为内核要求三端零依赖编译：macOS 自带 curl，
//! Windows 10+ 自带 curl.exe，安卓可以换成 JNI 里的 OkHttp，接口签名保持一致即可。
//! 这样"网络"就是可替换的外设，而不是把 TLS 栈焊死在核心里。

use std::path::Path;
use std::process::Command;
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

fn part_path(dest: &Path) -> std::path::PathBuf {
    let ext = dest
        .extension()
        .map(|e| format!("{}.part", e.to_string_lossy()))
        .unwrap_or_else(|| "part".to_string());
    dest.with_extension(ext)
}

/// 从 `Content-Range: bytes 0-0/20872903` 里取总长度
fn total_from_headers(headers: &str) -> Option<u64> {
    for line in headers.lines() {
        let lower = line.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-range:") {
            if let Some((_, total)) = v.split_once('/') {
                if let Ok(n) = total.trim().parse::<u64>() {
                    return Some(n);
                }
            }
        }
    }
    None
}

/// 先发一个 `Range: bytes=0-1` 探总长度：B 站 CDN 的 HEAD 不可用，但 Range 可用。
/// 返回 (总字节, 是否真的支持 Range)。
fn probe_total(url: &str, cookie_file: Option<&Path>, referer: Option<&str>) -> Option<(u64, bool)> {
    let mut cmd = curl_base();
    cmd.arg("--http1.1").arg("-L").arg("-f")
        .arg("-r").arg("0-1")
        .arg("-D").arg("-")
        .arg("-o").arg(if cfg!(windows) { "NUL" } else { "/dev/null" })
        .arg("-w").arg("__CODE__%{http_code}");
    if let Some(c) = cookie_file {
        cmd.arg("-b").arg(c);
    }
    if let Some(r) = referer {
        cmd.arg("-e").arg(r);
    }
    cmd.arg(url);

    let out = cmd.output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let code: u16 = text
        .rsplit("__CODE__")
        .next()
        .unwrap_or("0")
        .trim()
        .parse()
        .unwrap_or(0);
    if code == 206 {
        return total_from_headers(&text).map(|t| (t, true));
    }
    if code == 200 {
        for line in text.lines() {
            let lower = line.to_ascii_lowercase();
            if let Some(v) = lower.strip_prefix("content-length:") {
                if let Ok(n) = v.trim().parse::<u64>() {
                    return Some((n, false));
                }
            }
        }
    }
    None
}

fn append_file(src: &Path, dst: &Path) -> Result<(), String> {
    use std::io::Write;
    let data = std::fs::read(src).map_err(|e| e.to_string())?;
    let mut out = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(dst)
        .map_err(|e| e.to_string())?;
    out.write_all(&data).map_err(|e| e.to_string())?
}

/// 分块续传下载。
///
/// 和 `curl -C -` 的区别：自己在每次尝试前看 `.part` 已有多大，再发精确的 `Range`，
/// 并把拿到的块**追加**进去。B 站 mcdn 代理经常“传一半就断”，而且有时忽略 Range
/// 返回 200，`curl -C -` 遇到这两种情况会反复覆盖 `.part`、进度归零——这里都按情况处理。
pub fn download(
    url: &str,
    dest: &Path,
    expected_total: u64,
    cookie_file: Option<&Path>,
    referer: Option<&str>,
) -> Result<DownloadResult, String> {
    let part = part_path(dest);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    let probe = probe_total(url, cookie_file, referer);
    let (total, supports_range) = match probe {
        Some((t, r)) => (t, r),
        None => (expected_total, expected_total > 0),
    };

    let started = Instant::now();
    let mut last_error = String::from("未知错误");
    let mut ranges_off = !supports_range;

    for attempt in 1..=6u32 {
        let have = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        if total > 0 && have >= total {
            break;
        }
        let tmp = part.with_extension("fetching");
        let _ = std::fs::remove_file(&tmp);

        let mut cmd = curl_base();
        // B 站 mcdn 对 HTTP/2 不稳（err 92），统一 HTTP/1.1
        cmd.arg("--http1.1").arg("-L").arg("-f");
        if have > 0 && !ranges_off {
            cmd.arg("-r").arg(format!("{}-", have));
        }
        cmd.arg("-o").arg(&tmp).arg("-w").arg("%{http_code}");
        if let Some(c) = cookie_file {
            cmd.arg("-b").arg(c);
        }
        if let Some(r) = referer {
            cmd.arg("-e").arg(r);
        }
        cmd.arg(url);

        let out = cmd.output().map_err(|e| format!("调不动 curl：{}", e))?;
        let code: u16 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0);
        let got = std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);

        if code == 416 {
            let _ = std::fs::remove_file(&tmp);
            break; // 已经超出范围，说明拿全了
        }
        if code == 0 || code >= 400 {
            last_error = format!(
                "HTTP {}（第 {} 次）：{}",
                code,
                attempt,
                String::from_utf8_lossy(&out.stderr).trim()
            );
            let _ = std::fs::remove_file(&tmp);
            std::thread::sleep(Duration::from_secs(2));
            continue;
        }
        if code == 200 && have > 0 {
            // 服务端不认 Range，只能整个重拿；后续就别再发 Range 了
            ranges_off = true;
        }

        if code == 206 && have > 0 && got > 0 {
            append_file(&tmp, &part)?;
        } else if got > 0 {
            std::fs::rename(&tmp, &part).map_err(|e| e.to_string())?;
        } else {
            let _ = std::fs::remove_file(&tmp);
        }

        let now_bytes = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        let complete = if total > 0 {
            now_bytes >= total
        } else {
            out.status.success() && now_bytes > 0
        };
        if complete {
            break;
        }
        last_error = format!(
            "第 {} 次只拿到 {} 字节{}",
            attempt,
            got,
            if total > 0 {
                format!("，还差 {}", total.saturating_sub(now_bytes))
            } else {
                String::new()
            }
        );
        std::thread::sleep(Duration::from_secs(1));
    }

    let bytes = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if bytes == 0 {
        return Err(format!("下载失败：一个字节都没拿到（{}）", last_error));
    }
    if total > 0 && bytes < total {
        return Err(format!(
            "下载不完整：{} / {} 字节（{}）。直链可能已过期，重试会重新解析",
            bytes, total, last_error
        ));
    }
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

#[cfg(test)]
mod tests {
    use super::{part_path, total_from_headers};
    use std::path::Path;

    #[test]
    fn part_path_keeps_extension_and_appends_part() {
        assert_eq!(part_path(Path::new("a.mp4")), Path::new("a.mp4.part"));
        assert_eq!(part_path(Path::new("a.part1.mp4")), Path::new("a.part1.mp4.part"));
        assert_eq!(part_path(Path::new("noext")), Path::new("noext.part"));
    }

    #[test]
    fn total_from_content_range() {
        let headers = "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-1/20872903\r\n";
        assert_eq!(total_from_headers(headers), Some(20872903));
        assert_eq!(total_from_headers("HTTP/1.1 200 OK\r\n"), None);
        assert_eq!(total_from_headers("content-range: bytes 1-2/*"), None);
    }
}
