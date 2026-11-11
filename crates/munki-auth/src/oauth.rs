use crate::{secret, Error, Result};
use oauth2::{
    basic::BasicClient, AuthUrl, AuthorizationCode, ClientId, CsrfToken, PkceCodeChallenge,
    PkceCodeVerifier, RedirectUrl, RefreshToken, Scope, TokenResponse, TokenUrl,
};
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use subtle::ConstantTimeEq;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::{timeout_at, Instant},
};
use url::Url;

#[derive(Clone, Debug)]
pub enum Provider {
    Gmail,
    Yahoo,
    /// A tenant UUID, verified domain, common, organizations, or consumers.
    Microsoft {
        tenant: String,
    },
}
impl Provider {
    fn endpoints(&self) -> Result<(String, String)> {
        match self {
            Self::Gmail => Ok((
                "https://accounts.google.com/o/oauth2/v2/auth".into(),
                "https://oauth2.googleapis.com/token".into(),
            )),
            Self::Yahoo => Ok((
                "https://api.login.yahoo.com/oauth2/request_auth".into(),
                "https://api.login.yahoo.com/oauth2/get_token".into(),
            )),
            Self::Microsoft { tenant } => {
                if tenant.is_empty()
                    || tenant.len() > 253
                    || !tenant
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
                {
                    return Err(Error::Configuration("invalid Microsoft tenant"));
                }
                Ok((
                    format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/authorize"),
                    format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token"),
                ))
            }
        }
    }
}

#[derive(Clone)]
pub struct Config {
    pub provider: Provider,
    pub client_id: String,
    pub scopes: Vec<String>,
    pub timeout: Duration,
}
impl Config {
    fn validate(&self) -> Result<()> {
        if self.client_id.trim().is_empty()
            || self.scopes.is_empty()
            || self.scopes.iter().any(|s| s.trim().is_empty())
            || self.timeout.is_zero()
            || self.timeout > Duration::from_secs(600)
        {
            return Err(Error::Configuration(
                "client ID, scopes, and timeout (1ns..600s) required",
            ));
        }
        self.provider.endpoints()?;
        Ok(())
    }
}

/// One-use receiver. Drop closes the listener. No background task survives it.
pub struct Pending {
    listener: TcpListener,
    config: Config,
    redirect: String,
    state: CsrfToken,
    verifier: PkceCodeVerifier,
    deadline: Instant,
    token_endpoint: String,
}

/// Bind IPv4 loopback and return a URL for the caller to open in a browser.
pub async fn begin(config: Config) -> Result<(Url, Pending)> {
    config.validate()?;
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
    let redirect = format!(
        "http://127.0.0.1:{}/oauth/callback",
        listener.local_addr()?.port()
    );
    let (auth, token) = config.provider.endpoints()?;
    let client = BasicClient::new(ClientId::new(config.client_id.clone()))
        .set_auth_uri(AuthUrl::new(auth).map_err(|_| Error::Configuration("authorization URL"))?)
        .set_token_uri(TokenUrl::new(token).map_err(|_| Error::Configuration("token URL"))?)
        .set_redirect_uri(
            RedirectUrl::new(redirect.clone()).map_err(|_| Error::Configuration("redirect URL"))?,
        );
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let mut request = client
        .authorize_url(CsrfToken::new_random)
        .set_pkce_challenge(challenge);
    for scope in &config.scopes {
        request = request.add_scope(Scope::new(scope.clone()));
    }
    request = match config.provider {
        Provider::Gmail => request
            .add_extra_param("access_type", "offline")
            .add_extra_param("prompt", "consent"),
        Provider::Yahoo => request.add_extra_param("prompt", "consent"),
        Provider::Microsoft { .. } => request.add_scope(Scope::new("offline_access".into())),
    };
    let (url, state) = request.url();
    let token_endpoint = config.provider.endpoints()?.1;
    let deadline = Instant::now() + config.timeout;
    Ok((
        url,
        Pending {
            listener,
            config,
            redirect,
            state,
            verifier,
            deadline,
            token_endpoint,
        },
    ))
}

