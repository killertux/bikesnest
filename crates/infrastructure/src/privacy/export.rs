//! SQL-backed personal-data export repository.
//!
//! `assemble_payload` runs one read per section and builds the versioned
//! `ExportPayload`. By construction it never selects credential/session/token
//! hashes, CSRF tokens or audit rows — the export is the *data subject's* data
//! only. The download token is stored only as its SHA-256 hex hash
//! and compared in constant time.

use crate::Db;
use crate::auth::hash::sha256_hex;
use async_trait::async_trait;
use bikesnest_application::{
    Export, ExportAccount, ExportDownload, ExportFavorite, ExportPayload, ExportPhoto,
    ExportProposal, ExportProposalVote, ExportProvider, ExportReport, ExportRepository,
    ExportReview, ExportReviewRevision, ExportSession, ExportVerification, NewExport, PrivacyError,
};
use bikesnest_domain::{ExportState, UserId};
use chrono::{DateTime, Utc};

pub struct SqlxExportRepository {
    db: Db,
}

impl SqlxExportRepository {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

// Row structs — field names must match the SELECT column names (sqlx `query_as!`).

#[derive(sqlx::FromRow)]
struct AccountRow {
    id: i64,
    email: String,
    display_name: Option<String>,
    public_contribution_name: bool,
    public_contribution_name_updated_at: Option<DateTime<Utc>>,
    account_state: String,
    email_verified_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct RoleRow {
    role: String,
}

#[derive(sqlx::FromRow)]
struct IdentityRow {
    provider: String,
    provider_subject: String,
    email_verified: Option<bool>,
}

#[derive(sqlx::FromRow)]
struct SessionRow {
    created_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct FavoriteRow {
    location_id: i64,
    created_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct ReviewRow {
    id: i64,
    location_id: i64,
    rating: i16,
    body: String,
    public_author: bool,
    moderation_state: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct RevisionRow {
    review_id: i64,
    rating: i16,
    body: String,
    edited_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct VerificationRow {
    location_id: i64,
    kind: String,
    result: String,
    attribute_code: Option<String>,
    created_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct ProposalRow {
    location_id: i64,
    base_version: i64,
    kind: String,
    proposed: serde_json::Value,
    status: String,
    created_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct ProposalVoteRow {
    proposal_id: i64,
    vote: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct ReportRow {
    target_type: String,
    target_id: i64,
    reason: String,
    description: Option<String>,
    state: String,
    created_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct ParkingPhotoRow {
    location_id: i64,
    storage_key: String,
    thumbnail_key: Option<String>,
    content_type: String,
    moderation_state: String,
    created_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct ReviewPhotoRow {
    review_id: i64,
    storage_key: String,
    thumbnail_key: Option<String>,
    moderation_state: String,
    created_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct ExportRow {
    id: i64,
    user_id: i64,
    state: String,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    downloaded_at: Option<DateTime<Utc>>,
}

#[derive(sqlx::FromRow)]
struct DownloadRow {
    payload: serde_json::Value,
}

fn constant_time_hex_eq(a: &str, b: &str) -> bool {
    let (a, b) = match (a.len(), b.len()) {
        (la, lb) if la == lb && la % 2 == 0 => (a, b),
        _ => return false,
    };
    let mut diff = 0u8;
    for (x, y) in a.bytes().zip(b.bytes()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[async_trait]
impl ExportRepository for SqlxExportRepository {
    /// Assemble the payload as **one consistent snapshot**.
    ///
    /// Every section reads inside a single `REPEATABLE READ` transaction, so
    /// the export is the account as it stood at one instant. Read on the pool
    /// a section at a time (as this did before), a concurrent edit could land
    /// between two statements and the document would describe a state that
    /// never existed — a review counted in one section and missing from the
    /// next. The isolation level is set as the transaction's first statement,
    /// which is where Postgres accepts it.
    async fn assemble_payload(&self, user_id: UserId) -> Result<ExportPayload, PrivacyError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("export.assemble_payload", e))?;
        let mut tx = conn
            .begin_snapshot()
            .await
            .map_err(|e| db_err("export.assemble_payload", e))?;
        let now = Utc::now();

        let account = {
            let row = sqlx::query_as::<_, AccountRow>(
                r#"
                SELECT id, email, display_name, public_contribution_name,
                       public_contribution_name_updated_at, account_state, email_verified_at, created_at
                FROM users WHERE id = $1
                "#,
            )
            .bind(user_id.0)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| db_err("export.assemble_payload", e))?
            .ok_or(PrivacyError::NotFound)?;
            let roles: Vec<String> = sqlx::query_as::<_, RoleRow>(
                "SELECT role FROM user_roles WHERE user_id = $1 ORDER BY role",
            )
            .bind(user_id.0)
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| db_err("export.assemble_payload", e))?
            .into_iter()
            .map(|r| r.role)
            .collect();
            ExportAccount {
                user_id: row.id,
                email: row.email,
                display_name: row.display_name,
                public_contribution_name: row.public_contribution_name,
                public_contribution_name_updated_at: row.public_contribution_name_updated_at,
                account_state: row.account_state,
                email_verified_at: row.email_verified_at,
                created_at: row.created_at,
                roles,
            }
        };

        let authentication = sqlx::query_as::<_, IdentityRow>(
            r#"
            SELECT ai.provider, ai.provider_subject,
                   COALESCE((u.email_verified_at IS NOT NULL), false) AS email_verified
            FROM authentication_identities ai
            LEFT JOIN users u ON u.id = ai.user_id
            WHERE ai.user_id = $1
            "#,
        )
        .bind(user_id.0)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| db_err("export.assemble_payload", e))?
        .into_iter()
        .map(|r| ExportProvider {
            provider: r.provider,
            subject: r.provider_subject,
            email_verified: r.email_verified.unwrap_or(false),
        })
        .collect();

        let sessions = sqlx::query_as::<_, SessionRow>("SELECT created_at, last_seen_at, expires_at FROM sessions WHERE user_id = $1 ORDER BY created_at").bind(user_id.0)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| db_err("export.assemble_payload", e))?
        .into_iter()
        .map(|r| ExportSession {
            created_at: r.created_at,
            last_seen_at: r.last_seen_at,
            expires_at: r.expires_at,
        })
        .collect();

        let favorites = sqlx::query_as::<_, FavoriteRow>(
            "SELECT location_id, created_at FROM favorite WHERE user_id = $1 ORDER BY created_at",
        )
        .bind(user_id.0)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| db_err("export.assemble_payload", e))?
        .into_iter()
        .map(|r| ExportFavorite {
            location_id: r.location_id,
            created_at: r.created_at,
        })
        .collect();

        let reviews = {
            let rows = sqlx::query_as::<_, ReviewRow>(
                r#"
                SELECT id, location_id, rating, body, public_author, moderation_state, created_at, updated_at
                FROM review WHERE author_id = $1 ORDER BY created_at
                "#,
            )
            .bind(user_id.0)
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| db_err("export.assemble_payload", e))?;

            // One query for every review's history, grouped in Rust — not one
            // query per review. An author with 200 reviews used to cost 200
            // round trips inside the snapshot transaction, holding it open for
            // as long as that took.
            let review_ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
            let mut by_review: std::collections::HashMap<i64, Vec<ExportReviewRevision>> =
                std::collections::HashMap::with_capacity(rows.len());
            if !review_ids.is_empty() {
                let revisions = sqlx::query_as::<_, RevisionRow>(
                    r#"
                    SELECT review_id, rating, body, edited_at
                    FROM review_revision WHERE review_id = ANY($1)
                    ORDER BY review_id, edited_at
                    "#,
                )
                .bind(&review_ids)
                .fetch_all(&mut *tx)
                .await
                .map_err(|e| db_err("export.assemble_payload", e))?;
                for rev in revisions {
                    by_review
                        .entry(rev.review_id)
                        .or_default()
                        .push(ExportReviewRevision {
                            rating: rev.rating,
                            body: rev.body,
                            edited_at: rev.edited_at,
                        });
                }
            }

            rows.into_iter()
                .map(|r| ExportReview {
                    revisions: by_review.remove(&r.id).unwrap_or_default(),
                    id: r.id,
                    location_id: r.location_id,
                    rating: r.rating,
                    body: r.body,
                    public_author: r.public_author,
                    moderation_state: r.moderation_state,
                    created_at: r.created_at,
                    updated_at: r.updated_at,
                })
                .collect()
        };

        let verifications = sqlx::query_as::<_, VerificationRow>(
            r#"
            SELECT location_id, kind, result, attribute_code, created_at
            FROM verification WHERE user_id = $1 ORDER BY created_at
            "#,
        )
        .bind(user_id.0)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| db_err("export.assemble_payload", e))?
        .into_iter()
        .map(|r| ExportVerification {
            location_id: r.location_id,
            kind: r.kind,
            result: r.result,
            attribute_code: r.attribute_code,
            created_at: r.created_at,
        })
        .collect();

        let proposals = sqlx::query_as::<_, ProposalRow>(
            r#"
            SELECT location_id, base_version, kind, proposed, status, created_at
            FROM parking_proposal WHERE proposer_id = $1 ORDER BY created_at
            "#,
        )
        .bind(user_id.0)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| db_err("export.assemble_payload", e))?
        .into_iter()
        .map(|r| ExportProposal {
            location_id: r.location_id,
            base_version: r.base_version,
            kind: r.kind,
            proposed: r.proposed,
            status: r.status,
            created_at: r.created_at,
        })
        .collect();

        let proposal_votes = sqlx::query_as::<_, ProposalVoteRow>(
            "SELECT proposal_id, vote, created_at, updated_at FROM parking_proposal_vote WHERE voter_id = $1 ORDER BY created_at",
        )
        .bind(user_id.0)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| db_err("export.assemble_payload", e))?
        .into_iter()
        .map(|r| ExportProposalVote { proposal_id: r.proposal_id, vote: r.vote, created_at: r.created_at, updated_at: r.updated_at })
        .collect();

        let reports = sqlx::query_as::<_, ReportRow>(
            r#"
            SELECT target_type, target_id, reason, description, state, created_at
            FROM report WHERE reporter_id = $1 ORDER BY created_at
            "#,
        )
        .bind(user_id.0)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| db_err("export.assemble_payload", e))?
        .into_iter()
        .map(|r| ExportReport {
            target_type: r.target_type,
            target_id: r.target_id,
            reason: r.reason,
            description: r.description,
            state: r.state,
            created_at: r.created_at,
        })
        .collect();

        let mut photos = Vec::new();
        for r in sqlx::query_as::<_, ParkingPhotoRow>(r#"
            SELECT location_id, storage_key, thumbnail_key, content_type, moderation_state, created_at
            FROM parking_photo WHERE uploader_id = $1 ORDER BY created_at
            "#).bind(user_id.0)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| db_err("export.assemble_payload", e))?
        {
            photos.push(ExportPhoto {
                kind: "parking".to_string(),
                location_id: Some(r.location_id),
                review_id: None,
                storage_key: r.storage_key,
                thumbnail_key: r.thumbnail_key,
                content_type: Some(r.content_type),
                moderation_state: r.moderation_state,
                created_at: r.created_at,
            });
        }
        for r in sqlx::query_as::<_, ReviewPhotoRow>(
            r#"
            SELECT review_id, storage_key, thumbnail_key, moderation_state, created_at
            FROM review_photo WHERE uploader_id = $1 ORDER BY created_at
            "#,
        )
        .bind(user_id.0)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| db_err("export.assemble_payload", e))?
        {
            photos.push(ExportPhoto {
                kind: "review".to_string(),
                location_id: None,
                review_id: Some(r.review_id),
                storage_key: r.storage_key,
                thumbnail_key: r.thumbnail_key,
                content_type: None,
                moderation_state: r.moderation_state,
                created_at: r.created_at,
            });
        }

        // Read-only: the commit just releases the snapshot.
        tx.commit()
            .await
            .map_err(|e| db_err("export.assemble_payload", e))?;

        Ok(ExportPayload::new(
            account,
            authentication,
            sessions,
            favorites,
            reviews,
            verifications,
            proposals,
            proposal_votes,
            reports,
            photos,
            now,
        ))
    }

