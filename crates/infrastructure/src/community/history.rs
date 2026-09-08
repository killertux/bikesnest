//! SQL-backed contribution-history read model (contribution history).
//!
//! Aggregates a user's contributions across the creator / editor / reviewer /
//! verifier / proposer / favoriter / photo-uploader tables into one
//! time-ordered feed, in a single `UNION ALL` query instead of eight round
//! trips — each branch is narrowed to `user` (index-backed:
//! `parking_location.creator_id`, `parking_revision.editor_id`,
//! `parking_proposal.proposer_id`, `review.author_id`, `verification.user_id`,
//! `favorite`'s own PK, `parking_photo.uploader_id`,
//! `review_photo.uploader_id`), and the whole union is bounded with a keyset
//! cursor on `(at, id)` instead of loading every row ever and
//! sorting/truncating in Rust.
//!
//! `id` is a synthetic per-row cursor value, not a foreign key: each source
//! table has its own independent primary key, so every branch encodes
//! `pk * 10 + source_tag` (tags 1-8, one per source). Two different sources
//! can never collide on this value — for tags `t1 ≠ t2` in `1..=8`,
//! `pk1*10+t1 = pk2*10+t2` would require `(pk1-pk2)*10 = t2-t1`, impossible
//! since `|t2-t1| < 10` — so `(at, id)` is a valid total order for keyset
//! pagination even though `id` means nothing outside this query. A read
//! model only — tests use the committed-fixture pattern (it reads through the
//! pool, on other connections).
//!
//! The verification branch (tag 5) further splits `kind = 'verified'` from
//! `kind = 'parked_here'` — the `verification` table stores both signals, and
//! the web layer needs to tell a real existence/attribute verification
//! apart from a "parked here" note (which does not confirm anything about the
//! listing). Tags 7/8 (`parking_photo`/`review_photo`, uploader's own pending
//! upload) surface as `kind = 'photo.pending'` — the smaller of two options
//! considered for showing a user their pending photos in contribution history (the alternative,
//! a `PhotoRepository::pending_for_user` method threaded through
//! `PhotoService`/`AppState` plus a second web-layer read per contributions
//! request, was more surface for the same result).

use crate::Db;
use async_trait::async_trait;
use bikesnest_application::{ContributionError, ContributionHistoryReader, ContributionItem};
use bikesnest_domain::UserId;
use chrono::{DateTime, Utc};

pub struct SqlxContributionHistoryReader {
    db: Db,
}

impl SqlxContributionHistoryReader {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

#[derive(sqlx::FromRow)]
struct Row {
    kind: String,
    state: String,
    target: String,
    at: DateTime<Utc>,
    id: i64,
}

const UNION_SQL: &str = r#"
WITH events AS (
    SELECT 'added' AS kind, 'active' AS state, pl.name AS target, pl.created_at AS at,
           pl.id * 10 + 1 AS id
    FROM parking_location pl
    WHERE pl.creator_id = $1

    UNION ALL

    SELECT 'edited', 'history', pl.name, pr.created_at, pr.id * 10 + 2
    FROM parking_revision pr
    JOIN parking_location pl ON pl.id = pr.location_id
    WHERE pr.editor_id = $1 AND pr.change_kind = 'edit'

    UNION ALL

    SELECT 'proposed', lower(pp.status), pl.name, pp.created_at, pp.id * 10 + 3
    FROM parking_proposal pp
    JOIN parking_location pl ON pl.id = pp.location_id
    WHERE pp.proposer_id = $1

    UNION ALL

    SELECT 'reviewed', 'active', pl.name, r.created_at, r.id * 10 + 4
    FROM review r
    JOIN parking_location pl ON pl.id = r.location_id
    WHERE r.author_id = $1

    UNION ALL

    SELECT CASE WHEN v.kind = 'parked_here' THEN 'parked_here' ELSE 'verified' END,
           'active', pl.name, v.created_at, v.id * 10 + 5
    FROM verification v
    JOIN parking_location pl ON pl.id = v.location_id
    WHERE v.user_id = $1

    UNION ALL

    SELECT 'favorited', 'active', pl.name, f.created_at, f.location_id * 10 + 6
    FROM favorite f
    JOIN parking_location pl ON pl.id = f.location_id
    WHERE f.user_id = $1

    UNION ALL

    SELECT 'photo.pending', 'pending', pl.name, pp.created_at, pp.id * 10 + 7
    FROM parking_photo pp
    JOIN parking_location pl ON pl.id = pp.location_id
    WHERE pp.uploader_id = $1 AND pp.moderation_state = 'PENDING_REVIEW'

    UNION ALL

    SELECT 'photo.pending', 'pending', pl.name, rp.created_at, rp.id * 10 + 8
    FROM review_photo rp
    JOIN review r ON r.id = rp.review_id
    JOIN parking_location pl ON pl.id = r.location_id
    WHERE rp.uploader_id = $1 AND rp.moderation_state = 'PENDING_REVIEW'
)
SELECT kind, state, target, at, id
FROM events
WHERE $2::timestamptz IS NULL OR (at, id) < ($2::timestamptz, $3::bigint)
ORDER BY at DESC, id DESC
LIMIT $4::bigint
"#;

#[async_trait]
impl ContributionHistoryReader for SqlxContributionHistoryReader {
    async fn history(
        &self,
        user: UserId,
        after: Option<(DateTime<Utc>, i64)>,
        limit: i64,
    ) -> Result<Vec<ContributionItem>, ContributionError> {
        let limit = limit.clamp(1, 200);
        let (after_at, after_id) = match after {
            Some((at, id)) => (Some(at), Some(id)),
            None => (None, None),
        };
        let rows = sqlx::query_as::<_, Row>(UNION_SQL)
            .bind(user.0)
            .bind(after_at)
            .bind(after_id)
            .bind(limit)
            .fetch_all(
                &mut *self
                    .db
                    .acquire()
                    .await
                    .map_err(|e| db_err("history.acquire", e))?,
            )
            .await
            .map_err(|e| db_err("history.history", e))?;

        Ok(rows
            .into_iter()
            .map(|r| ContributionItem {
                kind: r.kind,
                target: r.target,
                state: r.state,
                at: r.at,
                id: r.id,
            })
            .collect())
    }
}

/// Classify + log the sqlx error (SQLSTATE, constraint), then map it onto
/// the feature error. `context` names the operation, e.g. `"history.history"`.
fn db_err(context: &'static str, e: sqlx::Error) -> ContributionError {
    crate::db_error::classify_and_log(context, e).into()
}
