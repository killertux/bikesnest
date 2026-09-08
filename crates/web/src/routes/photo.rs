//! Photo pipeline: upload (validate → process → queue for moderation)
//! and the moderation queue that publishes or rejects what was uploaded.

use axum::extract::{Form, Multipart, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use bikesnest_application::{PhotoError, PhotoKind, PhotoTarget};
use bikesnest_infrastructure::MapConfig;

use crate::auth::Auth;
use crate::client_ip::ClientIp;
use crate::i18n::{Locale, Translator};
use crate::state::AppState;
use crate::view;
use crate::{ModerationPhotosPage, PageLayout};

use super::common::{
    DEFAULT_PAGE_LIMIT, fragment_answer, parse_after_id, parse_datetime, render,
    urlencoding_rfc3339,
};
use super::moderation::{
    ModerationNotice, RejectReasonForm, moderation_error_message, moderation_notice,
    moderation_result,
};

/// POST /parking/{id}/photo — a verified user uploads one photo (multipart:
/// `photo` file + optional `alt`). Runs the same pipeline as review attachments and
/// holds the upload in `PENDING_REVIEW`. Returns a swap-safe fragment.
pub(crate) async fn upload_photo(
    State(state): State<AppState>,
    locale: Locale,
    ClientIp(ip): ClientIp,
    auth: Auth,
    Path(id): Path<i64>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    let tr = Translator::new(locale);
    let user = match auth.require_verified() {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let back = format!("/parking/{id}?photo=pending");
    let bad_request = |message: &str| {
        photo_upload_result(
            &headers,
            &state.map,
            tr,
            "error",
            message,
            StatusCode::BAD_REQUEST,
            &back,
        )
    };

    let mut photo_bytes: Option<Vec<u8>> = None;
    let mut alt: Option<String> = None;
    loop {
        let field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(_) => return bad_request(tr.t("photo.error.internal")),
        };
        match field.name().unwrap_or("") {
            "photo" => match field.bytes().await {
                Ok(b) => photo_bytes = Some(b.to_vec()),
                Err(_) => return bad_request(tr.t("photo.error.internal")),
            },
            "alt" => {
                if let Ok(text) = field.text().await {
                    alt = Some(text);
                }
            }
            _ => {
                // Drain/ignore unknown fields so the connection stays clean.
                let _ = field.bytes().await;
            }
        }
    }

    let Some(bytes) = photo_bytes else {
        return bad_request(tr.t("photo.error.internal"));
    };

    let target = PhotoTarget::Parking(id);
    let (state_name, message, status) = match state
        .photo
        .upload_photo(user, &ip, target, &bytes, alt.as_deref())
        .await
    {
        Ok(_) => (
            "success",
            tr.t("photo.upload.success").to_string(),
            StatusCode::OK,
        ),
        Err(e) => {
            let (status, message) = photo_error(tr, &e);
            ("error", message, status)
        }
    };
    photo_upload_result(
        &headers, &state.map, tr, state_name, &message, status, &back,
    )
}

/// GET /moderation/photos — the photo moderation queue (MODERATOR/ADMIN).
pub(crate) async fn moderation_photos(
    State(state): State<AppState>,
    locale: Locale,
    auth: Auth,
    Query(q): Query<ModerationNotice>,
) -> Response {
    let tr = Translator::new(locale);
    let user = match auth.require_moderator() {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let after = parse_datetime(&q.after_at).zip(parse_after_id(q.after_id));
    let (items, next_url) = match state
        .photo
        .list_pending_photos(user, after, DEFAULT_PAGE_LIMIT)
        .await
    {
        Ok(photos) => {
            let mut items = Vec::with_capacity(photos.len());
            for p in &photos {
                items.push(view::moderation_photo_vm(tr, &*state.storage, p).await);
            }
            // A full page (== the limit) may have more; a short page is the end.
            let next_url = (photos.len() as i64 == DEFAULT_PAGE_LIMIT)
                .then(|| photos.last())
                .flatten()
                .map(|last| {
                    format!(
                        "/moderation/photos?after_at={}&after_id={}",
                        urlencoding_rfc3339(last.created_at),
                        last.id
                    )
                });
            (items, next_url)
        }
        Err(_) => (Vec::new(), None),
    };
    render(
        ModerationPhotosPage {
            layout: PageLayout::for_request(
                tr.t("moderation.title").to_string(),
                "moderation",
                &auth,
                &state.map,
            ),
            tr,
            items,
            notice: moderation_notice(tr, &q),
            next_url,
        },
        StatusCode::OK,
    )
}

/// Parse a `{kind}` path segment into a [`PhotoKind`].
pub(crate) fn parse_photo_kind(s: &str) -> Option<PhotoKind> {
    PhotoKind::from_code(s)
}

/// The photo queue a moderation action returns a whole-document request to.
pub(crate) fn photo_queue_url(done: &str) -> String {
    format!("/moderation/photos?done={done}")
}

/// POST /moderation/photos/{kind}/{id}/approve (HTMX).
pub(crate) async fn moderation_photo_approve(
    State(state): State<AppState>,
    locale: Locale,
    auth: Auth,
    headers: HeaderMap,
    Path((kind, id)): Path<(String, i64)>,
) -> Response {
    let tr = Translator::new(locale);
    let user = match auth.require_moderator() {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let back = photo_queue_url("approved");
    let Some(kind) = parse_photo_kind(&kind) else {
        return photo_upload_result(
            &headers,
            &state.map,
            tr,
            "error",
            tr.t("moderation.invalid"),
            StatusCode::BAD_REQUEST,
            &back,
        );
    };
    let (name, message, status) = match state.photo.approve_photo(user, kind, id).await {
        Ok(()) => (
            "success",
            tr.t("moderation.approved").to_string(),
            StatusCode::OK,
        ),
        Err(e) => {
            let (status, message) = photo_error(tr, &e);
            ("error", message, status)
        }
    };
    photo_upload_result(&headers, &state.map, tr, name, &message, status, &back)
}

/// POST /moderation/photos/{kind}/{id}/reject (HTMX).
pub(crate) async fn moderation_photo_reject(
    State(state): State<AppState>,
    locale: Locale,
    auth: Auth,
    headers: HeaderMap,
    Path((kind, id)): Path<(String, i64)>,
    Form(form): Form<RejectReasonForm>,
) -> Response {
    let tr = Translator::new(locale);
    let user = match auth.require_moderator() {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let back = photo_queue_url("rejected");
    let Some(kind) = parse_photo_kind(&kind) else {
        return photo_upload_result(
            &headers,
            &state.map,
            tr,
            "error",
            tr.t("moderation.invalid"),
            StatusCode::BAD_REQUEST,
            &back,
        );
    };
    let (name, message, status) = match state.photo.reject_photo(user, kind, id, &form.reason).await
    {
        Ok(()) => (
            "success",
            tr.t("moderation.rejected").to_string(),
            StatusCode::OK,
        ),
        Err(e) => {
            let (status, message) = photo_error(tr, &e);
            ("error", message, status)
        }
    };
    photo_upload_result(&headers, &state.map, tr, name, &message, status, &back)
}

/// POST /moderation/photos/{kind}/{id}/hide (HTMX) — flips an approved photo to `HIDDEN`.
pub(crate) async fn moderation_photo_hide(
    State(state): State<AppState>,
    locale: Locale,
    auth: Auth,
    headers: HeaderMap,
    Path((kind, id)): Path<(String, i64)>,
) -> Response {
    let tr = Translator::new(locale);
    let user = match auth.require_moderator() {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let back = photo_queue_url("photo_hidden");
    let Some(kind) = parse_photo_kind(&kind) else {
        return moderation_result(
            &headers,
            &state.map,
            tr,
            "error",
            tr.t("moderation.invalid"),
            StatusCode::BAD_REQUEST,
            &back,
        );
    };
    let (name, message, status) = match state.moderation.hide_photo(user, kind, id).await {
        Ok(()) => (
            "success",
            tr.t("moderation.photo_hidden").to_string(),
            StatusCode::OK,
        ),
        Err(e) => {
            let (status, message) = moderation_error_message(tr, &e);
            ("error", message, status)
        }
    };
    moderation_result(&headers, &state.map, tr, name, &message, status, &back)
}

/// POST /moderation/photos/{kind}/{id}/restore (HTMX) — flips a hidden photo back to `APPROVED`.
pub(crate) async fn moderation_photo_restore(
    State(state): State<AppState>,
    locale: Locale,
    auth: Auth,
    headers: HeaderMap,
    Path((kind, id)): Path<(String, i64)>,
) -> Response {
    let tr = Translator::new(locale);
    let user = match auth.require_moderator() {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let back = photo_queue_url("photo_restored");
    let Some(kind) = parse_photo_kind(&kind) else {
        return moderation_result(
            &headers,
            &state.map,
            tr,
            "error",
            tr.t("moderation.invalid"),
            StatusCode::BAD_REQUEST,
            &back,
        );
    };
    let (name, message, status) = match state.moderation.restore_photo(user, kind, id).await {
        Ok(()) => (
            "success",
            tr.t("moderation.photo_restored").to_string(),
            StatusCode::OK,
        ),
        Err(e) => {
            let (status, message) = moderation_error_message(tr, &e);
            ("error", message, status)
        }
    };
    moderation_result(&headers, &state.map, tr, name, &message, status, &back)
}

pub(crate) fn photo_upload_result(
    headers: &HeaderMap,
    map: &MapConfig,
    tr: Translator,
    state: &'static str,
    message: &str,
    status: StatusCode,
    redirect_to: &str,
) -> Response {
    fragment_answer(headers, map, tr, status, message, redirect_to, || {
        render(
            crate::PhotoUploadResultVm {
                tr,
                state,
                message: message.to_string(),
            },
            status,
        )
    })
}

/// Map a [`PhotoError`] to a non-leaking status + friendly message.
pub(crate) fn photo_error(tr: Translator, e: &PhotoError) -> (StatusCode, String) {
    use PhotoError::*;
    let (status, key) = match e {
        NotVerified => (StatusCode::FORBIDDEN, "photo.error.not_verified"),
        RateLimited => (StatusCode::TOO_MANY_REQUESTS, "photo.error.rate_limited"),
        TooLarge => (StatusCode::BAD_REQUEST, "photo.error.too_large"),
        UnsupportedFormat => (StatusCode::BAD_REQUEST, "photo.error.unsupported"),
        Undecodable => (StatusCode::BAD_REQUEST, "photo.error.undecodable"),
        TooManyPixels => (StatusCode::BAD_REQUEST, "photo.error.too_many_pixels"),
        NotFound => (StatusCode::NOT_FOUND, "photo.error.not_found"),
        NotPending => (StatusCode::CONFLICT, "moderation.not_pending"),
        Unauthorized => (StatusCode::FORBIDDEN, "moderation.unauthorized"),
        InvalidField(_) => (StatusCode::BAD_REQUEST, "photo.error.invalid"),
        Storage(_) => (StatusCode::INTERNAL_SERVER_ERROR, "photo.error.internal"),
        Conflict => (StatusCode::CONFLICT, "error.conflict"),
        Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "error.unavailable"),
        Internal => (StatusCode::INTERNAL_SERVER_ERROR, "photo.error.internal"),
    };
    (status, tr.t(key).to_string())
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

    #[test]
    fn photo_conflict_is_409_and_unavailable_is_503() {
        let (status, message) = photo_error(en(), &PhotoError::Conflict);
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(message, en().t("error.conflict"));

        let (status, message) = photo_error(en(), &PhotoError::Unavailable);
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(message, en().t("error.unavailable"));
    }
}
