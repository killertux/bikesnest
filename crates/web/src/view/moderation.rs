//! Moderation view models: the photo, report and proposal queues.

use super::format::*;
use crate::i18n::Translator;
use bikesnest_application::{ObjectStorage, PendingPhoto};
use bikesnest_domain::ReportTargetType;

/// One photo in the moderator queue. Includes the presigned URL of
/// the *processed derivative* (exactly what would publish), a small preview and
/// an anonymized "Contributor #id" label — never an email/OAuth subject.
#[derive(Debug, Clone)]
pub struct ModerationPhotoVm {
    pub id: i64,
    pub kind: &'static str,
    pub location_id: i64,
    pub location_name: String,
    pub full_url: String,
    pub thumb_url: Option<String>,
    pub alt: String,
    pub dimensions: Option<String>,
    pub contributor_label: String,
    pub uploaded_label: String,
    /// The object is actually in storage. A presigned URL is issued whether or
    /// not the object exists, so without this check a missing file renders as a
    /// broken image and a moderator can approve a photo nobody can see. CSP
    /// forbids an `onerror` fallback, so the check happens server-side.
    pub available: bool,
    /// Pre-filled rejection reason for an unavailable image, so the moderator
    /// clears the queue in one click instead of inventing wording.
    pub missing_reason: &'static str,
}

pub async fn moderation_photo_vm(
    t: Translator,
    storage: &dyn ObjectStorage,
    p: &PendingPhoto,
) -> ModerationPhotoVm {
    // One HEAD per pending photo. The queue is a bounded page (≤50), and the
    // alternative is a moderator judging a broken image.
    let available = storage.exists(&p.storage_key).await.unwrap_or(false);
    let full_url = resolve_photo(storage, Some(&p.storage_key))
        .await
        .unwrap_or_default();
    let thumb_url = match p.thumbnail_key.as_deref().filter(|_| available) {
        Some(k) => resolve_photo(storage, Some(k)).await,
        None => None,
    };
    let alt = p
        .alt
        .clone()
        .unwrap_or_else(|| format!("Photo of {}", p.parent_name));
    let dimensions = match (p.width, p.height) {
        (Some(w), Some(h)) => Some(format!("{w} × {h}")),
        _ => None,
    };
    let contributor_label = p
        .uploader_id
        .map(|uid| format!("{} #{}", t.t("moderation.contributor"), uid.0))
        .unwrap_or_default();
    ModerationPhotoVm {
        id: p.id,
        kind: match p.kind {
            bikesnest_application::PhotoKind::Parking => "parking",
            bikesnest_application::PhotoKind::Review => "review",
        },
        location_id: p.parent_id,
        location_name: p.parent_name.clone(),
        full_url,
        thumb_url,
        alt,
        dimensions,
        contributor_label,
        uploaded_label: time_ago_label(t, p.created_at),
        available,
        missing_reason: t.t("moderation.photo_missing_reason"),
    }
}

/// The one "act on the reported content" button a queue row offers, when the
/// content is still in a state where acting is possible.
///
/// `url` always points at an endpoint that already existed — this adds a way
/// to reach the moderation actions from the queue, not new actions.
#[derive(Debug, Clone)]
pub struct ReportActionVm {
    pub url: String,
    pub label: &'static str,
    /// `hx-confirm` copy — every one of these hides or invalidates something.
    pub confirm: String,
    /// The reject-photo endpoint requires a reason; the others take none.
    pub needs_reason: bool,
}

