use super::*;
use crate::i18n::{Locale, Translator};
use bikesnest_application::{Cursor, SearchPage, Sort};
use bikesnest_test_support::TestObjectStorage;

fn page_with_next() -> SearchPage {
    SearchPage {
        items: Vec::new(),
        total: 42,
        next_cursor: Some(Cursor {
            sort: Sort::Distance,
            v: 250.0,
            id: 7,
        }),
    }
}

async fn cursor_url_for(query_string: &str) -> Option<String> {
    let storage = TestObjectStorage::new();
    build_results(
        Translator::new(Locale::En),
        &page_with_next(),
        None,
        None,
        query_string.to_string(),
        chrono::Utc::now(),
        &bikesnest_domain::DEFAULT_THRESHOLDS,
        &storage,
    )
    .await
    .cursor_url
}

/// The query string arrives without a leading "?" — the next-page link has
/// to open the query itself, or the first parameter fuses onto the path.
#[tokio::test]
async fn next_page_url_keeps_the_current_query() {
    let url = cursor_url_for("q=rua&radius=1000")
        .await
        .expect("a next page exists");
    assert!(
        url.starts_with("/search?q=rua"),
        "query must open with '?': {url}"
    );
    assert!(url.contains("&radius=1000"), "filters are kept: {url}");
    assert!(url.contains("&cursor="), "cursor is appended: {url}");
}

#[tokio::test]
async fn next_page_url_without_a_query_still_opens_the_query_string() {
    let url = cursor_url_for("").await.expect("a next page exists");
    assert!(url.starts_with("/search?cursor="), "{url}");
}

#[tokio::test]
async fn no_next_cursor_means_no_next_page_link() {
    let storage = TestObjectStorage::new();
    let page = SearchPage {
        items: Vec::new(),
        total: 3,
        next_cursor: None,
    };
    let results = build_results(
        Translator::new(Locale::En),
        &page,
        None,
        None,
        "q=rua".to_string(),
        chrono::Utc::now(),
        &bikesnest_domain::DEFAULT_THRESHOLDS,
        &storage,
    )
    .await;
    assert!(results.cursor_url.is_none());
}

/// the search map's JSON island must carry only what `search.js`
/// reads for a marker — not the full `CardVm` (labels, image paths,
/// security chips, …).
#[test]
fn map_item_json_contains_only_the_allowed_keys() {
    let item = MapItemVm {
        id: 42,
        n: 1,
        lat: -25.43,
        lon: -49.27,
        name: "Paraciclo Rua XV".to_string(),
        distance_label: "300 m".to_string(),
        cost_label: "Free".to_string(),
        href: "/parking/42".to_string(),
    };
    let value = serde_json::to_value(&item).unwrap();
    let mut keys: Vec<&str> = value
        .as_object()
        .expect("MapItemVm serializes to a JSON object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "cost_label",
            "distance_label",
            "href",
            "id",
            "lat",
            "lon",
            "n",
            "name"
        ]
    );
}

#[test]
fn mask_email_keeps_the_domain_and_one_character() {
    assert_eq!(mask_email("clemente@brick.so"), "c***@brick.so");
    // A one-character local part masks the same way, so the mask never
    // leaks how long the hidden part is.
    assert_eq!(mask_email("a@example.com"), "a***@example.com");
    assert_eq!(mask_email("@example.com"), "***@example.com");
    // Not an address at all: reveal nothing rather than echo it back.
    assert_eq!(mask_email("not-an-email"), "***");
    assert_eq!(mask_email(""), "***");
}

#[test]
fn audit_targets_only_link_where_a_page_exists() {
    assert_eq!(
        audit_target_url("parking_location", "12"),
        Some("/parking/12".to_string())
    );
    assert_eq!(
        audit_target_url("user", "7"),
        Some("/admin/users?q=7".to_string())
    );
    assert_eq!(
        audit_target_url("report", "3"),
        Some("/moderation/reports".to_string())
    );
    // A non-numeric handle (a token, a session hash) must never be pasted
    // into a path.
    assert_eq!(audit_target_url("parking_location", "abc"), None);
    assert_eq!(audit_target_url("user", "1; DROP TABLE"), None);
    // No page for this type, and no empty-id links.
    assert_eq!(audit_target_url("session", "9"), None);
    assert_eq!(audit_target_url("user", ""), None);
}

#[test]
fn short_durations_pick_the_two_largest_units() {
    use super::admin::short_duration;
    assert_eq!(short_duration(-5), "0 s");
    assert_eq!(short_duration(45), "45 s");
    assert_eq!(short_duration(12 * 60 + 30), "12 min");
    assert_eq!(short_duration(3_600), "1 h");
    assert_eq!(short_duration(3 * 3_600 + 5 * 60), "3 h 5 min");
    assert_eq!(short_duration(86_400), "1 d");
    assert_eq!(short_duration(2 * 86_400 + 4 * 3_600 + 59), "2 d 4 h");
}

#[test]
fn job_health_lists_the_worst_job_first() {
    use bikesnest_application::{JobHealthReport, JobQueueSummary, RecurringJobStatus};
    let now = chrono::Utc::now();
    let row = |kind: &str, run_at: chrono::DateTime<chrono::Utc>| RecurringJobStatus {
        id: 1,
        kind: kind.into(),
        state: "pending".into(),
        schedule: serde_json::json!({"every_seconds": 86_400}),
        last_success_at: None,
        next_run_at: run_at,
        attempts: 0,
        max_attempts: 5,
        last_error: None,
        lease_expires_at: None,
    };
    let report = JobHealthReport {
        checked_at: now,
        recurring: vec![
            row("on.time", now + chrono::Duration::minutes(5)),
            row("behind", now - chrono::Duration::hours(2)),
        ],
        queue: JobQueueSummary::default(),
    };
    let vm = job_health_vm(Translator::new(Locale::En), &report);
    assert_eq!(vm.rows[0].kind, "behind");
    assert_eq!(vm.rows[0].health_code, "late");
    assert_eq!(vm.rows[0].lateness_label.as_deref(), Some("2 h overdue"));
    assert_eq!(vm.rows[1].schedule_label, "Every 1 d");
    assert!(!vm.all_healthy);
}
