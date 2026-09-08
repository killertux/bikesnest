//! contributions to a location: adding one, editing one, and proposing a
//! change to one. The wire shape the three share — and the hours/security
//! grammars — live in `super::contribution_form`; this module is the handlers,
//! the form → domain mapping, and the pages they render.

use axum::extract::{Form, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use bikesnest_application::{ContributionError, NewParkingLocation, ParkingEdit, ProposalVote};
use bikesnest_domain::{
    Cost, CurrencyCode, GeoPoint, ModerationState, Money, ParkingLocation, ParkingType,
    PricingUnit, ProposalKind, ProposalPayload, ProposedChange,
};
use bikesnest_infrastructure::{FEATURED_ORIGIN, MapConfig};

use crate::auth::Auth;
use crate::client_ip::ClientIp;
use crate::i18n::{Locale, Translator};
use crate::state::AppState;
use crate::view;
use crate::{PageLayout, ParkingEditPage, ParkingNewConfirmPage, ParkingNewPage};

use super::common::render;
use super::contribution_form::{
    ContributionForm, DayFields, HoursError, hours_editor_vm, hours_fields_from, parse_hours,
    parse_security, security_editor_vm, security_fields_from,
};
use super::errors::not_found_page;
use super::moderation::non_empty;

pub(crate) fn parse_bool(s: &str) -> bool {
    s == "true" || s == "1" || s == "on"
}

/// Why a submission was refused. The hours editor reports per-day problems, so
/// its rejection has to survive as far as the row it belongs to rather than
/// collapsing into one "invalid field" banner.
#[derive(Debug)]
pub(crate) enum FormError {
    Contribution(ContributionError),
    Hours(HoursError),
}

impl From<ContributionError> for FormError {
    fn from(e: ContributionError) -> Self {
        FormError::Contribution(e)
    }
}

impl FormError {
    fn invalid(message: &str) -> Self {
        FormError::Contribution(ContributionError::InvalidField(message.to_string()))
    }

    /// The banner message, and the day whose row carries the field error.
    fn parts(&self, tr: Translator) -> (String, Option<HoursError>) {
        match self {
            FormError::Contribution(e) => (contribution_error_message(tr, e), None),
            FormError::Hours(h) => (tr.t(h.key).to_string(), Some(*h)),
        }
    }

    fn status(&self, default: StatusCode) -> StatusCode {
        match self {
            FormError::Contribution(e) => contribution_error_status(e, default),
            FormError::Hours(_) => default,
        }
    }

    /// Which input(s) this rejection belongs to, so the page
    /// can flag them directly instead of leaving the contributor to guess from
    /// one generic banner. The hours editor already reports its own per-day
    /// error, so it is deliberately not represented here.
    fn field_errors(&self, tr: Translator) -> view::FieldErrors {
        let mut out = view::FieldErrors::new();
        if let FormError::Contribution(ContributionError::InvalidField(raw)) = self {
            let message =
                contribution_error_message(tr, &ContributionError::InvalidField(raw.clone()));
            for field in invalid_field_names(raw) {
                out.push(field, message.clone());
            }
        }
        out
    }
}

/// `ContributionError::InvalidField` carries the raw validation message (the
/// application layer's own `"name is required"`, or a domain parse error's
/// `Display`), not a field tag — this is the one place that turns that text
/// back into the input name(s) the form should flag. New wording changes here
/// too; that coupling is the cost of not growing the application-layer error
/// into a field enum for what is, so far, a handful of known messages.
fn invalid_field_names(raw: &str) -> &'static [&'static str] {
    if raw.contains("name is required") {
        &["name"]
    } else if raw.contains("address is required") {
        &["address"]
    } else if raw.contains("latitude is required") {
        &["lat"]
    } else if raw.contains("longitude is required") {
        &["lon"]
    } else if raw.contains("coordinates out of range") {
        &["lat", "lon"]
    } else if raw.contains("currency") || raw.contains("pricing unit") {
        &["price"]
    } else {
        &[]
    }
}

/// Parse a price in major units ("5", "5.50", "5,50") into cents.
pub(crate) fn parse_price_major_to_cents(raw: &str) -> Option<i64> {
    let s = raw.trim().replace(',', ".");
    if s.is_empty() {
        return None;
    }
    let major: f64 = s.parse().ok()?;
    if major < 0.0 {
        return None;
    }
    Some((major * 100.0).round() as i64)
}

