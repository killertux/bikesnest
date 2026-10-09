//! Community view models: reviews, confidence, duplicates, contributions.

use super::format::*;
use crate::PhotoVm;
use crate::i18n::Translator;

/// One rendered review. `photos` are the review's APPROVED photos
/// already resolved to presigned URLs for the card's thumbnails.
#[derive(Debug, Clone)]
pub struct ReviewVm {
    pub id: i64,
    pub rating: u8,
    pub stars: String,
    /// The rating as words ("4 of 5 stars") for screen readers; `stars` is hidden.
    pub stars_label: String,
    pub body: String,
    pub created_label: String,
    pub author_label: String,
    pub is_own: bool,
    pub photos: Vec<PhotoVm>,
    /// False only when approved review media exists but its primary URL could
    /// not be signed. Review text remains independently available.
    pub media_available: bool,
}

pub fn review_vm(
    t: Translator,
    r: &bikesnest_application::Review,
    is_own: bool,
    photos: Vec<PhotoVm>,
) -> ReviewVm {
    let stars = "★".repeat(r.rating.value() as usize);
    let created_label = time_ago_label(t, r.created_at);
    ReviewVm {
        id: r.id,
        rating: r.rating.value(),
        stars,
        stars_label: t
            .t("review.stars_label")
            .replace("{n}", &r.rating.value().to_string()),
        body: r.body.as_str().to_string(),
        created_label,
        author_label: r
            .public_author_name
            .clone()
            .unwrap_or_else(|| t.t("collab.anonymous").to_string()),
        is_own,
        photos,
        media_available: true,
    }
}

/// Localized confidence label for the confidence badge.
pub fn confidence_label(t: Translator, c: bikesnest_domain::Confidence) -> &'static str {
    match c {
        bikesnest_domain::Confidence::Reported => t.t("confidence.reported"),
        bikesnest_domain::Confidence::Verified => t.t("confidence.verified"),
        bikesnest_domain::Confidence::RecentlyVerified => t.t("confidence.recently_verified"),
        bikesnest_domain::Confidence::Stale => t.t("confidence.stale"),
        bikesnest_domain::Confidence::Conflicting => t.t("confidence.conflicting"),
    }
}

/// One rendered confidence badge.
#[derive(Debug, Clone)]
pub struct ConfidenceVm {
    pub code: &'static str,
    pub label: &'static str,
}

pub fn confidence_vm(t: Translator, c: bikesnest_domain::Confidence) -> ConfidenceVm {
    ConfidenceVm {
        code: c.as_code(),
        label: confidence_label(t, c),
    }
}

/// One rendered attribution dispute tally.
#[derive(Debug, Clone)]
pub struct AttrDisputeVm {
    pub label: &'static str,
    pub incorrect: i64,
}

pub fn attr_dispute_vm(t: Translator, code: &str, incorrect: i64) -> AttrDisputeVm {
    AttrDisputeVm {
        label: attribute_label(t, code),
        incorrect,
    }
}

fn attribute_label(t: Translator, code: &str) -> &'static str {
    match code {
        "name" => t.t("attr.name"),
        "address" => t.t("attr.address"),
        "type" => t.t("attr.type"),
        "cost" => t.t("attr.cost"),
        "hours" => t.t("attr.hours"),
        "security" => t.t("attr.security"),
        "location" => t.t("attr.location"),
        _ => t.t("attr.unknown"),
    }
}

/// One rendered "recommended because…" reason.
#[derive(Debug, Clone)]
pub struct ReasonVm {
    pub label: &'static str,
    pub detail: String,
}

pub fn reason_vm(t: Translator, r: &bikesnest_application::Reason) -> ReasonVm {
    ReasonVm {
        label: t.t(r.label_key),
        detail: r.detail.clone(),
    }
}

/// One rendered row of the contribution history feed.
#[derive(Debug, Clone)]
pub struct ContributionVm {
    /// Stable code for the row's icon: "added" | "edited" | "proposed" |
    /// "reviewed" | "verified" | "parked_here" | "favorited" | "photo_pending"
    /// | "other". Kept separate from `kind_label` (which is localized) so the
    /// template can pick an icon without matching on translated text.
    pub kind_code: &'static str,
    pub kind_label: &'static str,
    pub target: String,
    pub state_label: &'static str,
    pub at_label: String,
}

/// One advisory duplicate candidate.
#[derive(Debug, Clone)]
pub struct DuplicateVm {
    pub id: i64,
    pub name: String,
    pub distance_label: String,
    pub similarity_label: String,
}

pub fn duplicate_vm(t: Translator, d: &bikesnest_application::DuplicateCandidate) -> DuplicateVm {
    DuplicateVm {
        id: d.id,
        name: d.name.clone(),
        distance_label: distance_label(t, d.distance_m),
        similarity_label: format!("{:.0}%", d.similarity * 100.0),
    }
}

pub fn contribution_vm(
    t: Translator,
    i: &bikesnest_application::ContributionItem,
) -> ContributionVm {
    // "parked_here" and "photo.pending" are their own kinds — a parked-here
    // signal is not a verification, and a pending photo isn't approved yet,
    // so neither should read as "Verificou" (a real existence/attribute
    // verification).
    let (kind_code, kind): (&'static str, &'static str) = match i.kind.as_str() {
        "added" => ("added", t.t("contrib.kind.added")),
        "edited" => ("edited", t.t("contrib.kind.edited")),
        "proposed" => ("proposed", t.t("contrib.kind.proposed")),
        "reviewed" => ("reviewed", t.t("contrib.kind.reviewed")),
        "verified" => ("verified", t.t("contrib.kind.verified")),
        "parked_here" => ("parked_here", t.t("contrib.kind.parked_here")),
        "favorited" => ("favorited", t.t("contrib.kind.favorited")),
        "photo.pending" => ("photo_pending", t.t("contrib.kind.photo_pending")),
        _ => ("other", t.t("contrib.kind.other")),
    };
    let state = match i.state.as_str() {
        "active" => t.t("contrib.state.active"),
        "pending" => t.t("contrib.state.pending"),
        "approved" => t.t("profile.approved"),
        "rejected" => t.t("profile.rejected"),
        "superseded" => t.t("profile.superseded"),
        "history" => t.t("contrib.state.history"),
        _ => t.t("contrib.state.other"),
    };
    ContributionVm {
        kind_code,
        kind_label: kind,
        target: i.target.clone(),
        state_label: state,
        at_label: time_ago_label(t, i.at),
    }
}
