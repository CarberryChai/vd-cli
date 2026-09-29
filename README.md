# vd —— Bilibili 视频下载 CLI

给一个链接、BV 号或 av 号，得到一个可播放的 `.mp4`。

```bash
vd "https://www.bilibili.com/video/BV1qt4y1X7TW"
vd BV1qt4y1X7TW
vd av114514 -q 720p -o ~/Downloads
vd "https://space.bilibili.com/23630128/channel/collectiondetail?sid=2045"
```

需求规格见 [spec.md](spec.md)。范围以那份文档为准：只做 B 站、只做 WEB 取流、
不做番剧 / 课程 / 直播 / 收藏夹 / 断点续传 / 并发下载。

## 安装

```bash
cargo build --release
# 产物：target/release/vd
```

需要 `ffmpeg` 在 `PATH` 上（混流用）。缺失时启动就会报错并给出安装命令：

| 平台 | 安装命令 |
|---|---|
| macOS | `brew install ffmpeg` |
| Debian / Ubuntu | `sudo apt install ffmpeg` |
| Fedora | `sudo dnf install ffmpeg` |
| Windows | `winget install Gyan.FFmpeg` |

用 `--no-mux` 可以完全跳过 ffmpeg（保留分离的 `.mp4` / `.m4a`）。

## 用法

```
vd <URL|ID> [OPTIONS]

-o, --output-dir <DIR>   输出目录 [默认: 当前目录]
-q, --quality <Q>        清晰度上限: max|1080p|720p|480p|360p [默认: max]
    --codec <C>          视频编码优先级: avc|hevc|av1 [默认: avc]
-p, --pages <SPEC>       分P选择: all|1|1,3,5|1-5 [默认: all]
    --cookie <STR>       B 站 Cookie 字符串
    --cookies-from-browser <B>   从本机浏览器读 Cookie [默认: auto]
                                 auto|chrome|chromium|brave|edge|vivaldi|opera
    --no-cookies-from-browser    不读浏览器 Cookie，只用未登录状态
    --browser-profile <NAME>     指定浏览器 profile（默认取最近活动的那个）
    --list-browsers      列出检测到的浏览器与可读状态
    --no-mux             保留分离的 .mp4/.m4a，不调用 ffmpeg
    --dry-run            只解析并打印将要下载的内容，不实际下载
    --json               以 JSON 输出结果，便于脚本处理
-v, --verbose            打印调试日志
    --fnval <N>          覆盖 DASH 请求掩码（默认 4048）
```

支持的输入形态：

| 输入 | 说明 |
|---|---|
| `BV1qt4y1X7TW` / `bv1qt4y1x7tw` | BV 号（前缀大小写不敏感，base58 部分大小写敏感） |
| `av114514` / `114514` | av 号 |
| `bilibili.com/video/BV...` | 完整链接（带任意查询串都行） |
| `b23.tv/xxx` | 短链，逐跳校验 host 后跟随 |
| `space.bilibili.com/{mid}/channel/collectiondetail?sid=` | 合集 |
| `space.bilibili.com/{mid}/channel/seriesdetail?sid=` | 系列 |
| `space.bilibili.com/{mid}/lists/{sid}?type=season\|series` | 合集 / 系列 |
| `bilibili.com/medialist/play/...?business=space_collection\|space_series&business_id=` | 合集 / 系列 |

### 输出路径

```
单视频        {output_dir}/{视频标题}.mp4
视频内分 P    {output_dir}/{视频标题}/[{分P序号}]{分P标题}.mp4
合集          {output_dir}/{合集标题}/[{合集内序号}]{视频标题}.mp4
```

序号按总条目数补零（10 个以内 1 位，100 个以内 2 位）。文件名会净化：替换
`\ / : * ? " < > |`、去掉控制字符、规避 Windows 保留名、按 UTF-8 字节截断到
200 字节（不切断多字节字符）。

### 退出码

| 码 | 含义 |
|---|---|
| 0 | 全部成功 |
| 1 | 通用失败 |
| 2 | 参数错误 |
| 3 | 网络错误 / 重试耗尽 / 触发风控 |
| 4 | 需要登录或 Cookie 失效 |
| 5 | 部分失败（合集场景：至少一个视频失败） |
| 130 | 用户中断（Ctrl+C） |

### 例子

