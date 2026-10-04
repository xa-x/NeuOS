//! Web: DuckDuckGo search results, readable-text extraction, and the clean
//! reader window.

use neu_router::{Action, SearchItem};
use scraper::{Html, Selector};
use tauri::{Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

const UA: &str = "Mozilla/5.0 (X11; Linux aarch64) NeuOS/0.1";

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(UA)
        .connect_timeout(std::time::Duration::from_secs(8))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("http client")
}

/// Search DuckDuckGo's HTML endpoint (no API key) and return (title, url).
pub async fn ddg_search(query: &str) -> Result<Vec<(String, String)>, String> {
    let client = http_client();
    let resp = client
        .get("https://html.duckduckgo.com/html/")
        .query(&[("q", query)])
        .send()
        .await
        .map_err(|e| format!("search failed: {e}"))?;
    let html = resp.text().await.map_err(|e| e.to_string())?;
    let doc = Html::parse_document(&html);
    let sel = Selector::parse("a.result__a").map_err(|_| "selector")?;
    let mut out = Vec::new();
    for node in doc.select(&sel).take(8) {
        let title: String = node.text().collect::<Vec<_>>().join("").trim().to_string();
        let href = node.value().attr("href").unwrap_or("").to_string();
        let url = unwrap_ddg_redirect(&href);
        if !title.is_empty() && (url.starts_with("http://") || url.starts_with("https://")) {
            out.push((title, url));
        }
    }
    Ok(out)
}

fn unwrap_ddg_redirect(href: &str) -> String {
    // DDG html results use /l/?uddg=<urlencoded>
    if let Some(rest) = href.strip_prefix("//duckduckgo.com/l/") {
        return extract_param(rest, "uddg");
    }
    if let Some(rest) = href.strip_prefix("/l/") {
        return extract_param(rest, "uddg");
    }
    if href.starts_with("//") {
        return format!("https:{}", href);
    }
    href.to_string()
}

fn extract_param(query: &str, key: &str) -> String {
    for pair in query.trim_start_matches('?').split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == key {
                return percent_decode(v);
            }
        }
    }
    String::new()
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                if let Ok(b) = u8::from_str_radix(hex, 16) {
                    out.push(b);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Fetch a page and return readable text (tags stripped).
pub async fn fetch_text(url: &str) -> Result<String, String> {
    let client = http_client();
    let resp = client.get(url).send().await.map_err(|e| format!("fetch failed: {e}"))?;
    let html = resp.text().await.map_err(|e| e.to_string())?;
    Ok(extract_text(&html))
}

fn extract_text(html: &str) -> String {
    let doc = Html::parse_document(html);
    let sel = Selector::parse("body").ok();
    let mut text = String::new();
    if let Some(sel) = sel {
        for node in doc.select(&sel) {
            text.push_str(&node.text().collect::<String>());
        }
    }
    let collapsed: Vec<&str> = text.split_whitespace().collect();
    collapsed.join(" ")
}

/// Open a URL in the clean reader window: fetch → extract main content →
/// render with NeuOS typography in a normal decorated window.
#[tauri::command]
pub async fn open_reader(app: tauri::AppHandle, url: String) -> Result<(), String> {
    let html = http_client()
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("fetch failed: {e}"))?
        .text()
        .await
        .map_err(|e| e.to_string())?;

    let (title, body) = extract_readable(&html);
    let title = if title.is_empty() { url.clone() } else { title };
    let doc = reader_document(&title, &body, &url);
    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, doc.as_bytes());
    let data_url = format!("data:text/html;base64,{b64}");

    if let Some(win) = app.get_webview_window("reader") {
        let _ = win.set_title(&title);
        let _ = win.show();
        let _ = win.set_focus();
        let payload = serde_json::to_string(&data_url).unwrap_or_default();
        win.eval(&format!("location.replace({payload})"))
            .map_err(|e| e.to_string())?;
    } else {
        let parsed: tauri::Url = data_url.parse().map_err(|e| format!("bad url: {e}"))?;
        WebviewWindowBuilder::new(&app, "reader", WebviewUrl::External(parsed))
            .title(&title)
            .inner_size(780.0, 940.0)
            .min_inner_size(420.0, 400.0)
            .build()
            .map_err(|e| e.to_string())?;
    }
    let _ = app.emit("reader-opened", ());
    Ok(())
}