/// Render a cent amount as a major-units string for the price input (no floats).
pub(crate) fn cents_to_major_string(cents: i64) -> String {
    let major = cents / 100;
    let frac = (cents % 100).abs();
    if frac == 0 {
        major.to_string()
    } else {
        format!("{major}.{frac:02}")
    }
}

pub(crate) fn cost_from_form(form: &ContributionForm) -> Result<Cost, FormError> {
    match form.cost_kind.as_str() {
        "free" => Ok(Cost::Free),
        "paid" => {
            let price = match (
                parse_price_major_to_cents(&form.price),
                &form.price_currency,
                &form.price_unit,
            ) {
                (Some(cents), cur, unit) if !cur.is_empty() && !unit.is_empty() => {
                    let currency =
                        CurrencyCode::parse(cur).map_err(|e| FormError::invalid(&e.to_string()))?;
                    let unit = PricingUnit::from_code(unit)
                        .map_err(|e| FormError::invalid(&e.to_string()))?;
                    Some(Money::new(cents, currency, unit))
                }
                _ => None,
            };
            Ok(Cost::Paid { price })
        }
        _ => Ok(Cost::Unknown),
    }
}

pub(crate) fn new_location_from_form(
    form: &ContributionForm,
) -> Result<NewParkingLocation, FormError> {
    let parking_type = ParkingType::from_code(&form.parking_type)
        .map_err(|e| FormError::invalid(&e.to_string()))?;
    let cost = cost_from_form(form)?;
    let lat = form
        .lat
        .trim()
        .parse::<f64>()
        .map_err(|_| FormError::invalid("latitude is required"))?;
    let lon = form
        .lon
        .trim()
        .parse::<f64>()
        .map_err(|_| FormError::invalid("longitude is required"))?;
    let point = GeoPoint::new(lat, lon).map_err(|e| FormError::invalid(&e.to_string()))?;
    // Normally absent: the contribution service derives the zone from the
    // point through its `TimezoneResolver`. The "Advanced" override is still
    // validated here so a typo fails the form rather than the insert.
    let timezone = if form.timezone.trim().is_empty() {
        None
    } else {
        Some(
            form.timezone
                .trim()
                .parse()
                .map_err(|_| FormError::invalid("invalid timezone"))?,
        )
    };
    let hours = parse_hours(&form.hours_fields()).map_err(FormError::Hours)?;
    Ok(NewParkingLocation {
        name: form.name.clone(),
        address: form.address.clone(),
        description: non_empty(&form.description),
        parking_type,
        cost,
        point,
        timezone,
        hours,
        security: parse_security(&form.security_fields()),
    })
}

pub(crate) fn edit_from_form(form: &ContributionForm) -> Result<ParkingEdit, FormError> {
    let parking_type = ParkingType::from_code(&form.parking_type)
        .map_err(|e| FormError::invalid(&e.to_string()))?;
    Ok(ParkingEdit {
        name: form.name.clone(),
        address: form.address.clone(),
        description: non_empty(&form.description),
        parking_type,
        cost: cost_from_form(form)?,
        // The editor round-trips the stored schedule, so an untouched form
        // re-submits exactly what was there — no "preserve on match" guessing.
        hours: parse_hours(&form.hours_fields()).map_err(FormError::Hours)?,
        security: parse_security(&form.security_fields()),
    })
}

/// The form's `cost_kind` value for a location (used to pre-fill the edit form).
pub(crate) fn cost_kind_string(cost: &Cost) -> String {
    match cost {
        Cost::Free => "free",
        Cost::Paid { .. } => "paid",
        Cost::Unknown => "unknown",
    }
    .to_string()
}

/// `(major-price, currency, unit)` as form strings, for pre-filling a paid
/// price. The user types a human-readable amount ("R$ 5"); the backend stores
/// cents — so we pre-fill in major units, not cents.
pub(crate) fn cost_price_strings(cost: &Cost) -> (String, String, String) {
    match cost {
        Cost::Paid { price: Some(p) } => (
            cents_to_major_string(p.cents()),
            p.currency().as_str().to_string(),
            p.unit().as_code().to_string(),
        ),
        _ => (String::new(), String::new(), String::new()),
    }
}

