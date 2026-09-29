# spec.md — `vd`：Bilibili 视频下载 CLI（MVP）

## 1. 目标与范围

### 1.1 MVP 要做什么

一条命令下载 B 站视频：

```bash
vd "https://www.bilibili.com/video/BV1qt4y1X7TW"
vd BV1qt4y1X7TW
vd "https://space.bilibili.com/23630128/channel/collectiondetail?sid=2045"
```

| # | 能力 | 说明 |
|---|---|---|
| 1 | 单视频 | 输入 BV 号 / av 号 / 完整链接 / `b23.tv` 短链 |
| 2 | 视频内分 P | 一个视频可能有多个分 P，默认全部下载，可用 `-p` 挑选 |
| 3 | 合集 / 系列 | 输入合集或系列的链接，批量下载其中全部视频 |
| 4 | DASH 取流 | 视频轨、音频轨分别下载，用 `ffmpeg` 混流成一个 mp4 |
| 5 | 清晰度选择 | `--quality` 指定上限，`--codec` 指定编码优先级 |
| 6 | Cookie | `--cookie` 传入，用于解锁登录后才能拿到的高清档位 |
| 7 | 进度显示 | 每个视频一条进度条：百分比 / 速度 / ETA |

**一条命令的完整效果**：给一个链接，得到一个可播放的 `.mp4` 文件，文件名是视频标题。

### 1.2 MVP 明确不做

以下都是有意排除的，不是遗漏。每条都附了理由，写代码时不要顺手加。

| 不做 | 理由 |
|---|---|
| 番剧 / 课程 / 直播 / 互动视频 | 各自需要独立的接口与鉴权分支，工作量与主流程相当 |
| 收藏夹 / UP 主全部视频 | 与合集相比只是换一个列表接口，但会引入分页与权限的额外组合 |
| YouTube 或其它站点 | MVP 只验证一条链路。抽象成 `trait` 是后续重构，现在不提前设计 |
| DRM / Widevine 解密 | 法律风险。遇到 `is_drm = true` 直接报错退出 |
| 弹幕、字幕、封面嵌入、章节 | 属于后处理增强，不影响"能下载到视频"这个核心目标 |
| 断点续传、多线程分片、限速 | 单连接顺序下载已能跑满普通带宽。这些是性能优化，应该在有真实痛点之后再做 |
| 批量并发下载 | 合集默认串行下载。并发会显著提高触发风控的概率 |
| 扫码登录 | `--cookie` 足够了。扫码登录需要二维码渲染与轮询状态机，成本不低 |
| 配置文件 | 参数都在命令行上，参数集小于 10 个时配置文件是负担 |
| 断点续传、缓存、档案（archive） | 同上，等有反复下载同一批内容的需求再说 |

---

## 2. 技术选型

保持在能用的最少量：

| 用途 | crate | 说明 |
|---|---|---|
| 异步运行时 | `tokio`（`rt-multi-thread`, `fs`, `macros`, `signal`） | |
| HTTP | `reqwest`（默认 TLS 或 `rustls-tls`） | 都要提交与读取响应体 |
| CLI | `clap` v4（derive） | |
| 序列化 | `serde` + `serde_json` | |
| 错误 | `anyhow` + `thiserror` | 内部用 `thiserror` 定义错误枚举，`main` 用 `anyhow` |
| 进度条 | `indicatif` | |
| 日志 | `tracing` + `tracing-subscriber` | |
| 摘要 | `md-5` + `hex` | WBI 签名 |
| 时间 | `anyhow` 不用，签名时间戳直接 `std::time::SystemTime` | 避免多引入一个 crate |

外部依赖只有 `ffmpeg`（混流）。启动时用 `which` 探测，缺失则报错并给出安装提示。

---

## 3. CLI

```
vd <URL|ID> [OPTIONS]

参数:
  <URL|ID>  视频链接、BV 号、av 号，或合集/系列链接

选项:
  -o, --output-dir <DIR>   输出目录 [默认: 当前目录]
  -q, --quality <Q>        清晰度上限: max|1080p|720p|480p|360p [默认: max]
      --codec <C>          视频编码优先级: avc|hevc|av1 [默认: avc]
  -p, --pages <SPEC>       分P选择: all|1|1,3,5|1-5 [默认: all]
      --cookie <STR>       B 站 Cookie 字符串
      --no-mux             保留分离的 .mp4/.m4a，不调用 ffmpeg
      --dry-run            只解析并打印将要下载的内容，不实际下载
      --json               以 JSON 输出结果，便于脚本处理
  -v, --verbose            打印调试日志
  -h, --help
  -V, --version
```

