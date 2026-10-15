//! Data-source secrets in the system keychain (macOS Keychain, Windows
//! Credential Manager, Secret Service on Linux). They are handed to a plugin
//! on stdin at run time and never written to settings files or logs.

use serde_json::{Map, Value as Json};

const SERVICE: &str = "KhonsuGraph";

fn entry(source: &str, key: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, &format!("source:{source}:{key}")).map_err(|e| e.to_string())
}

pub fn set(source: &str, key: &str, value: &str) -> Result<(), String> {
    entry(source, key)?.set_password(value).map_err(|e| e.to_string())
}

pub fn get(source: &str, key: &str) -> Result<Option<String>, String> {
    match entry(source, key)?.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(err) => Err(err.to_string()),
    }
}

pub fn delete(source: &str, key: &str) -> Result<(), String> {
    match entry(source, key)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(err) => Err(err.to_string()),
    }
}

/// Collects a source's secrets for one plugin run.
pub fn load_all(source: &str, keys: &[String]) -> Result<Map<String, Json>, String> {
    let mut secrets = Map::new();
    for key in keys {
        if let Some(value) = get(source, key)? {
            secrets.insert(key.clone(), Json::String(value));
        }
    }
    Ok(secrets)
}
