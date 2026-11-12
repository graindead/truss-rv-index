//! The HTTP client shared by the copied Truss modules (they call `crate::api::CLIENT`), plus a
//! counter of downloaded bytes for the scan statistics.

use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};

pub static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        // Say who we are (no pretending to be a web browser).
        .user_agent(concat!("truss-rv-index/", env!("CARGO_PKG_VERSION"), " (+https://github.com/graindead/truss-rv-index)"))
        .connect_timeout(std::time::Duration::from_secs(15))
        .pool_max_idle_per_host(4)
        .build()
        .expect("failed to build HTTP client")
});

static BYTES: AtomicU64 = AtomicU64::new(0);

/// Records `n` downloaded body bytes.
pub fn count(n: usize) {
    BYTES.fetch_add(n as u64, Ordering::Relaxed);
}

/// Body bytes downloaded so far by this process.
pub fn bytes() -> u64 {
    BYTES.load(Ordering::Relaxed)
}