**退出码**

| 码 | 含义 |
|---|---|
| 0 | 全部成功 |
| 1 | 通用失败 |
| 2 | 参数错误 |
| 3 | 网络错误 / 重试耗尽 |
| 4 | 需要登录或 Cookie 失效 |
| 5 | 部分失败（合集场景：至少一个视频失败） |
| 130 | 用户中断（Ctrl+C） |

---

## 4. 项目结构

```
vd-cli/
├── Cargo.toml
├── spec.md
└── src/
    ├── main.rs          # 入口，串起整个流程
    ├── cli.rs           # clap 定义 + Args 结构体
    ├── error.rs         # thiserror 错误枚举
    ├── bv.rs            # BV 号 ↔ av 号
    ├── wbi.rs           # WBI 密钥派生 + 签名
    ├── resolve.rs       # 输入 → 内部标识
    ├── api/
    │   ├── mod.rs       # 共享的 HTTP 客户端与 JSON 辅助
    │   ├── view.rs      # 视频信息
    │   ├── list.rs      # 合集 / 系列列表
    │   └── playurl.rs   # 取流
    ├── model.rs         # Page / Track / VideoInfo 等结构体
    ├── select.rs        # 轨道选择
    ├── download.rs      # 文件下载
    └── mux.rs           # ffmpeg 混流
```

模块之间的调用是单向的：`main → resolve → api → model/select → download → mux`。
`bv.rs`、`wbi.rs`、`select.rs` 是纯函数，不依赖网络，可以单独测试。

---

## 5. 处理流程

```
main
 ├─ 1. 解析参数，探测 ffmpeg
 ├─ 2. resolve(input) ────────────► Target
 │      单视频 ──► Target::Video { aid }
 │      合集   ──► Target::List { biz_id, list_type }
 ├─ 3. 取任务列表
 │      单视频 ──► view(aid) ─────► 视频标题 + Vec<Page{ aid, cid, title }>
 │      合集   ──► list(biz_id) ──► 合集标题 + Vec<Page{ aid, cid, title, upper }>
 ├─ 4. 按 -p 过滤分 P
 ├─ 5. 初始化 WBI 密钥（取一次，全局复用）
 ├─ 6. 对每个 Page：
 │      a. playurl(aid, cid, qn=0) ────► 解析 DASH 轨道
 │      b. playurl(aid, cid, qn=127) ──► 重发拿免二压（失败则沿用上一轮）
 │      c. select(tracks, prefs) ─────► 选中一条视频轨 + 一条音频轨
 │      d. download(视频轨 URL) ──────► {aid}.mp4
 │      e. download(音频轨 URL) ──────► {aid}.m4a
 │      f. mux(video, audio) ─────────► 最终 {title}.mp4
 ├─ 7. 汇总：失败的 Page 列表 → 退出码
```

---

## 6. 输入解析（`resolve.rs`）

### 6.1 内部标识

```rust
pub enum Target {
    /// 单个视频
    Video { aid: u64 },
    /// 合集或系列
    List { biz_id: u64, kind: ListKind },
}

pub enum ListKind {
    Collection,  // 合集，接口 type=8
    Series,      // 系列，接口 type=5
}
```

MVP 只保留这三个变体。不做 `ep:` / `cheese:` / `mid:` 这些前缀体系——它们服务于番剧课程空间等已排除的能力。

### 6.2 识别规则

按顺序匹配，命中即返回：

| 输入形态 | 结果 |
|---|---|
| 含 `bilibili.com/video/BV` 或 `bilibili.com/video/av` | 提取出 BV/av 号，转到下一行 |
| 以 `BV` 开头（11 位）| `Target::Video { aid: bvid_to_aid(bv)? }` |
| 以 `av` 或纯数字开头 | `Target::Video { aid }` |
| `space.bilibili.com/{mid}/channel/collectiondetail?sid={sid}` | `List { sid, Collection }` |
| `space.bilibili.com/{mid}/channel/seriesdetail?sid={sid}` | `List { sid, Series }` |
| `space.bilibili.com/{mid}/lists/{sid}?type=season` | `List { sid, Collection }` |
| `space.bilibili.com/{mid}/lists/{sid}?type=series` | `List { sid, Series }` |
| `bilibili.com/medialist/play/...business=space_collection&business_id={id}` | `List { id, Collection }` |
| `bilibili.com/medialist/play/...business=space_series&business_id={id}` | `List { id, Series }` |
| host 恰为 `b23.tv` | 跟随重定向取 `Location`，拿结果回到上表重试一次 |
| 其它 | 报错 `输入无法识别` |

