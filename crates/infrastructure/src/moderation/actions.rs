//! SQL-backed moderation repository: target-existence
//! checks, content flip actions (hide/restore), parking invalidation/restore and
//! proposal apply/reject. All writes that touch the location bump `version` and
//! append a ``moderation`` revision in one transaction.

use crate::Db;
use async_trait::async_trait;
use bikesnest_application::{
    ModerationError, ModerationRepository, PhotoKind, Proposal, ProposalApplication,
    ReportTargetPreview, review_excerpt,
};
use bikesnest_domain::{
    ModerationState, ProposalKind, ProposalPayload, ProposalStatus, ReportTargetType, UserId,
};
use chrono::{DateTime, Utc};
use std::collections::HashMap;

pub struct SqlxModerationRepository {
    db: Db,
}

impl SqlxModerationRepository {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

/// The dashboard's four counts, in one statement (four scalar subqueries).
const QUEUE_COUNTS_SQL: &str = r#"
    SELECT
        (SELECT COUNT(*) FROM parking_photo WHERE moderation_state = 'PENDING_REVIEW')
      + (SELECT COUNT(*) FROM review_photo WHERE moderation_state = 'PENDING_REVIEW')
        AS pending_photos,
        (SELECT COUNT(*) FROM report WHERE state = 'OPEN') AS open_reports,
        (SELECT COUNT(*) FROM report WHERE state = 'UNDER_REVIEW') AS under_review_reports,
        (SELECT COUNT(*) FROM parking_proposal WHERE status = 'PENDING') AS pending_proposals
"#;

#[derive(sqlx::FromRow)]
struct QueueCountsRow {
    pending_photos: i64,
    open_reports: i64,
    under_review_reports: i64,
    pending_proposals: i64,
}

impl SqlxModerationRepository {
    /// Runs the dashboard-counts query against any executor — the pool (what
    /// [`ModerationRepository::queue_counts`] uses) or a specific
    /// connection/transaction. Exposed so a test can take a race-free
    /// before/after delta by running both reads (and the fixture insert
    /// between them) on the *same* connection, immune to what other
    /// concurrently-running tests commit to these same global tables.
    pub async fn queue_counts_on<'e, E>(
        executor: E,
    ) -> Result<bikesnest_application::QueueCounts, ModerationError>
    where
        E: sqlx::PgExecutor<'e>,
    {
        let row = sqlx::query_as::<_, QueueCountsRow>(QUEUE_COUNTS_SQL)
            .fetch_one(executor)
            .await
            .map_err(|e| db_err("moderation.queue_counts", e))?;
        Ok(bikesnest_application::QueueCounts {
            pending_photos: row.pending_photos,
            open_reports: row.open_reports,
            under_review_reports: row.under_review_reports,
            pending_proposals: row.pending_proposals,
        })
    }
}

#[derive(sqlx::FromRow)]
struct ProposalRow {
    id: i64,
    location_id: i64,
    location_name: String,
    location_address: String,
    proposer_id: Option<i64>,
    base_version: i64,
    location_version: i64,
    kind: String,
    proposed: serde_json::Value,
    current_lat: Option<f64>,
    current_lon: Option<f64>,
    current_timezone: String,
    current_state: String,
    current_snapshot: serde_json::Value,
    status: String,
    created_at: DateTime<Utc>,
}

/// The columns every proposal read needs. The join already visits
/// `parking_location`, so carrying its current values costs nothing and saves
/// the moderation queue a second query per row to show a diff.
const PROPOSAL_COLUMNS: &str = r#"
    p.id, p.location_id, l.name AS location_name, l.address AS location_address,
    p.proposer_id, p.base_version, l.version AS location_version,
    p.kind, p.proposed,
    l.lat AS current_lat, l.lon AS current_lon, l.timezone AS current_timezone,
    l.moderation_state AS current_state,
    COALESCE((SELECT snapshot FROM parking_revision WHERE location_id=l.id AND version=l.version), '{}'::jsonb) AS current_snapshot,
    p.status, p.created_at
