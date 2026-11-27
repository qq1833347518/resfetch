# 路线图（按"能不能独立验收"排序）

每条都满足：不改内核的公共接口就能加，而且能用离线单测覆盖。

## v0.1（本项目当前）

- [x] 链接识别与归一化（抖音 / 快手 / 小红书 / B站 / 红果 / HLS / 直链），去跟踪参数
- [x] 文件名清洗（Windows 非法字符、保留名、UTF-8 安全截断）与同名去重
- [x] Cookie 保守合并：同键覆盖、异键保留、过期单列计数（对应旧版"丢 65 条"事故）
- [x] Cookie 发送挑选：按域（含子域标记）、路径前缀、过期过滤，路径长者优先
- [x] m3u8 解析：主/子列表、码率筛选、EXT-X-KEY / MAP / BYTERANGE、相对路径还原
- [x] m3u8 下载并拼接（明文分片），分片失败仍按任务级重试
- [x] EPUB3 打包：手写 ZIP、CRC32 校验、mimetype 首条目、XML 转义
- [x] 日志：分级、脱敏（cookie / token / 手机号 / 邮箱 / 本机路径）、5 秒去重
- [x] 任务状态机 + 指数退避（1→2→4→8→16→30 封顶）+ 稳定任务 ID
- [x] CLI：`probe / fetch / hls / cookies / epub / serve`，每个子命令都能单独验证
- [x] 本地 Web UI（三端共用），任务队列由后台线程串行执行
- [x] 集成测试 `cli/tests/e2e.rs`：起服务 → 建任务 → 校验 JSON 与失败记录

## v0.2（站内解析，按平台逐个做，做完一个就能用）

- [ ] 抖音：分享页 `_ROUTER_DATA` / `play_addr` 提取（不需要签名，只需 UA + Cookie）
- [ ] 快手：`/graphql` 的 `visionVideoDetail` 请求，需要 `kuaishou.web.cp.api` 有效 cookie
- [ ] 红果短剧：先探测 `protected_playback_unavailable`，再用已登录 cookie 重试
- [ ] B 站：`x/player/wbi/playurl`，需要 `wbi` 签名（纯算法，不用第三方库）
- [ ] HLS AES-128 本地解密（`AES-128` + IV）
- [ ] 任务持久化（`.resfetch/tasks.jsonl`，重启续跑）

## v0.3（壳）

- [ ] macOS：SwiftUI 菜单栏壳 + 分享面板接收链接
- [ ] Windows：WebView2 壳，注册 `resfetch://` 协议
- [ ] Android：WebView + `Intent.FILTER` 接收分享，OkHttp 替换 curl
- [ ] 三端统一"下载完成通知"

## 明确不做

- 指纹伪造、代理轮换、批量多账号 —— 这是对抗平台风控，会把用户的号玩死。
- 旧版 8.9 的结论也是这条：账号级风控只能靠冷却期和真实登录，不能靠技术绕。