/// The page chrome shared by every render of the "edit parking" form.
pub(crate) fn edit_parking_layout(map: &MapConfig, tr: Translator, auth: &Auth) -> PageLayout {
    PageLayout::for_request(tr.t("edit.title").to_string(), "edit", auth, map)
}

/// The page chrome shared by every render of the "add parking" form.
pub(crate) fn new_parking_layout(map: &MapConfig, tr: Translator, auth: &Auth) -> PageLayout {
    PageLayout::for_request(tr.t("new.title").to_string(), "new", auth, map)
}

/// Build a `ParkingEditPage` pre-filled from the published listing.
pub(crate) fn parking_edit_page_vm(
    layout: PageLayout,
    tr: Translator,
    id: i64,
    version: i64,
    loc: &ParkingLocation,
    notice: Option<String>,
    error: Option<String>,
) -> ParkingEditPage {
    let (price, price_currency, price_unit) = cost_price_strings(loc.cost());
    ParkingEditPage {
        layout,
        tr,
        id,
        version,
        name: loc.name().to_string(),
        address: loc.address().to_string(),
        description: loc.description().unwrap_or("").to_string(),
        parking_type: loc.parking_type().as_code().to_string(),
        cost_kind: cost_kind_string(loc.cost()),
        price,
        price_currency,
        price_unit,
        hours_days: hours_editor_vm(tr, &hours_fields_from(loc.hours()), None),
        security_states: security_editor_vm(tr, &security_fields_from(loc.security())),
        type_options: view::type_options(tr, Some(loc.parking_type().as_code())),
        lat: loc.point().lat(),
        lon: loc.point().lon(),
        error,
        // The two callers (a fresh GET, and re-rendering after a version
        // conflict) never reject one particular input — nothing to flag.
        field_errors: view::FieldErrors::new(),
        notice,
    }
}

