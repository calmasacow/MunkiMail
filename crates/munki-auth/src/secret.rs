//! Synchronous OS keyring operations. Async callers should use spawn_blocking.
use crate::{Error, Result};
use rand::RngCore;
use std::sync::Mutex;

pub fn get(service: &str, key: &str) -> Result<Option<String>> {
    match keyring::Entry::new(service, key)?.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.into()),
    }
}
pub fn set(service: &str, key: &str, value: &str) -> Result<()> {
    Ok(keyring::Entry::new(service, key)?.set_password(value)?)
}
pub fn delete(service: &str, key: &str) -> Result<()> {
    match keyring::Entry::new(service, key)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

static DB_KEY_LOCK: Mutex<()> = Mutex::new(());
/// Returns an existing 256-bit key or securely generates and persists one.
/// Serialize provisioning across processes; this lock covers only this process.
pub fn database_key(service: &str, key: &str) -> Result<[u8; 32]> {
    let _guard = DB_KEY_LOCK.lock().map_err(|_| Error::StoredSecret)?;
    if let Some(value) = get(service, key)? {
        let bytes: Vec<u8> = serde_json::from_str(&value).map_err(|_| Error::StoredSecret)?;
        return bytes.try_into().map_err(|_| Error::StoredSecret);
    }
    let mut bytes = [0; 32];
    rand::rng().fill_bytes(&mut bytes);
    set(
        service,
        key,
        &serde_json::to_string(&bytes).map_err(|_| Error::StoredSecret)?,
    )?;
    Ok(bytes)
}
