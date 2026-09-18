//! One cancellable search worker; only the latest submitted query reaches the UI.
use super::catalog::{self, AurComment, CommentLine, CommentSpan, Event, Package};
use anyhow::{Context, Result};
use raur::Raur;
use scraper::{ElementRef, Html, Selector};
use std::collections::HashSet;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc, Arc,
};
use std::time::Duration;

pub struct Search {
    requests: mpsc::Sender<(u64, String)>,
    latest: Arc<AtomicU64>,
}
impl Search {
    pub fn new(events: mpsc::Sender<Event>) -> Self {
        let (tx, rx) = mpsc::channel::<(u64, String)>();
        let latest = Arc::new(AtomicU64::new(0));
        let generation = latest.clone();
        std::thread::spawn(move || {
            while let Ok(mut request) = rx.recv() {
                for newer in rx.try_iter() {
                    request = newer;
                }
                let (id, query) = request;
                if query.trim().is_empty() {
                    continue;
                }
                let result = tokio::runtime::Runtime::new()
                    .map_err(anyhow::Error::from)
                    .and_then(|rt| rt.block_on(run(id, &query, &events, &generation)));
                if generation.load(Ordering::Relaxed) == id {
                    if let Err(e) = result {
                        let _ = events.send(Event::Search {
                            id,
                            packages: vec![],
                            done: true,
                            error: Some(format!("Search failed: {e:#}")),
                        });
                    }
                }
            }
        });
        Self {
            requests: tx,
            latest,
        }
    }
    pub fn submit(&self, id: u64, query: String) {
        self.latest.store(id, Ordering::Relaxed);
        let _ = self.requests.send((id, query));
    }
}
impl Drop for Search {
    fn drop(&mut self) {
        self.latest.fetch_add(1, Ordering::Relaxed);
    }
}
async fn run(
    id: u64,
    query: &str,
    events: &mpsc::Sender<Event>,
    latest: &AtomicU64,
) -> anyhow::Result<()> {
    let config = catalog::config()?;
    let targets = query
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let official = crate::search::search_repos(&config, &targets)?;
    let mut packages = official
        .into_iter()
        .map(|p| catalog::repository_package(&config, p))
        .collect::<Vec<_>>();
    if latest.load(Ordering::Relaxed) != id {
        return Ok(());
    }
    let _ = events.send(Event::Search {
        id,
        packages: packages.clone(),
        done: false,
        error: None,
    });
    let search = crate::search::search_aur(&config, &targets);
    tokio::pin!(search);
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let result = loop {
        if latest.load(Ordering::Relaxed) != id {
            return Ok(());
        }
        match tokio::time::timeout(Duration::from_millis(75), &mut search).await {
            Ok(result) => break result,
            Err(_) if std::time::Instant::now() >= deadline => {
                break Err(anyhow::anyhow!("AUR search timed out"))
            }
            Err(_) => {}
        }
    };
    let mut error = None;
    match result {
        Ok(aur) => {
            for p in aur {
                packages.push(catalog::aur_package(&config, p, false)?);
            }
        }
        Err(e) => {
            error = Some(format!(
                "AUR search failed; repository results are still available: {e:#}"
            ))
        }
    }
    if latest.load(Ordering::Relaxed) == id {
        let _ = events.send(Event::Search {
            id,
            packages,
            done: true,
            error,
        });
    }
    Ok(())
}
pub fn inspect_aur(name: String, key: String, sender: mpsc::Sender<Event>) {
    std::thread::spawn(move || {
        let result = tokio::runtime::Runtime::new()
            .map_err(anyhow::Error::from)
            .and_then(|rt| {
                rt.block_on(async {
                    let config = catalog::config()?;
                    let mut packages = tokio::time::timeout(
                        Duration::from_secs(15),
                        config.raur.info(&[name.as_str()]),
                    )
                    .await??;
                    let package = packages
                        .pop()
                        .ok_or_else(|| anyhow::anyhow!("AUR package no longer exists"))?;
                    let p: Package = catalog::aur_package(&config, package, true)?;
                    Ok::<_, anyhow::Error>(p.remote.unwrap().fields)
                })
            });
        let _ = sender.send(match result {
            Ok(fields) => Event::Inspection(key, fields),
            Err(e) => Event::DetailError(key, format!("AUR details failed: {e:#}")),
        });
    });
}

