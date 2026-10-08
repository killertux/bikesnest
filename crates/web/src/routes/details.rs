//! `/parking/{id}` — parking details, gallery, and community proposals.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use bikesnest_domain::{ModerationState, Role};

use crate::auth::Auth;
use crate::i18n::{Locale, Translator};
use crate::state::AppState;
use crate::view;
use crate::{DetailsPage, PhotoVm};

use super::common::render;
use super::errors::{internal_error, not_found_page};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DetailReadPlan {
    gallery: bool,
    community: bool,
    proposals: bool,
    history: bool,
}

/// The tab-to-reader contract used by the production handler. Compact pending
/// proposal/photo metadata is shared and therefore intentionally not optional.
fn detail_read_plan(tab: &str) -> DetailReadPlan {
    DetailReadPlan {
        gallery: tab == "current",
        community: tab == "current",
        proposals: tab == "approvals",
        history: tab == "history",
    }
}

/// Post-action confirmation flags on the details page (`?proposed=1`, `?edited=1`, …).
/// The last four are the no-JS landing spots for the fragment endpoints: with
/// scripting off those POSTs redirect here instead of answering with a partial.
#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct DetailsNotice {
    #[serde(default)]
    tab: String,
    #[serde(default)]
    after: Option<i64>,
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    proposal_error: Option<String>,
    #[serde(default)]
    added: Option<String>,
    /// A spot the contributor just created (the "what happens next" notice).
    #[serde(default)]
    created: Option<String>,
    #[serde(default)]
    edited: Option<String>,
    #[serde(default)]
    proposed: Option<String>,
    #[serde(default)]
    reviewed: Option<String>,
    #[serde(default)]
    verified: Option<String>,
    #[serde(default)]
    parked: Option<String>,
    #[serde(default)]
    reported: Option<String>,
    #[serde(default)]
    photo: Option<String>,
    #[serde(default)]
    voted: Option<String>,
}

/// One notice for the details page banner, newest/strongest action first.
pub(crate) fn details_notice(tr: Translator, q: &DetailsNotice) -> Option<String> {
    if q.proposal_error.is_some() {
        return Some(tr.t("profile.proposal_error").into());
    }
    if q.created.is_some() {
        Some(tr.t("contribution.created_notice").to_string())
    } else if q.proposed.is_some() {
        Some(tr.t("details.notice.proposed").to_string())
    } else if q.edited.is_some() {
        Some(tr.t("details.notice.edited").to_string())
    } else if q.reviewed.is_some() {
        Some(tr.t("details.notice.reviewed").to_string())
    } else if q.added.is_some() {
        Some(tr.t("details.notice.added").to_string())
    } else if q.verified.is_some() {
        Some(tr.t("verification.saved").to_string())
    } else if q.parked.is_some() {
        Some(tr.t("parked.saved").to_string())
    } else if q.reported.is_some() {
        Some(tr.t("report.submitted").to_string())
    } else if q.photo.is_some() {
        Some(tr.t("photo.upload.success").to_string())
    } else if q.voted.is_some() {
        Some(tr.t("collab.voted").to_string())
    } else {
        None
    }
}

