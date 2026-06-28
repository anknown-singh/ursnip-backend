use serde::{Deserialize, Serialize};

/// Pagination parameters for list endpoints.
///
/// Accepts optional `page` and `per_page` query parameters.
/// Defaults to page 1, 20 items per page, with a maximum of 100.
#[derive(Debug, Clone, Deserialize)]
pub struct Pagination {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
}

impl Pagination {
    const DEFAULT_PAGE: i64 = 1;
    const DEFAULT_PER_PAGE: i64 = 20;
    const MAX_PER_PAGE: i64 = 100;

    /// Returns the effective page number (minimum 1).
    pub fn effective_page(&self) -> i64 {
        self.page.unwrap_or(Self::DEFAULT_PAGE).max(1)
    }

    /// Returns the effective per_page value (clamped to 1..=100).
    pub fn effective_per_page(&self) -> i64 {
        self.per_page
            .unwrap_or(Self::DEFAULT_PER_PAGE)
            .clamp(1, Self::MAX_PER_PAGE)
    }

    /// Compute the SQL OFFSET value.
    pub fn offset(&self) -> i64 {
        (self.effective_page() - 1) * self.effective_per_page()
    }

    /// Compute the SQL LIMIT value.
    pub fn limit(&self) -> i64 {
        self.effective_per_page()
    }
}

impl Default for Pagination {
    fn default() -> Self {
        Self {
            page: Some(Self::DEFAULT_PAGE),
            per_page: Some(Self::DEFAULT_PER_PAGE),
        }
    }
}

/// Generic wrapper for paginated API responses.
#[derive(Debug, Clone, Serialize)]
pub struct PaginatedResponse<T: Serialize> {
    pub data: Vec<T>,
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
    pub total_pages: i64,
}

impl<T: Serialize> PaginatedResponse<T> {
    /// Construct a paginated response from data, total count, and pagination params.
    pub fn new(data: Vec<T>, total: i64, pagination: &Pagination) -> Self {
        let per_page = pagination.effective_per_page();
        let page = pagination.effective_page();
        let total_pages = if total == 0 {
            0
        } else {
            (total + per_page - 1) / per_page
        };

        Self {
            data,
            total,
            page,
            per_page,
            total_pages,
        }
    }
}

/// Client type: native desktop/mobile app or web browser app.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum ClientType {
    Native,
    Web,
}

/// User role within the system.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum Role {
    User,
    Admin,
}

/// Subscription tier.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum Tier {
    Free,
    Pro,
    Teams,
}

/// Subscription status reflecting billing lifecycle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum SubscriptionStatus {
    Active,
    PastDue,
    Cancelled,
    PendingPayment,
    Deactivated,
}