/// One row of the reports queue.
#[derive(Debug, Clone)]
pub struct ReportVm {
    pub id: i64,
    /// The submitting user's id (moderators may compare against the viewer to
    /// hide resolve/dismiss on one's own report); never rendered on public pages.
    /// `None` once the reporter's account is anonymized.
    pub reporter_id: Option<i64>,
    pub target_type_label: &'static str,
    pub target_id: i64,
    /// Where the reported content actually is. Empty when the target has been
    /// deleted since the report was filed.
    pub target_url: String,
    /// The location's name (what the moderator recognizes), falling back to
    /// "<type> #<id>" for a target that no longer resolves.
    pub target_label: String,
    pub target_address: String,
    /// A review excerpt, or the reported photo's description — whatever tells
    /// the moderator what they are judging without opening the target.
    pub preview: Option<String>,
    /// The reported photo (or the review's photo), if any.
    pub thumb_url: Option<String>,
    pub reason_label: &'static str,
    pub description: String,
    pub state_code: &'static str,
    pub state_label: &'static str,
    /// The badge's complete Tailwind class list. Built here rather than
    /// interpolated in the template (`bg-{{ color }}` never survives Tailwind's
    /// content scan).
    pub state_badge_class: &'static str,
    pub reporter_label: String,
    pub claimed_by_label: String,
    /// Absolute filed-at, with the relative phrase as the `title`.
    pub created_label: String,
    pub created_title: String,
    pub action: Option<ReportActionVm>,
}

/// The report-reason option list (value = code, label = i18n) for the modal/select.
use bikesnest_domain::REPORT_REASONS;

pub fn report_reason_options(t: Translator) -> Vec<OptionVm> {
    REPORT_REASONS
        .iter()
        .map(|code| OptionVm {
            value: code,
            label: report_reason_label(t, code),
            checked: false,
        })
        .collect()
}

fn report_target_label(t: Translator, code: &str) -> &'static str {
    match code {
        "parking" => t.t("report.target.parking"),
        "parking_photo" => t.t("report.target.parking_photo"),
        "review" => t.t("report.target.review"),
        "review_photo" => t.t("report.target.review_photo"),
        _ => t.t("report.target.other"),
    }
}

fn report_reason_label(t: Translator, reason: &str) -> &'static str {
    match reason {
        "nonexistent_parking" => t.t("report.reason.nonexistent_parking"),
        "incorrect_location" => t.t("report.reason.incorrect_location"),
        "incorrect_price" => t.t("report.reason.incorrect_price"),
        "incorrect_hours" => t.t("report.reason.incorrect_hours"),
        "incorrect_security" => t.t("report.reason.incorrect_security"),
        "duplicate" => t.t("report.reason.duplicate"),
        "inappropriate_photo" => t.t("report.reason.inappropriate_photo"),
        "inappropriate_review" => t.t("report.reason.inappropriate_review"),
        "spam" => t.t("report.reason.spam"),
        "abuse" => t.t("report.reason.abuse"),
        "other" => t.t("report.reason.other"),
        _ => t.t("report.reason.other"),
    }
}

fn report_state_label(t: Translator, s: bikesnest_domain::ReportState) -> &'static str {
    match s {
        bikesnest_domain::ReportState::Open => t.t("report.state.open"),
        bikesnest_domain::ReportState::UnderReview => t.t("report.state.under_review"),
        bikesnest_domain::ReportState::Resolved => t.t("report.state.resolved"),
        bikesnest_domain::ReportState::Dismissed => t.t("report.state.dismissed"),
    }
}

/// The complete badge classes per report state. A `bg-{{ color }}` built in the
/// template would never reach Tailwind's content scanner, so the class list is
/// spelled out here.
fn report_state_badge_class(s: bikesnest_domain::ReportState) -> &'static str {
    match s {
        bikesnest_domain::ReportState::Open => {
            "rounded-full bg-danger/10 px-2 py-0.5 font-medium text-danger"
        }
        bikesnest_domain::ReportState::UnderReview => {
            "rounded-full bg-aging/10 px-2 py-0.5 font-medium text-aging-strong"
        }
        bikesnest_domain::ReportState::Resolved | bikesnest_domain::ReportState::Dismissed => {
            "rounded-full bg-fresh/10 px-2 py-0.5 font-medium text-fresh-strong"
        }
    }
}

/// Deep-link to the reported content: the location page, or the location page
/// anchored at the specific review.
fn report_target_url(
    r: &bikesnest_application::Report,
    preview: Option<&bikesnest_application::ReportTargetPreview>,
) -> String {
    let Some(location_id) = preview.and_then(|p| p.location_id) else {
        return String::new();
    };
    match r.target_type {
        ReportTargetType::Review | ReportTargetType::ReviewPhoto => {
            match preview.and_then(|p| p.review_id) {
                Some(review_id) => format!("/parking/{location_id}#review-{review_id}"),
                None => format!("/parking/{location_id}"),
            }
        }
        _ => format!("/parking/{location_id}"),
    }
}

