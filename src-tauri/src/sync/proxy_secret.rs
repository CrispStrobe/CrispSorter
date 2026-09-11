//! OS-keychain storage for the optional network proxy password.

#[cfg(not(test))]
use crate::secrets::vault::{Entry, Error as VaultError};
#[cfg(test)]
use std::sync::{Mutex, OnceLock};

pub const SERVICE: &str = "CrispSorter.Proxy";
const ACCOUNT: &str = "default";

#[cfg(test)]
fn test_value() -> &'static Mutex<Option<String>> {
    static VALUE: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    VALUE.get_or_init(|| Mutex::new(None))
}

pub fn set(password: &str) -> anyhow::Result<()> {
    #[cfg(test)]
    {
        *test_value().lock().expect("test proxy secret") = Some(password.into());
        Ok(())
    }
    #[cfg(not(test))]
    {
        Entry::new(SERVICE, ACCOUNT)?.set_password(password)?;
        Ok(())
    }
}

pub fn get() -> anyhow::Result<Option<String>> {
    #[cfg(test)]
    {
        Ok(test_value().lock().expect("test proxy secret").clone())
    }
    #[cfg(not(test))]
    {
        match Entry::new(SERVICE, ACCOUNT)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(VaultError::NoEntry) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}

pub fn clear() -> anyhow::Result<()> {
    #[cfg(test)]
    {
        *test_value().lock().expect("test proxy secret") = None;
        Ok(())
    }
    #[cfg(not(test))]
    {
        match Entry::new(SERVICE, ACCOUNT)?.delete_credential() {
            Ok(()) | Err(VaultError::NoEntry) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn password_round_trip_and_clear() {
        clear().unwrap();
        assert_eq!(get().unwrap(), None);
        set("secret").unwrap();
        assert_eq!(get().unwrap().as_deref(), Some("secret"));
        clear().unwrap();
        assert_eq!(get().unwrap(), None);
    }
}