fn callback(line: &str, expected_state: &str) -> Result<String> {
    let mut parts = line.split_whitespace();
    if parts.next() != Some("GET") {
        return Err(Error::Callback("GET required"));
    }
    let target = parts.next().ok_or(Error::Callback("missing target"))?;
    if !matches!(parts.next(), Some("HTTP/1.1" | "HTTP/1.0"))
        || parts.next().is_some()
        || !target.starts_with("/oauth/callback?")
    {
        return Err(Error::Callback("invalid request path or version"));
    }
    let url = Url::parse(&format!("http://127.0.0.1{target}"))
        .map_err(|_| Error::Callback("invalid target"))?;
    if url.path() != "/oauth/callback" || url.fragment().is_some() {
        return Err(Error::Callback("invalid callback path"));
    }
    let mut state = None;
    let mut code = None;
    let mut error = None;
    for (key, value) in url.query_pairs() {
        let slot = match key.as_ref() {
            "state" => &mut state,
            "code" => &mut code,
            "error" => &mut error,
            _ => continue,
        };
        if slot.replace(value.into_owned()).is_some() {
            return Err(Error::Callback("duplicate parameter"));
        }
    }
    let state = state.ok_or(Error::Callback("missing state"))?;
    if !bool::from(state.as_bytes().ct_eq(expected_state.as_bytes())) {
        return Err(Error::Callback("state mismatch"));
    }
    if error.is_some() {
        if code.is_some() {
            return Err(Error::Callback("code and error present"));
        }
        return Err(Error::Denied);
    }
    code.filter(|c| !c.trim().is_empty())
        .ok_or(Error::Callback("missing code"))
}

impl Pending {
    /// Consume once; timeout includes callback reads and token exchange.
    pub async fn complete(self) -> Result<Tokens> {
        timeout_at(self.deadline, async {
            let (mut stream, peer) = self.listener.accept().await?;
            if !peer.ip().is_loopback() { return Err(Error::Callback("non-loopback peer")); }
            let mut data = Vec::new();
            loop {
                let byte = stream.read_u8().await?;
                data.push(byte);
                if data.len() > 8192 { return Err(Error::Callback("request too large")); }
                if data.ends_with(b"\r\n\r\n") { break; }
            }
            let request = std::str::from_utf8(&data).map_err(|_| Error::Callback("invalid request encoding"))?;
            let code = callback(request.lines().next().unwrap_or(""), self.state.secret());
            let status = if code.is_ok() { "200 OK" } else { "400 Bad Request" };
            let response = format!("HTTP/1.1 {status}\r\nContent-Length: 24\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nReturn to the mail app.\n");
            stream.write_all(response.as_bytes()).await?;
            let code = code?;
            let (auth, _) = self.config.provider.endpoints()?;
            let token = self.token_endpoint;
            let client = BasicClient::new(ClientId::new(self.config.client_id))
                .set_auth_uri(AuthUrl::new(auth).map_err(|_| Error::Configuration("authorization URL"))?)
                .set_token_uri(TokenUrl::new(token).map_err(|_| Error::Configuration("token URL"))?)
                .set_redirect_uri(RedirectUrl::new(self.redirect).map_err(|_| Error::Configuration("redirect URL"))?);
            let response = client.exchange_code(AuthorizationCode::new(code))
                .set_pkce_verifier(self.verifier).request_async(&http_client()?).await.map_err(|_| Error::Token)?;
            Tokens::from_response(response, None)
        }).await.map_err(|_| Error::Timeout)?
    }
}

fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| Error::Token)
}
fn now() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::Clock)?
        .as_secs())
}