```bash
# 只看要下载什么，不下载
vd --dry-run BV1qt4y1X7TW

# 脚本里用：JSON 输出
vd --json -q 720p BV1qt4y1X7TW

# 下载多分 P 视频的 1~3 分 P
vd -p 1-3 BV17x411w7KC

# 登录后拿 1080P（未登录时清晰度通常最高只有 480P）
vd --cookie "SESSDATA=xxx; bili_jct=yyy" -q 1080p BV1qt4y1X7TW

# 合集：串行下载全部视频
vd "https://space.bilibili.com/23630128/channel/collectiondetail?sid=2045"
```

### Cookie：解锁高清

未登录时清晰度通常只有 480P 左右。两种方式拿到登录态：

**方式一：直接从本机浏览器读**（默认开启，什么都不用加）

```bash
vd BV1qt4y1X7TW                                     # 默认就是 --cookies-from-browser auto
vd --cookies-from-browser chrome BV1qt4y1X7TW       # 指定浏览器（读不到就报错，不静默降级）
vd --browser-profile "Profile 1" BV1...             # 多账号时指定 profile
vd --no-cookies-from-browser BV1...                 # 明确不读浏览器
vd --list-browsers                                  # 看哪个浏览器能读、auto 会选谁
```

默认 `auto` 按 **chrome → edge → brave → vivaldi → chromium → opera** 的顺序挑第一个能读的。
读不到时**不会失败**，而是打一条警告并以未登录状态继续（清晰度通常最高 480P）——
这样刚装上去、还没配好权限的机器也能跑通。想让它读不到就报错，就显式写
`--cookies-from-browser chrome`。

profile 默认取**最近活动**的那个，也就是你平时在用的。

**方式二：手动传**

登录 B 站后打开任意页面，在开发者工具 Network 面板里找一个 `api.bilibili.com` 请求，
复制请求头 `Cookie` 的完整值：

```bash
vd --cookie "SESSDATA=xxx; bili_jct=yyy" -q 1080p BV1qt4y1X7TW
```

`--cookie` 与 `--cookies-from-browser`/`--no-cookies-from-browser` 不能同时用
（会直接被参数解析拒绝）。

#### macOS：需要「完全磁盘访问」

macOS 会保护浏览器数据目录，没有权限时报 `Operation not permitted`。开启方式：

```
系统设置 → 隐私与安全性 → 完全磁盘访问权限 → 打开开关并勾选你的终端 →
完全退出终端（⌘Q）后重新打开
```

在 Codex / ChatGPT 里运行时，要授权的是**启动它的那个 App**，不是终端。

解密 Cookie 还需要访问钥匙串里的 `<浏览器> Safe Storage` 条目，第一次会弹授权框，
选「允许」即可。授权不了也可以用 `--cookie` 退回手动方式。

Firefox 与 Safari 暂不支持自动读取（前者加密方式不同，后者是受保护的二进制格式），
用 `--cookie` 传即可。

## 工作原理

```
resolve(输入) → Target{Video|List}
   ↓
view(aid) / medialist(biz_id)  →  VideoInfo / ListInfo（标题 + 分 P 列表）
   ↓
按 -p 过滤；初始化 WBI 密钥（一次，全局复用）
   ↓
对每个分 P：
  playurl(qn=0)    → 拿到可用 DASH 轨道
  playurl(qn=127)  → 尝试拿"免二压"原始码率（失败则沿用上一轮）
  select()         → 按清晰度上限 + 编码偏好选一条视频轨 + 一条音频轨
  download()       → 流式写入 .part，校验字节数后改名
  mux()            → ffmpeg -c copy 混流成 mp4
```

一些实现上的取舍：

- **浏览器 Cookie**：Chromium 系把值用 `v10` 前缀的 AES-128-CBC 加密，密钥由
  PBKDF2-SHA1(1003 轮, salt `"saltysalt"`) 从钥匙串口令派生；读取前先把库复制一份，
  避免和运行中的浏览器争锁。
- **`buvid3` 前端指纹**：B 站的取流接口要求请求带这个 cookie，缺了会拿到
  `code: 0` + 只有 `data.v_voucher` 的风控响应（实测约 4/5 被拦）。程序启动时
  先问 `x/frontend/finger/spi` 要一个，失败就本地按同样格式生成；用户自己在
  `--cookie` 里带了 `buvid3` 就用用户的。
- **UA 必须在 WAF 白名单里**：UA 池里的每条都实测过。同一个 Linux 平台，
  `Chrome/125` 会被 100% 拦截（返回 `v_voucher`），`Chrome/126` 及以上则全部通过，
  所以池子里都是 `Chrome/140`。改这个列表前请先实测。