/// Parking details.
pub(crate) async fn parking_details(
    State(state): State<AppState>,
    locale: Locale,
    auth: Auth,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(q): Query<DetailsNotice>,
) -> Response {
    let tr = Translator::new(locale);
    match state.details.execute(id).await {
        Ok(Some(view)) => {
            // The public page returns 404 for a non-ACTIVE location (removed/
            // invalid/flagged). Moderators/admins still see the page with a banner.
            let is_moderator = auth
                .user
                .as_ref()
                .map(|u| u.has_role(Role::Moderator) || u.has_role(Role::Admin))
                .unwrap_or(false);
            if view.location.moderation_state() != ModerationState::Active && !is_moderator {
                return not_found_page(&headers, &state.map, &auth, tr);
            }
            let tab = match q.tab.as_str() {
                "history" => "history",
                "approvals" => "approvals",
                _ => "current",
            };
            let reads = detail_read_plan(tab);
            let limit = q.limit.unwrap_or(20).clamp(1, 50);
            let mut gallery_available = true;
            let mut gallery_total = 0;
            let gallery = if reads.gallery {
                match state.detail_reads.photos_page(id, 24).await {
                    Ok((photos, total)) => {
                        gallery_total = total;
                        let name = view.location.name().to_string();
                        let mut gallery = Vec::new();
                        let mut signing_failed = false;
                        for p in photos {
                            let Some(url) =
                                view::resolve_photo(&*state.storage, Some(&p.key)).await
                            else {
                                signing_failed = true;
                                continue;
                            };
                            let thumb_url = match p.thumbnail_key.as_deref() {
                                Some(k) => view::resolve_photo(&*state.storage, Some(k))
                                    .await
                                    .unwrap_or_else(|| url.clone()),
                                None => url.clone(),
                            };
                            gallery.push(PhotoVm {
                                url,
                                thumb_url,
                                alt: p.alt.unwrap_or_else(|| {
                                    tr.t("details.photo_alt").replace("{name}", &name)
                                }),
                            });
                        }
                        if signing_failed {
                            gallery_available = false;
                            tracing::warn!(
                                category = "gallery_signing_unavailable",
                                location_id = id
                            );
                        }
                        gallery
                    }
                    Err(_) => {
                        gallery_available = false;
                        tracing::warn!(category = "gallery_unavailable", location_id = id);
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            };
            let viewer = auth.user.as_ref().map(|u| u.id);
            let community = if reads.community {
                state
                    .detail_reads
                    .community(view.location.clone(), viewer, q.after, limit)
                    .await
                    .map_err(|_| {
                        tracing::warn!(category = "community_unavailable", location_id = id)
                    })
                    .ok()
            } else {
                None
            };
            let current_content_available = tab != "current" || community.is_some();
            // Post-action confirmation (e.g. "this change will be reviewed").
            let notice = details_notice(tr, &q);
            let mut page = DetailsPage::build_community(
                &state.map,
                tr,
                view,
                gallery,
                &auth,
                community,
                &*state.storage,
            )
            .await
            .notice(notice);
            page.tab = tab.into();
            page.gallery_available = gallery_available;
            page.gallery_total = gallery_total;
            page.current_content_available = current_content_available;
            if tab == "current" && page.reviews_has_more {
                page.reviews_next = page
                    .reviews
                    .last()
                    .map(|review| format!("/parking/{id}?limit={limit}&after={}", review.id));
            }

            match state.detail_reads.summary(id).await {
                Ok(summary) => page = page.pending_summary(summary),
                Err(_) => {
                    page.collaboration_summary_available = false;
                    tracing::warn!(category = "proposal_summary_unavailable", location_id = id);
                }
            }
            match state.detail_reads.pending_photos(id).await {
                Ok(count) => page.pending_photos = count,
                Err(_) => {
                    page.collaboration_summary_available = false;
                    tracing::warn!(
                        category = "pending_photo_count_unavailable",
                        location_id = id
                    );
                }
            }
            if reads.proposals {
                match state.detail_reads.proposals(id, q.after, limit).await {
                    Ok((items, total, has_more)) => {
                        page.proposals_total = total;
                        page = page.collaboration_proposals(items);
                        page.proposals_next = has_more.then(|| {
                            format!(
                                "/parking/{id}?tab=approvals&limit={limit}&after={}",
                                page.collaboration_proposals
                                    .last()
                                    .map(|p| p.id)
                                    .unwrap_or_default()
                            )
                        });
                    }
                    Err(_) => {
                        page.proposals_available = false;
                        tracing::warn!(category = "proposals_unavailable", location_id = id);
                    }
                }
            } else if reads.history {
                match state.detail_reads.history(id, q.after, limit).await {
                    Ok((items, total, has_more)) => {
                        page.history_total = total;
                        page = page.collaboration_history(items);
                        page.history_next = has_more.then(|| {
                            format!(
                                "/parking/{id}?tab=history&limit={limit}&after={}",
                                page.collaboration_history
                                    .last()
                                    .map(|r| r.version)
                                    .unwrap_or_default()
                            )
                        });
                    }
                    Err(_) => {
                        page.history_available = false;
                        tracing::warn!(category = "history_unavailable", location_id = id);
                    }
                }
            }
            render(page, StatusCode::OK)
        }
        Ok(None) => not_found_page(&headers, &state.map, &auth, tr),
        Err(_) => internal_error(&headers, &state.map, &auth, tr),
    }
}