    async fn create(&self, e: &NewExport) -> Result<i64, PrivacyError> {
        #[derive(sqlx::FromRow)]
        struct IdRow {
            id: i64,
        }
        let token_hash = sha256_hex(&e.token);
        let payload = serde_json::to_value(&e.payload).map_err(|_| PrivacyError::Internal)?;
        let row = sqlx::query_as::<_, IdRow>(
            r#"
            INSERT INTO personal_data_export (user_id, state, token_hash, payload, expires_at)
            VALUES ($1, 'READY', $2, $3, $4)
            RETURNING id
            "#,
        )
        .bind(e.user_id.0)
        .bind(token_hash)
        .bind(payload)
        .bind(e.expires_at)
        .fetch_one(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("export.acquire", e))?,
        )
        .await
        .map_err(|e| db_err("export.create", e))?;
        Ok(row.id)
    }

    async fn list_for_user(&self, user_id: UserId) -> Result<Vec<Export>, PrivacyError> {
        let rows = sqlx::query_as::<_, ExportRow>(
            r#"
            SELECT id, user_id, state, created_at, expires_at, downloaded_at
            FROM personal_data_export WHERE user_id = $1 ORDER BY created_at DESC
            "#,
        )
        .bind(user_id.0)
        .fetch_all(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("export.acquire", e))?,
        )
        .await
        .map_err(|e| db_err("export.list_for_user", e))?;
        rows.into_iter().map(ExportRow::into_export).collect()
    }

