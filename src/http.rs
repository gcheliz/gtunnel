//! The one door every HTTP client comes through.
//!
//! reqwest is built with `rustls-no-provider` so the binary carries `ring`
//! and not `aws-lc-rs` (1.4 MB smaller). The cost is that `build()` panics
//! when no process-wide provider is installed, and under `panic = "abort"`
//! that is a dead process. Installing it here, and not in `main`, covers unit
//! tests and every future call site; `clippy.toml` forbids the other doors.

use std::sync::Once;

pub fn client_builder() -> reqwest::ClientBuilder {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        // Err means another provider is already installed, which serves too
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
    #[allow(clippy::disallowed_methods)]
    reqwest::Client::builder()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_client_builds_twice_without_a_provider_installed_by_hand() {
        client_builder().build().expect("first client");
        client_builder().build().expect("second client");
    }
}