/// Which moderation action is still open on this target, if any. Returns
/// `None` once the content is already hidden/invalidated/rejected — offering
/// "hide" on a hidden review would just fail with `InvalidState`.
fn report_action(
    t: Translator,
    r: &bikesnest_application::Report,
    preview: Option<&bikesnest_application::ReportTargetPreview>,
    target_label: &str,
) -> Option<ReportActionVm> {
    let state = preview?.target_state.as_deref()?;
    let id = r.target_id;
    let (url, label, needs_reason) = match (r.target_type, state) {
        (ReportTargetType::Parking, "ACTIVE") => (
            format!("/moderation/parking/{id}/invalidate"),
            t.t("moderation.action.invalidate_parking"),
            false,
        ),
        (ReportTargetType::Review, "ACTIVE") => (
            format!("/moderation/reviews/{id}/hide"),
            t.t("moderation.action.hide_review"),
            false,
        ),
        (ReportTargetType::ParkingPhoto | ReportTargetType::ReviewPhoto, "APPROVED") => (
            format!(
                "/moderation/photos/{}/{id}/hide",
                photo_kind_code(r.target_type)
            ),
            t.t("moderation.action.hide_photo"),
            false,
        ),
        // Still in the upload queue: rejecting it is the terminal action, and
        // that endpoint wants a reason.
        (ReportTargetType::ParkingPhoto | ReportTargetType::ReviewPhoto, "PENDING_REVIEW") => (
            format!(
                "/moderation/photos/{}/{id}/reject",
                photo_kind_code(r.target_type)
            ),
            t.t("moderation.action.reject_photo"),
            true,
        ),
        _ => return None,
    };
    Some(ReportActionVm {
        confirm: t
            .t("moderation.confirm.act_on_content")
            .replace("{label}", label)
            .replace("{name}", target_label),
        url,
        label,
        needs_reason,
    })
}

/// The `{kind}` path segment the photo moderation endpoints expect.
fn photo_kind_code(target_type: ReportTargetType) -> &'static str {
    match target_type {
        ReportTargetType::ReviewPhoto => "review",
        _ => "parking",
    }
}

/// Build a queue row. `preview` is the batched lookup's entry for this
/// report's target (absent when the target has since been deleted), and
/// `thumb_url` a resolved presigned URL for its photo.
pub fn report_vm(
    t: Translator,
    r: &bikesnest_application::Report,
    preview: Option<&bikesnest_application::ReportTargetPreview>,
    thumb_url: Option<String>,
) -> ReportVm {
    let target_type_label = report_target_label(t, r.target_type.as_code());
    let target_label = preview
        .and_then(|p| p.location_name.clone())
        .unwrap_or_else(|| format!("{} #{}", target_type_label, r.target_id));
    let preview_text = match r.target_type {
        ReportTargetType::Review | ReportTargetType::ReviewPhoto => {
            preview.and_then(|p| p.review_excerpt.clone())
        }
        _ => None,
    }
    .filter(|s| !s.trim().is_empty());
    ReportVm {
        id: r.id,
        reporter_id: r.reporter_id.map(|u| u.0),
        target_type_label,
        target_id: r.target_id,
        target_url: report_target_url(r, preview),
        target_address: preview
            .and_then(|p| p.location_address.clone())
            .unwrap_or_default(),
        preview: preview_text,
        thumb_url,
        reason_label: report_reason_label(t, &r.reason),
        description: r.description.clone().unwrap_or_default(),
        state_code: r.state.as_code(),
        state_label: report_state_label(t, r.state),
        state_badge_class: report_state_badge_class(r.state),
        reporter_label: r
            .reporter_id
            .map(|u| format!("{} #{}", t.t("moderation.contributor"), u.0))
            .unwrap_or_else(|| t.t("report.reporter.anonymous").to_string()),
        claimed_by_label: r
            .claimed_by
            .map(|c| format!("{} #{}", t.t("moderation.moderator"), c.0))
            .unwrap_or_else(|| t.t("report.claimed.none").to_string()),
        created_label: iso_datetime_label(t, r.created_at),
        created_title: time_ago_label(t, r.created_at),
        action: report_action(t, r, preview, &target_label),
        target_label,
    }
}

