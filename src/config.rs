use std::env;

/// Email provider type: either SMTP relay or transactional API.
#[derive(Clone, Debug)]
pub enum EmailProviderType {
    Smtp,
    Api,
}

/// Application configuration loaded from environment variables.
///
/// Required variables terminate the process with a descriptive error if absent.
/// Optional variables fall back to sensible defaults.
#[derive(Clone, Debug)]
pub struct AppConfig {
    // Required (no defaults, panic on absence)
    pub database_url: String,
    pub jwt_secret: String,
    pub google_client_id: String,
    pub google_client_secret: String,
    pub github_client_id: String,
    pub github_client_secret: String,
    pub oauth_redirect_base_url: String,
    pub ai_provider_url: String,
    pub ai_provider_key: String,
    pub billing_webhook_secret: String,
    pub email_from_address: String,
    pub seed_admin_email: String,
    pub seed_admin_password: String,
    pub email_provider: EmailProviderType,

    // Email SMTP (required when email_provider = smtp)
    pub email_smtp_host: Option<String>,
    pub email_smtp_port: Option<u16>,
    pub email_smtp_user: Option<String>,
    pub email_smtp_password: Option<String>,

    // Email API (required when email_provider = api)
    pub email_api_key: Option<String>,
    pub email_api_url: Option<String>,

    // Optional with defaults
    pub email_from_name: String,
    pub port: u16,
    pub log_level: String,
    pub database_max_connections: u32,
    pub database_min_connections: u32,
    pub database_connect_timeout_secs: u64,
    pub database_idle_timeout_secs: u64,
    pub database_statement_timeout_secs: u64,
    pub cors_allowed_origins: Vec<String>,
    pub trusted_proxy_cidrs: Vec<String>,
    pub ws_max_connections: usize,
    pub ai_max_concurrent_requests: usize,
    pub shutdown_timeout_secs: u64,
}