匹配顺序上，**合集规则必须在 `space.bilibili.com/{mid}` 规则之前**，否则 `space.bilibili.com/123/channel/collectiondetail?sid=456` 会被当成 UP 主空间处理。

### 6.3 b23.tv 短链

只接受 host 精确等于 `b23.tv` 的输入。

```rust
let ok = url.host_str() == Some("b23.tv");
```

不要用 `contains("b23.tv")`——`evilb23.tv` 和 `b23.tv.evil.com` 都会命中，导致向攻击者的服务器发请求。

跟随重定向时**逐跳校验** host 必须属于 `bilibili.com` / `b23.tv`。只校验第一跳是不够的，可信域名的开放重定向可以把请求导向内网。

---

## 7. Bilibili 接口

### 7.1 WBI 签名（`wbi.rs`）

B 站的 `x/player/wbi/*` 系列接口要求查询串带签名。启动时取一次密钥，整个进程复用。

**密钥派生**

```rust
const MIXIN_KEY_ENC_TAB: [usize; 32] = [
    46, 47, 18,  2, 53,  8, 23, 32, 15, 50, 10, 31, 58,  3, 45, 35,
    27, 43,  5, 49, 33,  9, 42, 19, 29, 28, 14, 39, 12, 38, 41, 13,
];

/// 从 wbi_img 的两个 URL 里取文件名，拼接后按上面的表重排，取 32 位。
pub fn mixin_key(img_url: &str, sub_url: &str) -> Result<String, WbiError> {
    let orig = format!("{}{}", file_stem(img_url), file_stem(sub_url));
    // 表中最大索引是 58，所以长度必须 >= 59。写成 58 会在 orig[58] 处越界。
    if orig.len() < 59 {
        return Err(WbiError::KeyMaterialTooShort(orig.len()));
    }
    let bytes = orig.as_bytes();
    Ok(MIXIN_KEY_ENC_TAB.iter().map(|&i| bytes[i] as char).collect())
}

/// 取 URL 中最后一个 '/' 之后、最后一个 '.' 之前的部分。
fn file_stem(url: &str) -> &str {
    let s = url.rsplit('/').next().unwrap_or("");
    s.split('.').next().unwrap_or(s)
}
```

**签名**

```rust
pub fn sign(query: &str, mixin_key: &str) -> String {
    let w_rid = hex::encode(md5::Md5::digest(format!("{query}{mixin_key}").as_bytes()));
    format!("{query}&w_rid={w_rid}")
}
```

两个容易写错的点：

1. **签名前不要对参数排序**。输入就是原始查询串（不含 `?`）拼上 `mixin_key`，保持构造顺序。
2. **结果是 32 位小写十六进制**。`hex::encode` 默认就是小写，不要用 `to_uppercase`。

**获取密钥**

```
GET https://api.bilibili.com/x/web-interface/nav
```

从 `data.wbi_img.img_url` 与 `data.wbi_img.sub_url` 取。

⚠️ **未登录时这个接口返回 `code: -101`，但依然包含 `wbi_img`。** 必须在判断登录状态**之前**提取密钥，否则未登录用户拿不到密钥，后续所有签名请求都会被 `-352` 拒绝。这是个很容易踩的坑。

**时间戳**

参数里的 `wts` 是当前 Unix 秒。如果响应头里有 `Date`，可以用它与本地时间的差值修正后续时间戳——B 站签名有效期约 60 秒，容器或虚拟机时钟漂移会直接导致 `-352`。MVP 可以先用纯本地时间，遇到 `-352` 时把「检查系统时间」写进错误提示。

### 7.2 视频信息（`api/view.rs`）

```
GET https://api.bilibili.com/x/web-interface/view?aid={aid}
```

用到的字段：

