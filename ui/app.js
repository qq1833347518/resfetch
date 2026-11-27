// UI 只有这一份：桌面 WebView 和安卓 WebView 跑的是同一段代码，
// 所以任何交互问题都只会在一个地方出现、也只在一个地方修。

const $ = (sel) => document.querySelector(sel);

const state = { tasks: [], version: "", dir: "" };

function human(bytes) {
  if (!bytes) return "—";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let v = bytes, i = 0;
  while (v >= 1024 && i < units.length - 1) { v /= 1024; i++; }
  return i === 0 ? `${bytes} B` : `${v.toFixed(1)} ${units[i]}`;
}

const STATE_LABEL = {
  queued: "排队", running: "下载中", paused: "暂停",
  done: "完成", failed: "失败", skipped: "跳过",
};

function render() {
  const list = $("#tasks");
  list.innerHTML = "";
  $("#empty").classList.toggle("hidden", state.tasks.length > 0);

  const done = state.tasks.filter((t) => t.state === "done").length;
  const failed = state.tasks.filter((t) => t.state === "failed").length;
  const active = state.tasks.filter((t) => t.state === "running").length;
  $("#summary").textContent = state.tasks.length
    ? `共 ${state.tasks.length} · 完成 ${done} · 进行中 ${active}${failed ? ` · 失败 ${failed}` : ""}`
    : "";

  for (const t of state.tasks) {
    const li = document.createElement("li");
    li.className = "task";

    const row1 = document.createElement("div");
    row1.className = "row1";

    const name = document.createElement("span");
    name.className = "name";
    name.textContent = t.filename || "(未命名)";
    name.title = t.canonical;

    const badge = document.createElement("span");
    badge.className = "badge " + t.state;
    badge.textContent = STATE_LABEL[t.state] || t.state;

    row1.append(name, badge);

    const meta = document.createElement("div");
    meta.className = "meta";
    meta.textContent = `${t.platform} · ${t.kind} · ${human(t.bytes_done)}${t.bytes_total ? " / " + human(t.bytes_total) : ""}${t.attempts > 1 ? ` · 第 ${t.attempts} 次` : ""}`;

    const bar = document.createElement("div");
    bar.className = "bar";
    const fill = document.createElement("i");
    fill.style.width = `${Math.round((t.progress || 0) * 100)}%`;
    if (t.state === "done") fill.style.width = "100%";
    if (t.state === "failed") fill.style.background = "var(--err)";
    bar.append(fill);

    li.append(row1, meta, bar);

    if (t.last_error) {
      const err = document.createElement("div");
      err.className = "err";
      err.textContent = t.last_error;
      li.append(err);
    }
    list.append(li);
  }
}

async function poll() {
  try {
    const res = await fetch("/api/tasks", { cache: "no-store" });
    const data = await res.json();
    state.version = data.version;
    state.dir = data.dir;
    state.tasks = data.tasks || [];
    $("#ver").textContent = "v" + data.version;
    $("#dir").textContent = data.dir || "";
    $("#dir").title = data.dir || "";
    render();
  } catch (e) {
    $("#hint").textContent = "连不上本地服务：" + e.message;
  }
}

async function addLinks() {
  const text = $("#input").value.trim();
  if (!text) return;
  $("#hint").textContent = "解析中…";
  try {
    const res = await fetch("/api/tasks", { method: "POST", body: text });
    const data = await res.json();
    const added = (data.added || []).length;
    $("#hint").textContent = added ? `已加入 ${added} 条` : "没有识别到新链接（可能已在队列里）";
    if (added) $("#input").value = "";
    await poll();
  } catch (e) {
    $("#hint").textContent = "添加失败：" + e.message;
  }
}

async function clearDone() {
  try {
    await fetch("/api/clear", { method: "POST" });
    await poll();
  } catch (e) {
    $("#hint").textContent = "清空失败：" + e.message;
  }
}

$("#add").addEventListener("click", addLinks);
$("#clear").addEventListener("click", clearDone);
$("#input").addEventListener("keydown", (e) => {
  if ((e.metaKey || e.ctrlKey) && e.key === "Enter") addLinks();
});

poll();
setInterval(poll, 1000);
