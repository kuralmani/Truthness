//! Turn a URL (or pasted text) into the material the model will fact-check.

use std::net::IpAddr;

use anyhow::{anyhow, bail, Context, Result};
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use url::Url;

const BROWSER_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36";
const IOS_UA: &str = "com.google.ios.youtube/20.10.4 (iPhone16,2; U; CPU iOS 18_3_2 like Mac OS X;)";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Source {
    /// "youtube", "webpage" or "text".
    pub kind: String,
    pub url: Option<String>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub site: Option<String>,
    pub thumbnail: Option<String>,
    pub duration_secs: Option<u64>,
    /// For videos: whether we actually got the spoken words.
    pub has_transcript: bool,
    /// Caveats about what was (not) available; shown to the user and the model.
    pub notes: Vec<String>,
    /// The text given to the model. Not sent back to clients.
    #[serde(skip_serializing, default)]
    pub content: String,
}

/// What a client may send instead of (or along with) a URL.
#[derive(Debug, Default)]
pub struct Provided {
    pub text: Option<String>,
    pub title: Option<String>,
}

pub struct Extractor {
    pub http: reqwest::Client,
    /// Same as `http` but never follows redirects (we re-check each hop).
    pub http_no_redirect: reqwest::Client,
    pub allow_private_hosts: bool,
    pub max_chars: usize,
}

fn truncate(text: &str, max: usize) -> (String, bool) {
    if text.chars().count() <= max {
        (text.to_string(), false)
    } else {
        (text.chars().take(max).collect(), true)
    }
}

fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn youtube_id(url: &Url) -> Option<String> {
    let host = url.host_str()?.trim_start_matches("www.").trim_start_matches("m.").trim_start_matches("music.");
    let valid = |id: &str| (id.len() == 11 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')).then(|| id.to_string());
    let segs: Vec<&str> = url.path_segments().map(|s| s.collect()).unwrap_or_default();
    match host {
        "youtu.be" => valid(segs.first()?),
        "youtube.com" | "youtube-nocookie.com" => match segs.first().copied() {
            Some("watch") => url.query_pairs().find(|(k, _)| k == "v").and_then(|(_, v)| valid(&v)),
            Some("shorts" | "embed" | "live" | "v") => valid(segs.get(1)?),
            _ => None,
        },
        _ => None,
    }
}

/// Stable key for caching: the same video/page maps to the same string.
pub fn canonical_url(raw: &str) -> Result<Url> {
    let mut url = Url::parse(raw.trim()).or_else(|_| Url::parse(&format!("https://{}", raw.trim()))).context("that does not look like a URL")?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("only http(s) URLs are supported");
    }
    if let Some(id) = youtube_id(&url) {
        return Ok(Url::parse(&format!("https://www.youtube.com/watch?v={id}"))?);
    }
    url.set_fragment(None);
    Ok(url)
}

fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified() || v4.is_broadcast()
                || v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64 // CGNAT
        }
        IpAddr::V6(v6) => {
            v6.is_loopback() || v6.is_unspecified()
                || (v6.segments()[0] & 0xfe00) == 0xfc00 // unique local
                || (v6.segments()[0] & 0xffc0) == 0xfe80 // link local
                || v6.to_ipv4_mapped().map(|v4| is_private(IpAddr::V4(v4))).unwrap_or(false)
        }
    }
}

impl Extractor {
    pub async fn extract(&self, url: Option<&str>, provided: Provided) -> Result<Source> {
        let text = provided.text.as_deref().map(str::trim).filter(|t| !t.is_empty());
        let Some(raw_url) = url.map(str::trim).filter(|u| !u.is_empty()) else {
            let text = text.ok_or_else(|| anyhow!("provide a URL or some text to check"))?;
            return Ok(self.finish(Source { kind: "text".into(), title: provided.title, ..Default::default() }, text));
        };
        let url = canonical_url(raw_url)?;

        if let Some(id) = youtube_id(&url) {
            return self.youtube(&id, url.as_str()).await;
        }

        // The browser extension sends the page text it can already see, which
        // also works for pages behind a login.
        if let Some(text) = text {
            let src = Source {
                kind: "webpage".into(),
                url: Some(url.to_string()),
                title: provided.title,
                site: url.host_str().map(str::to_string),
                ..Default::default()
            };
            return Ok(self.finish(src, text));
        }
        self.webpage(url).await
    }

