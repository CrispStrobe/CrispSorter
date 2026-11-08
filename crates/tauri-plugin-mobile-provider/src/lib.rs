//! Read-only access to user-selected Android and iOS document providers.
//!
//! The plugin is also registered on other targets so its permissions exist
//! in the generated ACL; provider commands are implemented on mobile.

use tauri::{plugin::TauriPlugin, Runtime};

#[cfg(any(target_os = "android", target_os = "ios"))]
mod mobile {
    use serde_json::{json, Value};
    use tauri::{plugin::PluginHandle, AppHandle, Runtime, State};

    pub struct Provider<R: Runtime>(pub PluginHandle<R>);

    #[tauri::command]
    pub async fn pick_files<R: Runtime>(
        _app: AppHandle<R>,
        provider: State<'_, Provider<R>>,
    ) -> Result<Value, String> {
        provider
            .0
            .run_mobile_plugin_async("pickFiles", json!({}))
            .await
            .map_err(|e| e.to_string())
    }

    #[tauri::command]
    pub async fn pick_folder<R: Runtime>(
        _app: AppHandle<R>,
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
        _app: AppHandle<R>,
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

#[cfg(target_os = "ios")]
tauri::ios_plugin_binding!(init_plugin_mobile_provider);

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    let builder = tauri::plugin::Builder::new("mobile-provider");
    #[cfg(any(target_os = "android", target_os = "ios"))]
    let builder = builder
        .invoke_handler(tauri::generate_handler![
            mobile::pick_files,
            mobile::pick_folder,
            mobile::read_file
        ])
        .setup(|app, api| {
            use tauri::Manager;
            #[cfg(target_os = "android")]
            let handle = api.register_android_plugin(
                "com.crispstrobe.mobileprovider",
                "MobileProviderPlugin",
            )?;
            #[cfg(target_os = "ios")]
            let handle = api.register_ios_plugin(init_plugin_mobile_provider)?;
            app.manage(mobile::Provider(handle));
            Ok(())
        });
    builder.build()
}
