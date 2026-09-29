//! `x/web-interface/view`：视频信息与分 P 列表。

use serde_json::Value;

use super::{Api, check_biz_code, get_str, get_u32, get_u64};
use crate::error::{Error, Result};
use crate::model::{Page, VideoInfo};

impl Api {
    /// 拉取视频信息。
    pub async fn view(&self, aid: u64) -> Result<VideoInfo> {
        let url = format!("{}/x/web-interface/view?aid={aid}", self.bases().api);
        let resp = self.get_json(&url).await?;
        parse_view(&resp, aid)
    }
}

pub fn parse_view(resp: &Value, aid: u64) -> Result<VideoInfo> {
    // 先判断 code，再取 data：错误响应里没有 data 键
    check_biz_code(resp)?;

    let data = resp.get("data").ok_or_else(|| Error::Api {
        code: resp["code"].as_i64().unwrap_or(0),
        message: "响应缺少 data 字段".into(),
    })?;

    let title = get_str(data, "title").unwrap_or("").trim().to_string();
    let owner = get_str(&data["owner"], "name")
        .map(str::to_string)
        .filter(|s| !s.is_empty());
    let duration = get_u32(data, "duration").unwrap_or(0);
    let aid = get_u64(data, "aid").unwrap_or(aid);

    let raw_pages = data["pages"].as_array().cloned().unwrap_or_default();
    let single = raw_pages.len() <= 1;
    let mut pages: Vec<Page> = Vec::with_capacity(raw_pages.len());
    for (i, p) in raw_pages.iter().enumerate() {
        let index = get_u32(p, "page").unwrap_or(i as u32 + 1);
        let cid = get_u64(p, "cid").unwrap_or(0);
        let part = get_str(p, "part").unwrap_or("").trim();
        // 单 P 视频的 pages[0].part 常是无意义占位符（"P1" 之类），用视频标题更合理
        let page_title = if single || part.is_empty() {
            title.clone()
        } else {
            part.to_string()
        };
        pages.push(Page {
            index,
            aid,
            cid,
            title: page_title,
            duration: get_u32(p, "duration").unwrap_or(0),
            upper: owner.clone(),
        });
    }

    if pages.is_empty() {
        return Err(Error::Api {
            code: 0,
            message: "视频没有可用的分 P".into(),
        });
    }

    Ok(VideoInfo {
        aid,
        title,
        owner,
        duration,
        pages,
    })
}