| JSON 路径 | 用途 |
|---|---|
| `data.title` | 文件名 |
| `data.owner.name` | 元数据（MVP 仅打印） |
| `data.duration` | 校验用 |
| `data.pages[]` | **分 P 列表，核心字段** |

`pages[]` 每一项：

| 字段 | 说明 |
|---|---|
| `page` | 分 P 序号，从 1 开始 |
| `cid` | **取流必需的 ID** |
| `part` | 分 P 标题，需 `.trim()` |
| `duration` | 秒 |

映射为：

```rust
pub struct Page {
    pub index: u32,        // page
    pub aid: u64,
    pub cid: u64,
    pub title: String,     // part，单 P 时用视频标题
    pub duration: u32,
}
```

**单 P 视频的 `pages[0].part` 通常是无意义的占位符**（如 `"P1"`，有时甚至和视频标题相同）。当 `pages.len() == 1` 时，用 `data.title` 作为 `Page::title` 以得到合理的文件名。

**错误处理**：先判断 `code != 0`，再取 `data`。顺序反了的话，错误响应里没有 `data` 键，会抛出与真实原因无关的"字段不存在"错误。`code == -404` 意味着视频不存在或已删除。

### 7.3 合集 / 系列列表（`api/list.rs`）

两个接口，只有 `type` 参数不同（合集 `type=8`，系列 `type=5`）：

**取合集元信息**

```
GET https://api.bilibili.com/x/v1/medialist/info?type={8|5}&biz_id={sid}&tid=0
```

返回 `data.title`（合集名）、`data.intro`、`data.ctime`。

**取视频列表（分页）**

```
GET https://api.bilibili.com/x/v2/medialist/resource/list
      ?type={8|5}
      &oid={游标}
      &otype=2
      &biz_id={sid}
      &with_current=true
      &mobi_app=web
      &ps=20
      &direction=false
      &sort_field=1
      &tid=0
```

翻页逻辑（**三个必须处理的边界**）：

```rust
let mut cursor = String::new();
let mut seen = HashSet::new();
loop {
    let resp = fetch_list(/* oid: cursor */).await?;
    let data = resp["data"].as_object().ok_or(...)?;

    let prev_cursor = cursor.clone();
    for item in data["media_list"].as_array().unwrap_or(&vec![]) {
        // 游标永远取本页最后一条的 id，无论该条是否被跳过
        cursor = item["id"].as_str().unwrap_or("").to_string();

        // attr != 0 表示条目已失效（稿件删除/私密），跳过
        if item["attr"].as_i64().unwrap_or(0) != 0 { continue; }

        for page in item["pages"].as_array().unwrap_or(&vec![]) {
            // 去重后插入
        }
    }

    if !data["has_more"].as_bool().unwrap_or(false) { break; }

    // 兜底：接口说还有下一页，但游标没动 —— 再请求也是同一页，必须退出
    if cursor == prev_cursor {
        tracing::debug!("游标未推进，停止翻页");
        break;
    }
}
```

三个边界缺一不可：

1. **游标必须记录本页最后一条的 `id`**，哪怕这条被 `attr` 跳过了。否则整页失效时游标不前进，会无限循环请求同一页。
2. **`attr != 0` 的条目必须跳过**，它们是已删除或私密的稿件，拿去请求 `playurl` 只会得到错误。
3. **空页 / 游标未推进时必须 `break`**，这是对死循环的最后一道防线。

列表里每条 `media_list[]` 的字段：

| 字段 | 说明 |
|---|---|
| `id` | 视频 avid（也是翻页游标） |
| `title` | 视频标题 |
| `page` | 该视频的分 P 数 |
| `pages[].id` | cid |
| `pages[].page` | 分 P 序号 |
| `pages[].title` | 分 P 标题 |
| `pages[].duration` | 秒 |
| `upper.name` / `upper.mid` | UP 主信息 |
| `cover` / `intro` / `pubtime` | 元数据 |

**这里已经拿到了 `cid`，所以合集场景不需要再对每个视频调用一次 `view` 接口。** 直接拿 `(aid, cid)` 去请求 `playurl` 即可，能省掉 N 次请求。

分 P 的标题拼接规则（对齐 `page == 1` 的判断）：

```rust
let title = if item_page_count == 1 {
    item_title.clone()                          // 单 P：用视频标题
} else {
    format!("{item_title}_P{page}_P_title")     // 多 P：拼上分 P 信息
};
```