/// Secret-bearing data: deliberately no Debug implementation. Expiry is Unix seconds.
#[derive(Clone, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: u64,
}
impl Tokens {
    fn from_response(
        response: oauth2::basic::BasicTokenResponse,
        previous: Option<String>,
    ) -> Result<Self> {
        let lifetime = response.expires_in().ok_or(Error::Token)?.as_secs();
        if response.access_token().secret().is_empty() || lifetime == 0 {
            return Err(Error::Token);
        }
        Ok(Self {
            access_token: response.access_token().secret().clone(),
            refresh_token: response
                .refresh_token()
                .map(|r| r.secret().clone())
                .or(previous),
            expires_at: now()?.checked_add(lifetime).ok_or(Error::Token)?,
        })
    }
    pub fn load(service: &str, account: &str) -> Result<Option<Self>> {
        secret::get(service, account)?
            .map(|s| serde_json::from_str(&s).map_err(|_| Error::StoredSecret))
            .transpose()
    }
    pub fn save(&self, service: &str, account: &str) -> Result<()> {
        secret::set(
            service,
            account,
            &serde_json::to_string(self).map_err(|_| Error::StoredSecret)?,
        )
    }
    /// Refresh within 60 seconds of expiry. Retains an unrotated refresh token.
    /// Caller must persist refreshed tokens before using them.
    pub async fn refresh_if_needed(&mut self, config: &Config) -> Result<bool> {
        config.validate()?;
        if self.expires_at > now()?.saturating_add(60) {
            return Ok(false);
        }
        let refresh = self
            .refresh_token
            .clone()
            .filter(|s| !s.is_empty())
            .ok_or(Error::Token)?;
        let (_, endpoint) = config.provider.endpoints()?;
        let client = BasicClient::new(ClientId::new(config.client_id.clone()))
            .set_token_uri(TokenUrl::new(endpoint).map_err(|_| Error::Configuration("token URL"))?);
        let response = client
            .exchange_refresh_token(&RefreshToken::new(refresh.clone()))
            .request_async(&http_client()?)
            .await
            .map_err(|_| Error::Token)?;
        *self = Self::from_response(response, Some(refresh))?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> Config {
        Config {
            provider: Provider::Gmail,
            client_id: "public-client".into(),
            scopes: vec!["email".into()],
            timeout: Duration::from_secs(2),
        }
    }
    #[test]
    fn validates_callback() {
        assert_eq!(
            callback("GET /oauth/callback?state=s&code=c HTTP/1.1", "s").unwrap(),
            "c"
        );
        for target in [
            "/wrong?state=s&code=c",
            "/oauth/callback?state=x&code=c",
            "/oauth/callback?code=c",
            "/oauth/callback?state=s",
            "/oauth/callback?state=s&code=",
            "/oauth/callback?state=s&code=a&code=b",
            "/oauth/callback?state=s&state=s&code=c",
            "/oauth/callback?state=s&error=denied&code=c",
            "/oauth/callback?state=s&code=c#fragment",
            "http://evil.test/oauth/callback?state=s&code=c",
        ] {
            assert!(
                callback(&format!("GET {target} HTTP/1.1"), "s").is_err(),
                "{target}"
            );
        }
        assert!(matches!(
            callback(
                "GET /oauth/callback?state=s&error=access_denied HTTP/1.1",
                "s"
            ),
            Err(Error::Denied)
        ));
        assert!(callback("POST /oauth/callback?state=s&code=c HTTP/1.1", "s").is_err());
        assert!(Provider::Microsoft {
            tenant: "common/../../evil".into()
        }
        .endpoints()
        .is_err());
    }
    async fn send_callback(pending: &Pending, query: &str) -> tokio::net::TcpStream {
        let mut stream = tokio::net::TcpStream::connect(pending.listener.local_addr().unwrap())
            .await
            .unwrap();
        stream
            .write_all(
                format!("GET /oauth/callback?{query} HTTP/1.1\r\nHost: localhost\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
        stream
    }
    #[tokio::test]
    async fn exchanges_pkce_with_local_mock_only() {
        let mock = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (url, mut pending) = begin(config()).await.unwrap();
        assert!(url
            .query_pairs()
            .any(|(k, v)| k == "code_challenge_method" && v == "S256"));
        pending.token_endpoint = format!("http://{}/token", mock.local_addr().unwrap());
        let verifier = pending.verifier.secret().clone();
        let state = pending.state.secret().clone();
        let _browser = send_callback(&pending, &format!("state={state}&code=test-code")).await;
        let server = async {
            let (mut stream, _) = mock.accept().await.unwrap();
            let mut bytes = Vec::new();
            let header_end;
            loop {
                bytes.push(stream.read_u8().await.unwrap());
                if bytes.ends_with(b"\r\n\r\n") {
                    header_end = bytes.len();
                    break;
                }
            }
            let headers = String::from_utf8(bytes.clone()).unwrap();
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|s| s.trim().parse().unwrap())
                })
                .unwrap();
            bytes.resize(header_end + length, 0);
            stream.read_exact(&mut bytes[header_end..]).await.unwrap();
            let body: std::collections::HashMap<_, _> =
                url::form_urlencoded::parse(&bytes[header_end..])
                    .into_owned()
                    .collect();
            assert_eq!(body["code"], "test-code");
            assert_eq!(body["code_verifier"], verifier);
            assert_eq!(body["client_id"], "public-client");
            assert!(!body.contains_key("client_secret"));
            let json = r#"{"access_token":"access","refresh_token":"refresh","token_type":"Bearer","expires_in":3600}"#;
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}", json.len()).as_bytes()).await.unwrap();
        };
        let (tokens, ()) = tokio::join!(pending.complete(), server);
        let mut tokens = tokens.unwrap();
        assert_eq!(tokens.access_token, "access");
        assert_eq!(tokens.refresh_token.as_deref(), Some("refresh"));
        assert!(!tokens.refresh_if_needed(&config()).await.unwrap());
    }
    #[test]
    fn token_rotation_and_expiry_validation() {
        let response =
            |json: &str| serde_json::from_str::<oauth2::basic::BasicTokenResponse>(json).unwrap();
        let tokens = Tokens::from_response(
            response(r#"{"access_token":"new","token_type":"Bearer","expires_in":120}"#),
            Some("old-refresh".into()),
        )
        .unwrap();
        assert_eq!(tokens.refresh_token.as_deref(), Some("old-refresh"));
        let rotated = Tokens::from_response(response(r#"{"access_token":"new","refresh_token":"rotated","token_type":"Bearer","expires_in":120}"#), Some("old".into())).unwrap();
        assert_eq!(rotated.refresh_token.as_deref(), Some("rotated"));
        for json in [
            r#"{"access_token":"new","token_type":"Bearer"}"#,
            r#"{"access_token":"new","token_type":"Bearer","expires_in":0}"#,
        ] {
            assert!(Tokens::from_response(response(json), None).is_err());
        }
    }
    #[tokio::test]
    async fn timeout_and_invalid_loopback() {
        let mut cfg = config();
        cfg.timeout = Duration::from_millis(20);
        let (_, pending) = begin(cfg).await.unwrap();
        assert!(matches!(pending.complete().await, Err(Error::Timeout)));
        let (_, pending) = begin(config()).await.unwrap();
        let _browser = send_callback(&pending, "state=wrong&code=test").await;
        assert!(matches!(
            pending.complete().await,
            Err(Error::Callback("state mismatch"))
        ));
    }
}

#[cfg(test)]
mod yahoo_tests {
    use super::*;
    #[tokio::test]
    async fn public_yahoo_authorization_uses_pkce_and_pinned_endpoints() {
        let (url, _pending) = begin(Config {
            provider: Provider::Yahoo,
            client_id: "test-public-client".into(),
            scopes: vec!["mail-w".into()],
            timeout: Duration::from_secs(60),
        })
        .await
        .unwrap();
        assert_eq!(url.host_str(), Some("api.login.yahoo.com"));
        assert_eq!(url.path(), "/oauth2/request_auth");
        let params: std::collections::HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(params["code_challenge_method"], "S256");
        assert_eq!(params["scope"], "mail-w");
        assert!(!params.contains_key("client_secret"));
        assert!(params["redirect_uri"].starts_with("http://127.0.0.1:"));
        assert_eq!(
            Provider::Yahoo.endpoints().unwrap().1,
            "https://api.login.yahoo.com/oauth2/get_token"
        );
    }
}
