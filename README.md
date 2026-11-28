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
| 直链下载、分块续传（`Range` 探总长 + `.part` 追加，断连可续） | ✅ | `resfetch fetch --url ... --dir ...` |
| m3u8 解析：码率筛选、分片清单、加密识别、相对路径还原 | ✅ | `resfetch hls <url> --max-height 720` |
| m3u8 下载并拼接（含 AES-128 分片解密） | ✅（走 Web UI 任务队列） | `serve` 后贴链接 |
| HLS AES-128 解密：`EXT-X-KEY` IV / 媒体序号回退、取密钥、去 PKCS#7 | ✅ | 自动，无需开关 |
| 任务快照与重启续跑（`.resfetch/tasks.jsonl`，原子改名、坏行跳过、`running` 归一到排队） | ✅ | 自动，`serve` 启动时恢复 |
| Cookie 保守合并（不覆盖其它站点）、按域/路径挑选发送 | ✅ | `resfetch cookies merge --store ... --incoming ...` |
| EPUB3 打包（手写 ZIP，mimetype 首条目，CRC 校验） | ✅ | `resfetch epub --title ... --in --out` |
| 日志分级 + 脱敏（cookie/token/手机号/邮箱/本机路径） | ✅ | 代码内 `log::redact` |
| 抖音解析（`_ROUTER_DATA` / `RENDER_DATA` / 渲染后 DOM 三路，无水印直链、图集） | ✅ | `resfetch resolve <url> [--html 快照]` |
| B 站解析与下载（wbi 签名、DASH 音视频分离、durl 拼接） | ✅ | `resfetch bili <BV号\|链接> [--quality N]` |
| 快手 graphql / 红果 playback | 🚧 v0.2 | 见 `docs/ROADMAP.md` |

没做完的部分是**明确标注**的，不会假装能用：快手、红果的页面链接进任务会得到一条 `last_error`，说明缺的是哪一步（B 站已经不再是占位）。

### 加密 m3u8 到底能不能下

能，但只到 `METHOD=AES-128` 这一档：密钥就是 16 字节，一个密钥 URI 对应一份明文分片流，逐片解密再拼接即可。`EXT-X-KEY` 没写 `IV` 时按 `#EXT-X-MEDIA-SEQUENCE` + 分片序号推（RFC 8216 §5.2），这两条路径都有单测。

`SAMPLE-AES` 需要一套完整解密链（ffmpeg 内部那种），本版**显式报错而不是硬跑**——下回来一堆乱码比直接说"这档不支持"更浪费用户时间。

### 关掉窗口/断电后任务还在吗

在。每个任务状态变化都会把整份队列重写到 `<下载目录>/.resfetch/tasks.jsonl`（先写 `.tmp` 再原子改名）：一次几十 KB，比设计 WAL 划算。下次 `serve` 启动时自动读回：

- 上次在跑（`running`）的被杀掉的，回到 `queued` 接着跑，且**退避时间清零**（否则重启得干等到未来某个时间点）；
- 已完成（`done`）的不重下；
- 重试次数保留，避免"重启就重新拥有 3 次机会、变成无限重试"；
- 残缺行/未知状态行跳过并计数，不会因为一行坏了就丢掉整份队列。

### 抖音为什么不能"贴链接就下"

2026 年抖音已经不再把播放地址放在静态页里了：分享页的 `_ROUTER_DATA` 只剩骨架（`itemId`/`webId`/`serverToken`），真正取地址靠前端请求 + 签名参数，而且无登录态的裸请求大概率直接吃验证页。

本项目的取舍是**不伪造签名**（那是绕过反爬），于是留了两条都合法的通道：

1. **渲染后 DOM**：在已登录的浏览器里打开视频页，导出 HTML → `resfetch resolve <url> --html page.html`，从 `<video src>` 取直链；v0.3 的 macOS 壳会把这个动作变成点一下。
2. **直链**：从播放器复制 mp4 地址 → `resfetch fetch --url ...`，走断点续传。

解析器本身对三种形态都能吃，离线单测覆盖：SSR 壳页、URL 编码的 `RENDER_DATA`、渲染后 DOM（含 `blob:`/`.m3u8` 误命中排除）。

### B 站为什么能，以及为什么慢

链路很短：`x/web-interface/view` 拿标题和 cid → `x/web-interface/nav` 拿两个 wbi key → `x/player/wbi/playurl` 拿 DASH 地址。麻烦的是中间那个签名：

- 两个 key 的主干拼成 64 字符，按固定置换表取前 32 位当 `mixin_key`；
- 请求参数加 `wts`、按 key 升序、值做 B 站特例编码（保留 `-._~`，删掉 `!'()*`）；
- `w_rid = md5(query + mixin_key)`。MD5 自己写在 `core/src/md5.rs`（RFC 1321 向量 + 56/64 字节边界兜底），不引 crate。

接口文档（社区维护、最接近官方口径的记录）：

- wbi 签名：https://github.com/SocialSisterYi/bilibili-API-collect/blob/master/docs/misc/sign/wbi.md
- 视频信息 / 播放地址：https://github.com/SocialSisterYi/bilibili-API-collect/blob/master/docs/video/videostream_url.md

慢不是网络问题，是 B 站 CDN（mcdn 代理）本身脾气：HTTP/2 会 `err 92 stream not closed cleanly`、经常传一半就断、有时忽略 `Range` 直接回 200。`cli/src/http.rs` 的处理是：强制 HTTP/1.1、先 `Range: bytes=0-1` 探总长、每次按已下大小发精确 `Range` 并把分块**追加**进 `.part`、连续 4 次零字节就报错（不把死链拖成几分钟）。

DASH 是音视频分开的两条流。本机有 `ffmpeg` 时自动 `-c copy` 合成单文件；没有就把视频落到目标名、音频留成 `xxx.mp4.audio.m4s` 放旁边，并打印合并命令——不假装合并成功。

## 目录

```
core/        零依赖内核：link / naming / cookie / hls / aes / epub / log / plan / store / json / douyin / md5 / bilibili
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
| 为解密去引 `aes`/`cbc` 两个 crate | `aes.rs` 自己写：GF(2^8) 现算 S 盒、单向 CBC 解密，FIPS-197 / SP 800-38A 官方向量兜底 |
| 重启就得把链接重新贴一遍 | `store.rs` JSONL 快照 + 原子改名，`running` 回来接着跑 |
| 下载器只会 `-C -` 续传，CDN 一断就重头来 | `http.rs` 自己换算 `Range` 并追加分块；断一次只损失当前那段 |

## 一句话原则

**不碰风控。** 这个项目不做指纹伪造、不做代理轮换、不做多账号批量。平台账号级风控只能靠冷却期和真实登录恢复，技术绕不过去，硬绕只会把号玩死——旧版 8.9 的结论也是这样。
