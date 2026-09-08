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

/// Post-action confirmation flags on the details page (`?proposed=1`, `?edited=1`, …).
/// The last four are the no-JS landing spots for the fragment endpoints: with
/// scripting off those POSTs redirect here instead of answering with a partial.
#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct DetailsNotice {
    #[serde(default)]
    tab: String,
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
            // Approved photos. A read failure degrades to no
            // gallery rather than failing the page.
            let gallery = match state.photos.photos(id).await {
                Ok(photos) => {
                    let name = view.location.name().to_string();
                    let mut gallery = Vec::new();
                    for p in photos {
                        let Some(url) = view::resolve_photo(&*state.storage, Some(&p.key)).await
                        else {
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
                            alt: p.alt.unwrap_or_else(|| format!("Photo of {name}")),
                        });
                    }
                    gallery
                }
                Err(_) => Vec::new(),
            };
            let viewer = auth.user.as_ref().map(|u| u.id);
            // Community overlay (reviews, confidence, favorite, verification).
            // Reuses the location already loaded above instead of re-reading
            // the aggregate. A read failure degrades to the base detail page,
            // never a 500.
            let community = state
                .contributions
                .community_details(view.location.clone(), viewer)
                .await
                .ok();
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
            .collaboration_proposals(
                state
                    .contributions
                    .listing_proposals(id)
                    .await
                    .unwrap_or_default(),
            )
            .collaboration_history(
                state
                    .contributions
                    .revision_history(id)
                    .await
                    .unwrap_or_default(),
            )
            .notice(notice);
            page.pending_photos = state.photos.pending_count(id).await.unwrap_or_default();
            page.tab = match q.tab.as_str() {
                "history" => "history",
                "approvals" => "approvals",
                _ => "current",
            }
            .into();
            render(page, StatusCode::OK)
        }
        Ok(None) => not_found_page(&headers, &state.map, &auth, tr),
        Err(_) => internal_error(&headers, &state.map, &auth, tr),
    }
}
