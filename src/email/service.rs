use std::sync::Arc;

use tokio::time::Duration;
use tracing::error;

use crate::config::{AppConfig, EmailProviderType};

use super::api_provider::ApiProvider;
use super::smtp::SmtpProvider;

/// A structured email message ready for dispatch.
#[derive(Clone, Debug)]
pub struct EmailMessage {
    pub to: String,
    pub subject: String,
    pub html_body: String,
    pub text_body: String,
}

/// Trait abstracting over email delivery backends (SMTP, transactional API, etc.).
#[async_trait::async_trait]
pub trait EmailProvider: Send + Sync {
    async fn send(&self, message: &EmailMessage) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
}

/// High-level email service that selects the configured provider and dispatches
/// emails asynchronously (non-blocking to HTTP handlers) with retry logic.
#[derive(Clone)]
pub struct EmailService {
    provider: Arc<dyn EmailProvider>,
    config: Arc<AppConfig>,
}

impl EmailService {
    /// Create a new `EmailService`, selecting the provider based on `AppConfig::email_provider`.
    pub fn new(config: Arc<AppConfig>) -> Self {
        let provider: Arc<dyn EmailProvider> = match config.email_provider {
            EmailProviderType::Smtp => Arc::new(SmtpProvider::new(&config)),
            EmailProviderType::Api => Arc::new(ApiProvider::new(&config)),
        };

        Self { provider, config }
    }

    /// Dispatch an email asynchronously via `tokio::spawn`.
    ///
    /// This method returns immediately — the actual send (with retries) happens
    /// in a background task so that HTTP handlers are never blocked.
    pub fn send_email(&self, message: EmailMessage) {
        let provider = Arc::clone(&self.provider);
        let _config = Arc::clone(&self.config);

        tokio::spawn(async move {
            send_with_retry(&provider, &message).await;
        });
    }

    /// Access the underlying provider (useful for testing or direct sends).
    pub fn provider(&self) -> &Arc<dyn EmailProvider> {
        &self.provider
    }

    /// Access the service config.
    pub fn config(&self) -> &Arc<AppConfig> {
        &self.config
    }
}

/// Retry delays: 1 second, 5 seconds, 30 seconds (exponential backoff).
const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(5),
    Duration::from_secs(30),
];

/// Attempt to send an email with exponential backoff.
///
/// Makes up to 3 attempts with delays of 1s → 5s → 30s between failures.
/// Logs at ERROR level on final failure.
async fn send_with_retry(provider: &Arc<dyn EmailProvider>, message: &EmailMessage) {
    let max_attempts = RETRY_DELAYS.len();

    for attempt in 0..max_attempts {
        match provider.send(message).await {
            Ok(()) => return,
            Err(err) => {
                if attempt == max_attempts - 1 {
                    // Final attempt failed — log at ERROR and give up.
                    error!(
                        to = %message.to,
                        subject = %message.subject,
                        attempts = max_attempts,
                        error = %err,
                        "Email delivery failed after all retry attempts"
                    );
                } else {
                    // Sleep before next retry.
                    tokio::time::sleep(RETRY_DELAYS[attempt]).await;
                }
            }
        }
    }
}