合集内的下载顺序：按接口返回顺序串行下载。这是刻意的——并发会显著提高触发风控的概率。

### 7.4 取流（`api/playurl.rs`）

MVP 只实现 WEB 一种模式。TV / APP(gRPC) / 国际版留给后续。

**端点**

```
GET https://api.bilibili.com/x/player/wbi/playurl
```

**参数拼装**（顺序即签名顺序，不要重排）

```
support_multi_audio=true
&from_client=BROWSER
&avid={aid}
&cid={cid}
&fnval=4048            # 位掩码：请求 DASH（含 AV1 / 杜比视界 / Hi-Res 位）
&fnver=0
&fourk=1               # 允许 4K
&otype=json
&qn={qn}               # 见下方两轮请求
&wts={unix_seconds}
# 无 Cookie 且非 DRM 时追加：&try_look=1
# 整体过 WBI 签名后追加：&w_rid={32位小写十六进制}
```

**两轮请求**

| 轮次 | `qn` | 目的 |
|---|---|---|
| 第一轮 | `0` | 快速拿到可用轨道与可用清晰度列表 |
| 第二轮 | `127` | 请求最高画质，触发"免二压"返回原始码率版本 |

第二轮的规则：

- 响应**包含非空的 `dash.video`** → 用第二轮的结果**整体替换**第一轮的轨道列表。
- 响应被拒绝、超时、解析失败、或 `dash.video` 为空 → 沿用第一轮结果，打一条 warn 继续，**不要因此让整个视频失败**。
- 第二轮失败**不得吞掉用户取消**。如果 `CancellationToken` 已取消，必须直接向上传播，否则用户按 Ctrl+C 后流程还会继续跑。

### 7.5 DASH 解析

响应结构在不同接口版本下位置不同，按下面的顺序定位根节点：

```
若 resp.result 是对象 → root = resp.result.video_info ?? resp.result
否则若 resp.data 存在 → root = resp.data
否则 root = resp
```

**解析前必须先做两层校验**，否则会静默解析出零轨道，直到下载阶段才以难以定位的错误失败：

```rust
/// 顶层业务码
fn check_biz_code(root: &Value) -> Result<()> {
    let code = root["code"].as_i64().unwrap_or(0);
    if code == 0 { return Ok(()); }
    let msg = root["message"].as_str().unwrap_or("未知错误");
    Err(match code {
        -404  => Error::NotFound,
        -403  => Error::Forbidden,
        -412  => Error::RiskControl,
        -101  => Error::NeedLogin,
        -10403 => Error::VipRequired,
        -86038 => Error::RegionRestricted,
        _     => Error::Api { code, message: msg.into() },
    })
}

/// 播放限制（仅番剧响应有，UGC 可跳过但保留防御）
fn check_play_limit(root: &Value) -> Result<()> { /* play_check.limit_play_reason */ }
```

**视频轨**（`root.dash.video[]`）：

| JSON | 字段 |
|---|---|
| `id` | 清晰度码（见 §7.6 映射表） |
| `base_url` | 主地址 |
| `backup_url[]` | 备用 CDN 地址 |
| `codecid` | `"7"`→AVC，`"12"`→HEVC，`"13"`→AV1 |
| `bandwidth` | 除以 1000 得到 kbps |
| `width` / `height` / `frame_rate` | 分辨率与帧率 |
| `size` | 字节数 |

**音频轨**（`root.dash.audio[]`）：同上的 `id` / `base_url` / `backup_url` / `bandwidth`，外加 `codecs`。`codecs` 需要归一化，否则后面选轨会匹配不上：

| 原始值 | 归一化 |
|---|---|
| `mp4a.40.2` / `mp4a.40.5` | `M4A` |
| `ec-3` | `E-AC-3` |
| `fLaC` | `FLAC` |

**URL 候选列表的构造**：

```rust
fn collect_urls(node: &Value) -> Vec<String> {
    let mut urls = vec![];
    if let Some(u) = node["base_url"].as_str() { urls.push(u.to_string()); }
    if let Some(arr) = node["backup_url"].as_array() {
        urls.extend(arr.iter().filter_map(|v| v.as_str().map(String::from)));
    }
    // 过滤掉形如 http://1.2.3.4:8080/... 的 PCDN 节点，可用性差
    let filtered: Vec<String> = urls.iter()
        .filter(|u| !is_host_port_literal(u))
        .cloned().collect();
    if filtered.is_empty() { urls } else { filtered }
}
```

