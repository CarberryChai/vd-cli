//! `medialist` 合集 / 系列：元信息与分页视频列表。

use std::collections::HashSet;

use serde_json::Value;

use super::{Api, check_biz_code, get_str, get_u32, get_u64};
use crate::error::{Error, Result};
use crate::model::{ListInfo, Page};
use crate::resolve::ListKind;

impl Api {
    /// 拉取合集 / 系列的完整视频列表。
    pub async fn list(&self, biz_id: u64, kind: ListKind) -> Result<ListInfo> {
        let title = self.list_title(biz_id, kind).await?;
        let pages = self.list_pages(biz_id, kind).await?;
        Ok(ListInfo {
            biz_id,
            title,
            pages,
        })
    }

    /// 合集元信息：`data.title` 用作目录名。
    async fn list_title(&self, biz_id: u64, kind: ListKind) -> Result<String> {
        let url = format!(
            "{}/x/v1/medialist/info?type={}&biz_id={biz_id}&tid=0",
            self.bases().api,
            kind.api_type()
        );
        let resp = self.get_json(&url).await?;
        check_biz_code(&resp)?;
        let data = resp.get("data").ok_or_else(|| Error::Api {
            code: 0,
            message: "medialist/info 响应缺少 data".into(),
        })?;
        // 元信息拿不到标题不该让整批下载失败，退化为占位名
        let title = get_str(data, "title").unwrap_or("").trim().to_string();
        if title.is_empty() {
            tracing::warn!(
                "{} {biz_id} 没有标题，用 \"list-{biz_id}\" 作为目录名",
                kind.as_str()
            );
            return Ok(format!("list-{biz_id}"));
        }
        Ok(title)
    }

    /// 分页拉列表。
    ///
    /// 三个必须处理的边界（spec §7.3）：游标必须取本页最后一条的 id（哪怕被跳过）、
    /// `attr != 0` 的条目要跳过、游标未推进时必须 break。
    pub async fn list_pages(&self, biz_id: u64, kind: ListKind) -> Result<Vec<Page>> {
        let mut cursor = String::new();
        let mut pages: Vec<Page> = Vec::new();
        // (aid, cid) 去重
        let mut seen: HashSet<(u64, u64)> = HashSet::new();
        // 兜底：正常合集不会翻这么多页
        const MAX_ROUNDS: usize = 500;

        for round in 0..MAX_ROUNDS {
            let resp = self.fetch_list_page(biz_id, kind, &cursor).await?;
            let parsed = parse_list_page(&resp)?;
            let prev_cursor = cursor.clone();
            for page in parsed.pages {
                if seen.insert((page.aid, page.cid)) {
                    pages.push(page);
                }
            }
            cursor = parsed.cursor;

            if !parsed.has_more {
                break;
            }
            // 兜底：接口说还有下一页，但游标没动 —— 再请求也是同一页
            if cursor.is_empty() || cursor == prev_cursor {
                tracing::debug!("游标未推进（第 {} 轮），停止翻页", round + 1);
                break;
            }
        }

        if pages.is_empty() {
            return Err(Error::Api {
                code: 0,
                message: format!("{} {biz_id} 里没有可下载的视频", kind.as_str()),
            });
        }
        Ok(pages)
    }

    async fn fetch_list_page(&self, biz_id: u64, kind: ListKind, cursor: &str) -> Result<Value> {
        let url = format!(
            "{}/x/v2/medialist/resource/list?type={}&oid={}&otype=2&biz_id={biz_id}\
             &with_current=true&mobi_app=web&ps=20&direction=false&sort_field=1&tid=0",
            self.bases().api,
            kind.api_type(),
            cursor
        );
        let resp = self.get_json(&url).await?;
        check_biz_code(&resp)?;
        Ok(resp)
    }
}

/// 一页列表的解析结果（纯函数，便于测试翻页边界）。
#[derive(Debug, Clone)]
pub struct ListPage {
    pub pages: Vec<Page>,
    /// 下一页的游标：本页**最后一条**条目的 id，无论它是否被跳过
    pub cursor: String,
    pub has_more: bool,
}

/// 解析一页 `medialist/resource/list` 响应。
///
/// 只做「本页 → 本页内容」的映射；翻页循环由 `list_pages` 负责。
pub fn parse_list_page(resp: &Value) -> Result<ListPage> {
    // 先判断 code，再取 data
    check_biz_code(resp)?;
    let data = resp.get("data").ok_or_else(|| Error::Api {
        code: resp["code"].as_i64().unwrap_or(0),
        message: "medialist/resource/list 响应缺少 data".into(),
    })?;

    let empty: Vec<Value> = Vec::new();
    let media_list = data["media_list"].as_array().unwrap_or(&empty);
    let has_more = data["has_more"].as_bool().unwrap_or(false);
    let mut cursor = String::new();
    let mut pages: Vec<Page> = Vec::new();

    for item in media_list {
        // 游标永远取本页最后一条的 id，无论该条是否被跳过
        cursor = match get_u64(item, "id") {
            Some(id) => id.to_string(),
            None => item["id"].as_str().unwrap_or("").to_string(),
        };

        // attr != 0 表示条目已失效（稿件删除/私密）
        if item["attr"].as_i64().unwrap_or(0) != 0 {
            tracing::debug!("跳过已失效条目 {} (attr != 0)", item["id"]);
            continue;
        }

        let Some(aid) = get_u64(item, "id") else {
            continue;
        };
        let item_title = get_str(item, "title").unwrap_or("").trim().to_string();
        let upper = get_str(&item["upper"], "name")
            .map(str::to_string)
            .filter(|s| !s.is_empty());
        let item_pages = item["pages"].as_array().cloned().unwrap_or_default();
        if item_pages.is_empty() {
            // 没有 cid 就无法取流，跳过并说清楚
            tracing::warn!("条目 {aid} 没有 pages[]，跳过");
            continue;
        }
        let page_count = item_pages.len();
        for (i, p) in item_pages.iter().enumerate() {
            let Some(cid) = get_u64(p, "cid").or_else(|| get_u64(p, "id")) else {
                tracing::warn!("条目 {aid} 的分 P 缺少 cid，跳过");
                continue;
            };
            let index = get_u32(p, "page").unwrap_or(i as u32 + 1);
            let part = get_str(p, "title").unwrap_or("").trim().to_string();
            // 分 P 标题拼接规则：对齐 page == 1 的判断（单 P 直接用视频标题）
            let title = if page_count == 1 {
                if item_title.is_empty() {
                    format!("{aid}")
                } else {
                    item_title.clone()
                }
            } else if part.is_empty() {
                format!("{item_title}_P{index}")
            } else {
                format!("{item_title}_P{index}_{part}")
            };
            pages.push(Page {
                index,
                aid,
                cid,
                title,
                duration: get_u32(p, "duration").unwrap_or(0),
                upper: upper.clone(),
            });
        }
    }

    Ok(ListPage {
        pages,
        cursor,
        has_more,
    })
}
