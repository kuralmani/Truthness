//! Pluggable model back-ends. To add one, implement [`Provider`] and register
//! it in [`build`].

use std::{process::Stdio, sync::Arc, time::Duration};

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::{io::AsyncWriteExt, process::Command};

use crate::config::ProviderConfig;

pub struct AnalysisRequest {
    pub system: String,
    pub user: String,
    /// Set for YouTube sources so providers with native video understanding can use it.
    pub video_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub id: String,
    pub label: String,
    pub model: String,
    /// True when the provider can search the live web for evidence.
    pub grounded: bool,
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn info(&self) -> ProviderInfo;
    /// Returns the model's raw text answer (expected to contain one JSON object).
    async fn analyze(&self, req: &AnalysisRequest) -> Result<String>;
}

pub fn build(id: &str, cfg: &ProviderConfig, http: reqwest::Client) -> Arc<dyn Provider> {
    let id = id.to_string();
    match cfg.clone() {
        ProviderConfig::ClaudeCli { command, model, timeout_secs, label } => {
            Arc::new(ClaudeCli { id, command, model, timeout_secs, label })
        }
        ProviderConfig::Anthropic { api_key_env, model, base_url, web_search, web_search_tool, fallbacks, label } => {
            Arc::new(Anthropic { id, api_key_env, model, base_url, web_search, web_search_tool, fallbacks, label, http })
        }
        ProviderConfig::Gemini { api_key_env, model, google_search, native_video, label } => {
            Arc::new(Gemini { id, api_key_env, model, google_search, native_video, label, http })
        }
        ProviderConfig::OpenaiCompatible { base_url, api_key_env, model, label } => {
            Arc::new(OpenAiCompatible { id, base_url, api_key_env, model, label, http })
        }
        ProviderConfig::Command { command, args, timeout_secs, grounded, label } => {
            Arc::new(CommandProvider { id, command, args, timeout_secs, grounded, label })
        }
    }
}

fn api_key(env: &str) -> Result<String> {
    std::env::var(env)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| anyhow!("environment variable {env} is not set"))
}

async fn error_for_status(resp: reqwest::Response, what: &str) -> Result<Value> {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let snippet: String = body.chars().take(600).collect();
        bail!("{what} returned HTTP {status}: {snippet}");
    }
    serde_json::from_str(&body).with_context(|| format!("{what} returned invalid JSON"))
}

/// Run a command with `input` on stdin in an empty scratch directory; return stdout.
async fn run_cli(command: &str, args: &[String], input: &str, timeout_secs: u64) -> Result<String> {
    let workdir = std::env::temp_dir().join("truthness-cli");
    tokio::fs::create_dir_all(&workdir).await.ok();
    let mut child = Command::new(command)
        .args(args)
        .current_dir(&workdir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("could not start `{command}` (is it installed and on PATH?)"))?;
    let mut stdin = child.stdin.take().context("no stdin")?;
    let input = input.to_string();
    tokio::spawn(async move {
        let _ = stdin.write_all(input.as_bytes()).await;
        let _ = stdin.shutdown().await;
    });
    let out = tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait_with_output())
        .await
        .map_err(|_| anyhow!("`{command}` timed out after {timeout_secs}s"))??;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.status.success() && stdout.trim().is_empty() {
        let stderr: String = String::from_utf8_lossy(&out.stderr).chars().take(600).collect();
        bail!("`{command}` exited with {}: {stderr}", out.status);
    }
    Ok(stdout)
}

// ---------------------------------------------------------------- claude CLI

struct ClaudeCli {
    id: String,
    command: String,
    model: Option<String>,
    timeout_secs: u64,
    label: Option<String>,
}

#[async_trait]
impl Provider for ClaudeCli {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: self.id.clone(),
            label: self.label.clone().unwrap_or_else(|| "Claude (local CLI)".into()),
            model: self.model.clone().unwrap_or_else(|| "CLI default".into()),
            grounded: true,
        }
    }

    async fn analyze(&self, req: &AnalysisRequest) -> Result<String> {
        // The content being checked is untrusted, so the CLI gets web tools only:
        // no shell, no file access, no MCP servers.
        let mut args: Vec<String> = [
            "-p",
            "--output-format",
            "json",
            "--tools",
            "WebSearch,WebFetch",
            "--allowedTools",
            "WebSearch,WebFetch",
            "--strict-mcp-config",
            "--system-prompt",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        args.push(req.system.clone());
        if let Some(m) = &self.model {
            args.push("--model".into());
            args.push(m.clone());
        }
        let stdout = run_cli(&self.command, &args, &req.user, self.timeout_secs).await?;
        let v: Value = serde_json::from_str(stdout.trim())
            .with_context(|| format!("unexpected claude CLI output: {}", stdout.chars().take(300).collect::<String>()))?;
        let result = v["result"].as_str().unwrap_or_default().to_string();
        if v["is_error"].as_bool().unwrap_or(false) || result.is_empty() {
            bail!("claude CLI failed: {}", if result.is_empty() { v["subtype"].as_str().unwrap_or("no result") } else { &result });
        }
        Ok(result)
    }
}

// ------------------------------------------------------------- Anthropic API

struct Anthropic {
    id: String,
    api_key_env: String,
    model: String,
    base_url: String,
    web_search: bool,
    web_search_tool: String,
    fallbacks: bool,
    label: Option<String>,
    http: reqwest::Client,
}