pub fn view_pkgbuild(id: u64, name: String, base: Option<String>, sender: mpsc::Sender<Event>) {
    std::thread::spawn(move || {
        let result = (|| -> Result<(String, String)> {
            let config = catalog::config()?;
            let base = match base {
                Some(base) => base,
                None => {
                    let runtime = tokio::runtime::Runtime::new()?;
                    let mut packages = runtime.block_on(async {
                        let packages = tokio::time::timeout(
                            Duration::from_secs(15),
                            config.raur.info(&[name.as_str()]),
                        )
                        .await
                        .context("AUR lookup timed out")??;
                        Ok::<_, anyhow::Error>(packages)
                    })?;
                    let package = packages
                        .pop()
                        .ok_or_else(|| anyhow::anyhow!("AUR package no longer exists"))?;
                    let base = package.package_base;
                    let _ = sender.send(Event::AurResolved {
                        id,
                        name: name.clone(),
                        base: base.clone(),
                    });
                    base
                }
            };
            let temporary = tempfile::tempdir()?;
            let mut fetch = config.fetch.clone();
            fetch.clone_dir = temporary.path().join("clone");
            fetch.diff_dir = temporary.path().join("diff");
            let remote = (|| -> Result<String> {
                fetch.download(&[base.as_str()])?;
                let path = fetch.clone_dir.join(&base).join("PKGBUILD");
                std::fs::read_to_string(&path)
                    .with_context(|| format!("PKGBUILD was not found in {}", path.display()))
            })();
            let text = remote.or_else(|error| {
                let path = config.fetch.clone_dir.join(&base).join("PKGBUILD");
                std::fs::read_to_string(&path)
                    .with_context(|| format!("{error:#}; no cached PKGBUILD at {}", path.display()))
            })?;
            Ok((base, text))
        })();
        let _ = sender.send(match result {
            Ok((base, text)) => Event::Pkgbuild { id, base, text },
            Err(e) => Event::PkgbuildError {
                id,
                message: format!("PKGBUILD view failed: {e:#}"),
            },
        });
    });
}

pub fn view_comments(
    id: u64,
    base: String,
    sender: mpsc::Sender<Event>,
    generation: Arc<AtomicU64>,
) {
    std::thread::spawn(move || {
        let result = tokio::runtime::Runtime::new()
            .map_err(anyhow::Error::from)
            .and_then(|rt| rt.block_on(load_comments(id, &base, &sender, &generation)));
        if let Err(e) = result {
            if generation.load(Ordering::Relaxed) == id {
                let _ = sender.send(Event::Comments {
                    id,
                    comments: vec![],
                    done: true,
                    error: Some(format!("AUR comments failed: {e:#}")),
                });
            }
        }
    });
}

async fn load_comments(
    id: u64,
    base: &str,
    sender: &mpsc::Sender<Event>,
    generation: &AtomicU64,
) -> Result<()> {
    let config = catalog::config()?;
    let client = config.raur.client();
    let mut url = config.aur_url.join(&format!("packages/{base}"))?;
    let mut offset = 0usize;
    let mut seen = HashSet::new();
    loop {
        if generation.load(Ordering::Relaxed) != id {
            return Ok(());
        }
        url.query_pairs_mut()
            .clear()
            .append_pair("O", &offset.to_string())
            .append_pair("PP", "250");
        let response =
            tokio::time::timeout(Duration::from_secs(20), client.get(url.clone()).send())
                .await
                .with_context(|| format!("Timed out fetching {url}"))??;
        anyhow::ensure!(
            response.status().is_success(),
            "{url}: {}",
            response.status()
        );
        let html = tokio::time::timeout(Duration::from_secs(20), response.text())
            .await
            .context("Timed out reading AUR comments")??;
        let (page, next) = parse_comments_page(&html, &url, offset)?;
        let comments = page
            .into_iter()
            .filter(|comment| seen.insert(comment.id.clone()))
            .collect();
        let done = next.is_none();
        if generation.load(Ordering::Relaxed) != id {
            return Ok(());
        }
        if sender
            .send(Event::Comments {
                id,
                comments,
                done,
                error: None,
            })
            .is_err()
        {
            return Ok(());
        }
        match next {
            Some(next) => offset = next,
            None => return Ok(()),
        }
    }
}