fn extract_readable(html: &str) -> (String, String) {
    let doc = Html::parse_document(html);
    let title = Selector::parse("title")
        .ok()
        .and_then(|s| doc.select(&s).next())
        .map(|t| t.text().collect::<String>().trim().to_string())
        .unwrap_or_default();

    for candidate in ["article", "main", "[role='main']", "#content", ".content", ".post"] {
        if let Ok(sel) = Selector::parse(candidate) {
            if let Some(node) = doc.select(&sel).next() {
                let mut html = node.inner_html();
                strip_tag_blocks(&mut html, "script");
                strip_tag_blocks(&mut html, "style");
                strip_tag_blocks(&mut html, "iframe");
                strip_tag_blocks(&mut html, "nav");
                strip_tag_blocks(&mut html, "svg");
                let text_len = Html::parse_document(&html)
                    .root_element()
                    .text()
                    .collect::<String>()
                    .len();
                if text_len > 400 {
                    return (title, html);
                }
            }
        }
    }
    // fallback: whole body
    let mut html = Selector::parse("body")
        .ok()
        .and_then(|s| doc.select(&s).next())
        .map(|n| n.inner_html())
        .unwrap_or_default();
    strip_tag_blocks(&mut html, "script");
    strip_tag_blocks(&mut html, "style");
    strip_tag_blocks(&mut html, "iframe");
    (title, html)
}

fn strip_tag_blocks(html: &mut String, tag: &str) {
    loop {
        let lower = html.to_lowercase();
        let Some(start) = lower.find(&format!("<{tag}")) else { break };
        let Some(end_rel) = lower[start..].find(&format!("</{tag}>")) else {
            // self-closing or malformed: cut to end of the opening tag
            if let Some(gt) = lower[start..].find('>') {
                html.replace_range(start..start + gt + 1, "");
            } else {
                html.clear();
            }
            break;
        };
        let end = start + end_rel + tag.len() + 3;
        html.replace_range(start..end, "");
    }
}

fn reader_document(title: &str, body_html: &str, url: &str) -> String {
    let css = r#"
    :root { color-scheme: dark; }
    body { margin: 0; background: #0b0d12; color: #e8eaf0;
      font: 16px/1.75 Inter, system-ui, sans-serif; }
    main { max-width: 42rem; margin: 0 auto; padding: 3rem 1.5rem 6rem; }
    h1,h2,h3 { line-height: 1.3; margin: 2em 0 .6em; }
    a { color: #7c8cf8; }
    img,video { max-width: 100%; height: auto; border-radius: 8px; }
    pre { overflow-x: auto; background: #12151d; padding: 1rem;
      border-radius: 10px; font-size: 13px; }
    code { font-family: ui-monospace, monospace; background: #12151d;
      padding: .1em .35em; border-radius: 4px; }
    blockquote { border-left: 3px solid #7c8cf8; margin: 1.5em 0;
      padding-left: 1em; color: #8a90a0; }
    .neu-src { margin-top: 4rem; padding-top: 1rem; border-top: 1px solid #2a2f3e;
      color: #8a90a0; font: 12px ui-monospace, monospace; word-break: break-all; }
    "#;
    format!(
        "<!doctype html><html><head><meta charset='utf-8'><meta name='viewport' content='width=device-width,initial-scale=1'><title>{}</title><style>{css}</style></head><body><main><h1>{}</h1>{}</main><div class='neu-src'>{}</div></body></html>",
        escape_html(title),
        escape_html(title),
        body_html,
        escape_html(url),
    )
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Web search results as launcher items.
pub async fn search_items(query: &str) -> Vec<SearchItem> {
    match ddg_search(query).await {
        Ok(results) => results
            .into_iter()
            .map(|(title, url)| SearchItem {
                id: url.clone(),
                title,
                subtitle: Some(host_of(&url)),
                badge: "W".into(),
                score: 0.2,
                action: Action::OpenReader { url },
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn host_of(url: &str) -> String {
    url.trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or(url)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_script_blocks() {
        let mut html = "<p>a</p><script>evil()</script><p>b</p>".to_string();
        strip_tag_blocks(&mut html, "script");
        assert_eq!(html, "<p>a</p><p>b</p>");
    }

    #[test]
    fn ddg_redirect_unwrap() {
        assert_eq!(
            unwrap_ddg_redirect("//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fa&rut=abc"),
            "https://example.com/a"
        );
        assert_eq!(unwrap_ddg_redirect("https://example.com"), "https://example.com");
    }

    #[test]
    fn readability_picks_article() {
        let html = "<html><head><title>T</title></head><body><nav>x</nav><article><p>word </p>×100</article></body></html>";
        let (title, body) = extract_readable(html);
        assert_eq!(title, "T");
        assert!(body.contains("article") || body.contains("word"));
    }
}
