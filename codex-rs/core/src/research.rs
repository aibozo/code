use std::fs;
use std::path::{Path, PathBuf};
use regex_lite::Regex;
use chrono::Datelike;

const ARXIV_API_URL: &str = "https://export.arxiv.org/api/query";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ResearchSource {
    /// Canonical arXiv identifier when available (e.g., 2401.01234)
    #[serde(default)]
    pub id: String,
    pub url: String,
    pub title: String,
    pub year: i32,
    pub authors: Vec<String>,
    pub summary: String,
    pub score: f64,
}

fn fnv1a64(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325; // FNV offset basis
    for b in s.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn cache_root(cwd: &Path) -> PathBuf {
    cwd.join("harness").join("cache").join("research")
}

fn normalized_query(topic: &str, year_range: Option<(i32, i32)>) -> String {
    let mut q = topic.trim().to_lowercase();
    q = q.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some((y1, y2)) = year_range {
        q.push_str(&format!(" | years:{}-{}", y1, y2));
    }
    q
}

fn cache_path_for_query(cwd: &Path, topic: &str, year_range: Option<(i32, i32)>) -> PathBuf {
    let norm = normalized_query(topic, year_range);
    let h = fnv1a64(&norm);
    cache_root(cwd).join(format!("{:016x}.json", h))
}

/// Load cached results for a query if available.
pub fn load_cached_sources(cwd: &Path, topic: &str, year_range: Option<(i32, i32)>) -> Option<Vec<ResearchSource>> {
    let path = cache_path_for_query(cwd, topic, year_range);
    let text = fs::read_to_string(&path).ok()?;
    serde_json::from_str::<Vec<ResearchSource>>(&text).ok()
}

/// Save sources to deterministic cache file for a query. Overwrites existing.
pub fn save_cached_sources(cwd: &Path, topic: &str, year_range: Option<(i32, i32)>, sources: &[ResearchSource]) -> std::io::Result<()> {
    let path = cache_path_for_query(cwd, topic, year_range);
    if let Some(parent) = path.parent() { fs::create_dir_all(parent)?; }
    let json = serde_json::to_string_pretty(sources).unwrap_or_else(|_| "[]".to_string());
    fs::write(&path, json)
}

/// Offline-first query: attempts to load from cache; returns empty vec when not present.
pub fn query_arxiv_offline(cwd: &Path, topic: &str, year_range: Option<(i32, i32)>) -> Vec<ResearchSource> {
    let mut items = load_cached_sources(cwd, topic, year_range).unwrap_or_default();
    // Dedup by URL and apply stable ranking by score desc, then year desc, then title asc
    items.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.year.cmp(&a.year))
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
    });
    let mut seen = std::collections::HashSet::new();
    items.retain(|s| seen.insert(s.url.clone()));
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn cache_key_stability() {
        let tmp = tempdir().unwrap();
        let cwd = tmp.path();
        let p1 = cache_path_for_query(cwd, "  Transformers   in  Vision  ", Some((2020, 2024)));
        let p2 = cache_path_for_query(cwd, "transformers in vision", Some((2020, 2024)));
        assert_eq!(p1, p2);
        let p3 = cache_path_for_query(cwd, "transformers in vision", Some((2019, 2024)));
        assert_ne!(p2, p3);
    }

    #[test]
    fn parse_atom_extracts_entries() {
        let atom = r#"<?xml version=\"1.0\" encoding=\"UTF-8\"?>
            <feed xmlns=\"http://www.w3.org/2005/Atom\">
              <entry>
                <id>http://arxiv.org/abs/2401.00001</id>
                <updated>2024-01-02T00:00:00Z</updated>
                <published>2024-01-01T00:00:00Z</published>
                <title>Test Paper A</title>
                <summary>Interesting summary.</summary>
                <author><name>Alice</name></author>
                <author><name>Bob</name></author>
                <link href=\"http://arxiv.org/abs/2401.00001\" rel=\"alternate\"/>
              </entry>
              <entry>
                <id>http://arxiv.org/abs/2401.00002</id>
                <updated>2024-01-03T00:00:00Z</updated>
                <title>Test Paper B</title>
                <summary>Another summary.</summary>
                <author><name>Carol</name></author>
                <link href=\"http://arxiv.org/abs/2401.00002\" rel=\"alternate\"/>
              </entry>
            </feed>
        "#;
        let items = parse_arxiv_atom(atom);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "Test Paper A");
        assert_eq!(items[0].authors.len(), 2);
        assert!(items[0].id.contains("2401.00001"));
        assert_eq!(items[1].title, "Test Paper B");
    }

    #[test]
    fn offline_dedup_and_sort() {
        let tmp = tempdir().unwrap();
        let cwd = tmp.path();
        // Save two entries with duplicated URL/id; ensure dedup
        let sources = vec![
            ResearchSource { id: "x1".into(), url: "https://arxiv.org/abs/1111.0001".into(), title: "Zeta".into(), year: 2023, authors: vec!["A".into()], summary: "s".into(), score: 0.9 },
            ResearchSource { id: "x1".into(), url: "https://arxiv.org/abs/1111.0001".into(), title: "Zeta dup".into(), year: 2022, authors: vec!["B".into()], summary: "s".into(), score: 0.8 },
            ResearchSource { id: "x2".into(), url: "https://arxiv.org/abs/1111.0002".into(), title: "Alpha".into(), year: 2024, authors: vec!["C".into()], summary: "s".into(), score: 0.95 },
        ];
        save_cached_sources(cwd, "topic x", None, &sources).unwrap();
        let got = query_arxiv_offline(cwd, "topic x", None);
        // dedup preserves 2 items
        assert_eq!(got.len(), 2);
        // sorted by score desc then year desc then title asc; "Alpha" earlier by score
        assert_eq!(got[0].title, "Alpha");
    }
}