/// One "current → proposed" pair the moderator has to judge.
#[derive(Debug, Clone)]
pub struct ProposalDiffVm {
    pub label: String,
    pub current: String,
    pub proposed: String,
    /// `false` when the proposal would leave this field as it is — the row is
    /// still shown (context), just not highlighted.
    pub changed: bool,
}

/// The two points a move proposal's mini-map shows. Rendered as `data-*`
/// attributes for `details-map.js`, which is why they are pre-formatted
/// strings rather than floats.
#[derive(Debug, Clone)]
pub struct ProposalMapVm {
    pub current_lat: String,
    pub current_lon: String,
    pub proposed_lat: String,
    pub proposed_lon: String,
}

/// One row of the proposal review queue.
///
/// Everything the moderator needs to decide without leaving the page: where the
/// spot is, who asked, why, what would change, and — for a move — the approve
/// form already filled in with the proposed values (editing them is the
/// "modify" path; the merge rule lives in the application layer).
#[derive(Debug, Clone)]
pub struct ProposalVm {
    pub id: i64,
    pub location_id: i64,
    pub location_name: String,
    pub location_address: String,
    /// Stable kind code ("move_location" | "change_existence") — templates must
    /// branch on this, never on the localized label.
    pub kind_code: &'static str,
    pub kind_label: &'static str,
    pub proposer_label: String,
    /// The proposer's note, when they left one.
    pub reason: Option<String>,
    pub base_version: i64,
    /// Absolute submitted-at ("2026-09-04 14:02"), with the relative phrase
    /// ("yesterday") as the `title`.
    pub created_label: String,
    pub created_title: String,
    pub diff: Vec<ProposalDiffVm>,
    pub map: Option<ProposalMapVm>,
    /// The location moved on since this was written: approving it will be
    /// refused by the repository, so the queue says so up front.
    pub is_stale: bool,
    /// Six community approvals that could not publish: a moderator decides.
    pub is_escalated: bool,
    /// The stored payload could not be read; approving requires the moderator
    /// to supply every value.
    pub needs_manual_review: bool,
    /// Pre-filled approve-form values (empty only when there is nothing to
    /// pre-fill, i.e. a payload that needs manual review).
    pub form_lat: String,
    pub form_lon: String,
    pub form_timezone: String,
    /// The `existence` option the form preselects: "exists" | "removed" | "".
    pub form_existence: &'static str,
    /// `hx-confirm` copy for an approval that takes a spot off the map.
    /// `None` for every other approval.
    pub confirm: Option<String>,
}

fn proposal_kind_label(t: Translator, kind: bikesnest_domain::ProposalKind) -> &'static str {
    match kind {
        bikesnest_domain::ProposalKind::EditDetails => t.t("profile.edit_details"),
        bikesnest_domain::ProposalKind::MoveLocation => t.t("proposal.kind.move"),
        bikesnest_domain::ProposalKind::ChangeExistence => t.t("proposal.kind.existence"),
    }
}

fn existence_label(t: Translator, exists: bool) -> &'static str {
    if exists {
        t.t("proposal.existence.exists")
    } else {
        t.t("proposal.existence.removed")
    }
}

/// Coordinates at the precision the approve form round-trips (≈1 m), so
/// re-submitting an untouched form is a no-op rather than a tiny move.
fn coord(v: f64) -> String {
    format!("{v:.6}")
}

fn coord_pair(lat: Option<f64>, lon: Option<f64>, t: Translator) -> String {
    match (lat, lon) {
        (Some(lat), Some(lon)) => format!("{}, {}", coord(lat), coord(lon)),
        _ => t.t("proposal.value.unknown").to_string(),
    }
}

