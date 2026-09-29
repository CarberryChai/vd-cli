//! 输出路径规划与文件名净化（纯函数，无网络）。
//!
//! 标题完全由服务端控制，必须按 spec §11 净化后才落到文件系统上。

use std::path::{Path, PathBuf};

use crate::model::Page;

/// 单个路径分量的字节上限。文件系统的限制是 255 字节，留出余量。
const MAX_SEGMENT_BYTES: usize = 200;

/// Windows 保留名：不能作为文件名的部分。
const WINDOWS_RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// 净化一个路径分量 / 文件名。
pub fn sanitize(segment: &str) -> String {
    // 1. 替换非法字符
    let mut s: String = segment
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        // 2. 去掉控制字符（含 \n \r \t）、行分隔符与段分隔符
        .filter(|c| !is_strippable(*c))
        .collect();

    // 6. 按 UTF-8 字节数截断，且不切断多字节字符
    s = truncate_bytes(&s, MAX_SEGMENT_BYTES);

    // 5. 去掉结尾的 '.' 和空格（Windows 不允许）
    s = s.trim_end_matches(['.', ' ']).to_string();
    s = s.trim_start_matches(' ').to_string();

    // 4. Windows 保留名前后加下划线（扩展名要留在外面：CON.mp4 → _CON_.mp4）
    if let Some(stem) = reserved_stem(&s) {
        let extension = &s[stem.len()..];
        s = format!("_{stem}_{extension}");
    }

    // 3. 拒绝 "." 和 ".."；7. 空结果兜底
    if s.is_empty() || s == "." || s == ".." {
        return "untitled".to_string();
    }
    s
}

/// 控制类字符：`Cc` 之外，Unicode 的 `Zl`/`Zp`（行/段分隔符）同样不能进文件名。
fn is_strippable(c: char) -> bool {
    c.is_control() || matches!(c, '\u{2028}' | '\u{2029}')
}

/// 若 `s` 的「主干」（最后一个 '.' 之前）是 Windows 保留名，返回该主干的切片。
fn reserved_stem(s: &str) -> Option<&str> {
    let stem = s.rsplit_once('.').map(|(a, _)| a).unwrap_or(s);
    let stem = stem.trim_end_matches(['.', ' ']);
    if stem.is_empty() {
        return None;
    }
    let upper = stem.to_ascii_uppercase();
    WINDOWS_RESERVED
        .contains(&upper.as_str())
        .then(|| &s[..stem.len()])
}

/// 按 UTF-8 字节数截断，保证不落在多字节字符中间。
fn truncate_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// 序号宽度：10 个以内补 1 位，100 个以内补 2 位，以此类推。
pub fn index_width(total: usize) -> usize {
    total.max(1).to_string().len()
}

/// 一个待下载任务的输出位置。
#[derive(Debug, Clone)]
pub struct Layout {
    /// 最终 mp4 的完整路径
    pub final_path: PathBuf,
    /// 中间文件用的基础名（不含扩展名），也放在同一目录
    pub stem: PathBuf,
}