fn extract_first_tag_text(xml: &str, tag: &str) -> Option<String> {
    // Simple, tolerant extraction; not a full XML parser.
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    if let Some(i) = xml.find(&open) {
        if let Some(j) = xml[i + open.len()..].find(&close) {
            let raw = &xml[i + open.len()..i + open.len() + j];
            let trimmed = raw.trim().replace('\n', " ");
            return Some(trimmed);
        }
    }
    None
}

fn extract_authors(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let open = "<author>";
    let close = "</author>";
    while let Some(i) = xml[start..].find(open) {
        let i_abs = start + i;
        if let Some(j) = xml[i_abs..].find(close) {
            let block = &xml[i_abs..i_abs + j + close.len()];
            if let Some(name) = extract_first_tag_text(block, "name") { out.push(name); }
            start = i_abs + j + close.len();
            continue;
        }
        break;
    }
    out
}

fn extract_link_href(xml: &str) -> Option<String> {
    // Prefer alternate link rel when available, otherwise use id
    // <link href="..." rel="alternate"/>
    let re = Regex::new(r#"<link[^>]*href=\"([^\"]+)\"[^>]*/?>"#).ok()?;
    if let Some(cap) = re.captures_iter(xml).next() {
        return Some(cap.get(1)?.as_str().to_string());
    }
    extract_first_tag_text(xml, "id")
}

fn parse_year(s: &str) -> i32 {
    // arXiv dates like 2024-05-01T.. or plain year; extract first 4 digits
    let mut year = 0i32;
    for (i, ch) in s.chars().enumerate() {
        if ch.is_ascii_digit() {
            if i + 4 <= s.len() {
                let slice = &s[i..i + 4];
                if let Ok(y) = slice.parse::<i32>() { year = y; break; }
            }
        }
    }
    if year == 0 { chrono::Local::now().year() } else { year }
}

fn parse_arxiv_atom(atom: &str) -> Vec<ResearchSource> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let open = "<entry>";
    let close = "</entry>";
    while let Some(i) = atom[start..].find(open) {
        let i_abs = start + i;
        if let Some(j) = atom[i_abs..].find(close) {
            let block = &atom[i_abs + open.len()..i_abs + j];
            let title = extract_first_tag_text(block, "title").unwrap_or_else(|| "Untitled".to_string());
            let url = extract_link_href(block).unwrap_or_else(|| extract_first_tag_text(block, "id").unwrap_or_default());
            let summary = extract_first_tag_text(block, "summary").unwrap_or_default();
            let year = extract_first_tag_text(block, "published").or_else(|| extract_first_tag_text(block, "updated")).map(|v| parse_year(&v)).unwrap_or_else(|| chrono::Local::now().year());
            let authors = extract_authors(block);
            // Try to extract arXiv id from the URL or id text
            let mut id = String::new();
            if let Some(id_text) = extract_first_tag_text(block, "id") {
                id = id_text.rsplit('/').next().unwrap_or("").to_string();
            }
            if id.is_empty() {
                id = url.rsplit('/').next().unwrap_or("").to_string();
            }
            out.push(ResearchSource { id, url, title, year, authors, summary, score: 0.0 });
            start = i_abs + j + close.len();
            continue;
        }
        break;
    }
    // Score by baseline rank, recency, and simple query-term matches
    out
}