"#;

#[derive(sqlx::FromRow)]
struct ProposalLockRow {
    kind: String,
    proposer_id: Option<i64>,
    status: String,
    base_version: i64,
}

/// The AFTER-state core columns of a location, returned by the UPDATE…RETURNING.
#[derive(sqlx::FromRow)]
struct LocationRow {
    id: i64,
    name: String,
    address: String,
    description: Option<String>,
    parking_type: String,
    cost_kind: String,
    price_cents: Option<i64>,
    price_currency: Option<String>,
    price_unit: Option<String>,
    lat: Option<f64>,
    lon: Option<f64>,
    timezone: String,
    hours_unknown: bool,
    moderation_state: String,
    version: i64,
}

/// One report target, whatever its kind. Every column is nullable because a
/// single row shape serves all four queries; [`PreviewRow::into_preview`] keeps
/// the branch-free mapping honest.
#[derive(sqlx::FromRow)]
struct PreviewRow {
    target_id: i64,
    location_id: Option<i64>,
    location_name: Option<String>,
    location_address: Option<String>,
    review_id: Option<i64>,
    review_author_id: Option<i64>,
    review_rating: Option<i16>,
    review_body: Option<String>,
    photo_id: Option<i64>,
    photo_key: Option<String>,
    photo_thumbnail_key: Option<String>,
    target_state: Option<String>,
}

impl PreviewRow {
    fn into_preview(self) -> ReportTargetPreview {
        ReportTargetPreview {
            location_id: self.location_id,
            location_name: self.location_name,
            location_address: self.location_address,
            review_id: self.review_id,
            review_author_id: self.review_author_id.map(UserId),
            review_rating: self.review_rating,
            review_excerpt: self.review_body.as_deref().map(review_excerpt),
            photo_id: self.photo_id,
            photo_key: self.photo_key,
            photo_thumbnail_key: self.photo_thumbnail_key,
            target_state: self.target_state,
        }
    }
}

/// The preview query for one target kind. `target_state` is the reported
/// entity's own moderation state, which is what decides whether the queue's
/// "act on the content" button has anything left to do.
///
/// The body is cut down in SQL (`left(...)`) before it crosses the wire; the
/// application layer then trims it to the exact character budget.
fn preview_sql(target_type: ReportTargetType) -> &'static str {
    match target_type {
        ReportTargetType::Parking => {
            r#"
            SELECT l.id AS target_id, l.id AS location_id, l.name AS location_name,
                   l.address AS location_address,
                   NULL::bigint AS review_id, NULL::bigint AS review_author_id,
                   NULL::smallint AS review_rating, NULL::text AS review_body,
                   NULL::bigint AS photo_id, NULL::text AS photo_key,
                   NULL::text AS photo_thumbnail_key,
                   l.moderation_state AS target_state
            FROM parking_location l
            WHERE l.id = ANY($1)
            "#
        }
        ReportTargetType::Review => {
            r#"
            SELECT r.id AS target_id, l.id AS location_id, l.name AS location_name,
                   l.address AS location_address,
                   r.id AS review_id, r.author_id AS review_author_id,
                   r.rating AS review_rating, left(r.body, 400) AS review_body,
                   NULL::bigint AS photo_id, NULL::text AS photo_key,
                   NULL::text AS photo_thumbnail_key,
                   r.moderation_state AS target_state
            FROM review r
            JOIN parking_location l ON l.id = r.location_id
            WHERE r.id = ANY($1)
            "#
        }
        ReportTargetType::ParkingPhoto => {
            r#"
            SELECT p.id AS target_id, l.id AS location_id, l.name AS location_name,
                   l.address AS location_address,
                   NULL::bigint AS review_id, NULL::bigint AS review_author_id,
                   NULL::smallint AS review_rating, NULL::text AS review_body,
                   p.id AS photo_id, p.storage_key AS photo_key,
                   p.thumbnail_key AS photo_thumbnail_key,
                   p.moderation_state AS target_state
            FROM parking_photo p
            JOIN parking_location l ON l.id = p.location_id
            WHERE p.id = ANY($1)
            "#
        }
        ReportTargetType::ReviewPhoto => {
            r#"
            SELECT rp.id AS target_id, l.id AS location_id, l.name AS location_name,
                   l.address AS location_address,
                   r.id AS review_id, r.author_id AS review_author_id,
                   r.rating AS review_rating, left(r.body, 400) AS review_body,
                   rp.id AS photo_id, rp.storage_key AS photo_key,
                   rp.thumbnail_key AS photo_thumbnail_key,
                   rp.moderation_state AS target_state
            FROM review_photo rp
            JOIN review r ON r.id = rp.review_id
            JOIN parking_location l ON l.id = r.location_id
            WHERE rp.id = ANY($1)
            "#
        }
    }
}

