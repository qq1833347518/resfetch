//! EPUB3 打包。
//!
//! 旧版一度依赖第三方库；这里手写 ZIP（仅 store 存储），零依赖、三端一致。
//! EPUB 规范允许 mimetype 用存储方式，且必须是压缩包里的第一个条目——本实现严格照做。

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chapter {
    pub title: String,
    /// 已经是 HTML 片段（段落、<p> 等）；纯文本请先过 `text_to_html`
    pub html: String,
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for b in data {
        crc ^= *b as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

pub fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// 纯文本转 HTML：空行分段，段内换行保留。
pub fn text_to_html(text: &str) -> String {
    let mut out = String::new();
    for para in text.split("\n\n") {
        let p = para.trim();
        if p.is_empty() {
            continue;
        }
        out.push_str("<p>");
        out.push_str(&escape_xml(p).replace('\n', "<br/>"));
        out.push_str("</p>\n");
    }
    out
}

struct Entry {
    name: String,
    data: Vec<u8>,
}

fn zip_store(entries: &[Entry]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();

    for e in entries {
        let offset = out.len() as u32;
        let crc = crc32(&e.data);
        let size = e.data.len() as u32;

        // 本地文件头
        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&10u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0u16.to_le_bytes()); // flags
        out.extend_from_slice(&0u16.to_le_bytes()); // method = store
        out.extend_from_slice(&0u16.to_le_bytes()); // mod time
        out.extend_from_slice(&0x21u16.to_le_bytes()); // mod date: 1980-01-01
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra len
        out.extend_from_slice(e.name.as_bytes());
        out.extend_from_slice(&e.data);

        // 中央目录项
        central.extend_from_slice(b"PK\x01\x02");
        central.extend_from_slice(&20u16.to_le_bytes()); // version made by
        central.extend_from_slice(&10u16.to_le_bytes()); // version needed
        central.extend_from_slice(&0u16.to_le_bytes()); // flags
        central.extend_from_slice(&0u16.to_le_bytes()); // method
        central.extend_from_slice(&0u16.to_le_bytes()); // time
        central.extend_from_slice(&0x21u16.to_le_bytes()); // date
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes()); // extra
        central.extend_from_slice(&0u16.to_le_bytes()); // comment
        central.extend_from_slice(&0u16.to_le_bytes()); // disk start
        central.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        central.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(e.name.as_bytes());
    }

    let cd_offset = out.len() as u32;
    let cd_size = central.len() as u32;
    out.extend_from_slice(&central);

    // EOCD
    out.extend_from_slice(b"PK\x05\x06");
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// 读取 store 方式写出的 zip（给测试、也给安卓端校验用）
pub fn read_entries(data: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 30 <= data.len() {
        if &data[i..i + 4] != b"PK\x03\x04" {
            break;
        }
        let flags = u16::from_le_bytes([data[i + 6], data[i + 7]]);
        let method = u16::from_le_bytes([data[i + 8], data[i + 9]]);
        let crc = u32::from_le_bytes([data[i + 14], data[i + 15], data[i + 16], data[i + 17]]);
        let size = u32::from_le_bytes([data[i + 18], data[i + 19], data[i + 20], data[i + 21]]) as usize;
        let name_len = u16::from_le_bytes([data[i + 26], data[i + 27]]) as usize;
        let extra_len = u16::from_le_bytes([data[i + 28], data[i + 29]]) as usize;
        let name_start = i + 30;
        let name_end = name_start + name_len;
        if name_end > data.len() {
            return Err(Error::InvalidEpub("文件名越界".to_string()));
        }
        let name = String::from_utf8_lossy(&data[name_start..name_end]).to_string();
        let data_start = name_end + extra_len;
        let data_end = data_start + size;
        if data_end > data.len() {
            return Err(Error::InvalidEpub(format!("{} 数据越界", name)));
        }
        if flags & 0x08 != 0 || method != 0 {
            return Err(Error::InvalidEpub("只支持 store 方式且带长度字段的条目".to_string()));
        }
        let body = data[data_start..data_end].to_vec();
        if crc32(&body) != crc {
            return Err(Error::InvalidEpub(format!("{} CRC 校验失败", name)));
        }
        out.push((name, body));
        i = data_end;
    }
    Ok(out)
}

fn chapter_xhtml(title: &str, html: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops" lang="zh-CN">
<head><meta charset="utf-8"/><title>{t}</title>
<style>body{{font-family:-apple-system,"PingFang SC",serif;line-height:1.8;margin:2em}}h1{{font-size:1.4em}}</style>
</head>
<body><h1>{t}</h1>
{body}
</body></html>"#,
        t = escape_xml(title),
        body = html
    )
}