impl AppConfig {
    /// Load configuration from environment variables.
    ///
    /// Terminates the process with a descriptive error message if any required
    /// variable is missing or if email provider credentials do not match the
    /// selected provider type.
    pub fn from_env() -> Self {
        let mut missing: Vec<&str> = Vec::new();

        // Helper: collect a required var or record it as missing.
        macro_rules! require {
            ($name:expr) => {
                env::var($name).unwrap_or_else(|_| {
                    missing.push($name);
                    String::new()
                })
            };
        }

        // --- Required variables ---
        let database_url = require!("DATABASE_URL");
        let jwt_secret = require!("JWT_SECRET");
        let google_client_id = require!("GOOGLE_CLIENT_ID");
        let google_client_secret = require!("GOOGLE_CLIENT_SECRET");
        let github_client_id = require!("GITHUB_CLIENT_ID");
        let github_client_secret = require!("GITHUB_CLIENT_SECRET");
        let oauth_redirect_base_url = require!("OAUTH_REDIRECT_BASE_URL");
        let ai_provider_url = require!("AI_PROVIDER_URL");
        let ai_provider_key = require!("AI_PROVIDER_KEY");
        let billing_webhook_secret = require!("BILLING_WEBHOOK_SECRET");
        let email_from_address = require!("EMAIL_FROM_ADDRESS");
        let seed_admin_email = require!("SEED_ADMIN_EMAIL");
        let seed_admin_password = require!("SEED_ADMIN_PASSWORD");
        let email_provider_raw = require!("EMAIL_PROVIDER");

        // Terminate early if any required variables are missing.
        if !missing.is_empty() {
            eprintln!(
                "FATAL: the following required environment variables are not set: {}",
                missing.join(", ")
            );
            std::process::exit(1);
        }

        // --- Parse EMAIL_PROVIDER ---
        let email_provider = match email_provider_raw.to_lowercase().as_str() {
            "smtp" => EmailProviderType::Smtp,
            "api" => EmailProviderType::Api,
            other => {
                eprintln!(
                    "FATAL: EMAIL_PROVIDER must be \"smtp\" or \"api\", got: \"{}\"",
                    other
                );
                std::process::exit(1);
            }
        };

        // --- Conditional email credentials ---
        let (email_smtp_host, email_smtp_port, email_smtp_user, email_smtp_password) =
            match &email_provider {
                EmailProviderType::Smtp => {
                    let mut smtp_missing: Vec<&str> = Vec::new();

                    let host = env::var("EMAIL_SMTP_HOST").unwrap_or_else(|_| {
                        smtp_missing.push("EMAIL_SMTP_HOST");
                        String::new()
                    });
                    let port_str = env::var("EMAIL_SMTP_PORT").unwrap_or_else(|_| {
                        smtp_missing.push("EMAIL_SMTP_PORT");
                        String::new()
                    });
                    let user = env::var("EMAIL_SMTP_USER").unwrap_or_else(|_| {
                        smtp_missing.push("EMAIL_SMTP_USER");
                        String::new()
                    });
                    let password = env::var("EMAIL_SMTP_PASSWORD").unwrap_or_else(|_| {
                        smtp_missing.push("EMAIL_SMTP_PASSWORD");
                        String::new()
                    });

                    if !smtp_missing.is_empty() {
                        eprintln!(
                            "FATAL: EMAIL_PROVIDER is \"smtp\" but the following required SMTP variables are not set: {}",
                            smtp_missing.join(", ")
                        );
                        std::process::exit(1);
                    }

                    let port: u16 = port_str.parse().unwrap_or_else(|_| {
                        eprintln!(
                            "FATAL: EMAIL_SMTP_PORT must be a valid u16, got: \"{}\"",
                            port_str
                        );
                        std::process::exit(1);
                    });

                    (Some(host), Some(port), Some(user), Some(password))
                }
                EmailProviderType::Api => (None, None, None, None),
            };

        let (email_api_key, email_api_url) = match &email_provider {
            EmailProviderType::Api => {
                let mut api_missing: Vec<&str> = Vec::new();

                let key = env::var("EMAIL_API_KEY").unwrap_or_else(|_| {
                    api_missing.push("EMAIL_API_KEY");
                    String::new()
                });
                let url = env::var("EMAIL_API_URL").unwrap_or_else(|_| {
                    api_missing.push("EMAIL_API_URL");
                    String::new()
                });

                if !api_missing.is_empty() {
                    eprintln!(
                        "FATAL: EMAIL_PROVIDER is \"api\" but the following required API variables are not set: {}",
                        api_missing.join(", ")
                    );
                    std::process::exit(1);
                }

                (Some(key), Some(url))
            }
            EmailProviderType::Smtp => (None, None),
        };

        // --- Optional with defaults ---
        let email_from_name = env::var("EMAIL_FROM_NAME").unwrap_or_else(|_| "Ursnip".to_string());

        let port = env::var("PORT")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(8080);

        let log_level = env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string());

        let database_max_connections = env::var("DATABASE_MAX_CONNECTIONS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(20);

        let database_min_connections = env::var("DATABASE_MIN_CONNECTIONS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(5);

        let database_connect_timeout_secs = env::var("DATABASE_CONNECT_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(5);

        let database_idle_timeout_secs = env::var("DATABASE_IDLE_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(300);

        let database_statement_timeout_secs = env::var("DATABASE_STATEMENT_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(30);

        let cors_allowed_origins = env::var("CORS_ALLOWED_ORIGINS")
            .ok()
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        let trusted_proxy_cidrs = env::var("TRUSTED_PROXY_CIDRS")
            .ok()
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        let ws_max_connections = env::var("WS_MAX_CONNECTIONS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(10_000);

        let ai_max_concurrent_requests = env::var("AI_MAX_CONCURRENT_REQUESTS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(50);

        let shutdown_timeout_secs = env::var("SHUTDOWN_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(30);

        Self {
            database_url,
            jwt_secret,
            google_client_id,
            google_client_secret,
            github_client_id,
            github_client_secret,
            oauth_redirect_base_url,
            ai_provider_url,
            ai_provider_key,
            billing_webhook_secret,
            email_from_address,
            seed_admin_email,
            seed_admin_password,
            email_provider,
            email_smtp_host,
            email_smtp_port,
            email_smtp_user,
            email_smtp_password,
            email_api_key,
            email_api_url,
            email_from_name,
            port,
            log_level,
            database_max_connections,
            database_min_connections,
            database_connect_timeout_secs,
            database_idle_timeout_secs,
            database_statement_timeout_secs,
            cors_allowed_origins,
            trusted_proxy_cidrs,
            ws_max_connections,
            ai_max_concurrent_requests,
            shutdown_timeout_secs,
        }
    }
}
