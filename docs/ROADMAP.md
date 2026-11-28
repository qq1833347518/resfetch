# 路线图（按"能不能独立验收"排序）

每条都满足：不改内核的公共接口就能加，而且能用离线单测覆盖。

## v0.1（已完成）

- [x] 链接识别与归一化（抖音 / 快手 / 小红书 / B站 / 红果 / HLS / 直链），去跟踪参数
- [x] 文件名清洗（Windows 非法字符、保留名、UTF-8 安全截断）与同名去重
- [x] Cookie 保守合并：同键覆盖、异键保留、过期单列计数（对应旧版"丢 65 条"事故）
- [x] Cookie 发送挑选：按域（含子域标记）、路径前缀、过期过滤，路径长者优先
- [x] m3u8 解析：主/子列表、码率筛选、EXT-X-KEY / MAP / BYTERANGE、相对路径还原
- [x] m3u8 下载并拼接（明文分片），分片失败仍按任务级重试
- [x] EPUB3 打包：手写 ZIP、CRC32 校验、mimetype 首条目、XML 转义
- [x] 日志：分级、脱敏（cookie / token / 手机号 / 邮箱 / 本机路径）、5 秒去重
- [x] 任务状态机 + 指数退避（1→2→4→8→16→30 封顶）+ 稳定任务 ID
- [x] CLI：`probe / fetch / resolve / hls / cookies / epub / serve`，每个子命令都能单独验证
- [x] 本地 Web UI（三端共用），任务队列由后台线程串行执行
- [x] 集成测试 `cli/tests/e2e.rs`：起服务 → 建任务 → 校验 JSON 与失败记录

## v0.2（站内解析，按平台逐个做，做完一个就能用）

- [x] 抖音解析器：`_ROUTER_DATA` / `RENDER_DATA` / 渲染后 DOM 三路提取，无水印直链与图集，`resolve` 子命令
      （实测 2026 年 SSR 已只剩骨架，播放地址靠前端签名请求，所以另两条通道是设计选择，不是退路）
- [x] HLS AES-128 本地解密：`EXT-X-KEY` 显式 IV 与媒体序号回退、密钥 URI 缓存、逐片解密后拼接
      （自己写的 AES-128-CBC，FIPS-197 / NIST SP 800-38A 官方向量单测；SAMPLE-AES 显式拒绝）
- [x] 任务持久化：`.resfetch/tasks.jsonl` 一行一任务、`.tmp` 原子改名；启动时 `running`→`queued`
      续跑、`done` 不重复下、坏行跳过并计数（重试次数保留，避免重启变成无限重试）
- [x] 下载器重写：`Range` 探总长 + 分块追加续传（替代 `curl -C -`），修 B 站 mcdn "传一半断/忽略 Range"
- [x] B 站：`x/web-interface/view` → `x/web-interface/nav` → `x/player/wbi/playurl`
      wbi 签名与 MD5 都在内核里自写（`core/src/bilibili.rs`、`core/src/md5.rs`），真机接口已验证被接受
      CLI `resfetch bili` 与 Web UI 任务队均可用；DASH 音视频分离，有 ffmpeg 就合并
      官方口径文档：https://github.com/SocialSisterYi/bilibili-API-collect/blob/master/docs/misc/sign/wbi.md
- [ ] B 站备用 CDN：DASH 响应里有 `backupUrl`，主地址 403/超时后自动换备用（现在是靠分块重试硬扛）
- [ ] 抖音闭环：壳层 WebView 加载页面后导出 DOM，把"复制直链"这一步省掉（依赖 v0.3 桌面壳）
- [ ] 快手：`/graphql` 的 `visionVideoDetail` 请求，需要 `kuaishou.web.cp.api` 有效 cookie
- [ ] 红果短剧：先探测 `protected_playback_unavailable`，再用已登录 cookie 重试

## v0.3（壳）

- [ ] macOS：SwiftUI 菜单栏壳 + 分享面板接收链接
- [ ] Windows：WebView2 壳，注册 `resfetch://` 协议
- [ ] Android：WebView + `Intent.FILTER` 接收分享，OkHttp 替换 curl
- [ ] 三端统一"下载完成通知"

## 明确不做

- 指纹伪造、代理轮换、批量多账号 —— 这是对抗平台风控，会把用户的号玩死。
- 旧版 8.9 的结论也是这条：账号级风控只能靠冷却期和真实登录，不能靠技术绕。
