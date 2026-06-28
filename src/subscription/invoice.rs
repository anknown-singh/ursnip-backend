use rust_decimal::Decimal;
use rust_decimal::prelude::Zero;
use rust_decimal::RoundingStrategy;
use serde::Serialize;
use sqlx::PgPool;

use crate::errors::AppError;

// ─── Constants ──────────────────────────────────────────────────────────────────

/// Minimum billing cycle in months (Requirement 5.26).
const MINIMUM_BILLING_CYCLE_MONTHS: i32 = 12;

/// Default currency for invoices.
const DEFAULT_CURRENCY: &str = "USD";

// ─── Types ──────────────────────────────────────────────────────────────────────

/// Input parameters for computing an invoice.
#[derive(Debug, Clone)]
pub struct InvoiceRequest {
    pub base_price: Decimal,
    pub billing_cycle_months: i32,
    pub discount_type: Option<String>,
    pub discount_value: Option<Decimal>,
    pub country_code: Option<String>,
}

/// Computed invoice result with all pricing breakdown fields (Requirement 5.53).
#[derive(Debug, Clone, Serialize)]
pub struct Invoice {
    pub base_price: Decimal,
    pub discount_amount: Decimal,
    pub discount_type: Option<String>,
    pub subtotal_after_discount: Decimal,
    pub tax_rate: Decimal,
    pub tax_name: Option<String>,
    pub tax_amount: Decimal,
    pub total_amount: Decimal,
    pub currency: String,
    pub billing_cycle_months: i32,
}

// ─── Internal Row Types ─────────────────────────────────────────────────────────

#[derive(Debug, sqlx::FromRow)]
struct TaxRateRow {
    pub rate: Decimal,
    pub tax_name: String,
}

// ─── Public Functions ───────────────────────────────────────────────────────────

/// Compute a full invoice given base price, discount info, and user country.
///
/// Steps:
/// 1. Validate billing_cycle_months >= 12 (Requirement 5.26)
/// 2. Apply discount before tax (Requirements 5.32, 5.33)
/// 3. Look up tax rate from user's country_code (Requirements 5.51, 5.52)
/// 4. Compute total = subtotal + tax
/// 5. Round all monetary amounts to 2 decimal places (Requirement 5.53)
pub async fn compute_invoice(
    pool: &PgPool,
    request: &InvoiceRequest,
) -> Result<Invoice, AppError> {
    // Step 1: Validate minimum billing cycle (Requirement 5.26)
    if request.billing_cycle_months < MINIMUM_BILLING_CYCLE_MONTHS {
        return Err(AppError::MinimumBillingCycleNotMet);
    }

    // Step 2: Apply discount (Requirements 5.32, 5.33)
    let (discount_amount, subtotal_after_discount) = apply_discount(
        request.base_price,
        request.discount_type.as_deref(),
        request.discount_value,
    );

    // Round the subtotal before computing tax so that displayed values are consistent:
    // total = subtotal + tax (all rounded to 2dp)
    let subtotal_rounded = round2(subtotal_after_discount);
    let discount_rounded = round2(discount_amount);

    // Step 3: Calculate tax (Requirements 5.51, 5.52)
    let (tax_rate, tax_amount, tax_name) = calculate_tax(
        pool,
        request.country_code.as_deref(),
        subtotal_rounded,
    )
    .await?;

    // Round tax, then compute total as sum of rounded values for consistency
    let tax_rounded = round2(tax_amount);

    // Step 4: Compute total = rounded_subtotal + rounded_tax
    let total_amount = subtotal_rounded + tax_rounded;

    // Step 5: Build invoice with all amounts rounded to 2 decimal places
    let invoice = Invoice {
        base_price: round2(request.base_price),
        discount_amount: discount_rounded,
        discount_type: request.discount_type.clone(),
        subtotal_after_discount: subtotal_rounded,
        tax_rate,
        tax_name,
        tax_amount: tax_rounded,
        total_amount,
        currency: DEFAULT_CURRENCY.to_string(),
        billing_cycle_months: request.billing_cycle_months,
    };

    Ok(invoice)
}

/// Apply a discount to the base price.
///
/// - For "percentage" type: discount_amount = base_price × discount_value,
///   clamped so subtotal never goes below 0 (Requirement 5.33).
/// - For "flat" type: discount_amount = discount_value,
///   clamped so subtotal never goes below 0 (Requirement 5.33).
/// - If no discount type/value: discount_amount = 0.
///
/// Returns (discount_amount, subtotal_after_discount).
pub fn apply_discount(
    base_price: Decimal,
    discount_type: Option<&str>,
    discount_value: Option<Decimal>,
) -> (Decimal, Decimal) {
    let discount_amount = match (discount_type, discount_value) {
        (Some("percentage"), Some(rate)) => {
            // Percentage discount: multiply base_price by rate (0.01–1.00)
            let raw_discount = base_price * rate;
            // Clamp: discount cannot exceed the base price
            clamp_discount(raw_discount, base_price)
        }
        (Some("flat"), Some(flat_amount)) => {
            // Flat discount: subtract the flat amount
            // Clamp: discount cannot exceed the base price
            clamp_discount(flat_amount, base_price)
        }
        _ => Decimal::ZERO,
    };

    let subtotal = base_price - discount_amount;
    (discount_amount, subtotal)
}

