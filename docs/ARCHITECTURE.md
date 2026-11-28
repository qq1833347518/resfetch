# 架构与三端路线

一句话：**一份 Rust 内核 + 一份 Web UI + 三个很薄的壳。**
所有业务逻辑只存在一处，macOS / Windows / Android 的差别被压缩到"怎么打开一个 WebView、怎么把文件写到用户目录"。

```
┌─────────────────────────────────────────────────────────┐
│  外壳层（每端 ~200 行，可替换）                          │
│  macOS: SwiftUI + WKWebView     Windows: WinUI/WebView2  │
│  Android: WebView + JNI 桥      Linux: GTK-WebKit        │
└───────────────────────┬─────────────────────────────────┘
                        │ 本地 HTTP（127.0.0.1:8787）
┌───────────────────────┴─────────────────────────────────┐
│  服务层 cli/src/server.rs                       │
│  任务队列 · 重试退避 · 静态文件 · JSON API · 下载执行     │
│  快照读写（.resfetch/tasks.jsonl）                        │
└───────────────────────┬─────────────────────────────────┘
                        │ 直接函数调用（同一个进程）
┌───────────────────────┴─────────────────────────────────┐
│  内核层 core/（零第三方依赖，只依赖 std）                │
│  link.rs      链接识别 / 归一化 / 去追踪参数               │
│  naming.rs    文件名清洗（Windows+FAT 兼容）与去重         │
│  cookie.rs    保守合并、过期剔除、按域/路径挑选发送        │
│  hls.rs       m3u8 解析、码率选择、相对路径还原、媒体序号  │
│  aes.rs       AES-128-CBC 解密、IV 解析、序号 IV 推导      │
│  epub.rs      EPUB3 打包（手写 ZIP + CRC32）                │
│  log.rs       分级日志、脱敏（手机号/邮箱/cookie/路径）    │
│  plan.rs      任务状态机、指数退避、稳定任务 ID、JSON 输出 │
│  store.rs     任务快照读写（JSONL、原子改名、坏行跳过）    │
│  json.rs      极简 JSON 解析 / 内嵌裁剪 / 百分号解码       │
│  douyin.rs    抖音页面解析（SSR 数据 / 渲染 DOM）          │
│  md5.rs       MD5（RFC 1321），只为 B 站 wbi 的 w_rid       │
│  bilibili.rs  wbi 签名、view/playurl 解析、流挑选          │
└─────────────────────────────────────────────────────────┘
```

## 为什么是这个结构

**1. 内核零依赖。** 不引 `reqwest`、`serde`、`chrono`、`zip`、`aes`、`md-5`。
很实际的好处：三端交叉编译不会因为某个 crate 不支持目标平台而卡住，`cargo test` 秒级完成，安卓上也不必为体积翻倍买单。需要的能力（JSON、ZIP、AES、MD5、时间格式化、CRC32）都在几十到一百多行内自己实现，且都有单测。

**2. 网络是可替换的外设，不是焊死的依赖。**
`cli/src/http.rs` 调系统 `curl`（macOS/Windows 10+/Linux 都有）。安卓壳直接换成 Kotlin 的 `OkHttp`，只要实现同样的五个函数（`head` / `get_text` / `get_bytes` / `download` / `human`），内核一行不用改。

**3. UI 只有一份。**
桌面端和安卓端都是 WebView 指向 `127.0.0.1:8787`。界面改一次，三端同时生效；也不会出现"桌面端能拖拽、安卓端不能"这种逐渐分叉。

**4. 平台差异只在四个明确的接缝上。**
| 接缝 | macOS | Windows | Android |
|---|---|---|---|
| 打开 UI | WKWebView | WebView2 | android.webkit.WebView |
| 下载目录 | `~/Downloads/resfetch` | `%USERPROFILE%\Downloads\resfetch` | `/sdcard/Download/resfetch`（Scoped Storage） |
| Cookie 库 | `~/.resfetch/cookies.txt` | `%APPDATA%\resfetch\cookies.txt` | App 私有目录 |
| 网络 | 系统 curl | 系统 curl.exe | OkHttp（JNI 注入） |

**5. 原生能力同样走注入点。**
系统通知、分享面板接收链接、后台保活，这些"只有壳懂"的事通过内核预留的接缝注入，不写进内核——内核保持纯净，才谈得上三端复用。

**6. 加密是格式问题，不是安全边界。**
HLS 的密钥本来明文交给客户端，所以 `aes.rs` 只做"解"、不做"加"，也不做常量时间查表这类抵侧信道处理；换来的是零依赖、可交叉编译、有官方向量兜底的干净实现。

**7. 持久化只做"快照"。**
任务量级是几十到几百，每次状态变化整份重写（`.tmp` + rename）远比 WAL/增量回放便宜，而且断电最多影响到最后一行的写入，不会把已有队列写坏。安卓上同一套代码也成立（Scoped Storage 内部目录）。

**8. 平台接口拆成两半：算法进内核，HTTP 留外设。**
B 站这条链路是典型例子：wbi 签名、参数排序、值编码、MD5 都在 `core/` 里，是纯函数、有离线单测、和网络无关；真正发请求的 `nav` / `playurl` 留在 `cli/src/http.rs`。这样换 OkHttp 时只对接 HTTP，签名不会跟着重写一遍，也就不会出现"桌面能签、安卓签错"这种三端漂移。

## 下载器为什么不是一行 `curl -C -`

早期实现就是 `curl -C -`，在普通源站上没问题；碰到 B 站 mcdn 代理立刻原形里现：连传一半就断（curl 18/56）、HTTP/2 报 `err 92`、某些节点直接忽略 `Range` 返回 200。`-C -` 在这种情况下会把 `.part` 覆盖回去，看日志是"一直在下"，看文件是永远 0 字节。

现在的做法：

1. 先 `Range: bytes=0-1` 探一次，从 `Content-Range` 取真实总长（B 站 CDN 的 HEAD 不可用）；
2. 每轮看 `.part` 已多大，发精确 `Range`，把拿到的块追加进去；若服务端回 200（不认 Range）就改成整段重拿并记下这个事实；
3. 有进展就立刻要下一段，零进展才退避；连续 4 次零字节直接报错，不让死链拖成几分钟；
4. `--connect-timeout` + `--speed-limit/--speed-time` 负责"连上了但不给数据"这种卡死；
5. 结束时比对总长，不足就报"下载不完整"而不是改个后缀假装成功。

## 编译目标

```bash
cargo build --release                            # macOS / Linux
rustup target add x86_64-pc-windows-msvc
cargo build --release --target x86_64-pc-windows-msvc   # Windows

rustup target add aarch64-linux-android
cargo build --release --target aarch64-linux-android     # 安卓静态库，再配 JNI 壳
```

内核不含任何 `std::process` 调用，所以安卓上不会踩"fork 被禁"的坑；`curl` 只出现在 CLI/桌面服务层。
