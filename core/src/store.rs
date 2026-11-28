//! 任务快照持久化：`<下载目录>/.resfetch/tasks.jsonl`。
//!
//! 为什么是 JSONL 而不是一个 JSON 数组：一行一个任务，追加/截断都只碰一行，
//! 中途断电最多坏最后一行，其余任务照样能读回来。读的时候坏行跳过并计数，
//! 不会因为一行残缺就把整份队列判死。
//!
//! 不做"增量日志 + 回放"：任务数量是几十到几百的量级，每次状态变化整份重写
//! （先写 `.tmp` 再原子改名）比设计一套 WAL 简单得多，也不会写坏原文件。

use crate::json;
use crate::plan::{Task, TaskState};

pub const STATE_DIR: &str = ".resfetch";
pub const STATE_FILE: &str = "tasks.jsonl";

#[derive(Debug, Default, PartialEq)]
pub struct Decoded {
    pub tasks: Vec<Task>,
    /// 坏行条数（残缺 JSON、未知状态、缺关键字段）
    pub skipped: usize,
}

/// 整份编码：一行一个任务，结尾带换行（保证追加语义正确）
pub fn encode(tasks: &[Task]) -> String {
    let mut out = String::new();
    for t in tasks {
        out.push_str(&t.to_json());
        out.push('\n');
    }
    out
}

fn state_from_str(s: &str) -> Option<TaskState> {
    match s {
        "queued" => Some(TaskState::Queued),
        "running" => Some(TaskState::Running),
        "paused" => Some(TaskState::Paused),
        "done" => Some(TaskState::Done),
        "failed" => Some(TaskState::Failed),
        "skipped" => Some(TaskState::Skipped),
        _ => None,
    }
}

/// 解一行。缺 `canonical` / `filename` / `state` 的行视为坏行。
fn decode_line(line: &str) -> Option<Task> {
    let v = json::parse(line).ok()?;
    let canonical = v.get("canonical")?.as_str()?.to_string();
    let filename = v.get("filename")?.as_str()?.to_string();
    let state = state_from_str(v.get("state")?.as_str()?)?;

    Some(Task {
        id: v.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        url: v.get("url").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        canonical,
        platform: v.get("platform").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        kind: v.get("kind").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        filename,
        dir: v.get("dir").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        state,
        attempts: v.get("attempts").and_then(|x| x.as_i64()).unwrap_or(0).max(0) as u32,
        bytes_total: v.get("bytes_total").and_then(|x| x.as_i64()).unwrap_or(0).max(0) as u64,
        bytes_done: v.get("bytes_done").and_then(|x| x.as_i64()).unwrap_or(0).max(0) as u64,
        created: v.get("created").and_then(|x| x.as_i64()).unwrap_or(0),
        updated: v.get("updated").and_then(|x| x.as_i64()).unwrap_or(0),
        last_error: v.get("last_error").and_then(|x| x.as_str()).map(|s| s.to_string()),
    })
}

pub fn decode(text: &str) -> Decoded {
    let mut d = Decoded::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match decode_line(line) {
            Some(t) => d.tasks.push(t),
            None => d.skipped += 1,
        }
    }
    d
}

/// 重启后的状态归一：
/// - `running` 说明上次是被杀掉的，没有"正在跑"这回事了 → 回到 `queued` 重跑；
/// - `done` 保持 `done`（文件已在盘上，不重复下）；
/// - 重试次数保留，避免"重启就重置成 0 次、变成无限重试"。
pub fn normalize_on_load(t: &mut Task) {
    if t.state == TaskState::Running {
        t.state = TaskState::Queued;
    }
    if t.state.can_start() && t.updated < 0 {
        t.updated = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::Task;

    fn sample() -> Task {
        Task {
            id: "douyin-1".to_string(),
            url: "https://v.douyin.com/abc/".to_string(),
            canonical: "https://www.douyin.com/video/7".to_string(),
            platform: "douyin".to_string(),
            kind: "video".to_string(),
            filename: "标题 \"带引号\" 的视频.mp4".to_string(),
            dir: "/Users/demo/Downloads/resfetch".to_string(),
            state: TaskState::Queued,
            attempts: 2,
            bytes_total: 100,
            bytes_done: 42,
            created: 1_700_000_000,
            updated: 1_700_000_010,
            last_error: Some("HTTP 403\n签名过期".to_string()),
        }
    }

    #[test]
    fn roundtrip_preserves_fields_and_escapes() {
        let t = sample();
        let text = encode(&[t.clone()]);
        assert!(text.ends_with('\n'));
        assert_eq!(text.lines().count(), 1);

        let d = decode(&text);
        assert_eq!(d.skipped, 0);
        assert_eq!(d.tasks.len(), 1);
        assert_eq!(d.tasks[0], t);
    }

    #[test]
    fn multi_line_errors_do_not_break_jsonl() {
        let mut t = sample();
        t.last_error = Some("第一行\n第二行\r\n带\t制表符 \\ 反斜杠".to_string());
        let text = encode(&[t.clone()]);
        // 换行被转义成 \n，所以仍然只有一行
        assert_eq!(text.lines().count(), 1);
        let d = decode(&text);
        assert_eq!(d.tasks[0], t);
    }

    #[test]
    fn broken_lines_are_skipped_not_fatal() {
        let good = encode(&[sample()]);
        let text = format!(
            "{{\"canonical\":\"https://x/1\",\"filename\":\"a.mp4\",\"state\":\"queued\"}}\n\
             {{ not json\n\
             \n\
             {{\"canonical\":\"https://x/2\",\"filename\":\"b.mp4\",\"state\":\"weird\"}}\n\
             {}",
            good
        );
        let d = decode(&text);
        assert_eq!(d.tasks.len(), 2, "缺 id 但关键字段齐的行也算有效");
        assert_eq!(d.skipped, 2);
        assert_eq!(d.tasks[1].filename, "标题 \"带引号\" 的视频.mp4");
    }

    #[test]
    fn running_task_returns_to_queue_on_restart() {
        let mut t = sample();
        t.state = TaskState::Running;
        normalize_on_load(&mut t);
        assert_eq!(t.state, TaskState::Queued);
        // 重试次数不能被清零，否则重启变成无限重试
        assert_eq!(t.attempts, 2);
    }

    #[test]
    fn done_task_stays_done() {
        let mut t = sample();
        t.state = TaskState::Done;
        t.bytes_done = 100;
        normalize_on_load(&mut t);
        assert_eq!(t.state, TaskState::Done);
        assert_eq!(t.bytes_done, 100);
    }

    #[test]
    fn empty_file_decodes_to_empty_queue() {
        let d = decode("");
        assert_eq!(d, Decoded::default());
    }

    #[test]
    fn negative_numbers_are_clamped() {
        let text = "{\"canonical\":\"https://x/1\",\"filename\":\"a.mp4\",\"state\":\"queued\",\"attempts\":-3,\"bytes_done\":-1}\n";
        let d = decode(text);
        assert_eq!(d.tasks[0].attempts, 0);
        assert_eq!(d.tasks[0].bytes_done, 0);
    }
}
