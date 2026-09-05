//! SQL-backed parking contribution repository.
//!
//! Owns the create / optimistic-edit / proposal / history / duplicate-detection
//! writes. Sensitive changes (move / removal) become `PENDING` proposals; only
//! reversible fields are applied directly (//).

use crate::Db;
use crate::parking::SqlxParkingDetailsReader;
use async_trait::async_trait;
use bikesnest_application::{
    ContributionError, DuplicateCandidate, ListingProposal, NewParkingLocation, NewProposal,
    ParkingContributionRepository, ParkingDetailsReader, ParkingEdit, ProposalVote,
    ProposalVoteTotals,
};
use bikesnest_domain::{
    ChangeKind, Cost, GeoPoint, OpeningHours, ParkingLocation, RevisionSummary, SecurityFeature,
    SecurityState, UserId,
};

/// Advisory duplicate radius in metres.
const DUPLICATE_RADIUS_M: u32 = 500;
/// Similarity threshold above which a candidate is flagged.
const DUPLICATE_SIMILARITY: f64 = 0.55;

/// Returned `id` for the INSERT...RETURNING writes (compile-time checked).
#[derive(sqlx::FromRow)]
struct IdRow {
    id: i64,
}

/// The AFTER-state tuple returned by the optimistic `apply_edit` UPDATE.
/// It carries the columns the edit does NOT write (point, timezone, moderation
/// state) as well, so the revision snapshot is the row's true after-state
/// rather than a pre-transaction read that a concurrent write may have aged.
#[derive(sqlx::FromRow)]
struct EditApplyRow {
    version: i64,
    name: String,
    address: String,
    description: Option<String>,
    parking_type: String,
    lat: Option<f64>,
    lon: Option<f64>,
    timezone: String,
    moderation_state: String,
}

pub struct SqlxParkingContributionRepository {
    db: Db,
    details: SqlxParkingDetailsReader,
}

impl SqlxParkingContributionRepository {
    pub fn new(db: Db) -> Self {
        Self {
            details: SqlxParkingDetailsReader::new(db.clone()),
            db,
        }
    }
}

#[async_trait]
impl ParkingContributionRepository for SqlxParkingContributionRepository {
    async fn get_for_edit(&self, id: i64) -> Result<Option<ParkingLocation>, ContributionError> {
        self.details
            .details(id)
            .await
            .map_err(map_reader_err_to_contribution)
    }

    async fn create(
        &self,
        new: &NewParkingLocation,
        creator: UserId,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<i64, ContributionError> {
        let tz = new
            .timezone
            .ok_or_else(|| ContributionError::InvalidField("timezone is required".to_string()))?;
        let mut tx = self
            .db
            .pool()
            .begin()
            .await
            .map_err(|e| db_err("contribution.create", e))?;

        let (cost_kind, price_cents, price_currency, price_unit) = cost_parts(&new.cost);

        let row = sqlx::query_as::<_, IdRow>(
            r#"
            INSERT INTO parking_location
                (name, address, description, parking_type, cost_kind,
                 price_cents, price_currency, price_unit,
                 location, timezone, hours_unknown, moderation_state,
                 creator_id, version, last_meaningful_update_at, created_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8,
                    ST_SetSRID(ST_MakePoint($10, $9), 4326)::geography,
                    $11, $12, 'ACTIVE',
                    $13, 1, $14, $14, $14)
            RETURNING id
            "#,
        )
        .bind(new.name.trim())
        .bind(new.address.trim())
        .bind(new.description.as_deref())
        .bind(new.parking_type.as_code())
        .bind(cost_kind)
        .bind(price_cents)
        .bind(price_currency)
        .bind(price_unit)
        .bind(new.point.lat())
        .bind(new.point.lon())
        .bind(tz.name())
        .bind(new.hours.is_unknown())
        .bind(creator.0)
        .bind(now)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| db_err("contribution.create", e))?;
        let id = row.id;

        write_hours(&mut tx, id, &new.hours).await?;
        write_security(&mut tx, id, &new.security).await?;

        let snapshot = snapshot_of(
            &new.name,
            &new.address,
            new.description.as_deref(),
            new.parking_type.as_code(),
            &new.cost,
            &new.point,
            tz,
            &new.hours,
            &new.security,
            "ACTIVE",
        );
        insert_revision(
            &mut tx,
            id,
            1,
            creator,
            ChangeKind::Create,
            "added",
            snapshot,
        )
        .await?;

        tx.commit()
            .await
            .map_err(|e| db_err("contribution.create", e))?;
        Ok(id)
    }

