//! Model and platform proxy setup are outside this storage/HTTP contract.
pub mod proxy {
    #[derive(Default)]
    pub struct ProxyConfig;
    pub fn build_async_client_with_timeout(
        _: &ProxyConfig,
        timeout: std::time::Duration,
    ) -> anyhow::Result<reqwest::Client> {
        Ok(reqwest::Client::builder()
            .timeout(timeout)
            .no_proxy()
            .build()?)
    }
}
