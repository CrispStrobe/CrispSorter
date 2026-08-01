//! OS-keychain storage for the optional network proxy password.

use keyring::Entry;

pub const SERVICE: &str = "CrispSorter.Proxy";
const ACCOUNT: &str = "default";

pub fn set(password: &str) -> anyhow::Result<()> {
    Entry::new(SERVICE, ACCOUNT)?.set_password(password)?;
    Ok(())
}

pub fn get() -> anyhow::Result<Option<String>> {
    match Entry::new(SERVICE, ACCOUNT)?.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub fn clear() -> anyhow::Result<()> {
    match Entry::new(SERVICE, ACCOUNT)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use keyring::mock::default_credential_builder;
    use std::sync::Once;

    #[test]
    fn password_round_trip_and_clear() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| keyring::set_default_credential_builder(default_credential_builder()));
        clear().unwrap();
        assert_eq!(get().unwrap(), None);
        set("secret").unwrap();
        assert_eq!(get().unwrap().as_deref(), Some("secret"));
        clear().unwrap();
        assert_eq!(get().unwrap(), None);
    }
}
