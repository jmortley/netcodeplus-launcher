//! Thin wrapper around [`reqwest::Client`] with sane defaults for
//! the NetcodePlus launcher.

use std::time::Duration;

use crate::error::{NetError, Result};

/// HTTP client used by [`crate::fetch_text`] and [`crate::download`].
///
/// Internally a `reqwest::Client` configured with rustls-tls (so
/// trust comes from a bundled webpki root list, not the OS trust
/// store), a long-but-bounded timeout, and a launcher-identifying
/// User-Agent. Cheap to clone — internally `Arc`'d.
#[derive(Debug, Clone)]
pub struct Client {
    pub(crate) inner: reqwest::Client,
}

/// Knobs for [`Client::with_config`].
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Value sent in the `User-Agent` header.
    pub user_agent: String,
    /// Per-request timeout, measured from the start of the request
    /// to the end of the response body.
    pub request_timeout: Duration,
    /// Connection-establishment timeout.
    pub connect_timeout: Duration,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            user_agent: default_user_agent(),
            request_timeout: Duration::from_secs(60 * 30),
            connect_timeout: Duration::from_secs(15),
        }
    }
}

fn default_user_agent() -> String {
    format!(
        "netcodeplus-launcher/{} (+https://github.com/jmortley/netcodeplus-launcher)",
        env!("CARGO_PKG_VERSION")
    )
}

impl Client {
    /// Build a [`Client`] with default settings.
    ///
    /// # Errors
    ///
    /// Returns [`NetError::Http`] only if reqwest fails to construct
    /// its inner state (essentially never in practice).
    pub fn new() -> Result<Self> {
        Self::with_config(ClientConfig::default())
    }

    /// Build a [`Client`] with caller-supplied configuration.
    ///
    /// # Errors
    ///
    /// Returns [`NetError::Http`] if reqwest fails to construct.
    pub fn with_config(config: ClientConfig) -> Result<Self> {
        let inner = reqwest::Client::builder()
            .user_agent(config.user_agent)
            .timeout(config.request_timeout)
            .connect_timeout(config.connect_timeout)
            .https_only(false) // dev/test fixtures use http; production manifest URL must be https
            .build()
            .map_err(NetError::Http)?;
        Ok(Self { inner })
    }
}