pub fn proposal_vm(t: Translator, p: &bikesnest_application::Proposal) -> ProposalVm {
    use bikesnest_domain::ProposedChange;

    let mut diff = Vec::new();
    let mut map = None;
    let mut form_lat = String::new();
    let mut form_lon = String::new();
    let mut form_timezone = String::new();
    let mut form_existence = "";
    let mut confirm = None;

    match &p.change {
        ProposedChange::EditDetails(edit) => {
            let current = crate::profile::snapshot_values(&p.current_snapshot, t);
            for field in crate::profile::edit_values(edit, t) {
                let before = current
                    .iter()
                    .find(|f| f.key == field.key)
                    .map(|f| f.value.clone())
                    .unwrap_or_else(|| t.t("proposal.value.unknown").into());
                if before != field.value {
                    diff.push(ProposalDiffVm {
                        label: field.label,
                        current: before,
                        proposed: field.value,
                        changed: true,
                    });
                }
            }
        }
        ProposedChange::MoveLocation { lat, lon, timezone } => {
            let changed = p.current_lat != Some(*lat) || p.current_lon != Some(*lon);
            diff.push(ProposalDiffVm {
                label: t.t("proposal.field.coordinates").into(),
                current: coord_pair(p.current_lat, p.current_lon, t),
                proposed: format!("{}, {}", coord(*lat), coord(*lon)),
                changed,
            });
            let proposed_tz = timezone
                .clone()
                .unwrap_or_else(|| p.current_timezone.clone());
            diff.push(ProposalDiffVm {
                label: t.t("proposal.field.timezone").into(),
                current: p.current_timezone.clone(),
                proposed: proposed_tz.clone(),
                changed: proposed_tz != p.current_timezone,
            });
            // Two markers only when there is a "before" to compare against; a
            // location with no coordinates yet gets the single proposed pin.
            map = Some(ProposalMapVm {
                current_lat: p.current_lat.map(coord).unwrap_or_default(),
                current_lon: p.current_lon.map(coord).unwrap_or_default(),
                proposed_lat: coord(*lat),
                proposed_lon: coord(*lon),
            });
            form_lat = coord(*lat);
            form_lon = coord(*lon);
            form_timezone = proposed_tz;
        }
        ProposedChange::ChangeExistence { exists } => {
            let currently_exists = p.current_state == bikesnest_domain::ModerationState::Active;
            diff.push(ProposalDiffVm {
                label: t.t("proposal.field.existence").into(),
                current: existence_label(t, currently_exists).to_string(),
                proposed: existence_label(t, *exists).to_string(),
                changed: currently_exists != *exists,
            });
            form_existence = if *exists {
                bikesnest_domain::ProposedChange::EXISTS
            } else {
                bikesnest_domain::ProposedChange::REMOVED
            };
            // Taking a spot off the map is the one approval that is hard to
            // notice and annoying to undo, so it asks first.
            if !*exists {
                confirm = Some(
                    t.t("moderation.confirm.remove")
                        .replace("{name}", &p.location_name),
                );
            }
        }
        ProposedChange::Unknown => {}
    }

    ProposalVm {
        id: p.id,
        location_id: p.location_id,
        location_name: p.location_name.clone(),
        location_address: p.location_address.clone(),
        kind_code: p.kind.as_code(),
        kind_label: proposal_kind_label(t, p.kind),
        proposer_label: p
            .proposer_id
            .map(|u| format!("{} #{}", t.t("moderation.proposer"), u.0))
            .unwrap_or_else(|| t.t("report.reporter.anonymous").to_string()),
        reason: p.reason.clone(),
        base_version: p.base_version,
        created_label: iso_datetime_label(t, p.created_at),
        created_title: time_ago_label(t, p.created_at),
        diff,
        map,
        is_stale: p.is_stale(),
        is_escalated: p.escalated_at.is_some(),
        needs_manual_review: p.change == ProposedChange::Unknown,
        form_lat,
        form_lon,
        form_timezone,
        form_existence,
        confirm,
    }
}
