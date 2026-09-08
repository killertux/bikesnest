//! SQL-backed audit log. Writes rows to `audit_events`.

use crate::Db;
use async_trait::async_trait;
use bikesnest_application::{AuditError, AuditEvent, AuditLog};

pub struct SqlxAuditLog {
    db: Db,
}

impl SqlxAuditLog {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

/// Every key the application writes into `audit_events.metadata`, and the
/// contract that keeps the column out of the erasure path.
///
/// Anonymization nulls `actor_user_id` and rewrites an email-shaped
/// `target_id`, but it does **not** rewrite `metadata` — which is only correct
/// while no key here can hold personal data. Each key below is either an
/// internal identifier (a row id, a position), an enum code, a timestamp, or
/// content the user published deliberately and which survives anonymized as
/// community data. None is an email address, an IP address, a user agent or a
/// display name.
///
/// `crates/infrastructure/tests/privacy_test.rs` asserts that the keys the
/// application actually writes are a subset of this list, so adding a new one
/// forces the classification to be made explicitly (and, if it *is* personal
/// data, forces a scrub in `privacy/anonymize.rs`).
pub const AUDIT_METADATA_KEYS: &[&str] = &[
    // privacy.rs — export TTL, manual-request kind, retention step counts.
    "expires_at",
    "kind",
    "steps",
    // community.rs — parking *location* name, location version, star rating.
    "name",
    "version",
    "rating",
    // moderation.rs — the reported row, and the moderator's/reporter's reason.
    "target_type",
    "target_id",
    "reason",
    // photo.rs — the parent row a photo hangs off, and its gallery position.
    "parent_id",
    "position",
    // Historic: `photo.uploaded` wrote the parent location id under this name
    // before it was generalized to `parent_id`. Old rows still carry it.
    "location_id",
];

#[async_trait]
impl AuditLog for SqlxAuditLog {
    async fn record(&self, event: AuditEvent) -> Result<(), AuditError> {
        sqlx::query(
            r#"
            INSERT INTO audit_events
                (actor_user_id, action, target_type, target_id, result, metadata)
            VALUES ($1, $2, $3, $4, $5, $6)
            "#,
        )
        .bind(event.actor_user_id.map(|u| u.0))
        .bind(event.action)
        .bind(event.target_type)
        .bind(event.target_id)
        .bind(event.result)
        .bind(event.metadata)
        .execute(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| crate::db_error::classify_and_log("audit.acquire", e))?,
        )
        .await
        .map_err(|e| crate::db_error::classify_and_log("audit.record", e))?;
        Ok(())
    }
}