#[async_trait]
impl Provider for Anthropic {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: self.id.clone(),
            label: self.label.clone().unwrap_or_else(|| "Claude (Anthropic API)".into()),
            model: self.model.clone(),
            grounded: self.web_search,
        }
    }

    async fn analyze(&self, req: &AnalysisRequest) -> Result<String> {
        let key = api_key(&self.api_key_env)?;
        let mut messages = vec![json!({"role": "user", "content": req.user})];
        let mut text = String::new();
        // The server-side search loop can pause; resend to let it resume.
        for _ in 0..6 {
            let mut body = json!({
                "model": self.model,
                "max_tokens": 16000,
                "system": req.system,
                "messages": messages,
            });
            if self.web_search {
                body["tools"] = json!([{"type": self.web_search_tool, "name": "web_search", "max_uses": 8}]);
            }
            let mut rb = self
                .http
                .post(format!("{}/v1/messages", self.base_url.trim_end_matches('/')))
                .header("x-api-key", &key)
                .header("anthropic-version", "2026-10-21");
            if self.fallbacks {
                body["fallbacks"] = json!("default");
                rb = rb.header("anthropic-beta", "server-side-fallback-2026-07-01");
            }
            let v = error_for_status(rb.json(&body).send().await?, "Anthropic API").await?;
            let content = v["content"].as_array().cloned().unwrap_or_default();
            for block in &content {
                if block["type"] == "text" {
                    text.push_str(block["text"].as_str().unwrap_or_default());
                }
            }
            match v["stop_reason"].as_str().unwrap_or_default() {
                "pause_turn" => {
                    messages.truncate(1);
                    messages.push(json!({"role": "assistant", "content": content}));
                    text.clear();
                }
                "refusal" => bail!("the model declined to analyze this content"),
                "max_tokens" => bail!("the model's answer was cut off (max_tokens)"),
                _ => return Ok(text),
            }
        }
        bail!("Anthropic API: too many paused turns")
    }
}

// ---------------------------------------------------------------- Gemini API

struct Gemini {
    id: String,
    api_key_env: String,
    model: String,
    google_search: bool,
    native_video: bool,
    label: Option<String>,
    http: reqwest::Client,
}

#[async_trait]
impl Provider for Gemini {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: self.id.clone(),
            label: self.label.clone().unwrap_or_else(|| "Gemini (Google API)".into()),
            model: self.model.clone(),
            grounded: self.google_search,
        }
    }

    async fn analyze(&self, req: &AnalysisRequest) -> Result<String> {
        let key = api_key(&self.api_key_env)?;
        let mut parts = Vec::new();
        if let (true, Some(url)) = (self.native_video, &req.video_url) {
            parts.push(json!({"file_data": {"file_uri": url}}));
        }
        parts.push(json!({"text": req.user}));
        let mut body = json!({
            "system_instruction": {"parts": [{"text": req.system}]},
            "contents": [{"role": "user", "parts": parts}],
        });
        if self.google_search {
            body["tools"] = json!([{"google_search": {}}]);
        }
        let url = format!("https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent", self.model);
        let resp = self.http.post(url).header("x-goog-api-key", key).json(&body).send().await?;
        let v = error_for_status(resp, "Gemini API").await?;
        let text: String = v["candidates"][0]["content"]["parts"]
            .as_array()
            .map(|ps| ps.iter().filter_map(|p| p["text"].as_str()).collect())
            .unwrap_or_default();
        if text.is_empty() {
            bail!("Gemini returned no text (finish reason: {})", v["candidates"][0]["finishReason"]);
        }
        Ok(text)
    }
}

// --------------------------------------------------------- OpenAI-compatible

struct OpenAiCompatible {
    id: String,
    base_url: String,
    api_key_env: Option<String>,
    model: String,
    label: Option<String>,
    http: reqwest::Client,
}

#[async_trait]
impl Provider for OpenAiCompatible {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: self.id.clone(),
            label: self.label.clone().unwrap_or_else(|| format!("{} (OpenAI-compatible)", self.model)),
            model: self.model.clone(),
            grounded: false,
        }
    }

    async fn analyze(&self, req: &AnalysisRequest) -> Result<String> {
        let body = json!({
            "model": self.model,
            "messages": [
                {"role": "system", "content": req.system},
                {"role": "user", "content": req.user},
            ],
        });
        let mut rb = self.http.post(format!("{}/chat/completions", self.base_url.trim_end_matches('/')));
        if let Some(env) = &self.api_key_env {
            rb = rb.bearer_auth(api_key(env)?);
        }
        let v = error_for_status(rb.json(&body).send().await?, "chat completions API").await?;
        let text = v["choices"][0]["message"]["content"].as_str().unwrap_or_default().to_string();
        if text.is_empty() {
            bail!("the model returned an empty answer");
        }
        Ok(text)
    }
}

// ----------------------------------------------------------- generic command

struct CommandProvider {
    id: String,
    command: String,
    args: Vec<String>,
    timeout_secs: u64,
    grounded: bool,
    label: Option<String>,
}

#[async_trait]
impl Provider for CommandProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: self.id.clone(),
            label: self.label.clone().unwrap_or_else(|| format!("{} (command)", self.command)),
            model: self.command.clone(),
            grounded: self.grounded,
        }
    }

    async fn analyze(&self, req: &AnalysisRequest) -> Result<String> {
        let input = format!("{}\n\n{}", req.system, req.user);
        run_cli(&self.command, &self.args, &input, self.timeout_secs).await
    }
}