`is_host_port_literal` 用正则 `^https?://[^/:]+:\d+` 判断。过滤后若为空，回退到原始列表——宁可试一个差的也不要不试。

**保留全部候选 URL**，下载失败时按顺序换源，这是最有效的容错手段。

**其它轨道**：

- `root.dash.dolby.audio` 和 `root.dash.flac.audio` 若存在，追加到音频轨列表。它们可能只有一个对象（不是数组），也可能缺失，都要处理。
- `root.dash.duration` 是时长（秒）；缺失时回退到 `root.timelength / 1000`。
- `root.dural[]` 是老视频的 FLV 分段格式。MVP **遇到就直接报错退出**并提示「该视频不支持（老格式）」，不要尝试实现 FLV 拼接。

### 7.6 清晰度映射

```rust
pub const QUALITY_MAP: &[(&str, &str, u32)] = &[
    // (qn 码, 显示名, 排序权重)
    ("127", "8K 超高清",    80),
    ("126", "杜比视界",      75),
    ("125", "HDR 真彩",      70),
    ("120", "4K 超清",       60),
    ("116", "1080P 高帧率",  55),
    ("112", "1080P 高码率",  50),
    ("100", "智能修复",      45),
    ("80",  "1080P 高清",    40),
    ("74",  "720P 高帧率",   35),
    ("64",  "720P 高清",     30),
    ("48",  "720P 高清",     30),
    ("32",  "480P 清晰",     20),
    ("16",  "360P 流畅",     10),
    ("6",   "240P 流畅",      5),
    ("5",   "144P 流畅",      5),
];
```

`--quality` 到 qn 的映射：

| `--quality` | 权重上限 |
|---|---|
| `max` | 不限 |
| `1080p` | 55（允许 1080P 高帧率 / 高码率 / 高清） |
| `720p` | 35 |
| `480p` | 20 |
| `360p` | 10 |

**`--quality` 是上限而不是精确匹配**：源里没有对应档位时降级到可用的最高档位并打 warn，而不是报错。

### 7.7 错误码速查

| code | 含义 | 退出码 |
|---|---|---|
| `0` | 成功 | — |
| `-101` | 未登录 | 4 |
| `-352` | 签名校验失败 | 1 |
| `-404` | 视频不存在 | 1 |
| `-403` | 权限不足 | 4 |
| `-412` | 触发风控 | 3 |
| `-86038` | 区域限制 | 1 |
| `-10403` | 大会员专享 | 4 |

`-352` 的错误提示必须同时给出两个可能原因：

```
error: 接口签名校验失败
  → 请检查系统时间是否准确（签名有效期约 60 秒）
  → 或稍后重试
```

---

## 8. 轨道选择（`select.rs`）

纯函数，输入轨道列表与偏好，输出一对选中的轨道。

```rust
pub fn select_video(tracks: &[VideoTrack], prefs: &Prefs) -> Option<&VideoTrack>;
pub fn select_audio(tracks: &[AudioTrack], prefs: &Prefs) -> Option<&AudioTrack>;
```

**排序键（从高到低）**

视频：
1. 清晰度权重（§7.6），且必须 ≤ `--quality` 上限
2. 编码优先级（`--codec` 指定的那一种优先，其它排后面）
3. 码率（降序）

音频：
1. `FLAC` > `E-AC-3` > `M4A`
2. 码率（降序）

**降级链**：先按「清晰度上限 + 编码偏好」筛选；若无结果，放宽编码偏好再筛一次，打 warn；仍无结果则报错 `没有可用的视频流`。

默认不询问用户。MVP 不做交互式选择——CLI 工具要能在脚本里跑。

---

## 9. 下载（`download.rs`）

单连接顺序下载，够用且简单。

```
1. HEAD {url}  →  Content-Length
   若 HEAD 返回 405/403/501，退化为 GET 且不带 Range，边收边写

2. GET {url}，流式写入 {name}.part

3. 关闭文件，校验实际字节数 == Content-Length
   不等 → 删除 .part，报错（绝不产出长度不对的文件）

4. rename {name}.part → {name}
```

