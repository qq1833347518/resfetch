# resfetch · 资源下载器

一份 Rust 内核 + 一份 Web UI + 三个很薄的平台壳。目标是**同一套逻辑同时长出 macOS / Windows / Android 三端**，而不是每次换平台重写一遍。

```bash
# 编译并跑全部测试（核心零第三方依赖，不需要联网、不需要账号）
cargo build --release
cargo test

# 跑起来：本地 Web UI
cargo run --release -- serve --port 8787 --dir ~/Downloads/resfetch --ui ui
# 浏览器打开 http://127.0.0.1:8787
```

## 现在能干什么

| 能力 | 状态 | 命令 |
|---|---|---|
| 识别链接（抖音/快手/小红书/B站/红果/直链/HLS），去跟踪参数 | ✅ | `resfetch probe <url>` |
| 直链下载、断点续传（`.part` 命名，中断可续） | ✅ | `resfetch fetch --url ... --dir ...` |
| m3u8 解析：码率筛选、分片清单、加密识别、相对路径还原 | ✅ | `resfetch hls <url> --max-height 720` |
| m3u8 下载并拼接（明文分片） | ✅（走 Web UI 任务队列） | `serve` 后贴链接 |
| Cookie 保守合并（不覆盖其它站点）、按域/路径挑选发送 | ✅ | `resfetch cookies merge --store ... --incoming ...` |
| EPUB3 打包（手写 ZIP，mimetype 首条目，CRC 校验） | ✅ | `resfetch epub --title ... --in --out` |
| 日志分级 + 脱敏（cookie/token/手机号/邮箱/本机路径） | ✅ | 代码内 `log::redact` |
| 抖音解析（`_ROUTER_DATA` / `RENDER_DATA` / 渲染后 DOM 三路，无水印直链、图集） | ✅ | `resfetch resolve <url> [--html 快照]` |
| 快手 graphql / B站 wbi / 红果 playback | 🚧 v0.2 | 见 `docs/ROADMAP.md` |

没做完的部分是**明确标注**的，不会假装能用：快手/B站页面链接进任务会得到一条 `last_error`，说明缺的是哪一步。

### 抖音为什么不能"贴链接就下"

2026 年抖音已经不再把播放地址放在静态页里了：分享页的 `_ROUTER_DATA` 只剩骨架（`itemId`/`webId`/`serverToken`），真正取地址靠前端请求 + 签名参数，而且无登录态的裸请求大概率直接吃验证页。

本项目的取舍是**不伪造签名**（那是绕过反爬），于是留了两条都合法的通道：

1. **渲染后 DOM**：在已登录的浏览器里打开视频页，导出 HTML → `resfetch resolve <url> --html page.html`，从 `<video src>` 取直链；v0.3 的 macOS 壳会把这个动作变成点一下。
2. **直链**：从播放器复制 mp4 地址 → `resfetch fetch --url ...`，走断点续传。

解析器本身对三种形态都能吃，离线单测覆盖：SSR 壳页、URL 编码的 `RENDER_DATA`、渲染后 DOM（含 `blob:`/`.m3u8` 误命中排除）。

## 目录

```
core/        零依赖内核：link / naming / cookie / hls / epub / log / plan / json / douyin
cli/         CLI + 本地 HTTP 服务 + 任务队列（网络层是系统 curl）
ui/          三端共用的单页界面（桌面 WebView / 安卓 WebView 同一份）
docs/        架构与路线图
```

## 为什么值得换掉旧版

旧版（Swift 单平台）踩过的坑，这里都有对应的结构性答案：

| 旧版事故 | 新项目的处理 |
|---|---|
| 导入 cookie 时整文件覆盖，丢了 65 条 | `cookie::merge` 按 (domain,path,name) 三元组合并，异键一律保留；单测 `merge_never_drops_other_sites` 锁死这个行为 |
| bitbrowser:// 自定义 scheme 交给系统弹窗 | 内核不做 WebView 策略；壳层统一 `.cancel` 非 http(s)，UI 侧不可能再造出"系统弹窗" |
| 明明登录了却读不回 cookie | Cookie 读取与发送分离，`header_value` 按 RFC 6265 选域/路径，过期单列统计 |
| 日志里出现明文 cookie、手机号 | `log::redact` 默认打码，且同一条错误 5 秒去重 |
| 文件名在 Windows/安卓上写不出来 | `naming::sanitize` 按 Windows+FAT 最严格规则清洗，保留名 `CON/NUL` 处理，UTF-8 安全截断 |
| 三端各写一套重试/退避 | `plan::backoff_secs` 一处定义（1→2→4→8→16→30 封顶） |

## 一句话原则

**不碰风控。** 这个项目不做指纹伪造、不做代理轮换、不做多账号批量。平台账号级风控只能靠冷却期和真实登录恢复，技术绕不过去，硬绕只会把号玩死——旧版 8.9 的结论也是这样。