    async fn get(&self, id: i64) -> Result<Option<Export>, PrivacyError> {
        let row = sqlx::query_as::<_, ExportRow>(
            r#"
            SELECT id, user_id, state, created_at, expires_at, downloaded_at
            FROM personal_data_export WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("export.acquire", e))?,
        )
        .await
        .map_err(|e| db_err("export.get", e))?;
        match row {
            Some(r) => Ok(Some(r.into_export()?)),
            None => Ok(None),
        }
    }

    async fn consume_download(
        &self,
        id: i64,
        token: &[u8; 32],
        now: DateTime<Utc>,
    ) -> Result<ExportDownload, PrivacyError> {
        let token_hash = sha256_hex(token);
        let row = sqlx::query_as::<_, DownloadRow>(
            "SELECT payload FROM personal_data_export WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("export.acquire", e))?,
        )
        .await
        .map_err(|e| db_err("export.consume_download", e))?
        .ok_or(PrivacyError::NotFound)?;

        // Validate token + state + expiry against the authoritative row.
        #[derive(sqlx::FromRow)]
        struct CheckRow {
            token_hash: String,
            state: String,
            expires_at: DateTime<Utc>,
        }
        let check = sqlx::query_as::<_, CheckRow>(
            "SELECT token_hash, state, expires_at FROM personal_data_export WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("export.acquire", e))?,
        )
        .await
        .map_err(|e| db_err("export.consume_download", e))?
        .ok_or(PrivacyError::NotFound)?;

        if !constant_time_hex_eq(&check.token_hash, &token_hash) {
            return Err(PrivacyError::InvalidToken);
        }
        if check.state == "DOWNLOADED" {
            return Err(PrivacyError::AlreadyDownloaded);
        }
        if check.state == "EXPIRED" || now > check.expires_at {
            return Err(PrivacyError::Expired);
        }

        // Single-use transition, guarded so a concurrent win cannot double-download.
        let res = sqlx::query(
            r#"
            UPDATE personal_data_export
            SET state = 'DOWNLOADED', downloaded_at = $2
            WHERE id = $1 AND state = 'READY' AND expires_at > $2
            "#,
        )
        .bind(id)
        .bind(now)
        .execute(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("export.acquire", e))?,
        )
        .await
        .map_err(|e| db_err("export.consume_download", e))?;
        if res.rows_affected() != 1 {
            // Race: another request consumed it first.
            return Err(PrivacyError::AlreadyDownloaded);
        }
        let payload = serde_json::from_value(row.payload).map_err(|_| PrivacyError::Internal)?;
        Ok(ExportDownload { payload })
    }

    async fn purge_expired(&self, now: DateTime<Utc>) -> Result<u64, PrivacyError> {
        let res = sqlx::query(
            "DELETE FROM personal_data_export WHERE state = 'READY' AND expires_at < $1",
        )
        .bind(now)
        .execute(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("export.acquire", e))?,
        )
        .await
        .map_err(|e| db_err("export.purge_expired", e))?;
        Ok(res.rows_affected())
    }
}

impl ExportRow {
    fn into_export(self) -> Result<Export, PrivacyError> {
        let state = ExportState::from_code(&self.state).map_err(|_| PrivacyError::Internal)?;
        Ok(Export {
            id: self.id,
            user_id: UserId(self.user_id),
            state,
            created_at: self.created_at,
            expires_at: self.expires_at,
            downloaded_at: self.downloaded_at,
        })
    }
}

/// Classify + log the sqlx error (SQLSTATE, constraint), then map it onto
/// the feature error. `context` names the operation, e.g. `"export.create"`.
fn db_err(context: &'static str, e: sqlx::Error) -> PrivacyError {
    crate::db_error::classify_and_log(context, e).into()
}