    async fn apply_edit(
        &self,
        id: i64,
        expected_version: i64,
        edit: &ParkingEdit,
        editor: UserId,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<i64, ContributionError> {
        let mut tx = self
            .db
            .pool()
            .begin()
            .await
            .map_err(|e| db_err("contribution.apply_edit", e))?;

        // Lock the row and read the true before-state inside the transaction.
        // The UPDATE below carries both predicates, so a zero-row result cannot
        // say *which* one failed; this read separates "someone else edited it"
        // (VersionConflict) from "moderation took it down" (LocationNotActive)
        // without a race, because the lock is held until commit.
        let guard: Option<(i64, String)> = sqlx::query_as(
            "SELECT version, moderation_state FROM parking_location WHERE id = $1 FOR UPDATE",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_err("contribution.apply_edit", e))?;
        let Some((current_version, current_state)) = guard else {
            return Err(ContributionError::NotFound);
        };
        if current_state != bikesnest_domain::ModerationState::Active.as_code() {
            return Err(ContributionError::LocationNotActive);
        }
        if current_version != expected_version {
            return Err(ContributionError::VersionConflict);
        }

        let (cost_kind, price_cents, price_currency, price_unit) = cost_parts(&edit.cost);

        // Optimistic concurrency: only bump when `version` still matches.
        let row = sqlx::query_as::<_, EditApplyRow>(
            r#"
            UPDATE parking_location
            SET name = $1, address = $2, description = $3, parking_type = $4,
                cost_kind = $5, price_cents = $6, price_currency = $7, price_unit = $8,
                hours_unknown = $9,
                version = version + 1, updated_at = $10, last_meaningful_update_at = $10
            WHERE id = $11 AND version = $12 AND moderation_state = 'ACTIVE'
            RETURNING version, name, address, description, parking_type,
                      lat, lon, timezone, moderation_state
            "#,
        )
        .bind(edit.name.trim())
        .bind(edit.address.trim())
        .bind(edit.description.as_deref())
        .bind(edit.parking_type.as_code())
        .bind(cost_kind)
        .bind(price_cents)
        .bind(price_currency)
        .bind(price_unit)
        .bind(edit.hours.is_unknown())
        .bind(now)
        .bind(id)
        .bind(expected_version)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_err("contribution.apply_edit", e))?;

        let Some(row) = row else {
            // Unreachable while the FOR UPDATE lock above is held; kept as the
            // conservative fallback if that guard is ever removed.
            return Err(ContributionError::VersionConflict);
        };
        let EditApplyRow {
            version: new_version,
            name,
            address,
            description,
            parking_type,
            lat,
            lon,
            timezone,
            moderation_state,
        } = row;
        let point = GeoPoint::new(lat.unwrap_or(0.0), lon.unwrap_or(0.0))
            .map_err(|e| ContributionError::InvalidField(e.to_string()))?;
        let tz: chrono_tz::Tz = timezone.parse().map_err(|_| {
            ContributionError::InvalidField(format!("unknown timezone: {timezone}"))
        })?;

        write_hours(&mut tx, id, &edit.hours).await?;
        write_security(&mut tx, id, &edit.security).await?;

        let snapshot = snapshot_of(
            &name,
            &address,
            description.as_deref(),
            &parking_type,
            &edit.cost,
            &point,
            tz,
            &edit.hours,
            &edit.security,
            &moderation_state,
        );
        insert_revision(
            &mut tx,
            id,
            new_version,
            editor,
            ChangeKind::Edit,
            "edited",
            snapshot,
        )
        .await?;

        tx.commit()
            .await
            .map_err(|e| db_err("contribution.apply_edit", e))?;
        Ok(new_version)
    }

