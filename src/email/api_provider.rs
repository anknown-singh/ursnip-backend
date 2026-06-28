use std::sync::Arc;

use reqwest::Client;
use serde::Serialize;

use crate::config::AppConfig;

use super::service::{EmailMessage, EmailProvider};

/// HTTP API-based email provider using `reqwest`.
///
/// Sends emails by POSTing JSON to a configured transactional email API endpoint
/// with Bearer token authentication.
pub struct ApiProvider {
    client: Client,
    api_key: String,
    api_url: String,
    from_address: String,
    from_name: String,
}

/// JSON payload sent to the email API.
#[derive(Serialize)]
struct EmailPayload {
    from: EmailFrom,
    to: String,
    subject: String,
    html: String,
    text: String,
}

#[derive(Serialize)]
struct EmailFrom {
    email: String,
    name: String,
}

impl ApiProvider {
    /// Create a new `ApiProvider` from the application config.
    ///
    /// Panics if the required API config fields are missing (api_key, api_url).
    pub fn new(config: &Arc<AppConfig>) -> Self {
        let api_key = config
            .email_api_key
            .as_deref()
            .expect("EMAIL_API_KEY is required for API provider")
            .to_string();
        let api_url = config
            .email_api_url
            .as_deref()
            .expect("EMAIL_API_URL is required for API provider")
            .to_string();

        Self {
            client: Client::new(),
            api_key,
            api_url,
            from_address: config.email_from_address.clone(),
            from_name: config.email_from_name.clone(),
        }
    }
}

#[async_trait::async_trait]
impl EmailProvider for ApiProvider {
    async fn send(
        &self,
        message: &EmailMessage,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let payload = EmailPayload {
            from: EmailFrom {
                email: self.from_address.clone(),
                name: self.from_name.clone(),
            },
            to: message.to.clone(),
            subject: message.subject.clone(),
            html: message.html_body.clone(),
            text: message.text_body.clone(),
        };

        let response = self
            .client
            .post(&self.api_url)
            .bearer_auth(&self.api_key)
            .json(&payload)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "unable to read response body".to_string());
            return Err(format!("Email API returned {}: {}", status, body).into());
        }

        Ok(())
    }
}
