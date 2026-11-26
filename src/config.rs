//! Configuration: `truthness.toml` (optional) plus environment auto-detection.

use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub bind: String,
    /// Maximum number of analyses running at the same time.
    pub max_concurrent: usize,
    pub cache_dir: String,
    /// 0 disables the cache.
    pub cache_ttl_hours: u64,
    /// Allow fetching URLs that resolve to private/loopback addresses.
    pub allow_private_hosts: bool,
    pub default_provider: Option<String>,
    /// Maximum characters of page/transcript text sent to the model.
    pub max_content_chars: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:8787".into(),
            max_concurrent: 2,
            cache_dir: home_dir().map(|h| format!("{h}/cache")).unwrap_or_else(|| ".truthness/cache".into()),
            cache_ttl_hours: 168,
            allow_private_hosts: false,
            default_provider: None,
            max_content_chars: 40_000,
        }
    }
}

fn d_claude() -> String { "claude".into() }
fn d_timeout() -> u64 { 300 }
fn d_true() -> bool { true }
fn d_anthropic_key() -> String { "ANTHROPIC_API_KEY".into() }
fn d_anthropic_model() -> String { "claude-opus-5-5".into() }
fn d_anthropic_url() -> String { "https://api.anthropic.com".into() }
fn d_web_search_tool() -> String { "web_search_20260209".into() }
fn d_gemini_key() -> String { "GEMINI_API_KEY".into() }
fn d_gemini_model() -> String { "gemini-2.5-flash".into() }

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ProviderConfig {
    /// The local `claude` CLI (Claude Code) in headless mode, using your existing login.
    ClaudeCli {
        #[serde(default = "d_claude")]
        command: String,
        model: Option<String>,
        #[serde(default = "d_timeout")]
        timeout_secs: u64,
        label: Option<String>,
    },
    /// Anthropic Messages API with the web search server tool.
    Anthropic {
        #[serde(default = "d_anthropic_key")]
        api_key_env: String,
        #[serde(default = "d_anthropic_model")]
        model: String,
        #[serde(default = "d_anthropic_url")]
        base_url: String,
        #[serde(default = "d_true")]
        web_search: bool,
        #[serde(default = "d_web_search_tool")]
        web_search_tool: String,
        /// Let the API re-run a declined request on a fallback model.
        #[serde(default = "d_true")]
        fallbacks: bool,
        label: Option<String>,
    },
    /// Google Gemini API with Google Search grounding; can watch YouTube videos natively.
    Gemini {
        #[serde(default = "d_gemini_key")]
        api_key_env: String,
        #[serde(default = "d_gemini_model")]
        model: String,
        #[serde(default = "d_true")]
        google_search: bool,
        #[serde(default = "d_true")]
        native_video: bool,
        label: Option<String>,
    },
    /// Any OpenAI-compatible chat completions endpoint (OpenAI, Ollama, LM Studio, vLLM, ...).
    /// No live web search: verdicts rely on the model's own knowledge.
    OpenaiCompatible {
        base_url: String,
        api_key_env: Option<String>,
        model: String,
        label: Option<String>,
    },
    /// Any command-line tool that reads the prompt on stdin and prints the answer on stdout.
    Command {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default = "d_timeout")]
        timeout_secs: u64,
        /// Set to true if the tool can search the web itself.
        #[serde(default)]
        grounded: bool,
        label: Option<String>,
    },
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub server: ServerConfig,
    pub providers: BTreeMap<String, ProviderConfig>,
}

fn on_path(cmd: &str) -> bool {
    if cmd.contains('/') {
        return Path::new(cmd).exists();
    }
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
        .unwrap_or(false)
}

/// `~/.truthness`: where the installer puts things and where the cache lives by default.
fn home_dir() -> Option<String> {
    if let Ok(dir) = std::env::var("TRUTHNESS_HOME") {
        return Some(dir);
    }
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).ok()?;
    Some(Path::new(&home).join(".truthness").to_string_lossy().into_owned())
}

fn env_set(name: &str) -> bool {
    std::env::var(name).map(|v| !v.trim().is_empty()).unwrap_or(false)
}

impl Config {
    pub fn load(path: Option<&str>) -> Result<Self> {
        // Explicit path, then TRUTHNESS_CONFIG, then ./truthness.toml, then ~/.truthness/truthness.toml.
        let path = path
            .map(str::to_string)
            .or_else(|| std::env::var("TRUTHNESS_CONFIG").ok())
            .unwrap_or_else(|| {
                let local = "truthness.toml".to_string();
                match home_dir().map(|h| format!("{h}/truthness.toml")) {
                    Some(global) if !Path::new(&local).exists() && Path::new(&global).exists() => global,
                    _ => local,
                }
            });
        let mut cfg = if Path::new(&path).exists() {
            let raw = std::fs::read_to_string(&path).with_context(|| format!("reading {path}"))?;
            tracing::info!("loaded config from {path}");
            toml::from_str::<Config>(&raw).with_context(|| format!("parsing {path}"))?
        } else {
            Config::default()
        };
        if let Ok(bind) = std::env::var("TRUTHNESS_BIND") {
            cfg.server.bind = bind;
        }
        if cfg.providers.is_empty() {
            cfg.autodetect();
        }
        Ok(cfg)
    }

    /// With no providers configured, enable whatever is usable on this machine.
    fn autodetect(&mut self) {
        if on_path("claude") {
            self.providers.insert(
                "claude-cli".into(),
                ProviderConfig::ClaudeCli { command: d_claude(), model: None, timeout_secs: d_timeout(), label: None },
            );
        }
        if env_set("ANTHROPIC_API_KEY") {
            self.providers.insert(
                "anthropic".into(),
                ProviderConfig::Anthropic {
                    api_key_env: d_anthropic_key(),
                    model: d_anthropic_model(),
                    base_url: d_anthropic_url(),
                    web_search: true,
                    web_search_tool: d_web_search_tool(),
                    fallbacks: true,
                    label: None,
                },
            );
        }
        if env_set("GEMINI_API_KEY") {
            self.providers.insert(
                "gemini".into(),
                ProviderConfig::Gemini {
                    api_key_env: d_gemini_key(),
                    model: d_gemini_model(),
                    google_search: true,
                    native_video: true,
                    label: None,
                },
            );
        }
        // Other AI command-line tools, driven through the generic command provider.
        if on_path("codex") {
            self.providers.insert(
                "codex".into(),
                ProviderConfig::Command {
                    command: "codex".into(),
                    args: ["exec", "--sandbox", "read-only", "--skip-git-repo-check", "-"].map(String::from).to_vec(),
                    timeout_secs: d_timeout(),
                    grounded: false,
                    label: Some("Codex (local CLI)".into()),
                },
            );
        }
        if on_path("gemini") {
            self.providers.insert(
                "gemini-cli".into(),
                ProviderConfig::Command {
                    command: "gemini".into(),
                    args: ["-p", "Follow the instructions given on stdin."].map(String::from).to_vec(),
                    timeout_secs: d_timeout(),
                    grounded: false,
                    label: Some("Gemini (local CLI)".into()),
                },
            );
        }
        // Prefer the options that search the web and are known to work best.
        if self.server.default_provider.is_none() {
            self.server.default_provider = ["claude-cli", "anthropic", "gemini", "codex", "gemini-cli"]
                .into_iter()
                .find(|id| self.providers.contains_key(*id))
                .map(String::from);
        }
    }
}
