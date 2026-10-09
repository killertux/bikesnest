//! SQL-backed parking contribution repository.
//!
//! Owns listing creation, proposal submission, voting, history and duplicate
//! detection. User edits enter as `PENDING` proposals; approval publishes them.

use crate::Db;
use crate::community::voting::{eligible_voter, eligible_votes_join};
use crate::parking::SqlxParkingDetailsReader;
use async_trait::async_trait;
use bikesnest_application::{
    ContributionError, DuplicateCandidate, ListingProposal, NewParkingLocation, NewProposal,
    ParkingContributionRepository, ParkingDetailsReader, ParkingEdit, PendingFieldCue,
    PendingProposalSummary, ProposalVote, ProposalVoteTotals,
};
use bikesnest_domain::{
    ChangeKind, Cost, GeoPoint, OpeningHours, ParkingLocation, RevisionSummary, SecurityFeature,
    SecurityState, TimeRange, UserId,
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
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("contribution.create", e))?;
        let mut tx = conn
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
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("contribution.apply_edit", e))?;
        let mut tx = conn
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
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("contribution.create_proposal", e))?;
        let mut tx = conn
            .begin()
            .await
            .map_err(|e| db_err("contribution.create_proposal", e))?;
        let location: Option<(i64, String)> = sqlx::query_as(
            "SELECT version, moderation_state FROM parking_location WHERE id=$1 FOR UPDATE",
        )
        .bind(p.location_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_err("contribution.create_proposal", e))?;
        let (version, state) = location.ok_or(ContributionError::NotFound)?;
        if state != "ACTIVE" {
            return Err(ContributionError::LocationNotActive);
        }
        if version != p.base_version {
            return Err(ContributionError::VersionConflict);
        }
        if bikesnest_domain::ProposedChange::from_json(p.kind, &p.proposed)
            == bikesnest_domain::ProposedChange::Unknown
        {
            return Err(ContributionError::InvalidField("invalid proposal".into()));
        }
        // The preference setter takes this same user-row lock before it clears
        // live attribution. Reading the preference under the lock means an
        // opt-out cannot race this insert and reveal a new proposal.
        let public_author: Option<(bool,)> = sqlx::query_as(concat!(
            "SELECT u.public_contribution_name FROM users u WHERE u.id = $1 AND ",
            eligible_voter!(),
            " FOR UPDATE OF u"
        ))
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
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("contribution.vote_on_proposal", e))?;
        let mut tx = conn
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
            base_version: i64,
            kind: String,
            proposed: serde_json::Value,
            escalated_at: Option<chrono::DateTime<chrono::Utc>>,
        }
        let location: (i64,) =
            sqlx::query_as("SELECT location_id FROM parking_proposal WHERE id = $1")
                .bind(proposal_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| db_err("contribution.vote_on_proposal", e))?
                .ok_or(ContributionError::NotFound)?;
        let locked_location: Option<(i64, String)> = sqlx::query_as(
            "SELECT version, moderation_state FROM parking_location WHERE id = $1 FOR UPDATE",
        )
        .bind(location.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_err("contribution.vote_on_proposal", e))?;
        let (current_version, current_state) =
            locked_location.ok_or(ContributionError::NotFound)?;
        if current_state != "ACTIVE" {
            return Err(ContributionError::LocationNotActive);
        }
        let locked = sqlx::query_as::<_, LockedProposal>(
            "SELECT proposer_id, status, base_version, kind, proposed, escalated_at FROM parking_proposal WHERE id = $1 FOR UPDATE",
        )
        .bind(proposal_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_err("contribution.vote_on_proposal", e))?
        .ok_or(ContributionError::NotFound)?;
        if locked.base_version != current_version {
            return Err(ContributionError::VersionConflict);
        }
        if locked.status != "PENDING" {
            return Err(ContributionError::Conflict);
        }
        if locked.proposer_id == Some(voter.0) {
            return Err(ContributionError::Unauthorized);
        }
        // Do not lock the account row: deletion/anonymization owns that row
        // before cleaning vote rows. The final tally rechecks eligibility at
        // its statement snapshot instead of trusting eligibility at vote time.
        let eligible: Option<(i64,)> = sqlx::query_as(concat!(
            "SELECT u.id FROM users u WHERE u.id = $1 AND ",
            eligible_voter!()
        ))
        .bind(voter.0)
        .fetch_optional(&mut *tx)
        .await
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
        let totals = sqlx::query_as::<_, Totals>(concat!(
            "SELECT COUNT(*) FILTER (WHERE v.vote = 'APPROVE' AND u.id IS NOT NULL)::bigint AS approvals,
                    COUNT(*) FILTER (WHERE v.vote = 'REJECT' AND u.id IS NOT NULL)::bigint AS rejections
             FROM parking_proposal p",
            eligible_votes_join!(),
            "WHERE p.id = $1"
        ))
        .bind(proposal_id).fetch_one(&mut *tx).await
            .map_err(|e| db_err("contribution.vote_on_proposal", e))?;
        let mut published = false;
        if bikesnest_domain::should_publish(totals.approvals) && locked.escalated_at.is_none() {
            let kind = bikesnest_domain::ProposalKind::from_code(&locked.kind)?;
            let change = bikesnest_domain::ProposedChange::from_json(kind, &locked.proposed);
            match bikesnest_application::ProposalApplication::merge(
                kind,
                &change,
                &Default::default(),
            ) {
                Ok(applied) => {
                    crate::moderation::actions::approve_in_transaction(
                        &mut tx,
                        proposal_id,
                        voter,
                        applied,
                    )
                    .await
                    .map_err(|e| match e {
                        bikesnest_application::ModerationError::StaleProposal => {
                            ContributionError::VersionConflict
                        }
                        bikesnest_application::ModerationError::InvalidState => {
                            ContributionError::Conflict
                        }
                        _ => ContributionError::Internal,
                    })?;
                    published = true;
                }
                Err(error) => {
                    // The community approved a change that cannot be applied
                    // as stored. Retrying on every later vote would fail the
                    // same way, so hand it to a moderator, who can approve it
                    // with corrected values or reject it.
                    tracing::error!(
                        proposal_id,
                        location_id = location.0,
                        kind = %locked.kind,
                        error = ?error,
                        "community-approved proposal could not be merged; escalating to moderator review"
                    );
                    sqlx::query(
                        "UPDATE parking_proposal SET escalated_at = now() WHERE id = $1 AND escalated_at IS NULL",
                    )
                    .bind(proposal_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| db_err("contribution.escalate_proposal", e))?;
                }
            }
        }
        tx.commit()
            .await
            .map_err(|e| db_err("contribution.vote_on_proposal", e))?;
        Ok(ProposalVoteTotals {
            approvals: totals.approvals,
            rejections: totals.rejections,
            published,
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
            base_version: i64,
            proposer_id: Option<i64>,
            kind: String,
            proposed: serde_json::Value,
            status: String,
            approvals: i64,
            rejections: i64,
            created_at: chrono::DateTime<chrono::Utc>,
        }
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("contribution.listing_proposals", e))?;
        let rows = sqlx::query_as::<_, Row>(concat!(
            "SELECT p.id, p.kind, p.proposed, p.status, p.created_at, p.base_version, p.proposer_id,
                    COUNT(*) FILTER (WHERE v.vote = 'APPROVE' AND u.id IS NOT NULL)::bigint AS approvals,
                    COUNT(*) FILTER (WHERE v.vote = 'REJECT' AND u.id IS NOT NULL)::bigint AS rejections
             FROM parking_proposal p",
            eligible_votes_join!(),
            "WHERE p.location_id = $1
             GROUP BY p.id
             ORDER BY (p.status = 'PENDING') DESC, p.created_at DESC, p.id DESC
             LIMIT $2"
        ))
        .bind(location_id).bind(limit.clamp(1, 100)).fetch_all(&mut *conn).await
            .map_err(|e| db_err("contribution.listing_proposals", e))?;
        rows.into_iter()
            .map(|row| {
                Ok::<_, ContributionError>(ListingProposal {
                    id: row.id,
                    base_version: row.base_version,
                    proposer_id: row.proposer_id.map(UserId),
                    kind: bikesnest_domain::ProposalKind::from_code(&row.kind)
                        .map_err(|e| ContributionError::InvalidField(e.to_string()))?,
                    change: bikesnest_domain::ProposedChange::from_json(
                        bikesnest_domain::ProposalKind::from_code(&row.kind)
                            .map_err(|e| ContributionError::InvalidField(e.to_string()))?,
                        &row.proposed,
                    ),
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

    async fn pending_proposal_summary(
        &self,
        location_id: i64,
    ) -> Result<PendingProposalSummary, ContributionError> {
        #[derive(sqlx::FromRow)]
        struct Row {
            field: String,
            proposal_id: i64,
        }
        let total: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM parking_proposal WHERE location_id=$1 AND status='PENDING'",
        )
        .bind(location_id)
        .fetch_one(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("contribution.pending_summary_count.acquire", e))?,
        )
        .await
        .map_err(|e| db_err("contribution.pending_summary_count", e))?;
        let rows = sqlx::query_as::<_, Row>(
            r#"
            WITH current_snapshot AS (
              SELECT COALESCE(
                (SELECT snapshot FROM parking_revision WHERE location_id=$1 ORDER BY version DESC LIMIT 1),
                jsonb_build_object(
                  'name', l.name,
                  'address', l.address,
                  'description', l.description,
                  'type', l.parking_type,
                  'cost', jsonb_strip_nulls(jsonb_build_object(
                    'kind', l.cost_kind, 'cents', l.price_cents,
                    'currency', l.price_currency, 'unit', l.price_unit
                  )),
                  'point', jsonb_build_object('lat', l.lat, 'lon', l.lon),
                  'timezone', l.timezone,
                  'hours', jsonb_build_object(
                    'unknown', l.hours_unknown,
                    'rows', COALESCE((
                      SELECT jsonb_agg(jsonb_build_array(
                        h.day_of_week, h.opens_at::text, h.closes_at::text, h.all_day
                      ) ORDER BY h.day_of_week, h.opens_at)
                      FROM opening_hours h WHERE h.location_id=l.id
                    ), '[]'::jsonb)
                  ),
                  'security', COALESCE((
                    SELECT jsonb_agg(jsonb_build_array(s.feature_code, s.state)
                                     ORDER BY s.feature_code)
                    FROM parking_security s WHERE s.location_id=l.id
                  ), '[]'::jsonb),
                  'moderation_state', l.moderation_state
                )
              ) AS snapshot
              FROM parking_location l WHERE l.id=$1
            ), pending AS (
              SELECT id,kind,proposed FROM parking_proposal WHERE location_id=$1 AND status='PENDING'
            ), cues AS (
              SELECT p.id, k.field FROM pending p CROSS JOIN current_snapshot c
              CROSS JOIN LATERAL (VALUES ('name'),('address'),('description'),('type'),('cost'),('hours')) k(field)
              WHERE p.kind='edit_details' AND p.proposed->k.field IS DISTINCT FROM c.snapshot->k.field
              UNION ALL SELECT p.id,'point' FROM pending p WHERE p.kind='move_location'
              UNION ALL SELECT p.id,'timezone' FROM pending p WHERE p.kind='move_location' AND p.proposed ? 'timezone'
              UNION ALL SELECT p.id,'existence' FROM pending p WHERE p.kind='change_existence'
              UNION ALL
              SELECT p.id, proposed_item->>0 FROM pending p CROSS JOIN current_snapshot c
              CROSS JOIN LATERAL jsonb_array_elements(p.proposed->'security') proposed_item
              WHERE p.kind='edit_details' AND proposed_item IS DISTINCT FROM (
                SELECT current_item FROM jsonb_array_elements(c.snapshot->'security') current_item
                WHERE current_item->>0=proposed_item->>0 LIMIT 1)
            )
            SELECT field, max(id) AS proposal_id FROM cues GROUP BY field ORDER BY field
        "#,
        )
        .bind(location_id)
        .fetch_all(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("contribution.pending_summary.acquire", e))?,
        )
        .await
        .map_err(|e| db_err("contribution.pending_summary", e))?;
        Ok(PendingProposalSummary {
            total,
            fields: rows
                .into_iter()
                .map(|r| PendingFieldCue {
                    field: r.field,
                    proposal_id: r.proposal_id,
                })
                .collect(),
        })
    }

    async fn listing_proposals_page(
        &self,
        location_id: i64,
        after_id: Option<i64>,
        limit: i64,
    ) -> Result<(Vec<ListingProposal>, i64, bool), ContributionError> {
        let limit = limit.clamp(1, 50);
        let total: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM parking_proposal WHERE location_id=$1 AND status='PENDING'",
        )
        .bind(location_id)
        .fetch_one(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("contribution.proposal_count.acquire", e))?,
        )
        .await
        .map_err(|e| db_err("contribution.proposal_count", e))?;
        #[derive(sqlx::FromRow)]
        struct Row {
            id: i64,
            base_version: i64,
            proposer_id: Option<i64>,
            kind: String,
            proposed: serde_json::Value,
            status: String,
            approvals: i64,
            rejections: i64,
            created_at: chrono::DateTime<chrono::Utc>,
        }
        let rows = sqlx::query_as::<_, Row>(concat!(
            "SELECT p.id,p.base_version,p.proposer_id,p.kind,p.proposed,p.status,p.created_at,
               COUNT(*) FILTER (WHERE v.vote='APPROVE' AND u.id IS NOT NULL)::bigint approvals,
               COUNT(*) FILTER (WHERE v.vote='REJECT' AND u.id IS NOT NULL)::bigint rejections
             FROM parking_proposal p",
            eligible_votes_join!(),
            "WHERE p.location_id=$1 AND p.status='PENDING' AND ($2::bigint IS NULL OR p.id<$2)
             GROUP BY p.id ORDER BY p.id DESC LIMIT $3"
        ))
        .bind(location_id)
        .bind(after_id)
        .bind(limit + 1)
        .fetch_all(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("contribution.proposal_page.acquire", e))?,
        )
        .await
        .map_err(|e| db_err("contribution.proposal_page", e))?;
        let mut page = rows
            .into_iter()
            .map(|row| {
                let kind = bikesnest_domain::ProposalKind::from_code(&row.kind)
                    .map_err(|e| ContributionError::InvalidField(e.to_string()))?;
                Ok::<_, ContributionError>(ListingProposal {
                    id: row.id,
                    base_version: row.base_version,
                    proposer_id: row.proposer_id.map(UserId),
                    kind,
                    change: bikesnest_domain::ProposedChange::from_json(kind, &row.proposed),
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
            .collect::<Result<Vec<_>, _>>()?;
        let has_more = page.len() as i64 > limit;
        page.truncate(limit as usize);
        Ok((page, total, has_more))
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
            snapshot: serde_json::Value,
            summary: Option<String>,
            created_at: chrono::DateTime<chrono::Utc>,
        }
        let rows = sqlx::query_as::<_, RevRow>(
            r#"
            SELECT version, change_kind, summary, created_at, snapshot
            FROM parking_revision
            WHERE location_id = $1
            ORDER BY version DESC
            LIMIT $2
            "#,
        )
        .bind(id)
        .bind(limit)
        .fetch_all(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("contribution.acquire", e))?,
        )
        .await
        .map_err(|e| db_err("contribution.revision_history", e))?;

        rows.into_iter()
            .map(|r| {
                Ok::<_, ContributionError>(RevisionSummary {
                    version: r.version,
                    change_kind: ChangeKind::from_code(&r.change_kind)
                        .map_err(|e| ContributionError::InvalidField(e.to_string()))?,
                    summary: r.summary,
                    at: r.created_at,
                    snapshot: r.snapshot,
                })
            })
            .collect()
    }

    async fn revision_history_page(
        &self,
        id: i64,
        after_version: Option<i64>,
        limit: i64,
    ) -> Result<(Vec<RevisionSummary>, i64, bool), ContributionError> {
        let total: i64 =
            sqlx::query_scalar("SELECT count(*) FROM parking_revision WHERE location_id=$1")
                .bind(id)
                .fetch_one(
                    &mut *self
                        .db
                        .acquire()
                        .await
                        .map_err(|e| db_err("contribution.revision_count.acquire", e))?,
                )
                .await
                .map_err(|e| db_err("contribution.revision_count", e))?;
        #[derive(sqlx::FromRow)]
        struct RevRow {
            version: i64,
            change_kind: String,
            snapshot: serde_json::Value,
            summary: Option<String>,
            created_at: chrono::DateTime<chrono::Utc>,
        }
        let limit = limit.clamp(1, 50);
        let rows=sqlx::query_as::<_,RevRow>("SELECT version,change_kind,snapshot,summary,created_at FROM parking_revision WHERE location_id=$1 AND ($2::bigint IS NULL OR version<$2) ORDER BY version DESC LIMIT $3")
          .bind(id).bind(after_version).bind(limit + 1).fetch_all(&mut *self.db.acquire().await.map_err(|e|db_err("contribution.revision_page.acquire",e))?).await.map_err(|e|db_err("contribution.revision_page",e))?;
        let mut page = rows
            .into_iter()
            .map(|r| {
                Ok::<_, ContributionError>(RevisionSummary {
                    version: r.version,
                    change_kind: ChangeKind::from_code(&r.change_kind)
                        .map_err(|e| ContributionError::InvalidField(e.to_string()))?,
                    summary: r.summary,
                    at: r.created_at,
                    snapshot: r.snapshot,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let has_more = page.len() as i64 > limit;
        page.truncate(limit as usize);
        Ok((page, total, has_more))
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
        .fetch_all(&mut *self.db.acquire().await.map_err(|e| db_err("contribution.acquire", e))?)
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

pub(crate) fn cost_parts(
    cost: &Cost,
) -> (&'static str, Option<i64>, Option<String>, Option<String>) {
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
pub(crate) async fn write_hours(
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
            // All-day rows are stored with the canonical bounds, whatever
            // times the caller happened to carry.
            let stored = if range.all_day {
                TimeRange::all_day()
            } else {
                *range
            };
            opens.push(stored.opens_at);
            closes.push(stored.closes_at);
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
pub(crate) async fn write_security(
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
