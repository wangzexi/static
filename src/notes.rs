//! One public JSON, one scheduled cache, two representations. No request-time S3 reads.
use super::*;
use chrono::{DateTime, FixedOffset, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::{Arc, RwLock};

const LLM_GUIDE: &str = r#"```bash
# 搜索
curl -G 'https://zexi.me/llms.txt' --data-urlencode 'q=AI|Agent' -d 'regex=1' -d 'limit=20'

# 翻页
curl 'https://zexi.me/llms.txt?offset=20&limit=20'
```

"#;

#[derive(Default)]
pub struct Store {
    current: Option<Arc<Cache>>,
    previous: Option<Arc<Cache>>,
}

struct Cache {
    snapshot: String,
    html: String,
    pages: Vec<String>,
    sections: Vec<String>,
    item_html: Vec<String>,
    searchable: Vec<String>,
    markdown: String,
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn linked_text(mut s: &str) -> String {
    let mut result = String::new();
    while let Some(start) = [s.find("https://"), s.find("http://")]
        .into_iter()
        .flatten()
        .min()
    {
        result.push_str(&escape(&s[..start]));
        let tail = &s[start..];
        let end = tail
            .find(|c: char| c.is_whitespace() || "<>\"'[]（）。，；！？、".contains(c))
            .unwrap_or(tail.len());
        let url = tail[..end].trim_end_matches(['.', ',', ';', '!', '?', ')']);
        if Url::parse(url).is_ok() {
            let safe = escape(url);
            result.push_str(&format!(
                "<a href=\"{safe}\" target=\"_blank\" rel=\"noopener\">{safe}</a>"
            ));
        } else {
            result.push_str(&escape(url));
        }
        s = &tail[url.len()..];
    }
    result.push_str(&escape(s));
    result
}
fn original_link(url: &str) -> String {
    if Url::parse(url).is_ok_and(|u| matches!(u.scheme(), "http" | "https")) {
        format!(
            "<a class=\"feed-original\" href=\"{}\" target=\"_blank\" rel=\"noopener\">原文</a>",
            escape(url)
        )
    } else {
        String::new()
    }
}
fn social(s: &str, source: &str, emojis: &Value) -> String {
    if source != "微博" {
        return linked_text(s);
    }
    let mut result = String::new();
    let mut rest = s;
    while let Some(start) = rest.find('[') {
        result.push_str(&linked_text(&rest[..start]));
        rest = &rest[start..];
        if let Some(end) = rest.find(']') {
            let token = &rest[..=end];
            let name = &rest[1..end];
            if let Some(url) = emojis[name]
                .as_str()
                .filter(|u| u.starts_with("https://face.t.sinajs.cn/"))
            {
                let alt = escape(token);
                result.push_str(&format!("<img class=\"weibo-emoji\" src=\"{}\" alt=\"{alt}\" title=\"{alt}\" width=\"20\" height=\"20\" loading=\"lazy\" referrerpolicy=\"no-referrer\">", escape(url)));
                rest = &rest[end + 1..];
                continue;
            }
        }
        result.push('[');
        rest = &rest[1..];
    }
    result.push_str(&linked_text(rest));
    result
}
fn quotes_html(quotes: &Value, emojis: &Value, depth: usize) -> String {
    if depth > 32 {
        return String::new();
    }
    quotes
        .as_array()
        .into_iter()
        .flatten()
        .filter(|q| q["unavailable"] != true)
        .map(|q| {
            let original = original_link(text(q, "url"));
            let warning = if q["partial"] == true && q["unavailable"] != true {
                "<small>归档引用片段，可能不完整</small>"
            } else {
                ""
            };
            format!(
                "<blockquote class=\"feed-quote\"><p>{}</p>{warning}{original}{}</blockquote>",
                social(text(q, "text"), text(q, "source"), emojis),
                quotes_html(&q["quotes"], emojis, depth + 1)
            )
        })
        .collect()
}
fn quotes_md(quotes: &Value, depth: usize) -> String {
    if depth > 32 {
        return String::new();
    }
    quotes
        .as_array()
        .into_iter()
        .flatten()
        .filter(|q| q["unavailable"] != true)
        .map(|q| {
            format!(
                "{}\n\n{}",
                text(q, "text"),
                quotes_md(&q["quotes"], depth + 1)
            )
            .trim()
            .lines()
            .map(|line| format!("> {line}\n"))
            .collect::<String>()
                + "\n"
        })
        .collect()
}
fn date(item: &Value) -> Result<String, String> {
    let date = DateTime::parse_from_rfc3339(text(item, "publishedAt"))
        .map_err(|_| "Invalid publication date")?;
    Ok(date
        .with_timezone(&FixedOffset::east_opt(8 * 3600).unwrap())
        .format("%Y-%m-%d")
        .to_string())
}
fn render_item(item: &Value, emojis: &Value) -> Result<(String, String), String> {
    let source = text(item, "source");
    let article = source == "GitHub blog";
    let raw = text(item, "text");
    let id = escape(text(item, "id"));
    let date = date(item)?;
    let display_date = date
        .split('-')
        .map(|p| p.parse::<u32>().unwrap().to_string())
        .collect::<Vec<_>>()
        .join("/");
    let heading = if article {
        format!("<h3>{}</h3>", escape(text(item, "title")))
    } else {
        String::new()
    };
    let body = if article {
        let events =
            pulldown_cmark::Parser::new_ext(raw, pulldown_cmark::Options::all()).map(|event| {
                match event {
                    pulldown_cmark::Event::Html(s) | pulldown_cmark::Event::InlineHtml(s) => {
                        pulldown_cmark::Event::Text(s)
                    }
                    other => other,
                }
            });
        let mut html = String::new();
        pulldown_cmark::html::push_html(&mut html, events);
        let html = ammonia::Builder::default()
            .add_generic_attributes(["class"])
            .clean(&html)
            .to_string();
        let base = format!(
            "/content-assets/{}/",
            escape(text(item, "id").trim_start_matches("GitHub blog:"))
        );
        html.replace("src=\"assets/", &format!("src=\"{base}"))
            .replace("href=\"assets/", &format!("href=\"{base}"))
    } else {
        let mut content = raw;
        if item["hasVideo"] == true {
            if let Some(start) = raw.rfind("http") {
                let tail = raw[start..].trim_matches(|c: char| {
                    c.is_whitespace() || matches!(c, '\u{200b}' | '\u{200c}' | '\u{200d}')
                });
                if tail.starts_with("http://t.cn/") || tail.starts_with("https://t.cn/") {
                    if !tail.contains(char::is_whitespace) {
                        content = &raw[..start];
                    }
                }
            }
        }
        format!("<p>{}</p>", social(content, source, emojis))
    };
    let fallback = vec![item.clone()];
    let origins = item["origins"].as_array().unwrap_or(&fallback);
    let mut labels = String::new();
    for origin in origins {
        if text(origin, "source") == "GitHub blog" && article {
            let slug = percent_encode(
                text(item, "id")
                    .trim_start_matches("GitHub blog:")
                    .as_bytes(),
                URI_COMPONENT,
            );
            labels.push_str(&format!("<a href=\"https://github.com/wangzexi/blog/blob/main/{slug}/README.md\" target=\"_blank\" rel=\"noopener\">GitHub</a>"));
        } else if text(origin, "source") == "X" {
            if let Ok(url) = Url::parse(text(origin, "url")) {
                if url.scheme() == "https" && matches!(url.host_str(), Some("x.com" | "www.x.com"))
                {
                    labels.push_str(&format!(
                        "<a class=\"feed-origin\" href=\"{}\" target=\"_blank\" rel=\"noopener\">X</a>",
                        escape(url.as_str())
                    ));
                }
            }
        }
    }
    let mut videos = String::new();
    for path in item["videos"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let valid = path
            .strip_prefix("/content-assets/social/")
            .and_then(|p| p.strip_suffix(".mp4"))
            .is_some_and(|hash| {
                hash.len() == 64
                    && hash
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            });
        if valid {
            let path = escape(path);
            videos.push_str(&format!("<video class=\"feed-video\" controls playsinline preload=\"metadata\" aria-label=\"视频\" src=\"{path}\">浏览器无法播放此视频，<a href=\"{path}\">打开视频</a>。</video>"));
        }
    }
    if !videos.is_empty() {
        videos = format!("<div class=\"feed-videos\">{videos}</div>");
    }
    let video = if videos.is_empty()
        && item["hasVideo"] == true
        && text(item, "url").starts_with("https://weibo.com/")
    {
        format!(
            "<a class=\"feed-attachment\" href=\"{}\" target=\"_blank\" rel=\"noopener\">查看视频附件 ↗</a>",
            escape(text(item, "url"))
        )
    } else {
        String::new()
    };
    let mut images = String::new();
    let mut images_md = String::new();
    for (i, path) in item["images"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|p| p.starts_with("/content-assets/social/"))
        .enumerate()
    {
        let n = i + 1;
        let path = escape(path);
        images.push_str(&format!("<a href=\"{path}\" class=\"feed-image\" aria-label=\"放大第 {n} 张配图\"><img src=\"{path}\" alt=\"配图 {n}\" loading=\"lazy\" width=\"160\" height=\"160\" /></a>"));
        images_md.push_str(&format!("![配图](https://zexi.me{path})\n\n"));
    }
    if !images.is_empty() {
        images = format!("<div class=\"feed-images\">{images}</div>");
    }
    let class = if article { " markdown-section" } else { "" };
    let html = format!(
        "<article class=\"feed-item\" id=\"{id}\"><div class=\"feed-item__meta\">{labels}<time datetime=\"{}\">{display_date}</time></div>{heading}<div id=\"body-{id}\" class=\"feed-body{class}\" data-collapsed=\"true\">{body}{}{video}</div><button class=\"feed-expand\" type=\"button\" aria-expanded=\"false\" aria-controls=\"body-{id}\" hidden>展开</button>{images}{videos}</article>",
        escape(text(item, "publishedAt")),
        quotes_html(&item["quotes"], emojis, 0)
    );
    let heading = if article {
        format!("{}\n\n{date}", text(item, "title"))
    } else {
        date
    };
    let md = format!(
        "## {heading}\n\n{raw}\n\n{images_md}{}\n---\n\n",
        quotes_md(&item["quotes"], 0)
    );
    Ok((html, md))
}
impl Cache {
    fn build(data: Value, template: &str, emojis: &Value) -> Result<Self, String> {
        for marker in ["__FEED_ITEMS__", "__FEED_PAGES__", "__FEED_SNAPSHOT__"] {
            if !template.contains(marker) {
                return Err(format!("Missing template marker {marker}"));
            }
        }
        let mut items = data["items"].as_array().ok_or("Invalid items")?.clone();
        if data["total"].as_u64() != Some(items.len() as u64) {
            return Err("Invalid total".into());
        }
        let snapshot =
            format!("{:x}", Sha256::digest(serde_json::to_vec(&items).unwrap()))[..16].to_string();
        let mut ids = std::collections::HashSet::new();
        for item in &items {
            if text(item, "id").is_empty()
                || !ids.insert(text(item, "id"))
                || !item["text"].is_string()
            {
                return Err("Invalid item".into());
            }
        }
        items.sort_by(|a, b| text(b, "publishedAt").cmp(text(a, "publishedAt")));
        let rendered = items
            .iter()
            .map(|item| render_item(item, emojis))
            .collect::<Result<Vec<_>, _>>()?;
        let sections = rendered.iter().map(|(_, md)| md.clone()).collect();
        let mut cache = Self {
            snapshot,
            html: String::new(),
            pages: Vec::new(),
            sections,
            item_html: rendered.iter().map(|(html, _)| html.clone()).collect(),
            searchable: items
                .iter()
                .map(|item| {
                    format!(
                        "{}\n{}\n{}",
                        text(item, "title"),
                        text(item, "text"),
                        quotes_md(&item["quotes"], 0)
                    )
                })
                .collect(),
            markdown: String::new(),
        };
        for (page, chunk) in rendered.chunks(20).enumerate() {
            cache.pages.push(json!({"snapshotId":cache.snapshot,"offset":page*20,"total":items.len(),"html":chunk.iter().map(|(html,_)|html.as_str()).collect::<String>()}).to_string());
        }
        // Substitute metadata before user content so marker-like text stays literal.
        cache.html = template
            .replace("__FEED_PAGES__", &cache.pages.len().to_string())
            .replace("__FEED_SNAPSHOT__", &cache.snapshot)
            .replace(
                "__FEED_ITEMS__",
                &rendered
                    .iter()
                    .take(20)
                    .map(|(html, _)| html.as_str())
                    .collect::<String>(),
            );
        if cache.pages.len() <= 1 {
            cache.html = cache.html.replace(
                "id=\"feed-status\"",
                "data-complete=\"true\" id=\"feed-status\"",
            );
        }
        cache.markdown = cache.markdown(0, 20);
        Ok(cache)
    }
    fn markdown(&self, offset: usize, limit: usize) -> String {
        let start = offset.min(self.sections.len());
        let end = offset.saturating_add(limit).min(self.sections.len());
        let mut body = format!("# Zexi's Notes\n\n{LLM_GUIDE}");
        body.push_str(&self.sections[start..end].concat());
        if end < self.sections.len() {
            body.push_str(&format!(
                "[下一页](https://zexi.me/llms.txt?offset={end}&limit={limit})\n"
            ));
        } else {
            body.push_str("已到最后一页。\n");
        }
        body
    }
}
struct SearchQuery {
    q: String,
    regex: bool,
    offset: usize,
    limit: usize,
    snapshot: Option<String>,
}
impl SearchQuery {
    fn parse(query: Option<&str>) -> Result<Self, &'static str> {
        let params: std::collections::HashMap<_, _> =
            url::form_urlencoded::parse(query.unwrap_or_default().as_bytes())
                .into_owned()
                .collect();
        let q = params
            .get("q")
            .map(|q| q.trim().to_owned())
            .unwrap_or_default();
        if q.len() > 2048 {
            return Err("搜索内容过长，请缩短后重试。");
        }
        let regex = match params.get("regex").map(String::as_str) {
            None | Some("0") => false,
            Some("1") => true,
            _ => return Err("regex must be 0 or 1"),
        };
        let (offset, limit) = parse_llm_query(query)?;
        Ok(Self {
            q,
            regex,
            offset,
            limit,
            snapshot: params.get("snapshot").cloned(),
        })
    }
    fn params(&self, offset: usize, snapshot: &str) -> String {
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("q", &self.q)
            .append_pair("regex", if self.regex { "1" } else { "0" })
            .append_pair("offset", &offset.to_string())
            .append_pair("limit", &self.limit.to_string())
            .append_pair("snapshot", snapshot)
            .finish()
    }
}
impl Cache {
    fn search(&self, query: &SearchQuery) -> Result<Vec<usize>, &'static str> {
        let pattern = if query.regex {
            Some(
                regex::RegexBuilder::new(&query.q)
                    .case_insensitive(true)
                    .size_limit(1_000_000)
                    .dfa_size_limit(1_000_000)
                    .build()
                    .map_err(|_| "正则表达式无效或过于复杂，请修改后重试。")?,
            )
        } else {
            None
        };
        let needle = query.q.to_lowercase();
        Ok(self
            .searchable
            .iter()
            .enumerate()
            .filter_map(|(i, text)| {
                let matched = match &pattern {
                    Some(re) => re.is_match(text),
                    None => text.to_lowercase().contains(&needle),
                };
                matched.then_some(i)
            })
            .collect())
    }
    fn search_body(&self, query: &SearchQuery, markdown: bool) -> Result<String, &'static str> {
        let matches = self.search(query)?;
        let start = query.offset.min(matches.len());
        let end = start.saturating_add(query.limit).min(matches.len());
        let next = (end < matches.len()).then_some(end);
        if markdown {
            let mut body = format!(
                "# Zexi's Notes 搜索\n\n{LLM_GUIDE}共找到 {} 条笔记。\n\n",
                matches.len()
            );
            for &i in &matches[start..end] {
                body.push_str(&self.sections[i]);
            }
            if let Some(next) = next {
                body.push_str(&format!(
                    "[下一页](https://zexi.me/llms.txt?{})\n",
                    query.params(next, &self.snapshot)
                ));
            } else if matches.is_empty() {
                body.push_str("没有找到匹配的笔记。\n");
            } else {
                body.push_str("已到最后一页。\n");
            }
            Ok(body)
        } else {
            Ok(json!({"snapshotId":self.snapshot,"query":query.q,"regex":query.regex,"offset":query.offset,"limit":query.limit,"total":matches.len(),"nextOffset":next,"html":matches[start..end].iter().map(|&i|self.item_html[i].as_str()).collect::<String>()}).to_string())
        }
    }
}
async fn object(state: &AppState, path: &str) -> Result<Vec<u8>, String> {
    let response = fetch_object(state, &Method::GET, &HeaderMap::new(), "zexi.me", path)
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("{path}: {}", response.status()));
    }
    Ok(response.bytes().await.map_err(|e| e.to_string())?.to_vec())
}
pub async fn refresh(state: &AppState) -> Result<(), String> {
    let (data, template, emojis) = tokio::try_join!(
        object(state, "/feed/all.json"),
        object(state, "/notes-template.html"),
        object(state, "/notes-emojis.json")
    )?;
    let next = Arc::new(Cache::build(
        serde_json::from_slice(&data).map_err(|e| e.to_string())?,
        std::str::from_utf8(&template).map_err(|e| e.to_string())?,
        &serde_json::from_slice(&emojis).map_err(|e| e.to_string())?,
    )?);
    info!(items=next.sections.len(),snapshot=%next.snapshot,"notes cache refreshed");
    let mut store = state.notes.write().unwrap();
    if store
        .current
        .as_ref()
        .is_none_or(|old| old.snapshot != next.snapshot)
    {
        store.previous = store.current.take();
    }
    store.current = Some(next);
    Ok(())
}
fn seconds_until_six(now: i64) -> u64 {
    // 06:00 Asia/Shanghai = 22:00 UTC. Always schedule the next boundary.
    let boundary = (now - 22 * 3600).div_euclid(86400) * 86400 + 22 * 3600 + 86400;
    (boundary - now) as u64
}
pub async fn schedule(state: AppState) {
    loop {
        tokio::time::sleep(Duration::from_secs(seconds_until_six(
            Utc::now().timestamp(),
        )))
        .await;
        if let Err(error) = refresh(&state).await {
            error!(%error,"notes refresh failed; previous cache retained");
        }
    }
}
pub fn response(state: &AppState, method: &Method, path: &str, query: Option<&str>) -> Response {
    let store = state.notes.read().unwrap();
    let Some(current) = &store.current else {
        return plain(StatusCode::SERVICE_UNAVAILABLE, "notes unavailable");
    };
    let has_search = path == "/feed/search.json"
        || (path == "/llms.txt"
            && url::form_urlencoded::parse(query.unwrap_or_default().as_bytes())
                .any(|(key, _)| key == "q"));
    let (content_type, body) = if has_search {
        let query = match SearchQuery::parse(query) {
            Ok(q) => q,
            Err(error) => return plain(StatusCode::BAD_REQUEST, error),
        };
        let cache = match query.snapshot.as_deref() {
            None => current,
            Some(snapshot) if snapshot == current.snapshot => current,
            Some(snapshot) => match &store.previous {
                Some(previous) if snapshot == previous.snapshot => previous,
                _ => return plain(StatusCode::CONFLICT, "内容已更新，请重新搜索。"),
            },
        };
        let markdown = path == "/llms.txt";
        let body = match cache.search_body(&query, markdown) {
            Ok(body) => body,
            Err(error) => return plain(StatusCode::BAD_REQUEST, error),
        };
        (
            if markdown {
                "text/markdown; charset=utf-8"
            } else {
                "application/json; charset=utf-8"
            },
            body,
        )
    } else if matches!(path, "/" | "/index.html") {
        ("text/html; charset=utf-8", current.html.clone())
    } else if path == "/llms.txt" {
        let (offset, limit) = match parse_llm_query(query) {
            Ok(v) => v,
            Err(e) => return plain(StatusCode::BAD_REQUEST, e),
        };
        (
            "text/markdown; charset=utf-8",
            if offset == 0 && limit == 20 {
                current.markdown.clone()
            } else {
                current.markdown(offset, limit)
            },
        )
    } else {
        let parts: Vec<_> = path
            .trim_start_matches("/feed/runtime/")
            .split('/')
            .collect();
        if parts.len() != 2 {
            return plain(StatusCode::NOT_FOUND, "not found");
        }
        let cache = if parts[0] == current.snapshot {
            current
        } else if let Some(previous) = &store.previous {
            if parts[0] == previous.snapshot {
                previous
            } else {
                return plain(StatusCode::CONFLICT, "feed updated; reload");
            }
        } else {
            return plain(StatusCode::CONFLICT, "feed updated; reload");
        };
        let page = parts[1]
            .strip_prefix("page-")
            .and_then(|p| p.strip_suffix(".json"))
            .and_then(|p| p.parse::<usize>().ok())
            .unwrap_or(0);
        let Some(body) = page.checked_sub(1).and_then(|p| cache.pages.get(p)) else {
            return plain(StatusCode::NOT_FOUND, "not found");
        };
        ("application/json; charset=utf-8", body.clone())
    };
    drop(store);
    let mut response = Response::new(if method == Method::HEAD {
        Body::empty()
    } else {
        Body::from(body)
    });
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    with_cors(response)
}

pub fn empty_store() -> Arc<RwLock<Store>> {
    Arc::new(RwLock::new(Store::default()))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn data(n: usize) -> Value {
        json!({"snapshotId":"aaaaaaaaaaaaaaaa","total":n,"items":(0..n).map(|i|json!({"id":i.to_string(),"text":format!("note {i} [卡皮巴拉]"),"source":"微博","publishedAt":"2026-09-12T00:00:00Z","quotes":[]})).collect::<Vec<_>>()})
    }
    const TEMPLATE: &str = "__FEED_PAGES__ __FEED_SNAPSHOT__ __FEED_ITEMS__";
    #[test]
    fn search_covers_all_notes_titles_and_quotes_and_treats_keywords_literally() {
        let mut payload = data(43);
        payload["items"][42]["text"] = json!("Rust [a+b] 过去的一句话");
        payload["items"][41]["title"] = json!("SpecialTitle");
        payload["items"][40]["quotes"] = json!([{"text":"引用中的独特观点"}]);
        let cache = Cache::build(payload, TEMPLATE, &json!({})).unwrap();
        for (query, id) in [
            ("q=rust", 42),
            ("q=%5Ba%2Bb%5D", 42),
            ("q=specialtitle", 41),
            ("q=独特观点", 40),
        ] {
            let q = SearchQuery::parse(Some(query)).unwrap();
            assert_eq!(cache.search(&q).unwrap(), vec![id]);
        }
        let q = SearchQuery::parse(Some("q=Rust%7C独特观点&regex=1")).unwrap();
        assert_eq!(cache.search(&q).unwrap(), vec![40, 42]);
        assert!(
            cache
                .search(&SearchQuery::parse(Some("q=never-matches")).unwrap())
                .unwrap()
                .is_empty()
        );
        assert!(
            cache
                .search(&SearchQuery::parse(Some("q=%5B&regex=1")).unwrap())
                .is_err()
        );
        assert!(SearchQuery::parse(Some("q=x&regex=2")).is_err());
        assert!(SearchQuery::parse(Some("q=x&offset=-1")).is_err());
        assert!(SearchQuery::parse(Some("q=x&limit=0")).is_err());
        assert!(SearchQuery::parse(Some("q=x&limit=101")).is_err());
        assert!(SearchQuery::parse(Some(&format!("q={}", "x".repeat(2049)))).is_err());
    }
    #[test]
    fn search_paginates_results_and_preserves_query_and_snapshot_in_markdown() {
        let cache = Cache::build(data(43), TEMPLATE, &json!({})).unwrap();
        let q = SearchQuery::parse(Some("q=note&limit=20&offset=20")).unwrap();
        let page: Value = serde_json::from_str(&cache.search_body(&q, false).unwrap()).unwrap();
        assert_eq!(page["total"], 43);
        assert_eq!(page["nextOffset"], 40);
        assert_eq!(
            page["html"].as_str().unwrap().matches("<article ").count(),
            20
        );
        assert!(!page["html"].as_str().unwrap().contains("id=\"0\""));
        let md = cache.search_body(&q, true).unwrap();
        assert!(md.contains(&format!(
            "q=note&regex=0&offset=40&limit=20&snapshot={}",
            cache.snapshot
        )));
        let last = SearchQuery::parse(Some("q=note&offset=40")).unwrap();
        let page: Value = serde_json::from_str(&cache.search_body(&last, false).unwrap()).unwrap();
        assert_eq!(page["nextOffset"], Value::Null);
        assert_eq!(
            page["html"].as_str().unwrap().matches("<article ").count(),
            3
        );
        let empty = SearchQuery::parse(Some("q=note&offset=18446744073709551615")).unwrap();
        assert!(
            cache
                .search_body(&empty, true)
                .unwrap()
                .contains("已到最后")
        );
    }
    #[tokio::test]
    async fn search_routes_support_head_errors_and_previous_snapshot_without_storage() {
        let previous = Arc::new(Cache::build(data(2), TEMPLATE, &json!({})).unwrap());
        let current = Arc::new(Cache::build(data(43), TEMPLATE, &json!({})).unwrap());
        let state = AppState {
            endpoint: "http://127.0.0.1:1".into(),
            bucket: "sites".into(),
            client: Client::new(),
            notes: Arc::new(RwLock::new(Store {
                current: Some(current),
                previous: Some(previous.clone()),
            })),
        };
        let response = response(
            &state,
            &Method::GET,
            "/feed/search.json",
            Some(&format!("q=note&snapshot={}", previous.snapshot)),
        );
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&body).unwrap()["total"], 2);
        let head = super::response(&state, &Method::HEAD, "/llms.txt", Some("q=note"));
        assert_eq!(head.status(), StatusCode::OK);
        assert!(
            axum::body::to_bytes(head.into_body(), usize::MAX)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            super::response(
                &state,
                &Method::GET,
                "/feed/search.json",
                Some("q=x&snapshot=expired")
            )
            .status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            super::response(
                &state,
                &Method::GET,
                "/feed/search.json",
                Some("q=%5B&regex=1")
            )
            .status(),
            StatusCode::BAD_REQUEST
        );
    }
    #[test]
    fn scheduled_at_six_shanghai() {
        let t = DateTime::parse_from_rfc3339("2026-09-12T05:59:59+08:00")
            .unwrap()
            .timestamp();
        assert_eq!(seconds_until_six(t), 1);
        assert_eq!(seconds_until_six(t + 1), 86400);
        assert_eq!(seconds_until_six(t + 2), 86399);
    }
    #[test]
    fn full_first_last_and_empty() {
        let cache = Cache::build(data(43), TEMPLATE, &json!({})).unwrap();
        assert_eq!(cache.html.matches("<article ").count(), 20);
        assert_eq!(cache.pages.len(), 3);
        let last: Value = serde_json::from_str(&cache.pages[2]).unwrap();
        assert_eq!(last["offset"], 40);
        assert_eq!(
            last["html"].as_str().unwrap().matches("<article ").count(),
            3
        );
        assert_eq!(cache.markdown.matches("## ").count(), 20);
        assert_eq!(cache.sections.len(), 43);
        assert!(
            cache
                .markdown
                .contains("[下一页](https://zexi.me/llms.txt?offset=20&limit=20)")
        );
        assert!(cache.markdown(20, 20).contains("offset=40&limit=20"));
        assert!(!cache.markdown(40, 20).contains("下一页"));
        assert!(cache.markdown(usize::MAX, 100).contains("已到最后"));
        assert!(
            Cache::build(data(0), TEMPLATE, &json!({}))
                .unwrap()
                .pages
                .is_empty()
        );
    }
    #[test]
    fn rendering_is_safe_and_preserves_quotes_images_and_emoji() {
        let item = json!({"id":"<test>","text":"<script>alert(1)</script>[卡皮巴拉]","source":"微博","publishedAt":"2026-09-11T23:00:00Z","quotes":[{"source":"微博","text":"自己的前文","quotes":[{"text":"更早的前文"}]}],"images":["/content-assets/social/a.jpg","javascript:bad"]});
        let (html, md) =
            render_item(&item, &json!({"卡皮巴拉":"https://face.t.sinajs.cn/a.png"})).unwrap();
        assert!(!html.contains("<script>"));
        assert!(html.contains("weibo-emoji"));
        assert!(html.contains("2026/9/12"));
        assert_eq!(html.matches("<blockquote").count(), 2);
        assert!(html.contains("feed-image"));
        assert!(!html.contains("javascript:"));
        assert!(md.contains("> > 更早的前文"));
        let (html,_)=render_item(&json!({"id":"GitHub blog:test","source":"GitHub blog","text":"# Heading\n\n**bold**\n\n[x](javascript:alert(1))\n\n<img src=x onerror=alert(1)>","publishedAt":"2026-09-12T00:00:00Z"}),&json!({})).unwrap();
        assert!(html.contains("<strong>bold</strong>"));
        assert!(!html.contains("href=\"javascript:"));
        assert!(!html.contains("<img src=x"));
    }
    #[test]
    fn video_player_is_outside_collapsed_text_and_rejects_untrusted_paths() {
        let path = format!("/content-assets/social/{}.mp4", "a".repeat(64));
        let (html, _) = render_item(&json!({"id":"video", "source":"微博", "text":"正文", "publishedAt":"2026-09-13T00:00:00Z", "hasVideo":true, "url":"https://weibo.com/1/2", "videos":[path, "javascript:bad", "/content-assets/social/../../evil.mp4"]}), &json!({})).unwrap();
        assert_eq!(html.matches("<video ").count(), 1);
        assert!(html.contains("controls playsinline preload=\"metadata\""));
        assert!(html.find("<video ").unwrap() > html.find("</button>").unwrap());
        assert!(!html.contains("查看视频附件"));
        assert!(!html.contains("javascript:bad"));
        assert!(!html.contains("evil.mp4"));
        let (html, _) = render_item(&json!({"id":"missing", "source":"微博", "text":"正文", "publishedAt":"2026-09-13T00:00:00Z", "hasVideo":true, "url":"https://weibo.com/1/2"}), &json!({})).unwrap();
        assert!(html.contains("查看视频附件"));
    }
    #[test]
    fn quote_sources_are_clickable_and_escaped() {
        let html = quotes_html(
            &json!([{"source":"微博", "text":"来源：http://t.cn/AXc2V3Yt。<script>bad</script>", "url":"https://weibo.com/1727858283/Qt9rruBev"}]),
            &json!({}),
            0,
        );
        assert!(html.contains("href=\"http://t.cn/AXc2V3Yt\""));
        assert!(html.contains("href=\"https://weibo.com/1727858283/Qt9rruBev\""));
        assert!(html.contains(">原文</a>"));
        assert!(!html.contains("<script>"));
        assert!(original_link("javascript:alert(1)").is_empty());
    }
    #[test]
    fn malformed_data_and_template_rejected() {
        let mut d = data(1);
        d["total"] = json!(2);
        assert!(Cache::build(d, TEMPLATE, &json!({})).is_err());
        assert!(Cache::build(data(1), "bad template", &json!({})).is_err());
    }
    #[tokio::test]
    async fn requests_use_memory_refresh_is_atomic_and_old_pages_survive() {
        use std::sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        };
        let payload = Arc::new(Mutex::new(data(43)));
        let reads = Arc::new(AtomicUsize::new(0));
        let p = payload.clone();
        let r = reads.clone();
        let router = Router::new().fallback(move |req: Request<Body>| {
            let p = p.clone();
            let r = r.clone();
            async move {
                r.fetch_add(1, Ordering::SeqCst);
                let body = if req.uri().path().ends_with("all.json") {
                    p.lock().unwrap().to_string()
                } else if req.uri().path().ends_with("notes-template.html") {
                    TEMPLATE.into()
                } else {
                    "{}".into()
                };
                Response::new(Body::from(body))
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let state = AppState {
            endpoint,
            bucket: "sites".into(),
            client: Client::new(),
            notes: empty_store(),
        };
        assert_eq!(
            response(&state, &Method::GET, "/", None).status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        refresh(&state).await.unwrap();
        let old = state
            .notes
            .read()
            .unwrap()
            .current
            .as_ref()
            .unwrap()
            .snapshot
            .clone();
        for path in [
            "/",
            "/llms.txt",
            &format!("/feed/runtime/{old}/page-3.json"),
        ] {
            assert_eq!(
                response(&state, &Method::GET, path, None).status(),
                StatusCode::OK
            );
        }
        assert_eq!(reads.load(Ordering::SeqCst), 3);
        payload.lock().unwrap()["items"][0]["text"] = json!("new version");
        refresh(&state).await.unwrap();
        assert_ne!(
            state
                .notes
                .read()
                .unwrap()
                .current
                .as_ref()
                .unwrap()
                .snapshot,
            old
        );
        assert_eq!(
            response(
                &state,
                &Method::GET,
                &format!("/feed/runtime/{old}/page-3.json"),
                None
            )
            .status(),
            StatusCode::OK
        );
        payload.lock().unwrap()["total"] = json!(999);
        assert!(refresh(&state).await.is_err());
        assert!(
            state
                .notes
                .read()
                .unwrap()
                .current
                .as_ref()
                .unwrap()
                .markdown
                .contains("new version")
        );
        assert_eq!(
            response(&state, &Method::GET, "/llms.txt", Some("limit=0")).status(),
            StatusCode::BAD_REQUEST
        );
        server.abort();
        assert_eq!(
            response(&state, &Method::GET, "/", None).status(),
            StatusCode::OK
        );
    }
}