/// 规划一个分 P 的输出路径。
///
/// - 单视频（`pages.len() == 1`）：`{output_dir}/{视频标题}.mp4`
/// - 分 P：`{output_dir}/{视频标题}/[{分P序号}]{分P标题}.mp4`
/// - 合集：`{output_dir}/{合集标题}/[{合集内序号}]{视频标题}.mp4`
///
/// `number` 是文件名里那个序号：视频内分 P 用分 P 序号，合集用条目在合集里的位置。
pub fn plan(
    output_dir: &Path,
    container: &str,
    page: &Page,
    number: u32,
    total: usize,
    multi_page: bool,
) -> Layout {
    let dir_name = sanitize(container);
    let dir = if multi_page {
        output_dir.join(dir_name)
    } else {
        output_dir.to_path_buf()
    };

    let file_name = if multi_page {
        let width = index_width(total);
        format!(
            "[{:0width$}]{}",
            number,
            sanitize(&page.title),
            width = width
        )
    } else {
        sanitize(&page.title)
    };

    let stem = dir.join(file_name);
    Layout {
        final_path: stem.with_extension("mp4"),
        stem,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(index: u32, title: &str) -> Page {
        Page {
            index,
            aid: 1,
            cid: 2,
            title: title.into(),
            duration: 1,
            upper: None,
        }
    }

    #[test]
    fn replaces_illegal_chars() {
        assert_eq!(sanitize("a/b\\c:d*e?f\"g<h>i|j"), "a_b_c_d_e_f_g_h_i_j");
    }

    #[test]
    fn strips_control_chars() {
        assert_eq!(sanitize("a\nb\r\tc"), "abc");
        assert_eq!(sanitize("a\u{0}b\u{7}c"), "abc");
        // Unicode 控制类也要去掉
        assert_eq!(sanitize("a\u{2028}b"), "ab");
    }

    #[test]
    fn path_traversal_is_neutralized() {
        assert_eq!(sanitize(".."), "untitled");
        assert_eq!(sanitize("."), "untitled");
        // 分隔符被替换，不再是目录穿越
        assert_eq!(sanitize("../../etc/passwd"), ".._.._etc_passwd");
        assert_eq!(sanitize("/etc/passwd"), "_etc_passwd");
        assert!(!sanitize("../../etc/passwd").contains('/'));
    }

    #[test]
    fn windows_reserved_names() {
        assert_eq!(sanitize("CON"), "_CON_");
        assert_eq!(sanitize("nul"), "_nul_");
        assert_eq!(sanitize("COM1"), "_COM1_");
        assert_eq!(sanitize("con.mp4"), "_con_.mp4");
        assert_eq!(sanitize("LPT9"), "_LPT9_");
        // 不是保留名的不动
        assert_eq!(sanitize("CONSOLE"), "CONSOLE");
        assert_eq!(sanitize("COM10"), "COM10");
    }

    #[test]
    fn trailing_dots_and_spaces() {
        assert_eq!(sanitize("name..."), "name");
        assert_eq!(sanitize("name   "), "name");
        // "con. " → 去掉尾部的点和空格后变成 CON，仍需加下划线
        assert_eq!(sanitize("con. "), "_con_");
    }

    #[test]
    fn long_chinese_titles_are_truncated_by_bytes() {
        let title = "中".repeat(200); // 600 字节
        let out = sanitize(&title);
        assert!(out.len() <= MAX_SEGMENT_BYTES, "{}", out.len());
        assert!(out.len().is_multiple_of(3), "不能切断多字节字符");
        assert_eq!(out.chars().count(), MAX_SEGMENT_BYTES / 3);
        // 混合字符也不能切坏
        let mixed = format!("{}{}", "a".repeat(199), "中文标题");
        let out = sanitize(&mixed);
        assert!(out.len() <= MAX_SEGMENT_BYTES);
        assert!(out.is_char_boundary(out.len()));
    }

    #[test]
    fn empty_and_all_illegal_fall_back_to_untitled() {
        assert_eq!(sanitize(""), "untitled");
        assert_eq!(sanitize("\n\r\t"), "untitled");
        assert_eq!(sanitize("   "), "untitled");
        assert_eq!(sanitize("///"), "___");
        assert_eq!(sanitize("..."), "untitled");
    }

    #[test]
    fn index_width_matches_spec() {
        assert_eq!(index_width(0), 1);
        assert_eq!(index_width(9), 1);
        assert_eq!(index_width(10), 2);
        assert_eq!(index_width(99), 2);
        assert_eq!(index_width(100), 3);
        assert_eq!(index_width(1000), 4);
    }

    #[test]
    fn single_video_layout() {
        let layout = plan(Path::new("/out"), "标题", &page(1, "标题"), 1, 1, false);
        assert_eq!(layout.final_path, Path::new("/out/标题.mp4"));
        assert_eq!(layout.stem, Path::new("/out/标题"));
    }

    #[test]
    fn multi_page_layout_pads_index() {
        let layout = plan(
            Path::new("/out"),
            "某个视频",
            &page(3, "第三集"),
            3,
            12,
            true,
        );
        assert_eq!(layout.final_path, Path::new("/out/某个视频/[03]第三集.mp4"));
    }

    #[test]
    fn collection_layout_uses_container_dir_and_collection_order() {
        // 合集里的序号是条目在合集里的位置，而不是它在自己视频里的分 P 号
        let layout = plan(
            Path::new("/out"),
            "合集名",
            &page(1, "第七个"),
            7,
            100,
            true,
        );
        assert_eq!(layout.final_path, Path::new("/out/合集名/[007]第七个.mp4"));
    }
}
