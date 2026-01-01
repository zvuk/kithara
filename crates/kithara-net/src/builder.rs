use crate::base::ReqwestNet;
use crate::error::NetError;
use crate::traits::{Net, NetExt};
use crate::types::{NetOptions, RetryPolicy};

/// Builder for creating configured Net clients
pub struct NetBuilder {
    options: NetOptions,
}

impl NetBuilder {
    pub fn new() -> Self {
        Self {
            options: NetOptions::default(),
        }
    }

    pub fn with_request_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.options.request_timeout = timeout;
        self
    }

    pub fn with_retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.options.max_retries = policy.max_retries;
        self.options.retry_base_delay = policy.base_delay;
        self.options.max_retry_delay = policy.max_delay;
        self
    }

    pub fn build(self) -> Result<impl Net, NetError> {
        let base = ReqwestNet::with_timeout(self.options.request_timeout)?;
        let retry_policy = RetryPolicy::new(
            self.options.max_retries,
            self.options.retry_base_delay,
            self.options.max_retry_delay,
        );

        Ok(base.with_retry(retry_policy))
    }
}

impl Default for NetBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Convenience function to create a default client with retry and timeout
pub fn create_default_client() -> Result<impl Net, NetError> {
    NetBuilder::new().build()
}
