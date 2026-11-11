//! OS-backed secrets and native OAuth. See README.md for integration.
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
compile_error!("munki-auth requires a native keyring backend on Linux, macOS, or Windows");

pub mod oauth;
pub mod secret;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("OS keyring unavailable or locked; unlock your keychain and ensure Secret Service is running: {0}")]
    Keyring(#[from] keyring::Error),
    #[error("invalid authentication configuration: {0}")]
    Configuration(&'static str),
    #[error("invalid OAuth callback: {0}")]
    Callback(&'static str),
    #[error("provider denied authorization")]
    Denied,
    #[error("OAuth flow timed out")]
    Timeout,
    #[error("OAuth token request failed")]
    Token,
    #[error("stored secret has invalid format")]
    StoredSecret,
    #[error("system clock is invalid")]
    Clock,
    #[error("loopback listener failed: {0}")]
    Io(#[from] std::io::Error),
}
pub type Result<T> = std::result::Result<T, Error>;
