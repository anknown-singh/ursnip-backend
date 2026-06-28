pub mod api_provider;
pub mod service;
pub mod smtp;
pub mod templates;

pub use service::{EmailMessage, EmailProvider, EmailService};
