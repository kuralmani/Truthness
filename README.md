# Truthness

Fact-check a video or a web page before you believe it.

A lot of online content is made to get attention and sell something, and it mixes real facts with false impressions. Truthness reads a YouTube video (via its captions) or a web page, pulls out the factual claims, checks them against published sources with the AI model **you** choose, and gives you:

- an overall verdict (True · Mostly true · Mixed · Misleading · Mostly false · False · Can't verify) and a 0–100 score,
- a claim-by-claim breakdown with links to the sources used,
- what the content is selling and which persuasion tricks it uses,
- the context it leaves out.

It runs on your own machine. There is no account and no hosted service: your model, your keys, your data.

![A Truthness verdict: overall rating, score, summary and the claims that were checked](docs/screenshot.png)

## Quick start

You need [Rust](https://rustup.rs) and one AI model. The easiest model is the [Claude Code CLI](https://claude.com/claude-code): if `claude` is installed and logged in, there is nothing to configure.

```sh
curl -fsSL https://raw.githubusercontent.com/kuralmani/Truthness/main/install.sh | sh
truthness
```

Then open <http://localhost:8787>, paste a link and press **Check**. A check with web search usually takes one to two minutes; results are cached for a week.

Prefer to do it by hand?

```sh
git clone https://github.com/kuralmani/Truthness.git
cd Truthness
cargo run --release
```

### Install the browser extension (Chrome or Edge)

1. Get the `extension` folder: it is in your clone (the installer puts it at `~/.truthness/src/extension`), or download `truthness-extension.zip` from the [latest release](https://github.com/kuralmani/Truthness/releases/latest) and unzip it.
2. Open `chrome://extensions` (or `edge://extensions`) and turn on **Developer mode**.
3. Click **Load unpacked** and choose that folder.

Keep the server running, then:

- on YouTube, open a video or Short and click **Check the facts** (bottom right);
- on any other page, click the Truthness toolbar icon;
- or select some text, right-click and choose **Fact-check … with Truthness**.

The extension talks to `http://localhost:8787` by default. Change the server and the model under the extension's **Options**.

### Example setups

| You have | Do this |
|---|---|
| Claude Code CLI | Nothing. Run `truthness`. |
| An Anthropic API key | `ANTHROPIC_API_KEY=… truthness` |
| A Gemini API key | `GEMINI_API_KEY=… truthness` |
| Ollama or another local model | Copy `truthness.example.toml` to `truthness.toml`, uncomment the `[providers.ollama]` block, run `truthness` |
| Another CLI (Gemini CLI, …) | Same file, the `type = "command"` block |

`truthness.toml` is read from the folder you start the server in; pass another path as the first argument (`truthness /path/to/truthness.toml`) or set `TRUTHNESS_CONFIG`.

Try the API directly:

```sh
curl -s localhost:8787/api/check -H 'Content-Type: application/json' \
  -d '{"url": "https://en.wikipedia.org/wiki/Trihalomethane"}'
# {"id":"…"}   then poll:  curl -s localhost:8787/api/check/<id>
```

## What is in the repository

| Part | Where | What it is |
|---|---|---|
| Server | `src/` | Rust (axum). Extracts content, runs the model, caches results, serves the website. |
| Website | `web/` | Paste a link or text, get a verdict. No build step. |
| Browser extension | `extension/` | Chrome / Edge (Manifest V3). A "Check the facts" button on YouTube, and a toolbar button + right-click menu on every other page. |

## Choosing models

Copy `truthness.example.toml` to `truthness.toml`. Each `[providers.<name>]` block adds a model to the picker on the website and in the extension.

| `type` | Use it for | Searches the web? |
|---|---|---|
| `claude-cli` | Claude Code CLI with your existing login | yes |
| `anthropic` | Anthropic API (`ANTHROPIC_API_KEY`) | yes |
| `gemini` | Gemini API (`GEMINI_API_KEY`); can also watch YouTube videos directly | yes |
| `openai-compatible` | OpenAI, Ollama, LM Studio, vLLM, OpenRouter, … | no |
| `command` | Any CLI that reads a prompt on stdin and prints an answer | you say |

Models without web search can only answer from memory. Truthness tells them so, and labels their verdicts accordingly in the UI. Prefer a model that can search.

API keys are read from environment variables, never from the config file.

### Adding a provider in code

Implement the `Provider` trait in `src/providers/mod.rs` (one `analyze` method that takes a system prompt and a user prompt and returns the model's text), add a variant to `ProviderConfig` in `src/config.rs`, and register it in `providers::build`.

## API

```
POST /api/check        {"url": "...", "text": "...", "provider": "...", "force": false}  → {"id": "..."}
GET  /api/check/{id}   → {"status": "queued|extracting|analyzing|done|error", "message", "result", "error"}
GET  /api/providers    → {"providers": [...], "default": "..."}
```

Send `url`, `text`, or both (the extension sends both for ordinary pages so that it works behind logins).

## How to read a verdict

Truthness is a tool for thinking, not an oracle.

- The verdict is an AI model's assessment. Models make mistakes, miss recent events and can misread a source. Open the linked sources before you share a verdict or act on it.
- "Can't verify" is a real answer. The model is told to say so rather than guess.
- "Misleading" means the individual facts are largely real but the impression they are arranged to create is not. This is how most health and money content online misleads.
- Videos are judged on their captions. Auto-generated captions contain errors, and anything shown but not said is invisible to the model (unless you use a provider that watches the video, such as Gemini). If a video has no captions the result says so.

## Security and privacy notes

- **The content being checked is untrusted input.** The prompt tells the model to treat it as data, and the `claude-cli` provider is started with web search and web fetch as its only tools (no shell, no files, no MCP servers). A `command` provider gets whatever that command can do, so pick one accordingly.
- The server listens on `127.0.0.1` and sends no CORS headers, so other websites cannot use your browser to call it and spend your model quota. The extension calls it from its background worker.
- Fetching user-supplied URLs is restricted to public addresses (`allow_private_hosts = false`), checked on every redirect. This does not defend against DNS rebinding; put a real egress policy in front of a public deployment.
- The extension reads a page only when you ask it to. It sends the page text to the server you configured and nowhere else.
- **Before hosting this publicly**, add authentication or rate limiting in front of it: every check costs model usage.

## Known limits / ideas

- YouTube is the only video site with transcript support. TikTok, Instagram and X links are treated as ordinary pages, which usually yields little text. Audio transcription (e.g. Whisper) would fix this.
- YouTube changes its internals often; caption fetching (`src/extract.rs`) is the part most likely to need maintenance.
- Results live in a local file cache and jobs live in memory. A shared database and a public "already checked" index would let one check serve everyone.
- Running two different models and showing where they disagree would make verdicts more trustworthy.

## Development

```sh
cargo test                       # unit tests
./scripts/sync-extension.sh      # after editing web/render.js or web/verdict.css
./scripts/package-extension.sh   # builds truthness-extension.zip
```

In debug builds the website is read from `web/` on every request; release builds embed it in the binary.

## Contributing

Issues and pull requests are welcome, especially new providers, transcript support for more video sites, and fixes when YouTube changes. Please run `cargo test` before opening a pull request. Pushing a tag such as `v0.1.0` builds release binaries and the extension zip.

## License

[MIT](LICENSE)