/// 生成 EPUB3 字节流。`book_id` 建议传 "urn:uuid:..." 或稳定的内容哈希。
pub fn build_epub(
    title: &str,
    author: &str,
    language: &str,
    book_id: &str,
    chapters: &[Chapter],
) -> Result<Vec<u8>> {
    if chapters.is_empty() {
        return Err(Error::InvalidEpub("没有章节内容".to_string()));
    }

    let mut entries: Vec<Entry> = Vec::new();
    entries.push(Entry {
        name: "mimetype".to_string(),
        data: b"application/epub+zip".to_vec(),
    });
    entries.push(Entry {
        name: "META-INF/container.xml".to_string(),
        data: br#"<?xml version="1.0" encoding="utf-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>"#
            .to_vec(),
    });

    let mut manifest = String::new();
    let mut spine = String::new();
    let mut nav_items = String::new();
    for (idx, ch) in chapters.iter().enumerate() {
        let name = format!("ch{:03}.xhtml", idx + 1);
        manifest.push_str(&format!(
            "    <item id=\"ch{}\" href=\"{}\" media-type=\"application/xhtml+xml\"/>\n",
            idx + 1,
            name
        ));
        spine.push_str(&format!("    <itemref idref=\"ch{}\"/>\n", idx + 1));
        nav_items.push_str(&format!(
            "      <li><a href=\"{}\">{}</a></li>\n",
            name,
            escape_xml(&ch.title)
        ));
        entries.push(Entry {
            name: format!("OEBPS/{}", name),
            data: chapter_xhtml(&ch.title, &ch.html).into_bytes(),
        });
    }

    let opf = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="bookid" xml:lang="{lang}">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="bookid">{id}</dc:identifier>
    <dc:title>{title}</dc:title>
    <dc:creator>{author}</dc:creator>
    <dc:language>{lang}</dc:language>
    <meta property="dcterms:modified">1980-01-01T00:00:00Z</meta>
  </metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
{manifest}  </manifest>
  <spine>
{spine}  </spine>
</package>"#,
        lang = escape_xml(language),
        id = escape_xml(book_id),
        title = escape_xml(title),
        author = escape_xml(author),
        manifest = manifest,
        spine = spine
    );

    let nav = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops" lang="zh-CN">
<head><meta charset="utf-8"/><title>目录</title></head>
<body>
  <nav epub:type="toc" id="toc">
    <h1>目录</h1>
    <ol>
{items}    </ol>
  </nav>
</body></html>"#,
        items = nav_items
    );

    entries.push(Entry {
        name: "OEBPS/content.opf".to_string(),
        data: opf.into_bytes(),
    });
    entries.push(Entry {
        name: "OEBPS/nav.xhtml".to_string(),
        data: nav.into_bytes(),
    });

    Ok(zip_store(&entries))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<Chapter> {
        vec![
            Chapter { title: "第一集 开端".into(), html: "<p>正文一</p>".into() },
            Chapter { title: "第二集 & <危险>".into(), html: "<p>正文二</p>".into() },
        ]
    }

    #[test]
    fn builds_valid_structure() {
        let bytes = build_epub("测试书", "作者", "zh-CN", "urn:uuid:test", &sample()).unwrap();
        let entries = read_entries(&bytes).unwrap();
        let names: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names[0], "mimetype", "mimetype 必须是第一个条目");
        assert_eq!(entries[0].1, b"application/epub+zip");
        assert!(names.contains(&"META-INF/container.xml"));
        assert!(names.contains(&"OEBPS/content.opf"));
        assert!(names.contains(&"OEBPS/nav.xhtml"));
        assert!(names.contains(&"OEBPS/ch001.xhtml"));
        assert_eq!(entries.len(), 6);
    }

    #[test]
    fn crc_and_sizes_are_correct() {
        let bytes = build_epub("书", "作者", "zh-CN", "id", &sample()).unwrap();
        assert!(read_entries(&bytes).is_ok());
    }

    #[test]
    fn titles_are_escaped_in_opf_and_nav() {
        let bytes = build_epub("书&名", "作<者", "zh-CN", "id", &sample()).unwrap();
        let entries = read_entries(&bytes).unwrap();
        let opf = entries.iter().find(|(n, _)| n == "OEBPS/content.opf").unwrap();
        let opf_text = String::from_utf8_lossy(&opf.1);
        assert!(opf_text.contains("书&amp;名"));
        assert!(opf_text.contains("作&lt;者"));
        let nav = entries.iter().find(|(n, _)| n == "OEBPS/nav.xhtml").unwrap();
        assert!(String::from_utf8_lossy(&nav.1).contains("第二集 &amp; &lt;危险&gt;"));
    }

    #[test]
    fn rejects_empty_book() {
        assert!(matches!(build_epub("书", "作者", "zh-CN", "id", &[]), Err(Error::InvalidEpub(_))));
    }

    #[test]
    fn text_to_html_escapes() {
        let html = text_to_html("第一段 & <b>\n\n第二段");
        assert_eq!(html, "<p>第一段 &amp; &lt;b&gt;</p>\n<p>第二段</p>\n");
    }
}
