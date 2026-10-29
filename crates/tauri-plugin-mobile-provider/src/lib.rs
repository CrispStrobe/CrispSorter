//! Read-only access to user-selected Android document providers.
//!
//! The plugin is also registered on other targets so its permissions exist
//! in the generated ACL, but the commands are only implemented on Android.

use tauri::{plugin::TauriPlugin, Runtime};

#[cfg(target_os = "android")]
mod android {
    use serde_json::{json, Value};
    use tauri::{plugin::PluginHandle, Runtime, State};

    pub struct Provider<R: Runtime>(pub PluginHandle<R>);

    #[tauri::command]
    pub async fn pick_files<R: Runtime>(provider: State<'_, Provider<R>>) -> Result<Value, String> {
        provider
            .0
            .run_mobile_plugin_async("pickFiles", json!({}))
            .await
            .map_err(|e| e.to_string())
    }

    #[tauri::command]
    pub async fn pick_folder<R: Runtime>(
        provider: State<'_, Provider<R>>,
    ) -> Result<Value, String> {
        provider
            .0
            .run_mobile_plugin_async("pickFolder", json!({}))
            .await
            .map_err(|e| e.to_string())
    }

    #[tauri::command]
    pub async fn read_file<R: Runtime>(
        provider: State<'_, Provider<R>>,
        uri: String,
    ) -> Result<Value, String> {
        provider
            .0
            .run_mobile_plugin_async("readFile", json!({ "uri": uri }))
            .await
            .map_err(|e| e.to_string())
    }
}

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    let builder = tauri::plugin::Builder::new("mobile-provider");
    #[cfg(target_os = "android")]
    let builder = builder
        .invoke_handler(tauri::generate_handler![
            android::pick_files,
            android::pick_folder,
            android::read_file
        ])
        .setup(|app, api| {
            use tauri::Manager;
            let handle = api.register_android_plugin(
                "com.crispstrobe.mobileprovider",
                "MobileProviderPlugin",
            )?;
            app.manage(android::Provider(handle));
            Ok(())
        });
    builder.build()
}