pub async fn query_arxiv_online(
    cwd: &Path,
    topic: &str,
    year_range: Option<(i32, i32)>,
    max_results: usize,
) -> Vec<ResearchSource> {
    // Build query string
    let mut q = topic.trim().to_string();
    if let Some((y1, y2)) = year_range { q.push_str(&format!(" years:{}-{}", y1, y2)); }
    let search = urlencoding::encode(&format!("all:{}", q)).into_owned();
    let url = format!(
        "{}?search_query={}&start=0&max_results={}&sortBy=relevance&sortOrder=descending",
        ARXIV_API_URL,
        search,
        max_results.max(1).min(100)
    );

    let client = reqwest::Client::builder()
        .user_agent("Codex-Research/0.1 (+https://github.com/openai/codex)")
        .timeout(std::time::Duration::from_secs(8))
        .build();
    let atom = match client {
        Ok(c) => {
            let mut attempt = 0;
            let mut last: Option<String> = None;
            while attempt < 3 {
                match c.get(&url).send().await {
                    Ok(resp) => match resp.error_for_status() {
                        Ok(ok) => match ok.text().await { Ok(text) => { last = Some(text); break; }, Err(_) => {} },
                        Err(_) => {}
                    },
                    Err(_) => {}
                }
                attempt += 1;
                let backoff_ms = match attempt { 1 => 300, 2 => 800, _ => 1500 };
                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
            }
            last
        }
        Err(_) => None,
    };

    if let Some(atom) = atom {
        let mut items = parse_arxiv_atom(&atom);
        // Compute simple query-aware scores
        let topic_norm = topic.to_lowercase();
        let terms: Vec<&str> = topic_norm
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|t| !t.is_empty())
            .collect();
        let now_y = chrono::Local::now().year();
        for (rank_idx, it) in items.iter_mut().enumerate() {
            let base = 1.0f64 - (rank_idx as f64) * 0.01;
            let recency = 0.002f64 * (it.year.max(0) as f64 - now_y as f64);
            let lower_title = it.title.to_lowercase();
            let lower_sum = it.summary.to_lowercase();
            let mut match_score = 0.0f64;
            for t in &terms {
                if lower_title.contains(t) { match_score += 0.08; }
                if lower_sum.contains(t) { match_score += 0.03; }
            }
            it.score = (base + recency + match_score).clamp(-1.0, 10.0);
        }
        if !items.is_empty() {
            let _ = save_cached_sources(cwd, topic, year_range, &items);
        }
        // Dedup + stable sort: by score desc, then year desc, then title asc; dedup by id then url
        items.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal).then_with(|| b.year.cmp(&a.year)).then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase())));
        let mut seen = std::collections::HashSet::new();
        items.retain(|s| {
            let key = if !s.id.is_empty() { s.id.clone() } else { s.url.clone() };
            seen.insert(key)
        });
        items
    } else {
        // Fallback to offline cache
        query_arxiv_offline(cwd, topic, year_range)
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ResearchState {
    pub used_urls: Vec<String>,
}

pub fn load_research_state(ep_dir: &Path) -> ResearchState {
    let path = ep_dir.join("research_state.json");
    fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<ResearchState>(&s).ok())
        .unwrap_or_default()
}

pub fn save_research_state(ep_dir: &Path, state: &ResearchState) -> std::io::Result<()> {
    let path = ep_dir.join("research_state.json");
    fs::write(path, serde_json::to_string_pretty(state).unwrap_or_else(|_| "{}".to_string()))
}