fn parse_comments_page(
    html: &str,
    url: &url::Url,
    offset: usize,
) -> Result<(Vec<AurComment>, Option<usize>)> {
    let document = Html::parse_document(html);
    let section = Selector::parse("div.comments.package-comments").unwrap();
    let heading = Selector::parse("div.comments-header h3").unwrap();
    let header = Selector::parse("h4.comment-header").unwrap();
    let body = Selector::parse("div.article-content").unwrap();
    let pages = Selector::parse(".comments-header-nav a.page").unwrap();
    let mut comments = Vec::new();
    let mut next: Option<usize> = None;
    for section in document.select(&section) {
        let pinned = section
            .select(&heading)
            .next()
            .is_some_and(|h| h.text().collect::<String>().contains("Pinned"));
        for (header, body) in section.select(&header).zip(section.select(&body)) {
            let Some(id) = header.value().attr("id") else {
                continue;
            };
            let title = normalized(&header.text().collect::<String>());
            let mut lines = Vec::new();
            comment_blocks(body, url, &mut lines);
            while lines
                .last()
                .is_some_and(|line: &CommentLine| line.text().is_empty())
            {
                lines.pop();
            }
            comments.push(AurComment {
                id: id.to_owned(),
                header: title,
                pinned,
                lines,
            });
        }
        if !pinned {
            for page in section.select(&pages) {
                let Some(href) = page.value().attr("href") else {
                    continue;
                };
                let Ok(link) = url.join(href) else {
                    continue;
                };
                if link.path() != url.path() {
                    continue;
                }
                let candidate = link
                    .query_pairs()
                    .find(|(key, _)| key == "O")
                    .and_then(|(_, value)| value.parse::<usize>().ok());
                if let Some(candidate) = candidate.filter(|candidate| *candidate > offset) {
                    next = Some(next.map_or(candidate, |current| current.min(candidate)));
                }
            }
        }
    }
    Ok((comments, next))
}

