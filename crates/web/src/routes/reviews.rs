//! Activity on a location: reviews, verifications, "parked here" and
//! favorites, plus the two account pages that list what a user contributed.

use axum::extract::{Form, Multipart, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use bikesnest_application::{ContributionError, NewVerification, PhotoTarget};
use bikesnest_domain::{
    ExistenceResult, ReviewBody, SecurityState, StarRating, is_known_attribute_code,
};
use bikesnest_infrastructure::MapConfig;

use crate::auth::Auth;
use crate::client_ip::ClientIp;
use crate::htmx::fragment_or_redirect;
use crate::i18n::{Locale, Translator};
use crate::state::AppState;
use crate::view::{self, CardVm};
use crate::{ContributionsPage, FavoritesPage, PageLayout, ReviewFormPage};

use super::common::{
    DEFAULT_PAGE_LIMIT, fragment_answer, parse_after_id, parse_datetime, render,
    urlencoding_rfc3339,
};
use super::community::{contribution_error_message, contribution_error_status};

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct ReviewForm {
    #[serde(default)]
    rating: u8,
    #[serde(default)]
    body: String,
}

pub(crate) async fn review_page(
    State(state): State<AppState>,
    locale: Locale,
    auth: Auth,
    Path(id): Path<i64>,
) -> Response {
    let tr = Translator::new(locale);
    if let Err(resp) = auth.require_verified() {
        return resp;
    }
    let own = match state.details.execute(id).await {
        Ok(Some(view)) => state
            .contributions
            .community_details(view.location, auth.user.as_ref().map(|u| u.id))
            .await
            .ok()
            .and_then(|c| c.own_review),
        _ => None,
    };
    render(
        ReviewFormPage {
            layout: PageLayout::for_request(
                tr.t("review.title").to_string(),
                "review",
                &auth,
                &state.map,
            ),
            tr,
            id,
            rating: own.as_ref().map(|r| r.rating.value()).unwrap_or(0),
            body: own.map(|r| r.body.as_str().to_string()).unwrap_or_default(),
            error: None,
            field_errors: view::FieldErrors::new(),
        },
        StatusCode::OK,
    )
}

pub(crate) async fn review_post(
    State(state): State<AppState>,
    locale: Locale,
    ClientIp(ip): ClientIp,
    auth: Auth,
    Path(id): Path<i64>,
    mut multipart: Multipart,
) -> Response {
    let tr = Translator::new(locale);
    let user = match auth.require_verified() {
        Ok(u) => u,
        Err(resp) => return resp,
    };

    // Multipart form carries 0..N photos. Gather text fields, then
    // any uploaded `photo` files. The text publishes immediately; photos hold PENDING_REVIEW.
    let mut rating_u8 = 0u8;
    let mut body = String::new();
    let mut photos: Vec<Vec<u8>> = Vec::new();
    loop {
        let field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(_) => {
                return render_review_error(
                    &state.map,
                    tr,
                    auth,
                    id,
                    ReviewForm {
                        rating: rating_u8,
                        body,
                    },
                    tr.t("review.error.generic").to_string(),
                );
            }
        };
        match field.name().unwrap_or("") {
            "rating" => {
                if let Ok(text) = field.text().await {
                    rating_u8 = text.trim().parse().unwrap_or(0);
                }
            }
            "body" => {
                if let Ok(text) = field.text().await {
                    body = text;
                }
            }
            "photo" => {
                if let Ok(bytes) = field.bytes().await {
                    photos.push(bytes.to_vec());
                }
            }
            _ => {
                let _ = field.bytes().await;
            }
        }
    }

    let rating = match StarRating::new(rating_u8) {
        Ok(r) => r,
        Err(_) => {
            return render_review_field_error(
                &state.map,
                tr,
                auth,
                id,
                ReviewForm {
                    rating: rating_u8,
                    body,
                },
                tr.t("review.error.invalid").to_string(),
                Some("rating"),
            );
        }
    };
    let review_body = match ReviewBody::new(&body) {
        Ok(b) => b,
        Err(_) => {
            return render_review_field_error(
                &state.map,
                tr,
                auth,
                id,
                ReviewForm {
                    rating: rating_u8,
                    body,
                },
                tr.t("review.error.length").to_string(),
                Some("body"),
            );
        }
    };
    match state
        .contributions
        .upsert_review(user, id, rating, &review_body)
        .await
    {
        Ok(()) => {
            // Attach any uploaded photos to the (just-upserted) review, held PENDING_REVIEW.
            if !photos.is_empty()
                && let Ok(Some(view)) = state.details.execute(id).await
                && let Ok(own) = state
                    .contributions
                    .community_details(view.location, Some(user.id))
                    .await
                && let Some(review) = own.own_review
            {
                for p in photos {
                    let _ = state
                        .photo
                        .upload_photo(user, &ip, PhotoTarget::Review(review.id), &p, None)
                        .await;
                }
            }
            axum::response::Redirect::to(&format!("/parking/{id}?reviewed=1")).into_response()
        }
        Err(ContributionError::RateLimited) => render_review_error(
            &state.map,
            tr,
            auth,
            id,
            ReviewForm {
                rating: rating_u8,
                body,
            },
            tr.t("contribution.error.rate_limited").to_string(),
        ),
        // A duplicate review or a lost race is the user's to resolve (409); a
        // spot that no longer takes contributions is gone (404); an unreachable
        // database is ours (503). Everything else stays generic.
        Err(
            e @ (ContributionError::Conflict
            | ContributionError::Unavailable
            | ContributionError::LocationNotActive),
        ) => render_review_error_status(
            &state.map,
            tr,
            auth,
            id,
            ReviewForm {
                rating: rating_u8,
                body,
            },
            contribution_error_message(tr, &e),
            None,
            contribution_error_status(&e, StatusCode::OK),
        ),
        Err(_) => render_review_error(
            &state.map,
            tr,
            auth,
            id,
            ReviewForm {
                rating: rating_u8,
                body,
            },
            tr.t("review.error.generic").to_string(),
        ),
    }
}

