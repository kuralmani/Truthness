mod config;
mod extract;
mod providers;
mod verdict;

use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Context, Result};
use axum::{
    extract::{Path, State},
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

use extract::{Extractor, Provided, Source};
use providers::{AnalysisRequest, Provider, ProviderInfo};
use verdict::Verdict;

#[derive(RustEmbed)]
#[folder = "web/"]
struct WebAssets;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CheckResult {
    source: Source,
    verdict: Verdict,
    provider: ProviderInfo,
    /// Unix seconds.
    checked_at: u64,
    elapsed_ms: u64,
    #[serde(default)]
    cached: bool,
}

#[derive(Debug, Clone, Serialize)]
struct Job {
    id: String,
    /// "queued" | "extracting" | "analyzing" | "done" | "error"
    status: &'static str,
    message: String,
    result: Option<CheckResult>,
    error: Option<String>,
    #[serde(skip)]
    created: Instant,
}

#[derive(Debug, Deserialize)]
struct CheckRequest {
    url: Option<String>,
    text: Option<String>,
    title: Option<String>,
    provider: Option<String>,
    /// Skip the cache and re-run the analysis.
    #[serde(default)]
    force: bool,
}

struct App {
    providers: BTreeMap<String, Arc<dyn Provider>>,
    default_provider: Option<String>,
    extractor: Extractor,
    jobs: Mutex<HashMap<String, Job>>,
    limiter: Semaphore,
    cache_dir: PathBuf,
    cache_ttl: Duration,
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// YYYY-MM-DD (UTC) without pulling in a date crate.
fn today() -> String {
    let z = (now_unix() / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}")
}

impl App {
    fn set_job(&self, id: &str, f: impl FnOnce(&mut Job)) {
        if let Some(job) = self.jobs.lock().unwrap().get_mut(id) {
            f(job);
        }
    }

    fn cache_path(&self, key_material: &str, provider: &ProviderInfo) -> PathBuf {
        let mut h = Sha256::new();
        h.update(key_material.as_bytes());
        h.update([0]);
        h.update(provider.id.as_bytes());
        h.update([0]);
        h.update(provider.model.as_bytes());
        self.cache_dir.join(format!("{:x}.json", h.finalize()))
    }

    async fn cache_get(&self, path: &PathBuf) -> Option<CheckResult> {
        if self.cache_ttl.is_zero() {
            return None;
        }
        let raw = tokio::fs::read(path).await.ok()?;
        let mut r: CheckResult = serde_json::from_slice(&raw).ok()?;
        (now_unix().saturating_sub(r.checked_at) < self.cache_ttl.as_secs()).then(|| {
            r.cached = true;
            r
        })
    }

    async fn run(self: Arc<Self>, id: String, req: CheckRequest, provider: Arc<dyn Provider>) {
        let outcome = self.check(&id, req, provider).await;
        self.set_job(&id, |job| match outcome {
            Ok(result) => {
                job.status = "done";
                job.message = "Done".into();
                job.result = Some(result);
            }
            Err(e) => {
                tracing::warn!("check {id} failed: {e:#}");
                job.status = "error";
                job.error = Some(format!("{e:#}"));
            }
        });
    }

    async fn check(&self, id: &str, req: CheckRequest, provider: Arc<dyn Provider>) -> Result<CheckResult> {
        let info = provider.info();
        let key_material = match req.url.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
            Some(u) => extract::canonical_url(u)?.to_string(),
            None => format!("text:{}", req.text.as_deref().unwrap_or_default().trim()),
        };
        let cache_path = self.cache_path(&key_material, &info);
        if !req.force {
            if let Some(hit) = self.cache_get(&cache_path).await {
                return Ok(hit);
            }
        }

        let _permit = self.limiter.acquire().await?;
        let started = Instant::now();

        self.set_job(id, |j| {
            j.status = "extracting";
            j.message = "Reading the content…".into();
        });
        let source = self.extractor.extract(req.url.as_deref(), Provided { text: req.text, title: req.title }).await?;

        self.set_job(id, |j| {
            j.status = "analyzing";
            j.message = if info.grounded {
                format!("{} is checking the claims against the web…", info.label)
            } else {
                format!("{} is checking the claims…", info.label)
            };
        });
        let analysis = AnalysisRequest {
            system: verdict::system_prompt(info.grounded, &today()),
            user: verdict::user_prompt(&source),
            video_url: (source.kind == "youtube").then(|| source.url.clone()).flatten(),
        };
        let raw = provider.analyze(&analysis).await?;
        let verdict = verdict::parse(&raw)?;

        let result = CheckResult {
            source,
            verdict,
            provider: info,
            checked_at: now_unix(),
            elapsed_ms: started.elapsed().as_millis() as u64,
            cached: false,
        };
        if !self.cache_ttl.is_zero() {
            if let Err(e) = async {
                tokio::fs::create_dir_all(&self.cache_dir).await?;
                tokio::fs::write(&cache_path, serde_json::to_vec(&result)?).await?;
                anyhow::Ok(())
            }
            .await
            {
                tracing::warn!("could not write cache: {e:#}");
            }
        }
        Ok(result)
    }
}

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({"ok": true, "name": "truthness", "version": env!("CARGO_PKG_VERSION")}))
}

