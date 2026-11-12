//! The HTTP client shared by the copied Truss modules (they call `crate::api::CLIENT`), plus a
//! counter of downloaded bytes for the scan statistics.

use std::sync::LazyLock;

pub static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        // Say who we are (no pretending to be a web browser).
        .user_agent(concat!("truss-rv-index/", env!("CARGO_PKG_VERSION"), " (+https://github.com/graindead/truss-rv-index)"))
        .connect_timeout(std::time::Duration::from_secs(15))
        .pool_max_idle_per_host(20)
        .build()
        .expect("failed to build HTTP client")
});

tokio::task_local! {
    static CHART_BYTES: std::cell::Cell<u64>;
}

/// Records `n` downloaded body bytes (in total, and for the chart being scanned).
pub fn count(n: usize) {
    let _ = CHART_BYTES.try_with(|c| c.set(c.get() + n as u64));
}

/// Runs `f`, also returning the bytes it downloaded (correct with several scans at once).
pub async fn counted<F: std::future::Future>(f: F) -> (F::Output, u64) {
    CHART_BYTES
        .scope(std::cell::Cell::new(0), async {
            let r = f.await;
            (r, CHART_BYTES.with(|c| c.get()))
        })
        .await
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn counts_each_scan_separately() {
        let scan = |n: usize| async move {
            for _ in 0..5 {
                super::count(n);
                tokio::task::yield_now().await;
            }
        };
        let (a, b) = tokio::join!(super::counted(scan(10)), super::counted(scan(3)));
        assert_eq!((a.1, b.1), (50, 15));
    }
}
