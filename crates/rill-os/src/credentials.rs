//! Tokens in the OS credential store (Windows Credential Manager, macOS Keychain, Secret Service).

use crate::credential_service;

/// Reads a stored secret; `Ok(None)` when nothing is stored under `key`.
pub fn load(key: &str) -> Result<Option<String>, String> {
    match entry(key)?.get_password() {
        Ok(v) => Ok(Some(v)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(store_error("read", key, e)),
    }
}

/// Stores a secret under `key`, replacing any earlier one.
pub fn save(key: &str, value: &str) -> Result<(), String> {
    entry(key)?
        .set_password(value)
        .map_err(|e| store_error("save", key, e))
}

/// Removes a stored secret; succeeds if there was none.
pub fn delete(key: &str) -> Result<(), String> {
    match entry(key)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(store_error("delete", key, e)),
    }
}

fn entry(key: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(credential_service(), key).map_err(|e| store_error("open", key, e))
}

fn store_error(verb: &str, key: &str, e: keyring::Error) -> String {
    format!(
        "Credential store: couldn't {verb} \"{key}\": {e}. Check that the system keychain or credential manager is unlocked, then sign in again."
    )
}

#[cfg(test)]
mod tests {
    /// Touches the real OS store, so it only runs when asked: `cargo test -p rill-os -- --ignored`.
    #[test]
    #[ignore]
    fn round_trips_through_the_os_store() {
        let key = "rill-selftest";
        super::save(key, "value-1").unwrap();
        assert_eq!(super::load(key).unwrap().as_deref(), Some("value-1"));
        super::delete(key).unwrap();
        assert_eq!(super::load(key).unwrap(), None);
        super::delete(key).unwrap();
    }
}
