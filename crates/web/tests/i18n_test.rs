//! i18n formatting unit tests: locale-aware money, distance, rating and date formatting.

use bikesnest_web::i18n::{Locale, Translator};
use bikesnest_web::view::{format_money, iso_datetime_label};

fn en() -> Translator {
    Translator::new(Locale::En)
}

fn pt() -> Translator {
    Translator::new(Locale::PtBr)
}

#[test]
fn money_uses_locale_decimal_separator() {
    assert_eq!(format_money(en(), 1234.56), "1234.56");
    // pt-BR swaps the decimal separator to a comma.
    assert_eq!(format_money(pt(), 1234.56), "1234,56");
}

#[test]
fn datetime_uses_locale_order() {
    let dt = chrono::DateTime::parse_from_rfc3339("2024-03-05T14:30:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    // en keeps ISO-ish ordering.
    assert_eq!(iso_datetime_label(en(), dt), "2024-03-05 14:30");
    // pt-BR uses day/month/year.
    assert_eq!(iso_datetime_label(pt(), dt), "05/03/2024 14:30");
}

#[test]
fn distances_use_locale_decimal_separator() {
    use bikesnest_web::view::distance_label;
    // Below a kilometre there is no decimal to localise.
    assert_eq!(distance_label(en(), 340.4), "340 m");
    assert_eq!(distance_label(pt(), 340.4), "340 m");
    assert_eq!(distance_label(en(), 1530.0), "1.5 km");
    assert_eq!(distance_label(pt(), 1530.0), "1,5 km");
}

#[test]
fn ratings_use_locale_decimal_separator() {
    use bikesnest_web::view::rating_label;
    assert_eq!(rating_label(en(), Some(4.5), 12), "4.5 (12)");
    assert_eq!(rating_label(pt(), Some(4.5), 12), "4,5 (12)");
    // No rating yet is a translated phrase, not a number.
    assert_ne!(rating_label(pt(), None, 0), rating_label(en(), None, 0));
}
