//! Community pages and fragments: add/edit, reviews, favorites, photos.

use crate::routes;
// The add/edit form's editors are view models like any other, but they live
// next to the grammar that parses them back (routes::contribution_form).
use crate::i18n::Translator;
use crate::routes::contribution_form::{
    HiddenField as ContributionHiddenField, HoursDayVm as ContributionHoursDayVm,
    TriStateVm as ContributionTriStateVm,
};
use crate::{PageLayout, view};
use askama::Template;

/// Add a parking location.
#[derive(Template)]
#[template(path = "pages/parking_new.html")]
pub struct ParkingNewPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub name: String,
    pub address: String,
    pub description: String,
    pub parking_type: String,
    pub cost_kind: String,
    pub price: String,
    pub price_currency: String,
    pub price_unit: String,
    pub lat: String,
    pub lon: String,
    pub timezone: String,
    /// Where the picker centres its map when the form carries no position yet.
    pub default_lat: f64,
    pub default_lon: f64,
    pub hours_days: Vec<ContributionHoursDayVm>,
    pub security_states: Vec<ContributionTriStateVm>,
    pub hours_open: bool,
    pub security_open: bool,
    pub advanced_open: bool,
    pub type_options: Vec<view::OptionVm>,
    pub error: Option<String>,
    /// Which input(s) a rejected submission belongs to.
    pub field_errors: view::FieldErrors,
    pub duplicates: Vec<view::DuplicateVm>,
    /// Set when the add succeeded but similar listings turned up anyway — the
    /// safety net behind the interstitial, not the normal path.
    pub added_id: Option<i64>,
}

/// The duplicate interstitial, rendered *before* anything is created.
#[derive(Template)]
#[template(path = "pages/parking_new_confirm.html")]
pub struct ParkingNewConfirmPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub duplicates: Vec<view::DuplicateVm>,
    /// The whole submission, re-posted verbatim by "create it anyway".
    pub fields: Vec<ContributionHiddenField>,
}

/// Edit a location (reversible fields).
#[derive(Template)]
#[template(path = "pages/parking_edit.html")]
pub struct ParkingEditPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub id: i64,
    pub version: i64,
    pub name: String,
    pub address: String,
    pub description: String,
    pub parking_type: String,
    pub cost_kind: String,
    pub price: String,
    pub price_currency: String,
    pub price_unit: String,
    pub hours_days: Vec<ContributionHoursDayVm>,
    pub security_states: Vec<ContributionTriStateVm>,
    pub hours_open: bool,
    pub security_open: bool,
    pub type_options: Vec<view::OptionVm>,
    /// The spot's current position. Not editable here — moving a pin is a
    /// reviewed proposal — but it seeds the map on the "move the pin" form.
    pub lat: f64,
    pub lon: f64,
    pub error: Option<String>,
    /// Which input(s) a rejected submission belongs to.
    pub field_errors: view::FieldErrors,
    pub notice: Option<String>,
    pub proposals: routes::contribution_form::ProposalFormsVm,
}

/// Write or edit a review.
#[derive(Template)]
#[template(path = "pages/review_form.html")]
pub struct ReviewFormPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub id: i64,
    pub rating: u8,
    pub body: String,
    pub error: Option<String>,
    /// Which input(s) a rejected submission belongs to.
    pub field_errors: view::FieldErrors,
}

/// The favorites list.
#[derive(Template)]
#[template(path = "pages/favorites.html")]
pub struct FavoritesPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub items: Vec<view::CardVm>,
    pub notice: Option<String>,
    /// "Load more" link when the page is full (a next keyset page exists).
    pub next_url: Option<String>,
}

/// The contribution history.
#[derive(Template)]
#[template(path = "pages/contributions.html")]
pub struct ContributionsPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub items: Vec<view::ContributionVm>,
    pub next_url: Option<String>,
}

/// HTMX fragment: the favorite button state.
#[derive(Template)]
#[template(path = "partials/favorite_button.html")]
pub struct FavoriteButtonVm {
    pub tr: Translator,
    pub id: i64,
    pub is_favorited: bool,
    pub csrf: String,
}

/// HTMX fragment: a short verification confirmation, or its error state.
#[derive(Template)]
#[template(path = "partials/verification_result.html")]
pub struct VerificationResultVm {
    pub tr: Translator,
    /// `"success"` or `"error"` — picks the confirmation or the alert styling.
    pub state: &'static str,
    pub label: String,
}

/// HTMX fragment: a bare translated error, for endpoints whose success
/// response is a control rather than a toast (the favorite button).
#[derive(Template)]
#[template(path = "partials/fragment_error.html")]
pub struct FragmentErrorVm {
    pub tr: Translator,
    pub message: String,
}

/// Photo moderation queue page.
#[derive(Template)]
#[template(path = "pages/moderation_photos.html")]
pub struct ModerationPhotosPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub items: Vec<view::ModerationPhotoVm>,
    pub notice: Option<String>,
    pub next_url: Option<String>,
}

/// HTMX fragment for a photo upload result (success or error).
#[derive(Template)]
#[template(path = "partials/photo_upload_result.html")]
pub struct PhotoUploadResultVm {
    pub tr: Translator,
    /// "success" | "error".
    pub state: &'static str,
    pub message: String,
}