fn normalized(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn comment_blocks(element: ElementRef<'_>, page_url: &url::Url, lines: &mut Vec<CommentLine>) {
    for child in element.children().filter_map(ElementRef::wrap) {
        match child.value().name() {
            "p" => push_comment_line(lines, comment_spans(child, page_url)),
            "pre" => {
                blank_comment_line(lines);
                let text = child.text().collect::<String>().replace('\t', "    ");
                for line in text.trim_matches('\n').lines() {
                    lines.push(CommentLine::plain(line, true));
                }
                blank_comment_line(lines);
            }
            "ul" | "ol" => {
                let ordered = child.value().name() == "ol";
                for (index, item) in child
                    .children()
                    .filter_map(ElementRef::wrap)
                    .filter(|item| item.value().name() == "li")
                    .enumerate()
                {
                    let marker = if ordered {
                        format!("{}. ", index + 1)
                    } else {
                        "• ".to_owned()
                    };
                    let mut spans = vec![CommentSpan {
                        text: marker,
                        url: None,
                    }];
                    spans.extend(comment_spans(item, page_url));
                    push_comment_line(lines, spans);
                }
            }
            "blockquote" => {
                let mut quoted = Vec::new();
                comment_blocks(child, page_url, &mut quoted);
                for mut line in quoted {
                    if !line.text().is_empty() {
                        line.spans.insert(
                            0,
                            CommentSpan {
                                text: "│ ".into(),
                                url: None,
                            },
                        );
                    }
                    lines.push(line);
                }
            }
            "br" => blank_comment_line(lines),
            _ => comment_blocks(child, page_url, lines),
        }
    }
}

fn blank_comment_line(lines: &mut Vec<CommentLine>) {
    if lines.last().is_some_and(|line| !line.text().is_empty()) {
        lines.push(CommentLine::plain("", false));
    }
}

fn push_comment_line(lines: &mut Vec<CommentLine>, spans: Vec<CommentSpan>) {
    if spans.iter().all(|span| span.text.is_empty()) {
        return;
    }
    lines.push(CommentLine { spans, code: false });
    blank_comment_line(lines);
}

fn comment_spans(element: ElementRef<'_>, page_url: &url::Url) -> Vec<CommentSpan> {
    fn append(spans: &mut Vec<CommentSpan>, character: char, url: Option<&str>) {
        if let Some(last) = spans.last_mut().filter(|span| span.url.as_deref() == url) {
            last.text.push(character);
        } else {
            spans.push(CommentSpan {
                text: character.to_string(),
                url: url.map(str::to_owned),
            });
        }
    }
    fn walk(
        element: ElementRef<'_>,
        page_url: &url::Url,
        current_url: Option<&str>,
        pending_space: &mut bool,
        spans: &mut Vec<CommentSpan>,
    ) {
        for child in element.children() {
            if let Some(text) = child.value().as_text() {
                for character in text.chars() {
                    if character.is_whitespace() {
                        *pending_space = true;
                    } else if !character.is_control() {
                        if *pending_space && !spans.is_empty() {
                            append(spans, ' ', None);
                        }
                        *pending_space = false;
                        append(spans, character, current_url);
                    }
                }
            } else if let Some(child) = ElementRef::wrap(child) {
                if child.value().name() == "br" {
                    append(spans, '\n', None);
                    *pending_space = false;
                    continue;
                }
                let link = if child.value().name() == "a" {
                    child.value().attr("href").and_then(|href| {
                        page_url.join(href).ok().filter(|url| {
                            matches!(url.scheme(), "http" | "https") && url.host_str().is_some()
                        })
                    })
                } else {
                    None
                };
                walk(
                    child,
                    page_url,
                    link.as_ref().map(url::Url::as_str).or(current_url),
                    pending_space,
                    spans,
                );
            }
        }
    }
    let mut spans = Vec::new();
    walk(element, page_url, None, &mut false, &mut spans);
    auto_link_plain_urls(spans)
}

fn auto_link_plain_urls(spans: Vec<CommentSpan>) -> Vec<CommentSpan> {
    static URL: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let pattern = URL.get_or_init(|| regex::Regex::new(r"https?://[^\s<>]+").unwrap());
    let mut result = Vec::new();
    for span in spans {
        if span.url.is_some() {
            result.push(span);
            continue;
        }
        let mut end = 0;
        for found in pattern.find_iter(&span.text) {
            if found.start() > end {
                result.push(CommentSpan {
                    text: span.text[end..found.start()].to_owned(),
                    url: None,
                });
            }
            let trimmed = found
                .as_str()
                .trim_end_matches(['.', ',', ';', ':', '!', '?', ')']);
            let valid = url::Url::parse(trimmed)
                .ok()
                .filter(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some());
            result.push(CommentSpan {
                text: trimmed.to_owned(),
                url: valid.map(|url| url.to_string()),
            });
            if trimmed.len() < found.as_str().len() {
                result.push(CommentSpan {
                    text: found.as_str()[trimmed.len()..].to_owned(),
                    url: None,
                });
            }
            end = found.end();
        }
        if end < span.text.len() {
            result.push(CommentSpan {
                text: span.text[end..].to_owned(),
                url: None,
            });
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pinned_comments_code_blocks_and_next_page() {
        let html = r#"
            <div class="comments package-comments">
              <div class="comments-header"><h3>Pinned Comments</h3></div>
              <h4 id="comment-1" class="comment-header">maintainer commented on <a>2026-01-01</a></h4>
              <div id="comment-1-content" class="article-content"><div><p>Read this first.</p></div></div>
            </div>
            <div class="comments package-comments">
              <div class="comments-header"><h3>Latest Comments</h3>
                <p class="comments-header-nav"><a class="page" href="?O=250&amp;PP=250">Next ›</a></p>
              </div>
              <h4 id="comment-2" class="comment-header">user commented on <a>2026-01-02</a></h4>
              <div id="comment-2-content" class="article-content"><div>
                <p>Read the <a href="https://example.org/guide">guide</a> or https://example.org/archive. Then run this:</p><pre><code>makepkg -si
  --noconfirm</code></pre><p>Then retry.</p>
              </div></div>
            </div>
        "#;
        let url = url::Url::parse("https://aur.archlinux.org/packages/example?O=0&PP=250").unwrap();
        let (comments, next) = parse_comments_page(html, &url, 0).unwrap();
        assert_eq!(next, Some(250));
        assert_eq!(comments.len(), 2);
        assert!(comments[0].pinned);
        assert_eq!(comments[0].id, "comment-1");
        assert!(comments[1]
            .lines
            .iter()
            .any(|line| line.code && line.text() == "makepkg -si"));
        assert!(comments[1]
            .lines
            .iter()
            .any(|line| line.code && line.text() == "  --noconfirm"));
        assert!(comments[1]
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| {
                span.text == "guide" && span.url.as_deref() == Some("https://example.org/guide")
            }));
        assert!(comments[1]
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| {
                span.text == "https://example.org/archive"
                    && span.url.as_deref() == Some("https://example.org/archive")
            }));
    }
}
