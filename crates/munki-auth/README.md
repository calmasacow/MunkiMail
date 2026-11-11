# munki-auth

Native authentication for Linux (Secret Service), macOS (Keychain), and Windows
(Credential Manager). No file storage or plaintext fallback. Linux uses keyring
3.6's synchronous Secret Service backend, Rust crypto, and vendored D-Bus;
a running, unlocked Secret Service is required. The application should tell the
user to unlock their keychain when operations fail. No custom permission UI or
browser launcher is provided.

## Public API

- `secret::get(service, key) -> Result<Option<String>>`
- `secret::set(service, key, value) -> Result<()>`
- `secret::delete(service, key) -> Result<()>` (missing entries succeed)
- `secret::database_key(service, key) -> Result<[u8; 32]>`: loads or generates
  a 256-bit key using `rand::rng()`, persisting it before returning. Corrupt or
  inaccessible entries fail; they are never replaced. Provision from one process
  at a time: the in-process lock does not coordinate separate application processes.
- `oauth::begin(Config) -> Result<(Url, Pending)>`: binds `127.0.0.1` on an OS-assigned
  port, creates random state and S256 PKCE. Caller opens the URL with its browser
  integration. `Pending` owns the listener; dropping it cancels the flow.
- `Pending::complete(self) -> Result<Tokens>`: one-use callback receiver and token
  exchange, bounded by the timeout starting at `begin`. Rejects wrong path/method,
  duplicate parameters, absent or mismatched state, empty code, and provider errors.
  The first invalid callback fails the flow; begin again to retry. Requests are
  limited to 8 KiB. Provider text and token bodies are excluded from errors.
- `Tokens::{load,save}(service, account)`: JSON in the OS keyring. Use a distinct
  service namespace for OAuth, passwords, API keys, and database keys; account
  identifiers must include provider identity to avoid collisions.
- `Tokens::refresh_if_needed(&mut self, &Config) -> Result<bool>`: refreshes within
  60 seconds of expiry, preserves unrotated refresh tokens, leaves existing data
  intact on failure. Save immediately after a successful refresh. Providers must
  return nonzero `expires_in`. Missing refresh tokens require reauthorization.

Keyring APIs are synchronous: call through `tokio::task::spawn_blocking` from async
applications. Never log `Tokens`, returned secrets, or authorization callback URLs.
Tokens deliberately do not implement `Debug`.

`Config` contains `provider`, `client_id`, `scopes`, and `timeout` (nonzero, at most
600 seconds). Providers are `Provider::Gmail` and
`Provider::Microsoft { tenant: String }`. Microsoft accepts tenant identifiers,
verified domains, `common`, `organizations`, and `consumers`. Endpoints are built
only from fixed HTTPS provider hosts and paths. Arbitrary endpoints are unavailable
in the public API. Token HTTP redirects are disabled; requests time out after 30s.
Local HTTP token endpoints exist only through private test setup.

Register a **native/public client** with the provider and enable loopback redirects
for `http://127.0.0.1:{port}/oauth/callback`. Configure the actual client ID and
provider-approved scopes. No embedded client secret is accepted or assumed.
Google requests offline access and consent; Microsoft adds `offline_access`.
Provider registration policies still apply; registrations requiring a confidential
client secret are incompatible with this public-client API.

```rust,no_run
use munki_auth::oauth::{self, Config, Provider};
use std::time::Duration;
# async fn example() -> munki_auth::Result<()> {
let config = Config {
    provider: Provider::Gmail,
    client_id: "your-native-client-id".into(),
    scopes: vec!["https://mail.google.com/".into()],
    timeout: Duration::from_secs(180),
};
let (authorization_url, pending) = oauth::begin(config.clone()).await?;
// Pass authorization_url to the application's browser opener.
let mut tokens = pending.complete().await?;
// Save through spawn_blocking in async code, using Tokens::save.
if tokens.refresh_if_needed(&config).await? {
    // Persist rotated tokens through spawn_blocking before using access_token.
}
# Ok(())
# }
```

Parent integration: add `crates/munki-auth` to workspace members and add a path
dependency where needed. This crate intentionally does not edit the root manifest.
Tests use loopback sockets and a mock HTTP token server, never external providers
or the real keychain. Native keyring integration requires testing on each OS.