/// Look up tax rate from the tax_rates table using the user's country_code.
///
/// - If country_code is None or no matching active row exists, tax rate is 0 (Requirement 5.52).
/// - Tax = subtotal × tax_rate (Requirement 5.51).
///
/// Returns (tax_rate, tax_amount, tax_name).
pub async fn calculate_tax(
    pool: &PgPool,
    country_code: Option<&str>,
    subtotal: Decimal,
) -> Result<(Decimal, Decimal, Option<String>), AppError> {
    let country_code = match country_code {
        Some(cc) if !cc.is_empty() => cc,
        _ => return Ok((Decimal::ZERO, Decimal::ZERO, None)),
    };

    let row = sqlx::query_as::<_, TaxRateRow>(
        r#"
        SELECT rate, tax_name
        FROM tax_rates
        WHERE country_code = $1 AND active = true
        "#,
    )
    .bind(country_code)
    .fetch_optional(pool)
    .await
    .map_err(|_| AppError::InternalError)?;

    match row {
        Some(tax_row) => {
            let tax_amount = subtotal * tax_row.rate;
            Ok((tax_row.rate, tax_amount, Some(tax_row.tax_name)))
        }
        None => {
            // No tax rate found for this country — apply 0 (Requirement 5.52)
            Ok((Decimal::ZERO, Decimal::ZERO, None))
        }
    }
}

// ─── Private Helpers ────────────────────────────────────────────────────────────

/// Clamp a discount so it stays within [0, base_price].
fn clamp_discount(discount: Decimal, base_price: Decimal) -> Decimal {
    if discount < Decimal::ZERO {
        Decimal::ZERO
    } else if discount > base_price {
        base_price
    } else {
        discount
    }
}

/// Round a Decimal value to 2 decimal places using MidpointNearestEven (banker's rounding).
fn round2(value: Decimal) -> Decimal {
    value.round_dp_with_strategy(2, RoundingStrategy::MidpointNearestEven)
}

// ─── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    #[test]
    fn test_apply_discount_no_discount() {
        let (discount, subtotal) = apply_discount(d("100.00"), None, None);
        assert_eq!(discount, Decimal::ZERO);
        assert_eq!(subtotal, d("100.00"));
    }

    #[test]
    fn test_apply_discount_percentage() {
        // 20% off $100 = $20 discount, $80 subtotal
        let (discount, subtotal) = apply_discount(d("100.00"), Some("percentage"), Some(d("0.20")));
        assert_eq!(discount, d("20.00"));
        assert_eq!(subtotal, d("80.00"));
    }

    #[test]
    fn test_apply_discount_percentage_clamped() {
        // 150% off should be clamped to base_price
        let (discount, subtotal) = apply_discount(d("100.00"), Some("percentage"), Some(d("1.50")));
        assert_eq!(discount, d("100.00"));
        assert_eq!(subtotal, Decimal::ZERO);
    }

    #[test]
    fn test_apply_discount_flat() {
        // $30 flat off $100 = $30 discount, $70 subtotal
        let (discount, subtotal) = apply_discount(d("100.00"), Some("flat"), Some(d("30.00")));
        assert_eq!(discount, d("30.00"));
        assert_eq!(subtotal, d("70.00"));
    }

    #[test]
    fn test_apply_discount_flat_clamped() {
        // $150 flat off $100 should be clamped to $100
        let (discount, subtotal) = apply_discount(d("100.00"), Some("flat"), Some(d("150.00")));
        assert_eq!(discount, d("100.00"));
        assert_eq!(subtotal, Decimal::ZERO);
    }

    #[test]
    fn test_apply_discount_negative_value_treated_as_zero() {
        // Negative discount value should be clamped to 0
        let (discount, subtotal) = apply_discount(d("100.00"), Some("flat"), Some(d("-10.00")));
        assert_eq!(discount, Decimal::ZERO);
        assert_eq!(subtotal, d("100.00"));
    }

    #[test]
    fn test_round2_basic() {
        let val = d("99.999");
        assert_eq!(round2(val), d("100.00"));
    }

    #[test]
    fn test_round2_midpoint() {
        // Banker's rounding: 0.125 -> 0.12 (round to even)
        let val = d("0.125");
        assert_eq!(round2(val), d("0.12"));

        // 0.135 -> 0.14 (round to even)
        let val2 = d("0.135");
        assert_eq!(round2(val2), d("0.14"));
    }

    #[test]
    fn test_clamp_discount_within_range() {
        assert_eq!(clamp_discount(d("50"), d("100")), d("50"));
    }

    #[test]
    fn test_clamp_discount_exceeds_base() {
        assert_eq!(clamp_discount(d("200"), d("100")), d("100"));
    }

    #[test]
    fn test_clamp_discount_negative() {
        assert_eq!(clamp_discount(d("-5"), d("100")), Decimal::ZERO);
    }
}
