//! BikesNest domain crate.
//!
//! Pure business concepts. MUST NOT depend on axum, sqlx, askama, or any
//! infrastructure.

use thiserror::Error;

pub mod auth;
pub mod community;
pub mod freshness;
pub mod hours;
pub mod moderation;
pub mod parking;
pub mod parking_edit;
pub use parking_edit::ParkingEdit;
pub mod photo;
pub mod privacy;

pub use auth::{
    AccountState, AuthenticationProvider, CsrfToken, Password, PasswordPolicy, ProviderIdentity,
    Role, SessionId, User, VerificationToken,
};
pub use community::{
    AttributeResult, ChangeKind, Confidence, ExistenceResult, ExistenceSignal, ProposalKind,
    ProposalPayload, ProposalStatus, ProposedChange, ReviewBody, RevisionSummary, StarRating,
    VerificationKind, confidence, is_known_attribute_code,
};
pub use freshness::{DEFAULT_THRESHOLDS, FreshnessCategory, FreshnessThresholds, categorize};
pub use hours::{OpenStatus, OpeningHours, TimeRange, hms};
pub use moderation::{
    ModerationLimits, REPORT_REASONS, ReportDescription, ReportOutcome, ReportState,
    ReportTargetType, is_known_report_reason, reason_allowed_for,
};
pub use parking::{
    Cost, CurrencyCode, GeoPoint, ModerationState, Money, ParkingLocation, ParkingType,
    PricingUnit, Rating, SECURITY_FEATURE_CODES, SecurityFeature, SecurityState,
    is_known_security_code,
};
pub use photo::{
    ALLOWED_INPUT_FORMATS, DERIVATIVE_QUALITY, MAX_PHOTO_BYTES, MAX_PHOTO_MEGAPIXELS,
    PhotoDimensions, PhotoLimits, PhotoModerationState, THUMBNAIL_MAX_SIDE, bytes_within_limit,
    format_allowed,
};
pub use privacy::{
    ExportState, PRIVACY_REQUEST_SLA_DAYS, PolicyKind, PrivacyRequestKind, PrivacyRequestState,
    RetentionPolicy, anonymized_email, privacy_request_days_left,
};

/// Database identifier of a user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserId(pub i64);

/// Validated, normalized email address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserEmail(String);

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DomainError {
    #[error("email must not be empty")]
    EmptyEmail,
    #[error("email is not valid: {0}")]
    InvalidEmail(String),
    #[error("{0}")]
    Invalid(String),
    #[error("password does not meet the policy")]
    WeakPassword,
    #[error("unknown role code: {0}")]
    InvalidRole(String),
    #[error("unknown account state code: {0}")]
    InvalidState(String),
}

impl UserEmail {
    /// Validates and normalizes (lowercase, trimmed) an email address.
    ///
    /// keeps validation deliberately simple (shape check only);
    /// full RFC-style validation is unnecessary for storage and
    /// deliverability is confirmed by the verification flow.
    pub fn parse(raw: &str) -> Result<Self, DomainError> {
        let normalized = raw.trim().to_lowercase();
        if normalized.is_empty() {
            return Err(DomainError::EmptyEmail);
        }
        let Some((local, domain)) = normalized.split_once('@') else {
            return Err(DomainError::InvalidEmail(raw.to_string()));
        };
        if local.is_empty() || domain.is_empty() || !domain.contains('.') {
            return Err(DomainError::InvalidEmail(raw.to_string()));
        }
        Ok(Self(normalized))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for UserEmail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The language a user reads. Stored on the account so
/// anything written *outside* a request — a transactional email rendered by a
/// background job — can still address the person in their own language.
///
/// This is a validated code, not a presentation object: the domain and
/// application layers pass it around, and only the layers that actually render
/// text (web, infrastructure) turn it into a `bikesnest_i18n::Locale`. That is
/// what keeps the message catalog out of the inner layers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LocaleCode {
    /// Brazilian Portuguese — the product default.
    #[default]
    PtBr,
    En,
}

impl LocaleCode {
    /// Parse a stored/incoming code. Accepts the canonical forms plus the
    /// lowercase URL spelling (`/lang/pt-br`) and a bare `pt`.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "pt-br" | "pt" => Some(LocaleCode::PtBr),
            "en" => Some(LocaleCode::En),
            _ => None,
        }
    }

    /// The canonical code, as persisted in `users.locale` and as the email
    /// renderer parses it back.
    pub fn as_str(self) -> &'static str {
        match self {
            LocaleCode::PtBr => "pt-BR",
            LocaleCode::En => "en",
        }
    }
}

impl std::fmt::Display for LocaleCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_is_trimmed_and_lowercased() {
        let email = UserEmail::parse("  Ada@Example.COM ").expect("valid");
        assert_eq!(email.as_str(), "ada@example.com");
    }

    #[test]
    fn locale_code_parses_every_spelling_the_app_uses() {
        // Canonical (`users.locale`), the lowercase URL form (`/lang/pt-br`)
        // and the bare primary subtag all resolve.
        assert_eq!(LocaleCode::parse("pt-BR"), Some(LocaleCode::PtBr));
        assert_eq!(LocaleCode::parse("pt-br"), Some(LocaleCode::PtBr));
        assert_eq!(LocaleCode::parse("pt"), Some(LocaleCode::PtBr));
        assert_eq!(LocaleCode::parse("en"), Some(LocaleCode::En));
        assert_eq!(LocaleCode::parse("EN"), Some(LocaleCode::En));
        assert_eq!(LocaleCode::parse("fr"), None);
        // `as_str` emits exactly what the `users.locale` CHECK constraint allows.
        assert_eq!(LocaleCode::PtBr.as_str(), "pt-BR");
        assert_eq!(LocaleCode::En.as_str(), "en");
        assert_eq!(LocaleCode::default(), LocaleCode::PtBr);
    }

    #[test]
    fn email_requires_local_part_domain_and_tld_dot() {
        assert_eq!(UserEmail::parse(""), Err(DomainError::EmptyEmail));
        assert!(UserEmail::parse("no-at-sign").is_err());
        assert!(UserEmail::parse("@example.com").is_err());
        assert!(UserEmail::parse("user@nodot").is_err());
        assert!(UserEmail::parse("user@example.com").is_ok());
    }
}
