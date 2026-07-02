//! Paddle billing provider integration.
//!
//! Creates checkout transactions via the Paddle Billing API (v2).
//! Reference: https://developer.paddle.com/api-reference/transactions/create-transaction

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::config::AppConfig;
use crate::errors::AppError;

// ─── Constants ──────────────────────────────────────────────────────────────────

const PADDLE_SANDBOX_API: &str = "https://sandbox-api.paddle.com";
const PADDLE_PRODUCTION_API: &str = "https://api.paddle.com";

// ─── Public Types ───────────────────────────────────────────────────────────────

/// Parameters needed to create a Paddle transaction (checkout session).
#[derive(Debug, Clone)]
pub struct CreateTransactionParams {
    /// The tier being purchased (used to select Paddle price ID).
    pub tier: String,
    /// Total amount in cents (smallest currency unit).
    pub total_amount_cents: i64,
    /// Currency code (e.g., "USD").
    pub currency: String,
    /// Workspace ID for custom data / metadata.
    pub workspace_id: String,
    /// URL to redirect on successful payment.
    pub success_url: Option<String>,
    /// Country code for tax purposes (ISO 3166-1 alpha-2).
    pub country_code: Option<String>,
}

/// Result of creating a Paddle transaction.
#[derive(Debug, Clone)]
pub struct PaddleCheckoutResult {
    /// The URL to redirect the user to for payment.
    pub checkout_url: String,
    /// The Paddle transaction ID.
    pub transaction_id: String,
}

// ─── Paddle API Request/Response Types ──────────────────────────────────────────

#[derive(Debug, Serialize)]
struct CreateTransactionRequest {
    items: Vec<TransactionItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    checkout: Option<CheckoutSettings>,
    custom_data: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    currency_code: Option<String>,
}

#[derive(Debug, Serialize)]
struct TransactionItem {
    price: TransactionPrice,
    quantity: i32,
}

#[derive(Debug, Serialize)]
struct TransactionPrice {
    description: String,
    unit_price: UnitPrice,
    product: TransactionProduct,
    billing_cycle: BillingCycle,
    tax_mode: String,
}

#[derive(Debug, Serialize)]
struct UnitPrice {
    amount: String,
    currency_code: String,
}

#[derive(Debug, Serialize)]
struct TransactionProduct {
    name: String,
    description: String,
    tax_category: String,
}

#[derive(Debug, Serialize)]
struct BillingCycle {
    interval: String,
    frequency: i32,
}

#[derive(Debug, Serialize)]
struct CheckoutSettings {
    url: String,
}

#[derive(Debug, Deserialize)]
struct PaddleTransactionResponse {
    data: PaddleTransactionData,
}

#[derive(Debug, Deserialize)]
struct PaddleTransactionData {
    id: String,
    checkout: Option<PaddleCheckoutData>,
}

#[derive(Debug, Deserialize)]
struct PaddleCheckoutData {
    url: Option<String>,
}

// ─── Public Functions ───────────────────────────────────────────────────────────

/// Create a Paddle checkout transaction.
///
/// Calls the Paddle Transactions API to create a new transaction, then returns
/// the checkout URL for the user to complete payment.
///
/// If `PADDLE_API_KEY` is not configured, falls back to a mock checkout URL.
pub async fn create_checkout_transaction(
    config: &AppConfig,
    params: CreateTransactionParams,
) -> Result<PaddleCheckoutResult, AppError> {
    let api_key = match &config.paddle_api_key {
        Some(key) if !key.is_empty() => key.clone(),
        _ => {
            // Fallback: return mock checkout URL if Paddle is not configured
            info!("Paddle API key not configured, using mock checkout URL");
            let session_id = uuid::Uuid::new_v4();
            return Ok(PaddleCheckoutResult {
                checkout_url: format!(
                    "https://checkout.billing-provider.example/session/{}",
                    session_id
                ),
                transaction_id: session_id.to_string(),
            });
        }
    };

    let base_url = match config.paddle_environment.as_str() {
        "production" | "live" => PADDLE_PRODUCTION_API,
        _ => PADDLE_SANDBOX_API,
    };

    let plan_name = match params.tier.as_str() {
        "pro" => "UrSnip Pro",
        "teams" => "UrSnip Team",
        _ => "UrSnip",
    };

    let plan_description = match params.tier.as_str() {
        "pro" => "UrSnip Pro annual subscription",
        "teams" => "UrSnip Team annual subscription",
        _ => "UrSnip subscription",
    };

    // Build the request body using inline prices (no pre-created Price ID needed)
    let request_body = CreateTransactionRequest {
        items: vec![TransactionItem {
            price: TransactionPrice {
                description: plan_description.to_string(),
                unit_price: UnitPrice {
                    amount: params.total_amount_cents.to_string(),
                    currency_code: params.currency.to_uppercase(),
                },
                product: TransactionProduct {
                    name: plan_name.to_string(),
                    description: plan_description.to_string(),
                    tax_category: "standard".to_string(),
                },
                billing_cycle: BillingCycle {
                    interval: "year".to_string(),
                    frequency: 1,
                },
                tax_mode: "account_setting".to_string(),
            },
            quantity: 1,
        }],
        checkout: params.success_url.as_ref().map(|url| CheckoutSettings {
            url: url.clone(),
        }),
        custom_data: serde_json::json!({
            "workspace_id": params.workspace_id,
            "tier": params.tier,
        }),
        currency_code: Some(params.currency.to_uppercase()),
    };

    // Make the API call
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/transactions", base_url))
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&request_body)
        .send()
        .await
        .map_err(|e| {
            tracing::error!("Paddle API request failed: {}", e);
            AppError::InternalError
        })?;

    if !response.status().is_success() {
        let status = response.status();
        let error_body = response.text().await.unwrap_or_default();
        tracing::error!(
            "Paddle API returned error: status={}, body={}",
            status,
            error_body
        );
        return Err(AppError::InternalError);
    }

    let paddle_response: PaddleTransactionResponse = response.json().await.map_err(|e| {
        tracing::error!("Failed to parse Paddle response: {}", e);
        AppError::InternalError
    })?;

    // Extract checkout URL
    let checkout_url = paddle_response
        .data
        .checkout
        .and_then(|c| c.url)
        .unwrap_or_else(|| {
            // Construct the checkout URL from the transaction ID
            format!(
                "https://checkout.paddle.com/transactions/{}",
                paddle_response.data.id
            )
        });

    // Append success_url as a query parameter if provided and not already embedded
    let final_url = if let Some(ref success_url) = params.success_url {
        if !checkout_url.contains("success_url") {
            let separator = if checkout_url.contains('?') { "&" } else { "?" };
            format!(
                "{}{}success_url={}",
                checkout_url,
                separator,
                urlencoding::encode(success_url)
            )
        } else {
            checkout_url
        }
    } else {
        checkout_url
    };

    info!(
        transaction_id = %paddle_response.data.id,
        tier = %params.tier,
        "Created Paddle checkout transaction"
    );

    Ok(PaddleCheckoutResult {
        checkout_url: final_url,
        transaction_id: paddle_response.data.id,
    })
}

/// Convert a Decimal dollar amount to cents (smallest currency unit).
pub fn dollars_to_cents(amount: Decimal) -> i64 {
    let cents = amount * Decimal::from(100);
    cents.to_string().parse::<f64>().unwrap_or(0.0) as i64
}
