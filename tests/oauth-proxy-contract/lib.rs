//! Real OAuth, proxy, policy and command bodies; only keychain, app state and
//! operation-log sinks are fixtures. HTTP requests use loopback servers.
#![allow(dead_code)]
#[path = "../../src-tauri/src/sync/cert_pins.rs"]
pub mod production_cert_pins;
pub use production_cert_pins as cert_pins;
#[path = "../../src-tauri/src/sync/proxy.rs"]
pub mod production_proxy;
pub mod sync {
    pub use crate::production_cert_pins as cert_pins;
    pub use crate::production_proxy as proxy;
    pub mod proxy_secret {
        pub fn get() -> anyhow::Result<Option<String>> {
            Ok(None)
        }
    }
}
pub mod operation_log {
    pub struct OperationLog;
    impl OperationLog {
        pub fn start(_: &str, _: serde_json::Value) -> Self {
            Self
        }
        pub fn step(&self, _: &str, _: serde_json::Value) {}
        pub fn succeed(&self, _: serde_json::Value) {}
        pub fn fail(&self, _: &str) {}
        pub fn cancel(&self) {}
    }
}
pub struct Config {
    pub proxy_url: Option<String>,
    pub proxy_username: Option<String>,
}
impl Clone for Config {
    fn clone(&self) -> Self {
        Self {
            proxy_url: self.proxy_url.clone(),
            proxy_username: self.proxy_username.clone(),
        }
    }
}
pub struct Index {
    pub config: Config,
}
pub struct AppState {
    pub index: tokio::sync::Mutex<Index>,
    pub microsoft: bool,
}
pub mod drives {
    pub mod secret {
        use serde::{Deserialize, Serialize};
        use std::{
            collections::HashMap,
            sync::{Mutex, OnceLock},
        };
        include!(concat!(env!("OUT_DIR"), "/credentials.rs"));
        fn store() -> &'static Mutex<HashMap<String, DriveCredentials>> {
            static STORE: OnceLock<Mutex<HashMap<String, DriveCredentials>>> = OnceLock::new();
            STORE.get_or_init(|| Mutex::new(HashMap::new()))
        }
        pub fn install_mock_for_tests() {}
        pub fn get_credentials(id: &str) -> anyhow::Result<Option<DriveCredentials>> {
            Ok(store().lock().unwrap().get(id).cloned())
        }
        pub fn set_credentials(id: &str, value: &DriveCredentials) -> anyhow::Result<()> {
            store().lock().unwrap().insert(id.into(), value.clone());
            Ok(())
        }
        pub fn delete_credentials(id: &str) -> anyhow::Result<()> {
            store().lock().unwrap().remove(id);
            Ok(())
        }
    }
    include!(concat!(env!("OUT_DIR"), "/oauth_module.rs"));
    #[derive(Clone, Copy)]
    pub enum DriveType {
        GoogleDrive,
        OneDrive,
        Other,
    }
    pub struct DriveConfig {
        pub kind: DriveType,
    }
    pub mod commands {
        use super::{DriveConfig, DriveType};
        use crate::{operation_log::OperationLog, AppState};
        use serde_json::Value;
        type State<'a, T> = &'a T;
        async fn drive_operation(
            state: &AppState,
            _: &str,
            _: &str,
        ) -> Result<(OperationLog, DriveConfig), String> {
            Ok((
                OperationLog,
                DriveConfig {
                    kind: if state.microsoft {
                        DriveType::OneDrive
                    } else {
                        DriveType::GoogleDrive
                    },
                },
            ))
        }
        fn finish_operation<T>(
            _: &OperationLog,
            _: &Result<T, String>,
            _: &str,
            _: impl FnOnce(&T) -> Value,
        ) {
        }
        include!(concat!(env!("OUT_DIR"), "/commands.rs"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn actual_commands_use_persisted_proxy_and_keep_microsoft_revoke_local() {
        let state = AppState {
            microsoft: false,
            index: tokio::sync::Mutex::new(Index {
                config: Config {
                    proxy_url: None,
                    proxy_username: Some("invalid-without-url".into()),
                },
            }),
        };
        let error = drives::commands::drive_oauth_start(
            &state,
            "fixture".into(),
            "google".into(),
            "public".into(),
        )
        .await
        .unwrap_err();
        assert!(error.contains("proxy credentials require a proxy URL"));
        let error = drives::commands::drive_oauth_refresh(&state, "fixture".into())
            .await
            .unwrap_err();
        assert!(error.contains("proxy credentials require a proxy URL"));
        let error = drives::commands::drive_oauth_start(
            &state,
            "fixture".into(),
            "microsoft".into(),
            "public".into(),
        )
        .await
        .unwrap_err();
        assert!(error.contains("provider does not match"));
        let microsoft = AppState {
            microsoft: true,
            ..state
        };
        drives::commands::drive_oauth_revoke(&microsoft, "fixture-local-clear".into())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn valid_client_construction_stays_off_the_async_executor() {
        let state = AppState {
            microsoft: false,
            index: tokio::sync::Mutex::new(Index {
                config: Config {
                    proxy_url: Some("http://127.0.0.1:1".into()),
                    proxy_username: None,
                },
            }),
        };
        // Client construction succeeds; missing credentials stop before any
        // HTTP request. A blocking-client runtime panic must not be hidden as
        // an ordinary missing-credential failure by the command worker.
        let error = drives::commands::drive_oauth_refresh(&state, "fixture-no-credentials".into())
            .await
            .unwrap_err();
        assert!(error.contains("no OAuth credentials are stored"), "{error}");
    }
}

#[path = "../../src-tauri/src/drives/webdav_range.rs"]
mod webdav_range_response;

#[cfg(test)]
mod webdav_range_wiring;