pub(crate) fn render_review_error(
    map: &MapConfig,
    tr: Translator,
    auth: Auth,
    id: i64,
    form: ReviewForm,
    message: String,
) -> Response {
    render_review_field_error(map, tr, auth, id, form, message, None)
}

/// Like [`render_review_error`], but also flags the one input (`"rating"` or
/// `"body"`) the rejection is about — `None` for the errors
/// that are not about one particular field (rate limits, conflicts).
pub(crate) fn render_review_field_error(
    map: &MapConfig,
    tr: Translator,
    auth: Auth,
    id: i64,
    form: ReviewForm,
    message: String,
    field: Option<&'static str>,
) -> Response {
    render_review_error_status(map, tr, auth, id, form, message, field, StatusCode::OK)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_review_error_status(
    map: &MapConfig,
    tr: Translator,
    auth: Auth,
    id: i64,
    form: ReviewForm,
    message: String,
    field: Option<&'static str>,
    status: StatusCode,
) -> Response {
    let field_errors = match field {
        Some(f) => view::FieldErrors::single(f, message.clone()),
        None => view::FieldErrors::new(),
    };
    render(
        ReviewFormPage {
            layout: PageLayout::for_request(tr.t("review.title").to_string(), "review", &auth, map),
            tr,
            id,
            rating: form.rating,
            body: form.body,
            error: Some(message),
            field_errors,
        },
        status,
    )
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct VerifyForm {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    result: String,
    #[serde(default)]
    attribute_code: String,
}

pub(crate) async fn parking_verify_post(
    State(state): State<AppState>,
    locale: Locale,
    auth: Auth,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(form): Form<VerifyForm>,
) -> Response {
    let tr = Translator::new(locale);
    let user = match auth.require_verified() {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let ctx = VerifyCtx {
        headers: &headers,
        map: &state.map,
        tr,
        back: format!("/parking/{id}?verified=1"),
    };
    // Validate the submitted kind/result/attribute rather than silently
    // coercing unknown inputs into StillExists/Correct.
    let signal = match form.kind.as_str() {
        "attribute" => {
            let result = match form.result.as_str() {
                "correct" => bikesnest_domain::AttributeResult::Correct,
                "incorrect" => bikesnest_domain::AttributeResult::Incorrect,
                _ => {
                    return verify_bad_request(&ctx);
                }
            };
            if !is_known_attribute_code(&form.attribute_code) {
                return verify_bad_request(&ctx);
            }
            NewVerification::Attribute {
                location_id: id,
                user_id: user.id,
                code: form.attribute_code.clone(),
                result,
            }
        }
        "parked_here" => NewVerification::ParkedHere {
            location_id: id,
            user_id: user.id,
        },
        "existence" => {
            let result = match form.result.as_str() {
                "still_exists" => ExistenceResult::StillExists,
                "no_longer_exists" => ExistenceResult::NoLongerExists,
                "info_changed" => ExistenceResult::InfoChanged,
                _ => return verify_bad_request(&ctx),
            };
            NewVerification::Existence {
                location_id: id,
                user_id: user.id,
                result,
            }
        }
        _ => return verify_bad_request(&ctx),
    };
    match state.contributions.record_verification(user, &signal).await {
        Ok(()) => verification_saved(&ctx, tr.t("verification.saved")),
        Err(e) => verification_error(&ctx, &e),
    }
}

/// What the small verification fragments need to answer both callers: the
/// request's htmx headers, the map config for the styled error page, and the
/// page a whole-document request is redirected back to.
pub(crate) struct VerifyCtx<'a> {
    headers: &'a HeaderMap,
    map: &'a MapConfig,
    tr: Translator,
    back: String,
}

pub(crate) fn verification_result(
    ctx: &VerifyCtx<'_>,
    state: &'static str,
    label: &str,
    status: StatusCode,
) -> Response {
    let tr = ctx.tr;
    fragment_answer(ctx.headers, ctx.map, tr, status, label, &ctx.back, || {
        render(
            crate::VerificationResultVm {
                tr,
                state,
                label: label.to_string(),
            },
            status,
        )
    })
}

pub(crate) fn verification_saved(ctx: &VerifyCtx<'_>, label: &str) -> Response {
    verification_result(ctx, "success", label, StatusCode::OK)
}

pub(crate) fn verify_bad_request(ctx: &VerifyCtx<'_>) -> Response {
    verification_result(
        ctx,
        "error",
        ctx.tr.t("contribution.error.invalid"),
        StatusCode::BAD_REQUEST,
    )
}

/// Error state of the verification fragment. A lost race (409) and an
/// unreachable database (503) say so; every other variant keeps the generic
/// message and the 400 these endpoints have always returned.
pub(crate) fn verification_error(ctx: &VerifyCtx<'_>, e: &ContributionError) -> Response {
    verification_result(
        ctx,
        "error",
        &contribution_fragment_message(ctx.tr, e),
        contribution_error_status(e, StatusCode::BAD_REQUEST),
    )
}

/// Message for the small htmx fragments the detail page swaps in. Only the
/// variants a user can act on get their own copy; anything else stays
/// deliberately vague.
pub(crate) fn contribution_fragment_message(tr: Translator, e: &ContributionError) -> String {
    match e {
        ContributionError::Conflict
        | ContributionError::Unavailable
        | ContributionError::LocationNotActive => contribution_error_message(tr, e),
        _ => tr.t("contribution.error.generic").to_string(),
    }
}

pub(crate) async fn parking_parked_here_post(
    State(state): State<AppState>,
    locale: Locale,
    auth: Auth,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    let tr = Translator::new(locale);
    let user = match auth.require_verified() {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let ctx = VerifyCtx {
        headers: &headers,
        map: &state.map,
        tr,
        back: format!("/parking/{id}?parked=1"),
    };
    let signal = NewVerification::ParkedHere {
        location_id: id,
        user_id: user.id,
    };
    match state.contributions.record_verification(user, &signal).await {
        Ok(()) => verification_saved(&ctx, tr.t("parked.saved")),
        Err(e) => verification_error(&ctx, &e),
    }
}

pub(crate) async fn parking_favorite_post(
    State(state): State<AppState>,
    locale: Locale,
    auth: Auth,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    let tr = Translator::new(locale);
    let user = match auth.require_user() {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    // The success response *is* the button, so a whole-document caller (no JS,
    // or a boosted submit) is sent back to the page, which re-renders it in the
    // new state — no notice flag needed.
    let back = format!("/parking/{id}");
    match state.contributions.toggle_favorite(user.id, id).await {
        Ok(is_favorited) => fragment_or_redirect(
            &headers,
            render(
                crate::FavoriteButtonVm {
                    tr,
                    id,
                    is_favorited,
                    csrf: auth.csrf_value(),
                },
                StatusCode::OK,
            ),
            &back,
        ),
        // The error swap needs a fragment of its own. 409/503 for the mapper's
        // variants; everything else keeps the 500 it returned before, now with
        // translated copy.
        Err(e) => {
            let message = contribution_fragment_message(tr, &e);
            let status = contribution_error_status(&e, StatusCode::INTERNAL_SERVER_ERROR);
            fragment_answer(&headers, &state.map, tr, status, &message, &back, || {
                render(
                    crate::FragmentErrorVm {
                        tr,
                        message: message.clone(),
                    },
                    status,
                )
            })
        }
    }
}

/// Keyset cursor for the favorites page: `(created_at, location_id)`, the
/// same compound shape as the photo queue's cursor (favorites keep recency
/// order — "most recently favorited first" — which needs both fields).
#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct FavoritesQuery {
    #[serde(default)]
    after_at: String,
    #[serde(default)]
    after_id: i64,
}

pub(crate) async fn account_favorites(
    State(state): State<AppState>,
    locale: Locale,
    auth: Auth,
    Query(q): Query<FavoritesQuery>,
) -> Response {
    let tr = Translator::new(locale);
    let user = match auth.require_user() {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let after = parse_datetime(&q.after_at).zip(parse_after_id(q.after_id));
    let favorites = state
        .contributions
        .list_favorites(user.id, after, DEFAULT_PAGE_LIMIT)
        .await
        .unwrap_or_default();
    let next_url = (favorites.len() as i64 == DEFAULT_PAGE_LIMIT)
        .then(|| favorites.last())
        .flatten()
        .map(|last| {
            format!(
                "/account/favorites?after_at={}&after_id={}",
                urlencoding_rfc3339(last.created_at),
                last.location_id
            )
        });
    let now = chrono::Utc::now();
    let mut items = Vec::new();
    for entry in favorites {
        let tid = entry.location_id;
        // Read each favorite as a summary card (best-effort; skip missing).
        if let Some(view) = state.details.execute(tid).await.ok().flatten() {
            let loc = &view.location;
            let summary = bikesnest_application::ParkingSummary {
                id: loc.id(),
                name: loc.name().to_string(),
                address: loc.address().to_string(),
                parking_type: loc.parking_type(),
                cost: loc.cost().clone(),
                point: *loc.point(),
                distance_m: 0.0,
                security_yes: loc
                    .security()
                    .iter()
                    .filter(|f| f.state() == SecurityState::Yes)
                    .map(|f| f.code().to_string())
                    .collect(),
                rating: *loc.rating(),
                last_verified_at: loc.last_verified_at(),
                timezone: loc.timezone(),
                is_open_now: loc.hours().status_at(now, loc.timezone())
                    == bikesnest_domain::OpenStatus::Open,
                photo_key: None,
                // Favorites are listed whole, not paginated by keyset.
                sort_key: None,
            };
            let freshness = bikesnest_domain::categorize(
                loc.last_verified_at(),
                now,
                &state.freshness.thresholds,
            );
            let photo_url = view::resolve_photo(&*state.storage, None).await;
            items.push(CardVm::from_summary(tr, &summary, freshness, photo_url));
        }
    }
    render(
        FavoritesPage {
            layout: PageLayout::for_request(
                tr.t("favorites.title").to_string(),
                "account",
                &auth,
                &state.map,
            ),
            tr,
            items,
            notice: None,
            next_url,
        },
        StatusCode::OK,
    )
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct ContributionsQuery {
    /// Keyset cursor (`at`, RFC3339) of the last item on the previous page.
    #[serde(default)]
    after_at: String,
    /// Keyset cursor (opaque `id`) of the last item on the previous page.
    #[serde(default)]
    after_id: i64,
}

pub(crate) async fn account_contributions(
    State(state): State<AppState>,
    locale: Locale,
    auth: Auth,
    Query(q): Query<ContributionsQuery>,
) -> Response {
    let tr = Translator::new(locale);
    let user = match auth.require_user() {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let after = parse_datetime(&q.after_at).zip(parse_after_id(q.after_id));
    let history = state
        .contributions
        .contribution_history(user.id, after, DEFAULT_PAGE_LIMIT)
        .await
        .unwrap_or_default();
    let next_url = (history.len() as i64 == DEFAULT_PAGE_LIMIT)
        .then(|| history.last())
        .flatten()
        .map(|last| {
            format!(
                "/account/contributions?after_at={}&after_id={}",
                urlencoding_rfc3339(last.at),
                last.id
            )
        });
    let items = history
        .into_iter()
        .map(|i| view::contribution_vm(tr, &i))
        .collect();
    render(
        ContributionsPage {
            layout: PageLayout::for_request(
                tr.t("contrib.title").to_string(),
                "account",
                &auth,
                &state.map,
            ),
            tr,
            items,
            next_url,
        },
        StatusCode::OK,
    )
}

// ---------------------------------------------------------------------------
// Error-mapping unit tests
// ---------------------------------------------------------------------------
//
// The handlers that surface these are covered end to end in
// `tests/http_test.rs`; provoking a real database conflict through HTTP is
// inherently racy, so the mapping itself is pinned here instead.

#[cfg(test)]
mod tests {
    use super::*;

    fn en() -> Translator {
        Translator::new(Locale::En)
    }

    /// The three htmx endpoints on the detail page (verify, parked-here,
    /// favorite) used to collapse every error into a generic 400 or a bare
    /// "Internal" 500.
    #[test]
    fn detail_page_fragments_surface_conflict_409_and_unavailable_503() {
        // Verify / parked-here: 400 is the fallback for every other variant.
        assert_eq!(
            contribution_error_status(&ContributionError::Conflict, StatusCode::BAD_REQUEST),
            StatusCode::CONFLICT
        );
        assert_eq!(
            contribution_error_status(&ContributionError::Unavailable, StatusCode::BAD_REQUEST),
            StatusCode::SERVICE_UNAVAILABLE
        );
        // Favorite: 500 is the fallback, and it is no longer a bare string.
        assert_eq!(
            contribution_error_status(
                &ContributionError::Conflict,
                StatusCode::INTERNAL_SERVER_ERROR
            ),
            StatusCode::CONFLICT
        );
        assert_eq!(
            contribution_error_status(
                &ContributionError::Unavailable,
                StatusCode::INTERNAL_SERVER_ERROR
            ),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            contribution_error_status(&ContributionError::Internal, StatusCode::BAD_REQUEST),
            StatusCode::BAD_REQUEST
        );

        // The fragments carry the translated copy, not a bare English string.
        assert_eq!(
            contribution_fragment_message(en(), &ContributionError::Conflict),
            en().t("error.conflict")
        );
        assert_eq!(
            contribution_fragment_message(en(), &ContributionError::Unavailable),
            en().t("error.unavailable")
        );
        assert_eq!(
            contribution_fragment_message(en(), &ContributionError::Internal),
            en().t("contribution.error.generic")
        );
    }
}