    fn finish(&self, mut src: Source, text: &str) -> Source {
        let (content, cut) = truncate(text, self.max_chars);
        if cut {
            src.notes.push(format!("The content was long; only the first {} characters were checked.", self.max_chars));
        }
        src.content = content;
        src
    }

    // ------------------------------------------------------------- YouTube

    async fn youtube(&self, id: &str, url: &str) -> Result<Source> {
        let mut src = Source {
            kind: "youtube".into(),
            url: Some(url.to_string()),
            site: Some("YouTube".into()),
            thumbnail: Some(format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg")),
            ..Default::default()
        };
        let mut description = String::new();
        let mut transcript = None;

        match self.youtube_player(id).await {
            Ok(player) => {
                let d = &player["videoDetails"];
                src.title = d["title"].as_str().map(str::to_string);
                src.author = d["author"].as_str().map(str::to_string);
                src.duration_secs = d["lengthSeconds"].as_str().and_then(|s| s.parse().ok());
                description = d["shortDescription"].as_str().unwrap_or_default().to_string();
                let status = player["playabilityStatus"]["status"].as_str().unwrap_or("OK");
                if status != "OK" {
                    src.notes.push(format!("YouTube reported this video as {status}."));
                }
                match self.youtube_transcript(&player).await {
                    Ok(t) => transcript = t,
                    Err(e) => tracing::warn!("transcript fetch failed for {id}: {e:#}"),
                }
            }
            Err(e) => tracing::warn!("youtube player request failed for {id}: {e:#}"),
        }

        if src.title.is_none() {
            // Public oEmbed endpoint as a metadata fallback.
            let oembed = format!("https://www.youtube.com/oembed?format=json&url={}", urlencode(url));
            if let Ok(resp) = self.http.get(oembed).send().await {
                if let Ok(v) = resp.json::<Value>().await {
                    src.title = v["title"].as_str().map(str::to_string);
                    src.author = v["author_name"].as_str().map(str::to_string);
                }
            }
        }
        if src.title.is_none() && transcript.is_none() {
            bail!("could not load this YouTube video (it may be private, removed or region-locked)");
        }

        let mut content = String::new();
        if !description.trim().is_empty() {
            content.push_str(&format!("VIDEO DESCRIPTION:\n{}\n\n", description.trim()));
        }
        match transcript {
            Some(t) => {
                src.has_transcript = true;
                content.push_str(&format!("TRANSCRIPT (auto-generated captions may contain errors):\n{t}"));
            }
            None => {
                src.notes.push(
                    "No captions were available, so the spoken words could not be read. This check is based on the title and description only (unless the model can watch the video itself)."
                        .into(),
                );
                content.push_str("TRANSCRIPT: not available.");
            }
        }
        Ok(self.finish(src, &content))
    }

    /// YouTube's web caption URLs need a browser-minted token; the mobile
    /// player endpoint returns ones that work from a server.
    async fn youtube_player(&self, id: &str) -> Result<Value> {
        let body = json!({
            "context": {"client": {
                "clientName": "IOS", "clientVersion": "20.10.4", "hl": "en",
                "deviceMake": "Apple", "deviceModel": "iPhone16,2",
                "osName": "iPhone", "osVersion": "18.3.2.22D82",
            }},
            "videoId": id,
        });
        let resp = self
            .http
            .post("https://www.youtube.com/youtubei/v1/player?prettyPrint=false")
            .header("User-Agent", IOS_UA)
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
        Ok(resp.json().await?)
    }

    async fn youtube_transcript(&self, player: &Value) -> Result<Option<String>> {
        let Some(tracks) = player["captions"]["playerCaptionsTracklistRenderer"]["captionTracks"].as_array() else {
            return Ok(None);
        };
        let lang = |t: &Value| t["languageCode"].as_str().unwrap_or_default().to_string();
        let is_asr = |t: &Value| t["kind"] == "asr";
        // Prefer human-written English, then any human-written track, then
        // auto-generated English, then whatever exists.
        let track = tracks
            .iter()
            .find(|t| lang(t).starts_with("en") && !is_asr(t))
            .or_else(|| tracks.iter().find(|t| !is_asr(t)))
            .or_else(|| tracks.iter().find(|t| lang(t).starts_with("en")))
            .or_else(|| tracks.first());
        let Some(base) = track.and_then(|t| t["baseUrl"].as_str()) else { return Ok(None) };
        let body = self.http.get(format!("{base}&fmt=json3")).header("User-Agent", IOS_UA).send().await?.text().await?;
        let v: Value = serde_json::from_str(&body).context("caption track was empty or not JSON")?;

        let mut lines: Vec<(u64, String)> = Vec::new();
        for ev in v["events"].as_array().map(Vec::as_slice).unwrap_or_default() {
            let text: String = ev["segs"].as_array().map(|s| s.iter().filter_map(|x| x["utf8"].as_str()).collect()).unwrap_or_default();
            let text = squash(&text);
            if !text.is_empty() {
                lines.push((ev["tStartMs"].as_u64().unwrap_or(0) / 1000, text));
            }
        }
        if lines.is_empty() {
            return Ok(None);
        }
        // Group caption fragments into ~20 second paragraphs with a timestamp.
        let mut out = String::new();
        let mut block_start = None;
        for (t, text) in lines {
            if block_start.map(|s| t >= s + 20).unwrap_or(true) {
                out.push_str(&format!("{}[{}:{:02}] ", if out.is_empty() { "" } else { "\n" }, t / 60, t % 60));
                block_start = Some(t);
            } else {
                out.push(' ');
            }
            out.push_str(&text);
        }
        Ok(Some(out))
    }

    // ------------------------------------------------------------ Web page

    async fn guard(&self, url: &Url) -> Result<()> {
        if !matches!(url.scheme(), "http" | "https") {
            bail!("only http(s) URLs are supported");
        }
        if self.allow_private_hosts {
            return Ok(());
        }
        let host = url.host_str().ok_or_else(|| anyhow!("URL has no host"))?;
        let port = url.port_or_known_default().unwrap_or(443);
        let addrs = tokio::net::lookup_host((host, port)).await.with_context(|| format!("could not resolve {host}"))?;
        for a in addrs {
            if is_private(a.ip()) {
                bail!("refusing to fetch a private or local address ({host})");
            }
        }
        Ok(())
    }

    async fn fetch_html(&self, mut url: Url) -> Result<(Url, String)> {
        for _ in 0..6 {
            self.guard(&url).await?;
            let resp = self
                .http_no_redirect
                .get(url.clone())
                .header("User-Agent", BROWSER_UA)
                .header("Accept", "text/html,application/xhtml+xml")
                .header("Accept-Language", "en-US,en;q=0.8")
                .send()
                .await
                .with_context(|| format!("could not reach {}", url.host_str().unwrap_or("the site")))?;
            if resp.status().is_redirection() {
                let loc = resp.headers().get("location").and_then(|l| l.to_str().ok()).ok_or_else(|| anyhow!("redirect without a location"))?;
                url = url.join(loc)?;
                continue;
            }
            if !resp.status().is_success() {
                bail!("the page returned HTTP {}", resp.status());
            }
            let ctype = resp.headers().get("content-type").and_then(|c| c.to_str().ok()).unwrap_or("text/html").to_lowercase();
            if !(ctype.contains("html") || ctype.contains("text/plain")) {
                bail!("unsupported content type: {ctype}");
            }
            let bytes = resp.bytes().await?;
            if bytes.len() > 5_000_000 {
                bail!("the page is too large to check");
            }
            return Ok((url, String::from_utf8_lossy(&bytes).to_string()));
        }
        bail!("too many redirects")
    }

    async fn webpage(&self, url: Url) -> Result<Source> {
        let (final_url, html) = self.fetch_html(url).await?;
        let mut src = Source {
            kind: "webpage".into(),
            url: Some(final_url.to_string()),
            site: final_url.host_str().map(|h| h.trim_start_matches("www.").to_string()),
            ..Default::default()
        };
        let text = {
            let doc = Html::parse_document(&html);
            let sel = |s: &str| Selector::parse(s).expect("valid selector");
            let meta = |q: &str| {
                doc.select(&sel(q)).next().and_then(|e| e.value().attr("content")).map(squash).filter(|s| !s.is_empty())
            };
            src.title = meta("meta[property='og:title']")
                .or_else(|| doc.select(&sel("title")).next().map(|t| squash(&t.text().collect::<String>())))
                .filter(|s| !s.is_empty());
            src.author = meta("meta[name='author']").or_else(|| meta("meta[property='article:author']"));
            src.thumbnail = meta("meta[property='og:image']").filter(|u| u.starts_with("http"));
            if let Some(site) = meta("meta[property='og:site_name']") {
                src.site = Some(site);
            }
            let description = meta("meta[property='og:description']").or_else(|| meta("meta[name='description']"));

            // Take the densest of the usual "main content" containers.
            let blocks = sel("h1, h2, h3, p, li, blockquote, figcaption");
            let body_text = |root: scraper::ElementRef| {
                root.select(&blocks).map(|e| squash(&e.text().collect::<String>())).filter(|t| t.len() > 1).collect::<Vec<_>>().join("\n")
            };
            let mut best = String::new();
            for root in doc.select(&sel("article, main, [role='main'], body")) {
                let t = body_text(root);
                // Prefer a focused container unless it is nearly empty.
                if root.value().name() != "body" && t.len() > 600 && (best.is_empty() || t.len() > best.len()) {
                    best = t;
                } else if root.value().name() == "body" && best.is_empty() {
                    best = t;
                }
            }
            let mut text = String::new();
            if let Some(d) = description {
                text.push_str(&format!("PAGE DESCRIPTION: {d}\n\n"));
            }
            text.push_str(&best);
            text
        };
        if text.trim().len() < 200 {
            src.notes.push(
                "Very little readable text was found on this page (it may need a login or load its content with JavaScript). Try the browser extension, which reads the page as you see it."
                    .into(),
            );
        }
        if text.trim().is_empty() && src.title.is_none() {
            bail!("no readable content was found at that URL");
        }
        Ok(self.finish(src, &text))
    }
}

fn urlencode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn youtube_ids() {
        for u in [
            "https://www.youtube.com/shorts/pzftGaUqAwA",
            "https://youtu.be/pzftGaUqAwA?t=3",
            "https://m.youtube.com/watch?feature=share&v=pzftGaUqAwA",
            "youtube.com/embed/pzftGaUqAwA",
        ] {
            assert_eq!(canonical_url(u).unwrap().as_str(), "https://www.youtube.com/watch?v=pzftGaUqAwA", "{u}");
        }
        assert!(youtube_id(&Url::parse("https://www.youtube.com/@channel").unwrap()).is_none());
        assert!(canonical_url("file:///etc/passwd").is_err());
    }

    #[test]
    fn private_addresses() {
        for ip in ["127.0.0.1", "10.1.2.3", "192.168.0.1", "169.254.169.254", "100.64.0.1", "::1", "fd00::1", "::ffff:10.0.0.1"] {
            assert!(is_private(ip.parse().unwrap()), "{ip}");
        }
        assert!(!is_private("8.8.8.8".parse().unwrap()));
    }
}
