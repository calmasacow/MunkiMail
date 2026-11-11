# munki-ai

Optional suggestion-only AI for MunkiMail. No sending, routing, storage, or automatic actions.

Parent integration: add `crates/munki-ai` to root workspace members. App dependency should be optional, behind an opt-in Cargo feature. Runtime defaults remain disabled, without consent.

```rust,no_run
use munki_ai::{AiClient, Config, Provider};

# async fn example() -> Result<(), munki_ai::Error> {
let mut config = Config::new(
    Provider::OpenAiCompatible,
    "http://127.0.0.1:11434/v1/chat/completions",
    "llama3.2", // Must already be installed in Ollama.
);
// Set these only after the user explicitly enables AI and consents to sharing contents.
config.enabled = true;
config.consent_to_send_contents = true;
let client = AiClient::new(config, None)?;
let draft = client.draft_reply("User-approved email context").await?;
// Display draft for human review. No send action exists here.
let activity = client.take_activity(); // App may persist metadata records.
# Ok(())
# }
```

Anthropic: `Provider::Anthropic`, `https://api.anthropic.com/v1/messages`, e.g. `claude-sonnet-4-5`, in-memory API key. OpenAI: `https://api.openai.com/v1/chat/completions`, e.g. `gpt-4.1-mini`. User chooses models; availability depends on account/server. Endpoints are full API URLs, explicitly trusted by the user. HTTPS required except loopback HTTP. No URL credentials, query strings, fragments, redirects, or environment proxies.

All methods require enablement plus consent, including search translation. `set_enabled` and `set_consent_to_send_contents` support revocation through mutable app state. `&self` async methods work in Tauri state; avoid holding a synchronous mutex across awaits. Default timeout 30 seconds, configurable via `Config::timeout`. Keys are never serialized or written; use the app's credential vault to supply them. Errors omit remote bodies and URLs.

Classifier sends sender, subject, and first 200 Unicode characters of preview only. Query results reject unknown fields, invalid destinations/types, malformed sender addresses, empty searches, excessive terms, unreasonable timestamps, and inverted date ranges. Results remain untrusted suggestions; render text escaped. Activity records include only Unix time, provider enum, action enum, success. Drain periodically to bound memory; persistence is caller-owned.

Tests: `cargo test -p munki-ai` after workspace registration. HTTP tests use loopback mocks; no real credentials or provider calls.