#[async_trait]
impl ModerationRepository for SqlxModerationRepository {
    async fn target_exists(
        &self,
        target_type: ReportTargetType,
        target_id: i64,
    ) -> Result<bool, ModerationError> {
        let sql = match target_type {
            ReportTargetType::Parking => "SELECT 1 FROM parking_location WHERE id = $1",
            ReportTargetType::ParkingPhoto => "SELECT 1 FROM parking_photo WHERE id = $1",
            ReportTargetType::Review => "SELECT 1 FROM review WHERE id = $1",
            ReportTargetType::ReviewPhoto => "SELECT 1 FROM review_photo WHERE id = $1",
        };
        let row: Option<(i32,)> = sqlx::query_as(sql)
            .bind(target_id)
            .fetch_optional(
                &mut *self
                    .db
                    .acquire()
                    .await
                    .map_err(|e| db_err("moderation.acquire", e))?,
            )
            .await
            .map_err(|e| db_err("moderation.target_exists", e))?;
        Ok(row.is_some())
    }

    /// One statement per distinct target type on the page (≤4 for a queue page
    /// of any size), each an `= ANY($1)` over the ids of that type.
    async fn report_previews(
        &self,
        targets: &[(ReportTargetType, i64)],
    ) -> Result<HashMap<(ReportTargetType, i64), ReportTargetPreview>, ModerationError> {
        let mut out = HashMap::with_capacity(targets.len());
        for target_type in [
            ReportTargetType::Parking,
            ReportTargetType::ParkingPhoto,
            ReportTargetType::Review,
            ReportTargetType::ReviewPhoto,
        ] {
            let ids: Vec<i64> = targets
                .iter()
                .filter(|(t, _)| *t == target_type)
                .map(|(_, id)| *id)
                .collect();
            if ids.is_empty() {
                continue;
            }
            let rows = sqlx::query_as::<_, PreviewRow>(preview_sql(target_type))
                .bind(&ids)
                .fetch_all(
                    &mut *self
                        .db
                        .acquire()
                        .await
                        .map_err(|e| db_err("moderation.acquire", e))?,
                )
                .await
                .map_err(|e| db_err("moderation.report_previews", e))?;
            for row in rows {
                out.insert((target_type, row.target_id), row.into_preview());
            }
        }
        Ok(out)
    }