async fn list_providers(State(app): State<Arc<App>>) -> Json<serde_json::Value> {
    let list: Vec<ProviderInfo> = app.providers.values().map(|p| p.info()).collect();
    Json(json!({"providers": list, "default": app.default_provider}))
}

async fn start_check(State(app): State<Arc<App>>, Json(req): Json<CheckRequest>) -> Result<Json<serde_json::Value>, ApiError> {
    let bad = |m: &str| ApiError(StatusCode::BAD_REQUEST, m.to_string());
    let has_url = req.url.as_deref().map(|u| !u.trim().is_empty()).unwrap_or(false);
    let has_text = req.text.as_deref().map(|t| !t.trim().is_empty()).unwrap_or(false);
    if !has_url && !has_text {
        return Err(bad("provide a URL or some text to check"));
    }
    if req.text.as_deref().map(|t| t.len() > 400_000).unwrap_or(false) {
        return Err(bad("that text is too long"));
    }
    let provider_id = req
        .provider
        .clone()
        .filter(|p| !p.is_empty())
        .or_else(|| app.default_provider.clone())
        .ok_or_else(|| ApiError(StatusCode::SERVICE_UNAVAILABLE, "no AI provider is configured on the server; see truthness.example.toml".into()))?;
    let provider = app.providers.get(&provider_id).cloned().ok_or_else(|| bad(&format!("unknown provider '{provider_id}'")))?;

    let id = uuid::Uuid::new_v4().to_string();
    {
        let mut jobs = app.jobs.lock().unwrap();
        jobs.retain(|_, j| j.created.elapsed() < Duration::from_secs(3600));
        if jobs.values().filter(|j| !matches!(j.status, "done" | "error")).count() >= 20 {
            return Err(ApiError(StatusCode::TOO_MANY_REQUESTS, "the server is busy; try again in a minute".into()));
        }
        jobs.insert(
            id.clone(),
            Job { id: id.clone(), status: "queued", message: "Waiting to start…".into(), result: None, error: None, created: Instant::now() },
        );
    }
    tokio::spawn(app.clone().run(id.clone(), req, provider));
    Ok(Json(json!({"id": id})))
}

async fn get_check(State(app): State<Arc<App>>, Path(id): Path<String>) -> Result<Json<Job>, ApiError> {
    app.jobs
        .lock()
        .unwrap()
        .get(&id)
        .cloned()
        .map(Json)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "no such check (the server may have restarted)".into()))
}

async fn static_file(uri: Uri) -> Response {
    let path = match uri.path().trim_start_matches('/') {
        "" => "index.html",
        p => p,
    };
    match WebAssets::get(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.as_ref().to_string()), (header::CACHE_CONTROL, "no-cache".to_string())], file.data).into_response()
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    let config_path = std::env::args().nth(1);
    let cfg = config::Config::load(config_path.as_deref())?;

    let client = |redirects: bool| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(if redirects { 600 } else { 30 }))
            .connect_timeout(Duration::from_secs(15))
            .redirect(if redirects { reqwest::redirect::Policy::limited(5) } else { reqwest::redirect::Policy::none() })
            .build()
    };
    let http = client(true)?;

    let providers: BTreeMap<String, Arc<dyn Provider>> =
        cfg.providers.iter().map(|(id, pc)| (id.clone(), providers::build(id, pc, http.clone()))).collect();
    let default_provider = match &cfg.server.default_provider {
        Some(p) if providers.contains_key(p) => Some(p.clone()),
        Some(p) => return Err(anyhow!("default_provider '{p}' is not defined under [providers]")),
        None => providers.keys().next().cloned(),
    };

    if providers.is_empty() {
        tracing::warn!("no AI provider found. Install the claude, codex or gemini CLI, set ANTHROPIC_API_KEY / GEMINI_API_KEY, or create truthness.toml");
    }
    for p in providers.values() {
        let i = p.info();
        tracing::info!("provider {:<14} {} [{}]{}", i.id, i.label, i.model, if i.grounded { "" } else { " (no web search)" });
    }

    let app = Arc::new(App {
        providers,
        default_provider,
        extractor: Extractor {
            http,
            http_no_redirect: client(false)?,
            allow_private_hosts: cfg.server.allow_private_hosts,
            max_chars: cfg.server.max_content_chars,
        },
        jobs: Mutex::new(HashMap::new()),
        limiter: Semaphore::new(cfg.server.max_concurrent.max(1)),
        cache_dir: PathBuf::from(&cfg.server.cache_dir),
        cache_ttl: Duration::from_secs(cfg.server.cache_ttl_hours * 3600),
    });

    // No CORS headers on purpose: the website is same-origin and the browser
    // extension calls from its background worker. Other sites cannot drive
    // this API (and spend your model quota) from a visitor's browser.
    let router = Router::new()
        .route("/api/health", get(health))
        .route("/api/providers", get(list_providers))
        .route("/api/check", post(start_check))
        .route("/api/check/{id}", get(get_check))
        .fallback(static_file)
        .with_state(app);

    let listener = tokio::net::TcpListener::bind(&cfg.server.bind).await.with_context(|| format!("binding {}", cfg.server.bind))?;
    tracing::info!("Truthness is running at http://{}", listener.local_addr()?);
    axum::serve(listener, router).await?;
    Ok(())
}