pub(crate) async fn parking_new_page(
    State(state): State<AppState>,
    locale: Locale,
    auth: Auth,
) -> Response {
    let tr = Translator::new(locale);
    if let Err(resp) = auth.require_verified() {
        return resp;
    }
    render(
        new_page_vm(
            new_parking_layout(&state.map, tr, &auth),
            tr,
            &ContributionForm::default(),
            None,
            view::FieldErrors::new(),
            Vec::new(),
            None,
        ),
        StatusCode::OK,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn parking_new_post(
    State(state): State<AppState>,
    locale: Locale,
    ClientIp(ip): ClientIp,
    auth: Auth,
    Form(form): Form<ContributionForm>,
) -> Response {
    let tr = Translator::new(locale);
    let user = match auth.require_verified() {
        Ok(u) => u,
        Err(resp) => return resp,
    };

    let new = match new_location_from_form(&form) {
        Ok(n) => n,
        Err(e) => return render_form_error(&state.map, tr, auth, &form, e),
    };

    // Duplicate detection runs BEFORE anything is created: "you added spot
    // 8380 — by the way, it may already exist" is not a warning, it is a
    // duplicate. On candidates the contributor gets the interstitial and
    // decides; nothing has been written yet.
    let confirmed = parse_bool(&form.confirm);
    if !confirmed {
        match state
            .contributions
            .find_duplicates(user, new.point, &new.name)
            .await
        {
            Ok(candidates) if !candidates.is_empty() => {
                return render(
                    ParkingNewConfirmPage {
                        layout: new_parking_layout(&state.map, tr, &auth),
                        tr,
                        duplicates: candidates
                            .iter()
                            .map(|d| view::duplicate_vm(tr, d))
                            .collect(),
                        fields: form.hidden_fields(),
                    },
                    StatusCode::OK,
                );
            }
            Ok(_) => {}
            Err(ContributionError::NotVerified) => {
                return axum::response::Redirect::to("/account?verify=1").into_response();
            }
            // A pre-check that could not run must not block the contribution:
            // `add_parking_location` runs the same query as its safety net.
            Err(_) => {}
        }
    }

    match state
        .contributions
        .add_parking_location(user, &ip, new)
        .await
    {
        Ok(outcome) => {
            // The safety net: a spot created between the pre-check and the
            // insert still gets flagged, now as an advisory on a real row.
            let duplicates: Vec<view::DuplicateVm> = if confirmed {
                Vec::new()
            } else {
                outcome
                    .duplicates
                    .iter()
                    .map(|d| view::duplicate_vm(tr, d))
                    .collect()
            };
            if duplicates.is_empty() {
                axum::response::Redirect::to(&format!("/parking/{}?created=1", outcome.id))
                    .into_response()
            } else {
                render(
                    new_page_vm(
                        new_parking_layout(&state.map, tr, &auth),
                        tr,
                        &form,
                        None,
                        view::FieldErrors::new(),
                        duplicates,
                        Some(outcome.id),
                    ),
                    StatusCode::OK,
                )
            }
        }
        Err(ContributionError::NotVerified) => {
            axum::response::Redirect::to("/account?verify=1").into_response()
        }
        Err(ContributionError::RateLimited) => render(
            new_page_vm(
                new_parking_layout(&state.map, tr, &auth),
                tr,
                &form,
                Some((tr.t("contribution.error.rate_limited").to_string(), None)),
                view::FieldErrors::new(),
                Vec::new(),
                None,
            ),
            StatusCode::TOO_MANY_REQUESTS,
        ),
        Err(e) => render_form_error(&state.map, tr, auth, &form, e.into()),
    }
}

/// The add page, rendered from whatever the form currently holds. `error` is
/// the banner message plus (for an hours problem) the day whose row is wrong.
#[allow(clippy::too_many_arguments)]
pub(crate) fn new_page_vm(
    layout: PageLayout,
    tr: Translator,
    form: &ContributionForm,
    error: Option<(String, Option<HoursError>)>,
    field_errors: view::FieldErrors,
    duplicates: Vec<view::DuplicateVm>,
    added_id: Option<i64>,
) -> ParkingNewPage {
    let (message, hours_error) = match error {
        Some((message, hours_error)) => (Some(message), hours_error),
        None => (None, None),
    };
    let hours_fields: [DayFields; 7] = form.hours_fields();
    ParkingNewPage {
        layout,
        tr,
        name: form.name.clone(),
        address: form.address.clone(),
        description: form.description.clone(),
        parking_type: if form.parking_type.is_empty() {
            "rack".to_string()
        } else {
            form.parking_type.clone()
        },
        cost_kind: if form.cost_kind.is_empty() {
            "unknown".to_string()
        } else {
            form.cost_kind.clone()
        },
        price: form.price.clone(),
        price_currency: form.price_currency.clone(),
        price_unit: form.price_unit.clone(),
        lat: form.lat.clone(),
        lon: form.lon.clone(),
        timezone: form.timezone.clone(),
        default_lat: FEATURED_ORIGIN.0,
        default_lon: FEATURED_ORIGIN.1,
        hours_days: hours_editor_vm(tr, &hours_fields, hours_error),
        security_states: security_editor_vm(tr, &form.security_fields()),
        type_options: view::type_options(tr, Some(&form.parking_type)),
        error: message,
        field_errors,
        duplicates,
        added_id,
    }
}

pub(crate) fn render_form_error(
    map: &MapConfig,
    tr: Translator,
    auth: Auth,
    form: &ContributionForm,
    e: FormError,
) -> Response {
    let field_errors = e.field_errors(tr);
    render(
        new_page_vm(
            new_parking_layout(map, tr, &auth),
            tr,
            form,
            Some(e.parts(tr)),
            field_errors,
            Vec::new(),
            None,
        ),
        e.status(StatusCode::BAD_REQUEST),
    )
}

pub(crate) fn contribution_error_message(tr: Translator, e: &ContributionError) -> String {
    match e {
        ContributionError::NotVerified => tr.t("contribution.error.not_verified").to_string(),
        ContributionError::RateLimited => tr.t("contribution.error.rate_limited").to_string(),
        ContributionError::VersionConflict => {
            tr.t("contribution.error.version_conflict").to_string()
        }
        ContributionError::NotFound => tr.t("contribution.error.not_found").to_string(),
        ContributionError::LocationNotActive => tr.t("contribution.error.not_active").to_string(),
        ContributionError::InvalidField(_) => tr.t("contribution.error.invalid").to_string(),
        ContributionError::Unauthorized => tr.t("contribution.error.unauthorized").to_string(),
        ContributionError::Timezone => tr.t("contribution.error.timezone").to_string(),
        ContributionError::Conflict => tr.t("error.conflict").to_string(),
        ContributionError::Unavailable => tr.t("error.unavailable").to_string(),
        ContributionError::Internal => tr.t("contribution.error.internal").to_string(),
    }
}

/// Status for a re-rendered contribution form. The variants with a status of
/// their own say so here; every other variant keeps whatever the calling flow
/// already chose (a form re-render is a 400 or a 200).
pub(crate) fn contribution_error_status(e: &ContributionError, default: StatusCode) -> StatusCode {
    match e {
        ContributionError::Conflict => StatusCode::CONFLICT,
        ContributionError::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        ContributionError::LocationNotActive => StatusCode::NOT_FOUND,
        _ => default,
    }
}

pub(crate) async fn parking_edit_page(
    State(state): State<AppState>,
    locale: Locale,
    auth: Auth,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    let tr = Translator::new(locale);
    if let Err(resp) = auth.require_verified() {
        return resp;
    }
    let Some(view) = state.details.execute(id).await.ok().flatten() else {
        return not_found_page(&headers, &state.map, &auth, tr);
    };
    if view.location.moderation_state() != ModerationState::Active {
        return not_found_page(&headers, &state.map, &auth, tr);
    }
    let loc = &view.location;
    render(
        parking_edit_page_vm(
            edit_parking_layout(&state.map, tr, &auth),
            tr,
            id,
            loc.version(),
            loc,
            None,
            None,
        ),
        StatusCode::OK,
    )
}

pub(crate) async fn parking_edit_post(
    State(state): State<AppState>,
    locale: Locale,
    auth: Auth,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(form): Form<ContributionForm>,
) -> Response {
    let tr = Translator::new(locale);
    let user = match auth.require_verified() {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    // The 404 gate: a spot that is gone or hidden must not render an edit form
    // (the service re-checks inside its transaction). Its position comes back
    // with it, because a re-render still has to seed the move-proposal map.
    let Some(current) = state
        .details
        .execute(id)
        .await
        .ok()
        .flatten()
        .filter(|v| v.location.moderation_state() == ModerationState::Active)
    else {
        return not_found_page(&headers, &state.map, &auth, tr);
    };
    let point = *current.location.point();
    let edit = match edit_from_form(&form) {
        Ok(e) => e,
        Err(e) => return contribution_edit_error(&state.map, tr, auth, id, point, &form, e),
    };
    match state
        .contributions
        .apply_parking_edit(user, id, form.version, &edit)
        .await
    {
        Ok(_) => axum::response::Redirect::to(&format!("/parking/{id}?proposed=1&tab=approvals"))
            .into_response(),
        Err(ContributionError::VersionConflict) => {
            let Some(view) = state.details.execute(id).await.ok().flatten() else {
                return not_found_page(&headers, &state.map, &auth, tr);
            };
            let loc = view.location;
            render(
                parking_edit_page_vm(
                    edit_parking_layout(&state.map, tr, &auth),
                    tr,
                    id,
                    loc.version(),
                    &loc,
                    Some(tr.t("contribution.error.version_conflict").to_string()),
                    None,
                ),
                StatusCode::OK,
            )
        }
        Err(ContributionError::RateLimited) => contribution_edit_notice(
            &state.map,
            tr,
            auth,
            id,
            point,
            &form,
            tr.t("contribution.error.rate_limited").to_string(),
        ),
        Err(ContributionError::LocationNotActive) => {
            not_found_page(&headers, &state.map, &auth, tr)
        }
        Err(e) => contribution_edit_error(&state.map, tr, auth, id, point, &form, e.into()),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn contribution_edit_error(
    map: &MapConfig,
    tr: Translator,
    auth: Auth,
    id: i64,
    point: GeoPoint,
    form: &ContributionForm,
    e: FormError,
) -> Response {
    let field_errors = e.field_errors(tr);
    let (message, hours_error) = e.parts(tr);
    render(
        edit_page_vm(
            map,
            tr,
            &auth,
            id,
            point,
            form,
            hours_error,
            Some(message),
            field_errors,
            None,
        ),
        e.status(StatusCode::OK),
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn contribution_edit_notice(
    map: &MapConfig,
    tr: Translator,
    auth: Auth,
    id: i64,
    point: GeoPoint,
    form: &ContributionForm,
    notice: String,
) -> Response {
    render(
        edit_page_vm(
            map,
            tr,
            &auth,
            id,
            point,
            form,
            None,
            None,
            view::FieldErrors::new(),
            Some(notice),
        ),
        StatusCode::OK,
    )
}

/// The edit page rendered from a rejected submission (so nothing the
/// contributor typed is lost on the way back).
#[allow(clippy::too_many_arguments)]
pub(crate) fn edit_page_vm(
    map: &MapConfig,
    tr: Translator,
    auth: &Auth,
    id: i64,
    point: GeoPoint,
    form: &ContributionForm,
    hours_error: Option<HoursError>,
    error: Option<String>,
    field_errors: view::FieldErrors,
    notice: Option<String>,
) -> ParkingEditPage {
    ParkingEditPage {
        layout: edit_parking_layout(map, tr, auth),
        tr,
        id,
        version: form.version,
        name: form.name.clone(),
        address: form.address.clone(),
        description: form.description.clone(),
        parking_type: form.parking_type.clone(),
        cost_kind: form.cost_kind.clone(),
        price: form.price.clone(),
        price_currency: form.price_currency.clone(),
        price_unit: form.price_unit.clone(),
        hours_days: hours_editor_vm(tr, &form.hours_fields(), hours_error),
        security_states: security_editor_vm(tr, &form.security_fields()),
        type_options: view::type_options(tr, Some(&form.parking_type)),
        lat: point.lat(),
        lon: point.lon(),
        error,
        field_errors,
        notice,
    }
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct ProposalForm {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    lat: String,
    #[serde(default)]
    lon: String,
    #[serde(default)]
    timezone: String,
    #[serde(default)]
    existence: String,
    #[serde(default)]
    reason: String,
}

pub(crate) async fn parking_proposal_post(
    State(state): State<AppState>,
    locale: Locale,
    auth: Auth,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(form): Form<ProposalForm>,
) -> Response {
    let tr = Translator::new(locale);
    let user = match auth.require_verified() {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let kind = match ProposalKind::from_code(&form.kind) {
        Ok(k) => k,
        Err(_) => return axum::response::Redirect::to(&format!("/parking/{id}")).into_response(),
    };
    let change = match kind {
        ProposalKind::EditDetails => return proposal_error(id),
        ProposalKind::MoveLocation => {
            let Ok(lat) = form.lat.trim().parse::<f64>() else {
                return proposal_error(id);
            };
            let Ok(lon) = form.lon.trim().parse::<f64>() else {
                return proposal_error(id);
            };
            ProposedChange::MoveLocation {
                lat,
                lon,
                timezone: non_empty(&form.timezone),
            }
        }
        ProposalKind::ChangeExistence => match parse_proposed_existence(&form.existence) {
            Some(exists) => ProposedChange::ChangeExistence { exists },
            None => return proposal_error(id),
        },
    };
    if change == ProposedChange::Unknown {
        return proposal_error(id);
    }
    let proposed = ProposalPayload::new(change, Some(&form.reason)).to_json();
    match state
        .contributions
        .propose_location_change(user, id, kind, proposed)
        .await
    {
        Ok(_) => axum::response::Redirect::to(&format!("/parking/{id}?proposed=1&tab=approvals"))
            .into_response(),
        Err(ContributionError::LocationNotActive) => {
            not_found_page(&headers, &state.map, &auth, tr)
        }
        Err(_) => proposal_error(id),
    }
}

/// Read the existence radio on the "propose removal" form.
///
/// Two vocabularies reach this field. `removed`/`exists` are the payload's own
/// codes (what the moderation queue's approve form posts). `no_longer_exists`
/// and `info_changed` are the *verification* codes the edit page's radios have
/// posted since — they were stored verbatim and the queue, which only knew
/// `removed`, read `no_longer_exists` as "still exists", quietly inverting
/// every removal proposal a rider filed. Both vocabularies are mapped here, so
/// the stored payload is canonical whichever form submitted it.
pub(crate) fn parse_proposed_existence(raw: &str) -> Option<bool> {
    match raw.trim() {
        "removed" | "no_longer_exists" => Some(false),
        "exists" | "info_changed" | "still_exists" => Some(true),
        _ => None,
    }
}

/// The proposal forms live on the details/edit page; a rejected submission
/// returns there with an error flag rather than rendering a bare 400.
pub(crate) fn proposal_error(id: i64) -> Response {
    axum::response::Redirect::to(&format!("/parking/{id}?proposal_error=1")).into_response()
}

/// POST /parking/proposals/{proposal_id}/vote. The response never reveals a
/// voter list; clients return to the listing where only aggregate read models
/// may be rendered.
#[derive(Debug, serde::Deserialize)]
pub(crate) struct ProposalVoteForm {
    #[serde(default)]
    vote: String,
    #[serde(default)]
    location_id: i64,
}

pub(crate) async fn parking_proposal_vote_post(
    State(state): State<AppState>,
    auth: Auth,
    Path(proposal_id): Path<i64>,
    Form(form): Form<ProposalVoteForm>,
) -> Response {
    let user = match auth.require_verified() {
        Ok(user) => user,
        Err(response) => return response,
    };
    let Some(vote) = ProposalVote::from_code(&form.vote) else {
        return proposal_error(form.location_id);
    };
    match state
        .contributions
        .vote_on_proposal(user, proposal_id, vote)
        .await
    {
        Ok(totals) => {
            if totals.published
                && let Ok(Some(view)) = state.details.execute(form.location_id).await
                && view.location.moderation_state() != bikesnest_domain::ModerationState::Active
            {
                return axum::response::Redirect::to("/account/contributions").into_response();
            }
            axum::response::Redirect::to(&format!(
                "/parking/{}?voted=1&tab=approvals",
                form.location_id
            ))
            .into_response()
        }
        Err(_) => proposal_error(form.location_id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bikesnest_domain::{OpeningHours, SecurityState, TimeRange, hms};

    fn en() -> Translator {
        Translator::new(Locale::En)
    }

    #[test]
    fn contribution_conflict_is_409_and_unavailable_is_503() {
        assert_eq!(
            contribution_error_status(&ContributionError::Conflict, StatusCode::BAD_REQUEST),
            StatusCode::CONFLICT
        );
        assert_eq!(
            contribution_error_status(&ContributionError::Unavailable, StatusCode::OK),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            contribution_error_status(&ContributionError::NotFound, StatusCode::BAD_REQUEST),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            contribution_error_message(en(), &ContributionError::Conflict),
            en().t("error.conflict")
        );
        assert_eq!(
            contribution_error_message(en(), &ContributionError::Unavailable),
            en().t("error.unavailable")
        );
    }

    fn base_form() -> ContributionForm {
        ContributionForm {
            name: "Spot".to_string(),
            address: "Rua X, 1".to_string(),
            parking_type: "rack".to_string(),
            cost_kind: "unknown".to_string(),
            lat: "-25.43".to_string(),
            lon: "-49.27".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn a_form_with_no_hours_or_security_fields_still_parses() {
        let new = new_location_from_form(&base_form()).expect("the no-JS minimum is enough");
        assert_eq!(new.hours, OpeningHours::Unknown);
        assert!(new.security.is_empty());
        assert!(new.timezone.is_none(), "derived from the point server-side");
    }

    #[test]
    fn the_form_carries_weekly_hours_and_a_definitive_no() {
        let form = ContributionForm {
            h_mon_state: "ranges".to_string(),
            h_mon_1_open: "22:00".to_string(),
            h_mon_1_close: "02:00".to_string(),
            h_tue_state: "closed".to_string(),
            sec_cctv: "no".to_string(),
            sec_well_lit: "yes".to_string(),
            ..base_form()
        };
        let new = new_location_from_form(&form).unwrap();
        assert_eq!(
            new.hours,
            OpeningHours::weekly(vec![(1, TimeRange::new(hms(22, 0), hms(2, 0)))]),
            "the overnight range survives and Tuesday is closed (no row)"
        );
        let cctv = new
            .security
            .iter()
            .find(|f| f.code() == "cctv")
            .expect("cctv recorded");
        assert_eq!(cctv.state(), SecurityState::No);
    }

    #[test]
    fn an_overlapping_day_is_reported_against_that_day() {
        let form = ContributionForm {
            h_wed_state: "ranges".to_string(),
            h_wed_1_open: "09:00".to_string(),
            h_wed_1_close: "18:00".to_string(),
            h_wed_2_open: "17:00".to_string(),
            h_wed_2_close: "20:00".to_string(),
            ..base_form()
        };
        let err = new_location_from_form(&form).unwrap_err();
        let (message, hours) = err.parts(en());
        assert_eq!(message, en().t("form.hours.overlap"));
        assert_eq!(hours.expect("a day-level error").day, 3);
    }
}