    async fn hide_review(&self, id: i64, moderator: UserId) -> Result<(), ModerationError> {
        let _ = moderator;
        let res = sqlx::query(
            "UPDATE review SET moderation_state = 'HIDDEN', updated_at = now()
             WHERE id = $1 AND moderation_state = 'ACTIVE'",
        )
        .bind(id)
        .execute(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("moderation.acquire", e))?,
        )
        .await
        .map_err(|e| db_err("moderation.hide_review", e))?;
        if res.rows_affected() != 1 {
            return Err(ModerationError::InvalidState);
        }
        Ok(())
    }

    async fn restore_review(&self, id: i64, moderator: UserId) -> Result<(), ModerationError> {
        let _ = moderator;
        let res = sqlx::query(
            "UPDATE review SET moderation_state = 'ACTIVE', updated_at = now()
             WHERE id = $1 AND moderation_state = 'HIDDEN'",
        )
        .bind(id)
        .execute(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("moderation.acquire", e))?,
        )
        .await
        .map_err(|e| db_err("moderation.restore_review", e))?;
        if res.rows_affected() != 1 {
            return Err(ModerationError::InvalidState);
        }
        Ok(())
    }

    async fn hide_photo(
        &self,
        kind: PhotoKind,
        id: i64,
        moderator: UserId,
    ) -> Result<(), ModerationError> {
        let table = kind.table();
        let res = sqlx::query(&format!(
            "UPDATE {table}
             SET moderation_state = 'HIDDEN', reviewed_by = $2, reviewed_at = now()
             WHERE id = $1 AND moderation_state = 'APPROVED'"
        ))
        .bind(id)
        .bind(moderator.0)
        .execute(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("moderation.acquire", e))?,
        )
        .await
        .map_err(|e| db_err("moderation.hide_photo", e))?;
        if res.rows_affected() != 1 {
            return Err(ModerationError::InvalidState);
        }
        Ok(())
    }

    async fn restore_photo(
        &self,
        kind: PhotoKind,
        id: i64,
        moderator: UserId,
    ) -> Result<(), ModerationError> {
        let table = kind.table();
        let res = sqlx::query(&format!(
            "UPDATE {table}
             SET moderation_state = 'APPROVED', reviewed_by = $2, reviewed_at = now()
             WHERE id = $1 AND moderation_state = 'HIDDEN'"
        ))
        .bind(id)
        .bind(moderator.0)
        .execute(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("moderation.acquire", e))?,
        )
        .await
        .map_err(|e| db_err("moderation.restore_photo", e))?;
        if res.rows_affected() != 1 {
            return Err(ModerationError::InvalidState);
        }
        Ok(())
    }

    async fn set_parking_state(
        &self,
        id: i64,
        from: &[ModerationState],
        to: ModerationState,
        moderator: UserId,
    ) -> Result<(), ModerationError> {
        let from_codes: Vec<String> = from.iter().map(|s| s.as_code().to_string()).collect();
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("moderation.set_parking_state", e))?;
        let mut tx = conn
            .begin()
            .await
            .map_err(|e| db_err("moderation.set_parking_state", e))?;

        let row = sqlx::query_as::<_, LocationRow>(
            r#"
            UPDATE parking_location
            SET moderation_state = $2, version = version + 1, updated_at = now()
            WHERE id = $1 AND moderation_state = ANY($3)
            RETURNING id, name, address, description, parking_type, cost_kind, price_cents,
                      price_currency, price_unit, lat, lon, timezone, hours_unknown,
                      moderation_state, version
            "#,
        )
        .bind(id)
        .bind(to.as_code())
        .bind(&from_codes)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_err("moderation.set_parking_state", e))?;
        let Some(row) = row else {
            return Err(ModerationError::InvalidState);
        };

        let snapshot = snapshot_with(&mut tx, &row).await?;
        insert_revision(
            &mut tx,
            row.id,
            row.version,
            moderator,
            "moderated",
            snapshot,
        )
        .await?;
        tx.commit()
            .await
            .map_err(|e| db_err("moderation.set_parking_state", e))?;
        Ok(())
    }

    /// Oldest first (`id ASC`), same reasoning as `report.list`: a FIFO queue
    /// with a simple, exact keyset cursor.
    async fn list_pending_proposals(
        &self,
        after_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<Proposal>, ModerationError> {
        let limit = limit.clamp(1, 200);
        // `after_id` of `None` still binds a parameter (a NULL) rather than
        // switching to a second SQL string: one statement, one plan.
        let sql = format!(
            r#"
            SELECT {PROPOSAL_COLUMNS}
            FROM parking_proposal p
            JOIN parking_location l ON l.id = p.location_id
            WHERE p.status = 'PENDING' AND ($1::bigint IS NULL OR p.id > $1::bigint)
            ORDER BY p.id ASC
            LIMIT $2
            "#
        );
        let rows = sqlx::query_as::<_, ProposalRow>(&sql)
            .bind(after_id)
            .bind(limit)
            .fetch_all(
                &mut *self
                    .db
                    .acquire()
                    .await
                    .map_err(|e| db_err("moderation.acquire", e))?,
            )
            .await
            .map_err(|e| db_err("moderation.list_pending_proposals", e))?;
        rows.into_iter().map(map_proposal).collect()
    }

    /// The dashboard's four counts in one statement (four scalar
    /// subqueries), instead of loading and `.len()`-ing four full lists.
    async fn queue_counts(&self) -> Result<bikesnest_application::QueueCounts, ModerationError> {
        Self::queue_counts_on(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("moderation.acquire", e))?,
        )
        .await
    }

    async fn get_proposal(&self, id: i64) -> Result<Option<Proposal>, ModerationError> {
        let sql = format!(
            r#"
            SELECT {PROPOSAL_COLUMNS}
            FROM parking_proposal p
            JOIN parking_location l ON l.id = p.location_id
            WHERE p.id = $1
            "#
        );
        let row = sqlx::query_as::<_, ProposalRow>(&sql)
            .bind(id)
            .fetch_optional(
                &mut *self
                    .db
                    .acquire()
                    .await
                    .map_err(|e| db_err("moderation.acquire", e))?,
            )
            .await
            .map_err(|e| db_err("moderation.get_proposal", e))?;
        match row {
            Some(r) => Ok(Some(map_proposal(r)?)),
            None => Ok(None),
        }
    }

    async fn approve_proposal(
        &self,
        id: i64,
        moderator: UserId,
        applied: ProposalApplication,
    ) -> Result<(), ModerationError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("moderation.approve_proposal", e))?;
        let mut tx = conn
            .begin()
            .await
            .map_err(|e| db_err("moderation.approve_proposal", e))?;

        approve_in_transaction(&mut tx, id, moderator, applied).await?;

        tx.commit()
            .await
            .map_err(|e| db_err("moderation.approve_proposal", e))?;
        Ok(())
    }

    async fn reject_proposal(
        &self,
        id: i64,
        moderator: UserId,
        reason: &str,
    ) -> Result<(), ModerationError> {
        let res = sqlx::query(
            r#"UPDATE parking_proposal SET status = 'REJECTED', resolved_by = $2, resolved_at = now(), decision_reason = NULLIF($3, ''),
              decision_approvals = (SELECT COUNT(*) FROM parking_proposal_vote v JOIN users u ON u.id = v.voter_id WHERE v.proposal_id = parking_proposal.id AND v.vote = 'APPROVE' AND u.account_state = 'ACTIVE' AND u.email_verified_at IS NOT NULL),
              decision_rejections = (SELECT COUNT(*) FROM parking_proposal_vote v JOIN users u ON u.id = v.voter_id WHERE v.proposal_id = parking_proposal.id AND v.vote = 'REJECT' AND u.account_state = 'ACTIVE' AND u.email_verified_at IS NOT NULL)
             WHERE id = $1 AND status = 'PENDING'"#,
        )
        .bind(id)
        .bind(moderator.0)
        .bind(reason.trim())
        .execute(&mut *self.db.acquire().await.map_err(|e| db_err("moderation.acquire", e))?)
        .await
        .map_err(|e| db_err("moderation.reject_proposal", e))?;
        if res.rows_affected() != 1 {
            return Err(ModerationError::InvalidState);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse a proposal row at the boundary — this is the only place the stored
/// JSON payload is interpreted.
///
/// `kind` and `status` are `CHECK`-constrained columns, so an unreadable one is
/// a schema bug and still errors. The `proposed` JSON is not constrained, so it
/// degrades to `ProposedChange::Unknown`: a row written by a future (or broken)
/// version becomes a "needs manual review" card instead of failing the page.
fn map_proposal(r: ProposalRow) -> Result<Proposal, ModerationError> {
    let kind = ProposalKind::from_code(&r.kind).map_err(ModerationError::from)?;
    let payload = ProposalPayload::from_json(kind, &r.proposed);
    Ok(Proposal {
        id: r.id,
        location_id: r.location_id,
        location_name: r.location_name,
        location_address: r.location_address,
        proposer_id: r.proposer_id.map(UserId),
        base_version: r.base_version,
        location_version: r.location_version,
        kind,
        change: payload.change,
        reason: payload.reason,
        current_lat: r.current_lat,
        current_lon: r.current_lon,
        current_timezone: r.current_timezone,
        current_state: ModerationState::from_code(&r.current_state)
            .map_err(ModerationError::from)?,
        current_snapshot: r.current_snapshot,
        status: ProposalStatus::from_code(&r.status).map_err(ModerationError::from)?,
        created_at: r.created_at,
    })
}

async fn insert_revision(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    location_id: i64,
    version: i64,
    editor: UserId,
    summary: &str,
    snapshot: serde_json::Value,
) -> Result<(), ModerationError> {
    sqlx::query(
        r#"
        INSERT INTO parking_revision
            (location_id, version, editor_id, change_kind, summary, snapshot)
        VALUES ($1, $2, $3, 'moderation', $4, $5)
        "#,
    )
    .bind(location_id)
    .bind(version)
    .bind(editor.0)
    .bind(summary)
    .bind(snapshot)
    .execute(&mut **tx)
    .await
    .map_err(|e| db_err("moderation.insert_revision", e))?;
    Ok(())
}

/// Read an after-state snapshot (name/address/type/cost/point/tz/hours/security/
/// moderation_state) for a location row — following the  snapshot shape —
/// reading the unchanged hours + security rows from the open transaction.
async fn snapshot_with(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    row: &LocationRow,
) -> Result<serde_json::Value, ModerationError> {
    let hours = read_hours_json(tx, row.id, row.hours_unknown).await?;
    let security = read_security_json(tx, row.id).await?;
    let cost = cost_json(
        &row.cost_kind,
        row.price_cents,
        row.price_currency.as_deref(),
        row.price_unit.as_deref(),
    );
    Ok(serde_json::json!({
        "name": row.name,
        "address": row.address,
        "description": row.description,
        "type": row.parking_type,
        "cost": cost,
        "point": { "lat": row.lat.unwrap_or(0.0), "lon": row.lon.unwrap_or(0.0) },
        "timezone": row.timezone,
        "hours": hours,
        "security": security,
        "moderation_state": row.moderation_state,
    }))
}

async fn read_hours_json(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    location_id: i64,
    unknown: bool,
) -> Result<serde_json::Value, ModerationError> {
    #[derive(sqlx::FromRow)]
    struct HourRow {
        day_of_week: i16,
        opens_at: chrono::NaiveTime,
        closes_at: chrono::NaiveTime,
        all_day: bool,
    }
    let rows = sqlx::query_as::<_, HourRow>("SELECT day_of_week, opens_at, closes_at, all_day FROM opening_hours WHERE location_id = $1 ORDER BY day_of_week, opens_at").bind(location_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(|e| db_err("moderation.read_hours_json", e))?;
    let rows_json: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            serde_json::json!([
                r.day_of_week,
                r.opens_at.to_string(),
                r.closes_at.to_string(),
                r.all_day,
            ])
        })
        .collect();
    Ok(serde_json::json!({ "unknown": unknown, "rows": rows_json }))
}

async fn read_security_json(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    location_id: i64,
) -> Result<serde_json::Value, ModerationError> {
    #[derive(sqlx::FromRow)]
    struct SecRow {
        feature_code: String,
        state: i16,
    }
    let rows = sqlx::query_as::<_, SecRow>(
        "SELECT feature_code, state FROM parking_security WHERE location_id = $1",
    )
    .bind(location_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(|e| db_err("moderation.read_security_json", e))?;
    let sec_json: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| serde_json::json!([r.feature_code, r.state]))
        .collect();
    Ok(serde_json::Value::Array(sec_json))
}

fn cost_json(
    kind: &str,
    cents: Option<i64>,
    currency: Option<&str>,
    unit: Option<&str>,
) -> serde_json::Value {
    match kind {
        "free" => serde_json::json!({ "kind": "free" }),
        "unknown" => serde_json::json!({ "kind": "unknown" }),
        "paid" => match (cents, currency, unit) {
            (Some(c), Some(cur), Some(u)) => {
                serde_json::json!({ "kind": "paid", "cents": c, "currency": cur, "unit": u })
            }
            _ => serde_json::json!({ "kind": "paid" }),
        },
        _ => serde_json::json!({ "kind": "unknown" }),
    }
}

/// Classify + log the sqlx error (SQLSTATE, constraint), then map it onto
/// the feature error. `context` names the operation, e.g. `"moderation.hide_review"`.
fn db_err(context: &'static str, e: sqlx::Error) -> ModerationError {
    crate::db_error::classify_and_log(context, e).into()
}

/// Shared atomic publisher; callers hold a transaction through vote/decision and revision.
pub(crate) async fn approve_in_transaction(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: i64,
    moderator: UserId,
    applied: ProposalApplication,
) -> Result<(), ModerationError> {
    // Lock order — location, then this proposal, then its siblings by id.
    // Taking the location first is what keeps two moderators approving two
    // proposals on the SAME location from deadlocking: they queue on the
    // one location row instead of each holding a proposal the other wants.
    // Never reverse this, and never lock a sibling before the location.
    let locked: Option<(i64, i64)> = sqlx::query_as(
        r#"
            SELECT id, version FROM parking_location
            WHERE id = (SELECT location_id FROM parking_proposal WHERE id = $1)
            FOR UPDATE
            "#,
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| db_err("moderation.approve_proposal", e))?;
    let Some((location_id, current_version)) = locked else {
        return Err(ModerationError::NotFound);
    };

    let prop = sqlx::query_as::<_, ProposalLockRow>(
            "SELECT status, base_version, kind, proposer_id FROM parking_proposal WHERE id = $1 FOR UPDATE",
        )
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| db_err("moderation.approve_proposal", e))?;
    let Some(prop) = prop else {
        return Err(ModerationError::NotFound);
    };
    if prop.status != ProposalStatus::Pending.as_code() {
        return Err(ModerationError::InvalidState);
    }
    // The proposal describes a change to the location AS IT WAS. If the
    // location moved on since, applying it would silently overwrite work
    // the proposer never saw — the moderator has to look again.
    if prop.base_version != current_version {
        return Err(ModerationError::StaleProposal);
    }
    if prop.kind != applied.kind().as_code() || prop.proposer_id == Some(moderator.0) {
        return Err(ModerationError::InvalidState);
    }
    let base_version = prop.base_version;

    let row = match &applied {
            ProposalApplication::EditDetails(edit) => {
                let (kind, cents, currency, unit) = crate::community::contribution::cost_parts(&edit.cost);
                let row = sqlx::query_as::<_, LocationRow>(r#"
                    UPDATE parking_location SET name=$1, address=$2, description=$3, parking_type=$4,
                        cost_kind=$5, price_cents=$6, price_currency=$7, price_unit=$8, hours_unknown=$9,
                        version=version+1, updated_at=now(), last_meaningful_update_at=now()
                    WHERE id=$10 AND version=$11 AND moderation_state='ACTIVE'
                    RETURNING id, name, address, description, parking_type, cost_kind, price_cents,
                              price_currency, price_unit, lat, lon, timezone, hours_unknown, moderation_state, version
                "#).bind(edit.name.trim()).bind(edit.address.trim()).bind(&edit.description).bind(edit.parking_type.as_code())
                .bind(kind).bind(cents).bind(currency).bind(unit).bind(edit.hours.is_unknown()).bind(location_id).bind(base_version)
                .fetch_optional(&mut **tx).await.map_err(|e| db_err("moderation.approve_proposal",e))?;
                crate::community::contribution::write_hours(tx, location_id, &edit.hours).await.map_err(|_| ModerationError::Internal)?;
                crate::community::contribution::write_security(tx, location_id, &edit.security).await.map_err(|_| ModerationError::Internal)?;
                row
            }
            ProposalApplication::MoveLocation { lat, lon, timezone } => {
                sqlx::query_as::<_, LocationRow>(
                    r#"
                    UPDATE parking_location
                    SET location = ST_SetSRID(ST_MakePoint($2, $1), 4326)::geography,
                        timezone = $3, version = version + 1, updated_at = now(), last_meaningful_update_at = now()
                    WHERE id = $4 AND version = $5
                    RETURNING id, name, address, description, parking_type, cost_kind, price_cents,
                              price_currency, price_unit, lat, lon, timezone, hours_unknown,
                              moderation_state, version
                    "#,
                )
                .bind(lat)
                .bind(lon)
                .bind(timezone.name())
                .bind(location_id)
                .bind(base_version)
                .fetch_optional(&mut **tx)
                .await
                .map_err(|e| db_err("moderation.approve_proposal", e))?
            }
            ProposalApplication::ChangeExistence { exists } => {
                let state = if *exists { "ACTIVE" } else { "REMOVED" };
                sqlx::query_as::<_, LocationRow>(
                    r#"
                    UPDATE parking_location
                    SET moderation_state = $2, version = version + 1, updated_at = now()
                    WHERE id = $1 AND version = $3
                    RETURNING id, name, address, description, parking_type, cost_kind, price_cents,
                              price_currency, price_unit, lat, lon, timezone, hours_unknown,
                              moderation_state, version
                    "#,
                )
                .bind(location_id)
                .bind(state)
                .bind(base_version)
                .fetch_optional(&mut **tx)
                .await
                .map_err(|e| db_err("moderation.approve_proposal", e))?
            }
        };
    let Some(row) = row else {
        // The location is locked above, so it exists: 0 rows can only mean
        // the `version = base_version` predicate failed.
        return Err(ModerationError::StaleProposal);
    };

    let snapshot = snapshot_with(tx, &row).await?;
    let summary = match &applied {
        ProposalApplication::EditDetails(_) => "proposal applied (details)",
        ProposalApplication::MoveLocation { .. } => "proposal applied (move)",
        ProposalApplication::ChangeExistence { .. } => "proposal applied (existence)",
    };
    insert_revision(tx, location_id, row.version, moderator, summary, snapshot).await?;

    sqlx::query(r#"
            UPDATE parking_proposal SET status = 'APPROVED', resolved_by = $2, resolved_at = now(),
              decision_approvals = (SELECT COUNT(*) FROM parking_proposal_vote v JOIN users u ON u.id = v.voter_id WHERE v.proposal_id = parking_proposal.id AND v.vote = 'APPROVE' AND u.account_state = 'ACTIVE' AND u.email_verified_at IS NOT NULL),
              decision_rejections = (SELECT COUNT(*) FROM parking_proposal_vote v JOIN users u ON u.id = v.voter_id WHERE v.proposal_id = parking_proposal.id AND v.vote = 'REJECT' AND u.account_state = 'ACTIVE' AND u.email_verified_at IS NOT NULL)
            WHERE id = $1
        "#)
            .bind(id)
            .bind(moderator.0)
            .execute(&mut **tx)
            .await
            .map_err(|e| db_err("moderation.approve_proposal", e))?;
    // Supersede the other PENDING proposals on this location. The
    // sub-select takes their row locks in id order, so two transactions
    // that reach this point on different locations never cross-lock.
    sqlx::query(
        r#"
            UPDATE parking_proposal SET status = 'SUPERSEDED'
            WHERE id IN (
                SELECT id FROM parking_proposal
                WHERE location_id = $1 AND status = 'PENDING' AND id <> $2
                ORDER BY id
                FOR UPDATE
            )
            "#,
    )
    .bind(location_id)
    .bind(id)
    .execute(&mut **tx)
    .await
    .map_err(|e| db_err("moderation.approve_proposal", e))?;

    Ok(())
}