    async fn create_proposal(&self, p: &NewProposal) -> Result<i64, ContributionError> {
        let mut tx = self
            .db
            .pool()
            .begin()
            .await
            .map_err(|e| db_err("contribution.create_proposal", e))?;
        // The preference setter takes this same user-row lock before it clears
        // live attribution. Reading the preference under the lock means an
        // opt-out cannot race this insert and reveal a new proposal.
        let public_author: Option<(bool,)> = sqlx::query_as(
            "SELECT public_contribution_name FROM users WHERE id = $1 AND account_state = 'ACTIVE' AND email_verified_at IS NOT NULL FOR UPDATE",
        )
        .bind(p.proposer_id.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_err("contribution.create_proposal", e))?;
        let Some((public_author,)) = public_author else {
            return Err(ContributionError::NotVerified);
        };
        let row = sqlx::query_as::<_, IdRow>(
            r#"
            INSERT INTO parking_proposal
                (location_id, proposer_id, base_version, kind, proposed, status, public_author)
            VALUES ($1, $2, $3, $4, $5, 'PENDING', $6)
            RETURNING id
            "#,
        )
        .bind(p.location_id)
        .bind(p.proposer_id.0)
        .bind(p.base_version)
        .bind(p.kind.as_code())
        .bind(&p.proposed)
        .bind(public_author)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| db_err("contribution.create_proposal", e))?;
        tx.commit()
            .await
            .map_err(|e| db_err("contribution.create_proposal", e))?;
        Ok(row.id)
    }

    async fn vote_on_proposal(
        &self,
        proposal_id: i64,
        voter: UserId,
        vote: ProposalVote,
    ) -> Result<ProposalVoteTotals, ContributionError> {
        let mut tx = self
            .db
            .pool()
            .begin()
            .await
            .map_err(|e| db_err("contribution.vote_on_proposal", e))?;
        // Lock the location before the proposal, matching moderation's lock
        // order. Separate statements make that order explicit; `FOR UPDATE`
        // on a join otherwise leaves lock acquisition order to the planner.
        #[derive(sqlx::FromRow)]
        struct LockedProposal {
            proposer_id: Option<i64>,
            status: String,
        }
        let location: (i64,) =
            sqlx::query_as("SELECT location_id FROM parking_proposal WHERE id = $1")
                .bind(proposal_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| db_err("contribution.vote_on_proposal", e))?
                .ok_or(ContributionError::NotFound)?;
        let locked_location: Option<(i64,)> =
            sqlx::query_as("SELECT id FROM parking_location WHERE id = $1 FOR UPDATE")
                .bind(location.0)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| db_err("contribution.vote_on_proposal", e))?;
        if locked_location.is_none() {
            return Err(ContributionError::NotFound);
        }
        let locked = sqlx::query_as::<_, LockedProposal>(
            "SELECT proposer_id, status FROM parking_proposal WHERE id = $1 FOR UPDATE",
        )
        .bind(proposal_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_err("contribution.vote_on_proposal", e))?
        .ok_or(ContributionError::NotFound)?;
        if locked.status != "PENDING" {
            return Err(ContributionError::Conflict);
        }
        if locked.proposer_id == Some(voter.0) {
            return Err(ContributionError::Unauthorized);
        }
        // Do not lock the account row: deletion/anonymization owns that row
        // before cleaning vote rows. The final tally filters account state, so
        // a concurrent suspension/deletion cannot count toward the threshold.
        let eligible: Option<(i64,)> = sqlx::query_as(
            "SELECT id FROM users WHERE id = $1 AND account_state = 'ACTIVE' AND email_verified_at IS NOT NULL"
        ).bind(voter.0).fetch_optional(&mut *tx).await
            .map_err(|e| db_err("contribution.vote_on_proposal", e))?;
        if eligible.is_none() {
            return Err(ContributionError::NotVerified);
        }
        sqlx::query(
            r#"
            INSERT INTO parking_proposal_vote (proposal_id, voter_id, vote)
            VALUES ($1, $2, $3)
            ON CONFLICT (proposal_id, voter_id)
            DO UPDATE SET vote = EXCLUDED.vote, updated_at = now()
        "#,
        )
        .bind(proposal_id)
        .bind(voter.0)
        .bind(vote.as_code())
        .execute(&mut *tx)
        .await
        .map_err(|e| db_err("contribution.vote_on_proposal", e))?;
        #[derive(sqlx::FromRow)]
        struct Totals {
            approvals: i64,
            rejections: i64,
        }
        let totals = sqlx::query_as::<_, Totals>(r#"
            SELECT COUNT(*) FILTER (WHERE v.vote = 'APPROVE')::bigint AS approvals,
                   COUNT(*) FILTER (WHERE v.vote = 'REJECT')::bigint AS rejections
            FROM parking_proposal_vote v JOIN users u ON u.id = v.voter_id
            WHERE v.proposal_id = $1 AND u.account_state = 'ACTIVE' AND u.email_verified_at IS NOT NULL
        "#).bind(proposal_id).fetch_one(&mut *tx).await
            .map_err(|e| db_err("contribution.vote_on_proposal", e))?;
        tx.commit()
            .await
            .map_err(|e| db_err("contribution.vote_on_proposal", e))?;
        Ok(ProposalVoteTotals {
            approvals: totals.approvals,
            rejections: totals.rejections,
        })
    }

    async fn listing_proposals(
        &self,
        location_id: i64,
        limit: i64,
    ) -> Result<Vec<ListingProposal>, ContributionError> {
        #[derive(sqlx::FromRow)]
        struct Row {
            id: i64,
            kind: String,
            proposed: serde_json::Value,
            status: String,
            approvals: i64,
            rejections: i64,
            created_at: chrono::DateTime<chrono::Utc>,
        }
        let rows = sqlx::query_as::<_, Row>(r#"
            SELECT p.id, p.kind, p.proposed, p.status, p.created_at,
                   COUNT(*) FILTER (WHERE v.vote = 'APPROVE' AND u.id IS NOT NULL)::bigint AS approvals,
                   COUNT(*) FILTER (WHERE v.vote = 'REJECT' AND u.id IS NOT NULL)::bigint AS rejections
            FROM parking_proposal p
            LEFT JOIN parking_proposal_vote v ON v.proposal_id = p.id
            LEFT JOIN users u ON u.id = v.voter_id
                 AND u.account_state = 'ACTIVE' AND u.email_verified_at IS NOT NULL
            WHERE p.location_id = $1
            GROUP BY p.id
            ORDER BY p.created_at DESC, p.id DESC
            LIMIT $2
        "#).bind(location_id).bind(limit.clamp(1, 100)).fetch_all(self.db.pool()).await
            .map_err(|e| db_err("contribution.listing_proposals", e))?;
        rows.into_iter()
            .map(|row| {
                Ok(ListingProposal {
                    id: row.id,
                    kind: bikesnest_domain::ProposalKind::from_code(&row.kind)
                        .map_err(|e| ContributionError::InvalidField(e.to_string()))?,
                    reason: row
                        .proposed
                        .get("reason")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    status: bikesnest_domain::ProposalStatus::from_code(&row.status)
                        .map_err(|e| ContributionError::InvalidField(e.to_string()))?,
                    approvals: row.approvals,
                    rejections: row.rejections,
                    created_at: row.created_at,
                })
            })
            .collect()
    }

    async fn revision_history(
        &self,
        id: i64,
        limit: i64,
    ) -> Result<Vec<RevisionSummary>, ContributionError> {
        let limit = limit.clamp(1, 200);
        #[derive(sqlx::FromRow)]
        struct RevRow {
            version: i64,
            change_kind: String,
            summary: Option<String>,
            created_at: chrono::DateTime<chrono::Utc>,
        }
        let rows = sqlx::query_as::<_, RevRow>(
            r#"
            SELECT version, change_kind, summary, created_at
            FROM parking_revision
            WHERE location_id = $1
            ORDER BY version DESC
            LIMIT $2
            "#,
        )
        .bind(id)
        .bind(limit)
        .fetch_all(self.db.pool())
        .await
        .map_err(|e| db_err("contribution.revision_history", e))?;

        rows.into_iter()
            .map(|r| {
                Ok(RevisionSummary {
                    version: r.version,
                    change_kind: ChangeKind::from_code(&r.change_kind)
                        .map_err(|e| ContributionError::InvalidField(e.to_string()))?,
                    summary: r.summary,
                    at: r.created_at,
                })
            })
            .collect()
    }

    /// Near neighbours to compare a proposed location against.
    ///
    /// The `LIMIT 50` is a cap on how much name/address similarity work
    /// happens in Rust, so the fifty rows have to be the fifty *nearest*: with
    /// no `ORDER BY` they were fifty arbitrary rows, and the duplicate a
    /// contributor was about to create could sit outside them. Ordering by
    /// `<->` also lets the GIST index supply the rows already sorted.
    async fn duplicate_candidates(
        &self,
        point: GeoPoint,
        name: &str,
    ) -> Result<Vec<DuplicateCandidate>, ContributionError> {
        #[derive(sqlx::FromRow)]
        struct CandidateRow {
            id: i64,
            name: String,
            address: String,
            distance_m: Option<f64>,
        }
        let rows = sqlx::query_as::<_, CandidateRow>(r#"
            SELECT id, name, address, ST_Distance(location, ST_SetSRID(ST_MakePoint($2, $1), 4326)::geography) AS distance_m
            FROM parking_location
            WHERE moderation_state = 'ACTIVE'
              AND ST_DWithin(location, ST_SetSRID(ST_MakePoint($2, $1), 4326)::geography, $3)
            ORDER BY location <-> ST_SetSRID(ST_MakePoint($2, $1), 4326)::geography
            LIMIT 50
            "#).bind(point.lat()).bind(point.lon()).bind(f64::from(DUPLICATE_RADIUS_M))
        .fetch_all(self.db.pool())
        .await
        .map_err(|e| db_err("contribution.duplicate_candidates", e))?;

        let mut candidates: Vec<DuplicateCandidate> = rows
            .into_iter()
            .map(|r| {
                let similarity = max_similarity(name, &r.name, &r.address);
                DuplicateCandidate {
                    id: r.id,
                    name: r.name,
                    address: r.address,
                    distance_m: r.distance_m.unwrap_or(0.0),
                    similarity,
                }
            })
            .filter(|c| c.similarity >= DUPLICATE_SIMILARITY)
            .collect();
        candidates.sort_by(|a, b| {
            b.similarity
                .partial_cmp(&a.similarity)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(candidates)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Classify + log the sqlx error (SQLSTATE, constraint), then map it onto
/// the feature error. `context` names the operation, e.g. `"contribution.create"`.
fn db_err(context: &'static str, e: sqlx::Error) -> ContributionError {
    crate::db_error::classify_and_log(context, e).into()
}

fn map_reader_err_to_contribution(e: bikesnest_application::ReaderError) -> ContributionError {
    match e {
        bikesnest_application::ReaderError::Unavailable => ContributionError::Unavailable,
        bikesnest_application::ReaderError::Unexpected(_) => ContributionError::Internal,
    }
}

fn cost_parts(cost: &Cost) -> (&'static str, Option<i64>, Option<String>, Option<String>) {
    match cost {
        Cost::Free => ("free", None, None, None),
        Cost::Unknown => ("unknown", None, None, None),
        Cost::Paid { price: None } => ("paid", None, None, None),
        Cost::Paid { price: Some(p) } => (
            "paid",
            Some(p.cents()),
            Some(p.currency().as_str().to_string()),
            Some(p.unit().as_code().to_string()),
        ),
    }
}

/// Replaces a location's opening hours in two statements (was: one DELETE +
/// one INSERT per row). `opening_hours` has no natural per-range unique key —
/// an edit can change a range's times entirely, which is a different primary
/// key tuple — so a delete-then-insert stays the right shape; the insert side
/// is now one multi-row statement via `unnest` instead of N round trips.
async fn write_hours(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: i64,
    hours: &OpeningHours,
) -> Result<(), ContributionError> {
    sqlx::query("DELETE FROM opening_hours WHERE location_id = $1")
        .bind(id)
        .execute(&mut **tx)
        .await
        .map_err(|e| db_err("contribution.write_hours", e))?;
    if let OpeningHours::Weekly(rows) = hours
        && !rows.is_empty()
    {
        let mut days: Vec<i16> = Vec::with_capacity(rows.len());
        let mut opens: Vec<chrono::NaiveTime> = Vec::with_capacity(rows.len());
        let mut closes: Vec<chrono::NaiveTime> = Vec::with_capacity(rows.len());
        let mut all_day: Vec<bool> = Vec::with_capacity(rows.len());
        for (day, range) in rows {
            days.push(i16::from(*day));
            opens.push(if range.all_day {
                chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap()
            } else {
                range.opens_at
            });
            closes.push(if range.all_day {
                chrono::NaiveTime::from_hms_opt(23, 59, 59).unwrap()
            } else {
                range.closes_at
            });
            all_day.push(range.all_day);
        }
        sqlx::query(
            r#"
            INSERT INTO opening_hours (location_id, day_of_week, opens_at, closes_at, all_day)
            SELECT $1, d, o, c, a
            FROM UNNEST($2::smallint[], $3::time[], $4::time[], $5::bool[]) AS t(d, o, c, a)
            "#,
        )
        .bind(id)
        .bind(days)
        .bind(opens)
        .bind(closes)
        .bind(all_day)
        .execute(&mut **tx)
        .await
        .map_err(|e| db_err("contribution.write_hours", e))?;
    }
    Ok(())
}

/// Upserts a location's security attributes in one statement (was: one
/// DELETE plus N single-row INSERTs). Every call writes all codes in
/// [`bikesnest_domain::SECURITY_FEATURE_CODES`] (defaulting to `Unknown` for
/// codes the caller didn't set) — the row set per location never shrinks —
/// so `ON CONFLICT … DO UPDATE` is equivalent to delete-then-insert with no
/// separate DELETE needed.
async fn write_security(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: i64,
    security: &[SecurityFeature],
) -> Result<(), ContributionError> {
    let codes: Vec<&'static str> = bikesnest_domain::SECURITY_FEATURE_CODES.to_vec();
    let states: Vec<i16> = codes
        .iter()
        .map(|code| {
            let state = security
                .iter()
                .find(|f| f.code() == *code)
                .map(|f| f.state())
                .unwrap_or(SecurityState::Unknown);
            state_smallint(state)
        })
        .collect();
    sqlx::query(
        r#"
        INSERT INTO parking_security (location_id, feature_code, state)
        SELECT $1, c, s
        FROM UNNEST($2::text[], $3::smallint[]) AS t(c, s)
        ON CONFLICT (location_id, feature_code) DO UPDATE SET state = EXCLUDED.state
        "#,
    )
    .bind(id)
    .bind(codes)
    .bind(states)
    .execute(&mut **tx)
    .await
    .map_err(|e| db_err("contribution.write_security", e))?;
    Ok(())
}

fn state_smallint(state: SecurityState) -> i16 {
    match state {
        SecurityState::Unknown => 0,
        SecurityState::Yes => 1,
        SecurityState::No => 2,
    }
}

async fn insert_revision(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    location_id: i64,
    version: i64,
    editor: UserId,
    kind: ChangeKind,
    summary: &str,
    snapshot: serde_json::Value,
) -> Result<(), ContributionError> {
    sqlx::query(
        r#"
        INSERT INTO parking_revision
            (location_id, version, editor_id, change_kind, summary, snapshot)
        VALUES ($1, $2, $3, $4, $5, $6)
        "#,
    )
    .bind(location_id)
    .bind(version)
    .bind(editor.0)
    .bind(kind.as_code())
    .bind(summary)
    .bind(snapshot)
    .execute(&mut **tx)
    .await
    .map_err(|e| db_err("contribution.insert_revision", e))?;
    Ok(())
}

/// Snapshot of the tracked fields AFTER a change. Used for history and
/// stateless reconstruction at any version.
#[allow(clippy::too_many_arguments)]
fn snapshot_of(
    name: &str,
    address: &str,
    description: Option<&str>,
    parking_type: &str,
    cost: &Cost,
    point: &GeoPoint,
    tz: chrono_tz::Tz,
    hours: &OpeningHours,
    security: &[SecurityFeature],
    moderation_state: &str,
) -> serde_json::Value {
    let cost_json = match cost {
        Cost::Free => serde_json::json!({ "kind": "free" }),
        Cost::Unknown => serde_json::json!({ "kind": "unknown" }),
        Cost::Paid { price: None } => serde_json::json!({ "kind": "paid" }),
        Cost::Paid { price: Some(p) } => serde_json::json!({
            "kind": "paid",
            "cents": p.cents(),
            "currency": p.currency().as_str(),
            "unit": p.unit().as_code(),
        }),
    };
    let hours_json = match hours {
        OpeningHours::Unknown => serde_json::json!({ "unknown": true, "rows": [] }),
        OpeningHours::Weekly(rows) => serde_json::json!({
            "unknown": false,
            "rows": rows.iter().map(|(day, r)| serde_json::json!([
                day,
                r.opens_at.to_string(),
                r.closes_at.to_string(),
                r.all_day,
            ])).collect::<Vec<_>>(),
        }),
    };
    let security_json: Vec<serde_json::Value> = security
        .iter()
        .map(|f| serde_json::json!([f.code(), state_smallint(f.state())]))
        .collect();
    serde_json::json!({
        "name": name,
        "address": address,
        "description": description,
        "type": parking_type,
        "cost": cost_json,
        "point": { "lat": point.lat(), "lon": point.lon() },
        "timezone": tz.name(),
        "hours": hours_json,
        "security": security_json,
        "moderation_state": moderation_state,
    })
}

// ---------------------------------------------------------------------------
// Duplicate name-similarity: case/diacritic-folded trigram Jaccard,
// blended with address token overlap. Pure, deterministic, no external crate.
// ---------------------------------------------------------------------------

fn max_similarity(submitted: &str, existing_name: &str, existing_address: &str) -> f64 {
    let name_sim = trigram_similarity(submitted, existing_name);
    let addr_sim = token_overlap(submitted, existing_address);
    name_sim.max(addr_sim)
}

/// Unicode normalization to NFC lowercase without a unicode crate.
fn fold(s: &str) -> String {
    s.chars().flat_map(|c| c.to_lowercase()).collect::<String>()
}

/// Character trigram Jaccard similarity (case/diacritic folded).
fn trigram_similarity(a: &str, b: &str) -> f64 {
    let a = fold(a);
    let b = fold(b);
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let grams = |s: &str| -> std::collections::HashSet<String> {
        let chars: Vec<char> = s.chars().collect();
        if chars.len() < 3 {
            return [s.to_string()].into_iter().collect();
        }
        chars
            .windows(3)
            .map(|w| w.iter().collect::<String>())
            .collect()
    };
    let ga = grams(&a);
    let gb = grams(&b);
    let inter = ga.intersection(&gb).count();
    let union = ga.union(&gb).count();
    if union == 0 {
        0.0
    } else {
        inter as f64 / union as f64
    }
}

/// Fraction of the submitted name's tokens appearing in the existing address.
fn token_overlap(name: &str, address: &str) -> f64 {
    let folded = fold(name);
    let tokens: Vec<&str> = folded.split_whitespace().collect();
    if tokens.is_empty() {
        return 0.0;
    }
    let address = fold(address);
    let hits = tokens.iter().filter(|t| address.contains(**t)).count();
    hits as f64 / tokens.len() as f64
}
