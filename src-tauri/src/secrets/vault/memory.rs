//! Secrets that never touch disk.
//!
//! Nothing is persisted: close the app and the keys are gone. That is
//! the point — it is the choice for a shared or managed machine where
//! the user would rather paste an API key each session than leave it
//! anywhere at rest, and it is the only vault that needs no trust in the
//! filesystem or the OS credential store at all.
//!
//! It doubles as the test vault: the unit tests of every secret module
//! install one instead of the elaborate per-`Entry` mock the `keyring`
//! crate requires.

use super::{Error, Vault};
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
pub struct MemoryVault {
    items: Mutex<HashMap<(String, String), String>>,
}

impl MemoryVault {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Vault for MemoryVault {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>, Error> {
        Ok(self
            .items
            .lock()
            .map_err(|_| Error::Other("memory vault poisoned".into()))?
            .get(&(service.to_string(), account.to_string()))
            .cloned())
    }

    fn set(&self, service: &str, account: &str, value: &str) -> Result<(), Error> {
        self.items
            .lock()
            .map_err(|_| Error::Other("memory vault poisoned".into()))?
            .insert((service.to_string(), account.to_string()), value.to_string());
        Ok(())
    }

    fn delete(&self, service: &str, account: &str) -> Result<(), Error> {
        self.items
            .lock()
            .map_err(|_| Error::Other("memory vault poisoned".into()))?
            .remove(&(service.to_string(), account.to_string()));
        Ok(())
    }

    fn kind(&self) -> &'static str {
        "session"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_deletes() {
        let v = MemoryVault::new();
        assert_eq!(v.get("s", "a").unwrap(), None);
        v.set("s", "a", "secret").unwrap();
        assert_eq!(v.get("s", "a").unwrap().as_deref(), Some("secret"));
        v.delete("s", "a").unwrap();
        assert_eq!(v.get("s", "a").unwrap(), None);
    }

    #[test]
    fn service_and_account_are_both_part_of_the_key() {
        let v = MemoryVault::new();
        v.set("one", "a", "1").unwrap();
        v.set("two", "a", "2").unwrap();
        assert_eq!(v.get("one", "a").unwrap().as_deref(), Some("1"));
        assert_eq!(v.get("two", "a").unwrap().as_deref(), Some("2"));
    }

    #[test]
    fn delete_is_idempotent() {
        let v = MemoryVault::new();
        v.delete("s", "missing").unwrap();
        v.delete("s", "missing").unwrap();
    }
}
