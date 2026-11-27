//! 任务模型、状态机与重试退避。
//!
//! 三端共用同一份任务结构，UI 只要读 JSON 就行；重试策略也只有一处，
//! 不会出现"桌面端退避 1s、安卓端退避 10s"这种各写一套的老问题。

use crate::link::ParsedLink;
use crate::naming;
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Queued,
    Running,
    Paused,
    Done,
    Failed,
    Skipped,
}

impl TaskState {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskState::Queued => "queued",
            TaskState::Running => "running",
            TaskState::Paused => "paused",
            TaskState::Done => "done",
            TaskState::Failed => "failed",
            TaskState::Skipped => "skipped",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, TaskState::Done | TaskState::Failed | TaskState::Skipped)
    }

    /// 只有这些状态能继续推进
    pub fn can_start(&self) -> bool {
        matches!(self, TaskState::Queued | TaskState::Paused)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    pub id: String,
    pub url: String,
    pub canonical: String,
    pub platform: String,
    pub kind: String,
    pub filename: String,
    pub dir: String,
    pub state: TaskState,
    pub attempts: u32,
    pub bytes_total: u64,
    pub bytes_done: u64,
    pub created: i64,
    pub updated: i64,
    pub last_error: Option<String>,
}

impl Task {
    pub fn from_link(link: &ParsedLink, dir: &str, now: i64) -> Task {
        let filename = naming::from_url_and_title(&link.canonical, None, link.kind.default_ext());
        Task {
            id: format!("{}-{}", link.platform.as_str(), &stable_hash(&link.canonical)),
            url: link.original.clone(),
            canonical: link.canonical.clone(),
            platform: link.platform.as_str().to_string(),
            kind: link.kind.as_str().to_string(),
            filename,
            dir: dir.to_string(),
            state: TaskState::Queued,
            attempts: 0,
            bytes_total: 0,
            bytes_done: 0,
            created: now,
            updated: now,
            last_error: None,
        }
    }

    pub fn progress(&self) -> f64 {
        if self.bytes_total == 0 {
            return if self.state == TaskState::Done { 1.0 } else { 0.0 };
        }
        (self.bytes_done as f64 / self.bytes_total as f64).clamp(0.0, 1.0)
    }

    pub fn to_json(&self) -> String {
        format!(
            "{{\"id\":\"{}\",\"url\":\"{}\",\"canonical\":\"{}\",\"platform\":\"{}\",\"kind\":\"{}\",\"filename\":\"{}\",\"dir\":\"{}\",\"state\":\"{}\",\"attempts\":{},\"bytes_total\":{},\"bytes_done\":{},\"progress\":{:.3},\"created\":{},\"updated\":{},\"last_error\":{}}}",
            json_escape(&self.id),
            json_escape(&self.url),
            json_escape(&self.canonical),
            json_escape(&self.platform),
            json_escape(&self.kind),
            json_escape(&self.filename),
            json_escape(&self.dir),
            self.state.as_str(),
            self.attempts,
            self.bytes_total,
            self.bytes_done,
            self.progress(),
            self.created,
            self.updated,
            match &self.last_error {
                Some(e) => format!("\"{}\"", json_escape(e)),
                None => "null".to_string(),
            }
        )
    }
}

pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            _ => out.push(c),
        }
    }
    out
}