**必须带的请求头**

```rust
req.header("Referer", "https://www.bilibili.com/")
req.header("User-Agent", UA)   // 与 API 请求用同一个 UA，进程内固定
req.header("Cookie", cookie)   // 若提供了 Cookie
```

`Referer` 不能省。B 站 CDN 会校验来源，缺失时可能返回 403。

**UA 在进程内固定**。同一进程里不同请求用不同 UA 是最明显的爬虫特征。启动时随机挑一个并在整个运行期间复用。

**媒体请求禁止自动跟随重定向**。这些请求带着 Cookie，3xx 可以把凭据导向任意主机。`reqwest` 的 redirect policy 设为 `Policy::none()`，遇到 3xx 直接报错。

**超时**：Connect 10s，整体无进度 30s（`read_timeout`）。不要设一个全局的短超时——大文件下载本来就要几分钟。

**进度条**：`indicatif`，显示已下载/总大小、速度、ETA。非 TTY（管道、CI）时自动降级为纯文本或不输出。

**重试**：网络错误重试 3 次，退避 1s / 2s / 4s。4xx 不重试（除 429，按 `Retry-After` 等待）。

---

## 10. 混流（`mux.rs`）

```bash
ffmpeg -hide_banner -loglevel error \
  -i {aid}.mp4 -i {aid}.m4a \
  -map 0:v:0 -map 1:a:0 \
  -c copy \
  -movflags +faststart \
  -metadata title="{视频标题}" -metadata artist="{UP主}" \
  "{输出路径}.mp4"
```

要点：

- **`-c copy`**，不重编码。这是下载器，不是转码器。
- `-movflags +faststart` 把 moov box 前置，让文件可以边下边播。
- `-map 0:v:0 -map 1:a:0` 显式指定轨道，避免 ffmpeg 自己挑错。
- **元数据值必须净化**：去掉换行符。标题里含 `\n` 可以注入伪造的元数据行。规则：先替换 `\n`/`\r` 为空格，再截断到合理长度（如 200 字符）。
- 超时 30 分钟，到点 `kill`。损坏的输入会让 ffmpeg 挂死。
- ffmpeg 缺失时报 `EX_FFMPEG` 并给出针对当前平台的安装命令，而不是让它以"文件找不到"的形式冒出来。

混流成功后删除中间的 `.mp4` / `.m4a`，除非指定了 `--no-mux`。

---

## 11. 输出路径

**单视频**

```
{output_dir}/{视频标题}.mp4
```

**分 P（`pages.len() > 1`）**

```
{output_dir}/{视频标题}/[{序号}]{分P标题}.mp4
```

**合集**

```
{output_dir}/{合集标题}/[{序号}]{视频标题}.mp4
```

序号补零，宽度取总条目数的位数（10 个以内补 1 位，100 个以内补 2 位，以此类推）。

**文件名净化（必做）**——标题完全由服务端控制，必须处理：

```rust
fn sanitize(segment: &str) -> String {
    // 1. 替换非法字符：\ / : * ? " < > |
    // 2. 去掉控制字符（含 \n \r \t 与 Unicode 控制类）
    // 3. 拒绝 "." 和 ".."
    // 4. Windows 保留名（CON PRN AUX NUL COM1-9 LPT1-9）前后加下划线
    // 5. 去掉结尾的 '.' 和空格（Windows 不允许）
    // 6. 按 UTF-8 字节数截断到 200 字节以内，注意不要切断多字节字符
    // 7. 若结果为空的，用 "untitled" 兜底
}
```

**按字节截断而不是按字符**：文件系统的限制是 255 字节，中文一个字占 3 字节，按字符截断会超限。同时要保证不在 UTF-8 字符中间切断。

---

## 12. 测试

### 12.1 单元测试（无网络）

`bv.rs`、`wbi.rs`、`select.rs`、文件名净化是纯函数，必须全覆盖：

| 模块 | 用例 |
|---|---|
| BV ↔ av | 已知向量（`BV1qt4y1X7TW`）；往返一致；非法长度；非法字符；空串 |
| WBI 签名 | 固定 query + 固定 key → 固定 `_w_rid`；确认输出是小写十六进制；确认不会重排参数 |
| mixin_key | 标准向量；长度 58 时返回错误而不是 panic |
| 轨道选择 | 各 `--quality` 上限语义；没有匹配时降级；空列表 |
| 文件名净化 | `../`、绝对路径、控制字符、Windows 保留名、超长中文标题、全非法字符 |
| 分 P 过滤 | `all`、单个、逗号列表、范围、越界索引 |