- **WBI 签名**：参数按构造顺序签名，不排序。未登录时 `nav` 返回 `code: -101` 但
  仍然带 `wbi_img`，密钥必须在判断登录状态**之前**提取。
- **两轮取流**：第二轮的任何失败（拒绝、超时、解析失败、`dash.video` 为空）都只
  warn 并沿用第一轮结果；但用户取消必须向上传播。
- **URL 候选**：每个轨道保留 `base_url` + `backup_url[]`，过滤掉 PCDN 字面量 IP
  节点，下载失败时按顺序换源。
- **媒体请求禁止跟随重定向**：这些请求带着 Cookie，3xx 可以把凭据导向任意主机。
- **`Referer` 不能省**：B 站 CDN 会校验来源，缺失时可能返回 403。
- **UA 进程内固定**：同一进程里不同请求用不同 UA 是最明显的爬虫特征。
- **集合串行下载**：并发会显著提高触发风控（`-412`）的概率。

## 排查「触发风控」

如果遇到 `error: 触发风控`，按顺序检查：

1. **UA 是否被拦**：这类拦截的响应是 `code: 0` + `data.v_voucher`。跑
   `vd -v --dry-run <视频>` 看 debug 日志里有没有「被风控拦截（data 里只有
   v_voucher）」。若是，说明当前 UA 被 WAF 判定为爬虫，升级 `USER_AGENTS` 里的
   Chrome 版本号即可（见上）。真正的限流（`-412`）是另一回事，等几分钟再跑。
2. **是否短时间内请求太多**：连续对多个视频取值流会累积风控分。等几分钟，
   或换网络。
3. **是否真的是登录墙**：如果错误是「需要登录」而不是「触发风控」，加
   `--cookie`。未登录时清晰度上限只有 480P 左右。

## 开发

```bash
cargo test                                     # 单元 + 契约测试（无网络）
cargo test --test manual_e2e -- --ignored --nocapture   # 真实网络端到端（需 ffmpeg）
```

测试分三层：

- **单元测试**：`bv.rs`、`wbi.rs`、`select.rs`、`path.rs`、`cli.rs` 都是纯函数，
  无网络。
- **契约测试**：用 `wiremock` 起本地服务器，把 `playurl` 的 `nav` / `view` /
  `medialist` 响应换成 `tests/fixtures/` 里裁剪好的真实响应，断言解析、错误映射
  与翻页边界（游标不推进、`attr != 0`、空页）。
- **端到端**：`tests/cli_e2e.rs` 跑真正的二进制对着 mock 服务器，断言退出码与
  `--json` 输出；`tests/manual_e2e.rs` 打真实 B 站并校验 `ffprobe` 能读出轨道。

### 模块结构

```
src/
├── main.rs          # 入口，串起整个流程
├── cli.rs           # clap 定义 + Args
├── error.rs         # thiserror 错误枚举 + 退出码 / 提示映射
├── bv.rs            # BV 号 ↔ av 号          （纯函数）
├── wbi.rs           # WBI 密钥派生 + 签名    （纯函数）
├── resolve.rs       # 输入 → Target          （纯函数 + b23.tv 跟随）
├── browser.rs       # 从本机浏览器读 Cookie  （读库 + 解密）
├── buvid.rs         # buvid3 指纹            （纯函数）
├── model.rs         # Page / Track / 清晰度表
├── select.rs        # 轨道选择               （纯函数）
├── path.rs          # 输出路径 + 文件名净化  （纯函数）
├── download.rs      # 文件下载
├── mux.rs           # ffmpeg 混流
└── api/
    ├── mod.rs       # 共享 HTTP 客户端 / JSON 辅助
    ├── view.rs      # 视频信息
    ├── list.rs      # 合集 / 系列列表
    └── playurl.rs   # 取流
```

调用方向是单向的：`main → resolve → api → model/select → download → mux`。

## 已知限制

- 未登录时清晰度受限（通常最高 480P）——这是 B 站的服务端限制，不是 bug。
  加 `--cookie` 解锁。
- 番剧 / 课程 / 直播 / 互动视频、收藏夹、UP 主全部视频都不支持（见 spec §1.2）。
- 遇到 DRM（`is_drm = true`）或只有老 FLV 分段的视频直接报错退出。
- 没有断点续传、多线程分片、限速、批量并发——单连接顺序下载已够跑满普通带宽。
- 触发风控（`-412`）时请稍后重试，不要立刻重跑。