/// 由链接内容决定的稳定 id（不用随机数，重启后同一链接还能对上）
pub fn stable_hash(s: &str) -> u64 {
    // FNV-1a 64：跨平台确定性，不依赖 std 的 Hash（后者带随机种子）
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// 指数退避：1, 2, 4, 8, 16, 30, 30…（封顶 30 秒，避免把风控当成可用性去硬刷）
pub fn backoff_secs(attempts: u32) -> u64 {
    match attempts {
        0 | 1 => 1,
        2 => 2,
        3 => 4,
        4 => 8,
        5 => 16,
        _ => 30,
    }
}

#[derive(Debug, Default)]
pub struct Queue {
    pub tasks: Vec<Task>,
    taken: HashSet<String>,
}

impl Queue {
    pub fn new() -> Self {
        Queue::default()
    }

    /// 同一条链接只入队一次（归一化后比较）
    pub fn push(&mut self, mut task: Task) -> bool {
        if self.tasks.iter().any(|t| t.canonical == task.canonical) {
            return false;
        }
        task.filename = naming::unique(&task.filename, &self.taken);
        self.taken.insert(task.filename.clone());
        self.tasks.push(task);
        true
    }

    pub fn next_runnable(&mut self, now: i64) -> Option<&mut Task> {
        let idx = self.tasks.iter().position(|t| t.state.can_start())?;
        let t = &mut self.tasks[idx];
        t.state = TaskState::Running;
        t.attempts += 1;
        t.updated = now;
        Some(t)
    }

    pub fn pending_count(&self) -> usize {
        self.tasks.iter().filter(|t| t.state.can_start()).count()
    }

    pub fn done_count(&self) -> usize {
        self.tasks.iter().filter(|t| t.state == TaskState::Done).count()
    }

    pub fn to_json_array(&self) -> String {
        let items: Vec<String> = self.tasks.iter().map(|t| t.to_json()).collect();
        format!("[{}]", items.join(","))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link;

    const NOW: i64 = 1_800_000_000;

    #[test]
    fn backoff_is_monotonic_and_capped() {
        let seq: Vec<u64> = (0..9).map(backoff_secs).collect();
        assert_eq!(seq, vec![1, 1, 2, 4, 8, 16, 30, 30, 30]);
    }

    #[test]
    fn state_transitions() {
        assert!(TaskState::Queued.can_start());
        assert!(TaskState::Paused.can_start());
        assert!(!TaskState::Running.can_start());
        assert!(TaskState::Done.is_terminal());
        assert!(!TaskState::Failed.can_start());
        assert!(!TaskState::Queued.is_terminal());
    }

    #[test]
    fn task_json_is_parsable_shape() {
        let link = link::parse("https://www.douyin.com/video/7301234567890123456").unwrap();
        let t = Task::from_link(&link, "/tmp/out", NOW);
        let json = t.to_json();
        assert!(json.starts_with('{') && json.ends_with('}'));
        assert!(json.contains("\"platform\":\"douyin\""));
        assert!(json.contains("\"state\":\"queued\""));
        assert!(json.contains("\"progress\":0.000"));
        assert!(json.contains("\"last_error\":null"));
    }

    #[test]
    fn progress_math() {
        let link = link::parse("https://x.com/a.mp4").unwrap();
        let mut t = Task::from_link(&link, "/tmp", NOW);
        t.bytes_total = 1000;
        t.bytes_done = 250;
        assert!((t.progress() - 0.25).abs() < 1e-9);
        t.bytes_done = 2000;
        assert!((t.progress() - 1.0).abs() < 1e-9, "进度不能超过 100%");
        t.state = TaskState::Done;
        t.bytes_total = 0;
        assert_eq!(t.progress(), 1.0);
    }

    #[test]
    fn queue_dedupes_same_link_ignoring_tracking_params() {
        let mut q = Queue::new();
        let a = link::parse("https://www.douyin.com/video/123?utm_source=x").unwrap();
        let b = link::parse("https://www.douyin.com/video/123?utm_source=y").unwrap();
        assert!(q.push(Task::from_link(&a, "/tmp", NOW)));
        assert!(!q.push(Task::from_link(&b, "/tmp", NOW)));
        assert_eq!(q.tasks.len(), 1);
    }

    #[test]
    fn queue_makes_filenames_unique() {
        let mut q = Queue::new();
        let a = link::parse("https://x.com/a/1.mp4").unwrap();
        let mut t1 = Task::from_link(&a, "/tmp", NOW);
        t1.filename = "视频.mp4".into();
        q.push(t1);
        let b = link::parse("https://x.com/a/2.mp4").unwrap();
        let mut t2 = Task::from_link(&b, "/tmp", NOW);
        t2.filename = "视频.mp4".into();
        q.push(t2);
        assert_eq!(q.tasks[1].filename, "视频 (1).mp4");
    }

    #[test]
    fn next_runnable_marks_running_and_counts_attempts() {
        let mut q = Queue::new();
        let a = link::parse("https://x.com/a/1.mp4").unwrap();
        q.push(Task::from_link(&a, "/tmp", NOW));
        assert_eq!(q.pending_count(), 1);
        let t = q.next_runnable(NOW).unwrap();
        assert_eq!(t.state, TaskState::Running);
        assert_eq!(t.attempts, 1);
        assert!(q.next_runnable(NOW).is_none());
    }

    #[test]
    fn stable_hash_is_deterministic_and_short() {
        assert_eq!(stable_hash("abc"), stable_hash("abc"));
        assert_ne!(stable_hash("abc"), stable_hash("abd"));
        assert_eq!(format!("{:016x}", stable_hash("abc")).len(), 16);
    }

    #[test]
    fn json_escape_handles_quotes_and_newlines() {
        assert_eq!(json_escape("a\"b\\c\nd"), "a\\\"b\\\\c\\nd");
    }
}