### 12.2 契约测试（mock HTTP）

用 `wiremock` 或 `httpmock` 起本地服务器，把 API 的 base URL 做成可注入的常量。

必须覆盖的 fixture：

- `nav` 未登录（`code: -101` 且含 `wbi_img`）→ 断言密钥被正确提取
- `view` 单 P → 断言用视频标题当文件名
- `view` 多 P → 断言分 P 列表正确
- `view` `code: -404` → 断言映射为 `NotFound`
- `playurl` 完整 DASH 响应 → 断言轨道数量、base_url 与 backup_url 都被收集
- `playurl` 只有 `durl` → 断言报"老格式不支持"
- `playurl` `message` 含限制文案 → 断言映射为对应错误
- `playurl` 响应是 HTML（风控页）→ 断言报可读错误而不是 JSON 解析失败
- 合集列表两页 + `has_more` + 游标未推进 → 断言不会死循环

fixture 用真实的响应裁剪后存进 `tests/fixtures/`。

### 12.3 端到端

`#[ignore]` 标记的手动测试：对一个长期稳定的公开 BV 视频跑完整流程，断言产出的 mp4 用 `ffprobe` 能读出正确的时长与轨道数。CI 上不跑。

---

## 13. 里程碑

| 阶段 | 内容 | 验收标准 |
|---|---|---|
| **M0** | 项目骨架、CLI 参数、`bv.rs`、`wbi.rs` | `vd --help` 可用；BV 转换与 WBI 签名单测通过 |
| **M1** | `view` + `playurl`（单轮）+ 轨道解析 + `--dry-run` | `vd --dry-run BV1xx` 能打印出视频标题、分 P 列表、可用清晰度与编码 |
| **M2** | 下载 + 混流 | 能下载一个公开视频得到可播放的 mp4 |
| **M3** | 分 P 下载 + `-p` 选择 + 目录结构 | 多分 P 视频能全部下载并放进同一目录 |
| **M4** | 合集 / 系列 + 翻页 | 输入合集链接能下载全部视频，退出码正确反映失败数 |
| **M5** | 第二轮 `qn=127` + `--cookie` + 错误信息打磨 | 登录后能拿到 1080P；所有错误都能给出人话提示与下一步 |

**每个里程碑都必须包含**：对应的测试、README 更新、以及把错误信息通读一遍确认「是不是人话、有没有告诉用户下一步」。

---

## 14. 已知风险

| # | 风险 | 缓解 |
|---|---|---|
| 1 | 未登录时清晰度受限（通常最高 480P） | 这是 B 站的服务端限制，不是 bug。错误信息里明确提示加 `--cookie` |
| 2 | `fnval=4048` 的位掩码语义若变更，可能拿不到某些档位 | `fnval` 定义为常量并加注释；保留 `--fnval` 覆盖开关 |
| 3 | WBI 派生规则变更 | 逻辑集中在 `wbi.rs` 一个文件，可快速替换 |
| 4 | 触发风控（`-412`） | 合集串行下载、UA 固定、失败退避；遇到 `-412` 明确提示"稍后重试"而不是立刻重试 |
| 5 | `b23.tv` 短链解析失败 | 只接受精确 host，逐跳校验；失败时提示用户直接用完整链接 |
| 6 | 部分视频只有老 FLV 格式 | MVP 明确报错退出，不做半吊子的支持 |

---

## 15. 后续（不在 MVP 内，仅供规划）

按预期收益排序：

1. **断点续传 + 多线程分片** —— 大文件下载失败的痛点最真实
2. **字幕下载** —— 接口简单（`x/player/wbi/v2`），收益明确
3. **封面下载与嵌入** —— 让输出文件更完整
4. **番剧 / 课程支持** —— 复用现有的 `playurl` 链路，主要是元数据接口不同
5. **扫码登录** —— 去掉 `--cookie` 的手工步骤
6. **TV / APP 接口** —— 某些内容只有 APP 接口能拿到最高画质
7. **抽成 `trait Extractor`** —— 只有在真的要加第二个站点时才做，不要提前抽象
