//! Application-layer tests for `ResolveDestination`: the peek → charge →
//! geocode rule every billable entry point shares.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use bikesnest_application::{
    AddressSuggestion, DestinationError, GeoHit, GeocodeBudget, GeocodeError, Geocoder,
    RateLimitError, RateLimiter, ResolveDestination, SearchInput,
};
use bikesnest_domain::GeoPoint;

/// A provider that counts billable calls and holds one "cached" query.
#[derive(Default)]
struct FakeGeocoder {
    calls: AtomicUsize,
    cached: Option<&'static str>,
}

fn hit(label: &str) -> GeoHit {
    GeoHit {
        label: label.to_string(),
        point: GeoPoint::new(-25.4297, -49.2705).unwrap(),
    }
}

#[async_trait]
impl Geocoder for FakeGeocoder {
    async fn geocode(&self, query: &str) -> Result<Option<GeoHit>, GeocodeError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Some(hit(query)))
    }

    async fn suggest(
        &self,
        query: &str,
        _limit: usize,
        _session_token: Option<&str>,
        _language_code: &str,
    ) -> Result<Vec<AddressSuggestion>, GeocodeError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(vec![AddressSuggestion {
            label: query.to_string(),
            reference: None,
            point: None,
        }])
    }

    async fn resolve_suggestion(
        &self,
        reference: &str,
        _session_token: Option<&str>,
    ) -> Result<Option<GeoHit>, GeocodeError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Some(hit(reference)))
    }

    fn peek(&self, query: &str) -> Option<GeoHit> {
        (Some(query) == self.cached).then(|| hit(query))
    }
}

/// Allows `limit` hits per key; records every key it was asked about.
#[derive(Default)]
struct CountingLimiter {
    keys: Mutex<Vec<String>>,
    broken: bool,
}

#[async_trait]
impl RateLimiter for CountingLimiter {
    async fn check(
        &self,
        key: &str,
        limit: u32,
        _window: Duration,
    ) -> Result<bool, RateLimitError> {
        if self.broken {
            return Err(RateLimitError::Unavailable);
        }
        let mut keys = self.keys.lock().unwrap();
        keys.push(key.to_string());
        let used = keys.iter().filter(|k| *k == key).count();
        Ok(used <= limit as usize)
    }
}

const BUDGET: GeocodeBudget = GeocodeBudget {
    per_client: 1,
    window: Duration::from_secs(60),
};

fn use_case(
    cached: Option<&'static str>,
    broken: bool,
) -> (ResolveDestination, Arc<FakeGeocoder>, Arc<CountingLimiter>) {
    let geocoder = Arc::new(FakeGeocoder {
        calls: AtomicUsize::new(0),
        cached,
    });
    let limiter = Arc::new(CountingLimiter {
        keys: Mutex::new(Vec::new()),
        broken,
    });
    (
        ResolveDestination::new(geocoder.clone(), limiter.clone(), BUDGET),
        geocoder,
        limiter,
    )
}

fn search(query: Option<&str>, coords: bool) -> SearchInput {
    SearchInput {
        query: query.map(str::to_string),
        lat: coords.then_some(-25.0),
        lon: coords.then_some(-49.0),
        ..SearchInput::default()
    }
}

#[tokio::test]
async fn a_cached_answer_is_never_charged() {
    let (uc, geocoder, limiter) = use_case(Some("rua xv"), false);
    for _ in 0..3 {
        assert!(uc.geocode("1.2.3.4", "rua xv").await.unwrap().is_some());
        assert!(
            uc.admit_search("1.2.3.4", &search(Some("rua xv"), false))
                .await
        );
    }
    assert_eq!(geocoder.calls.load(Ordering::SeqCst), 0);
    assert!(limiter.keys.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_miss_is_charged_per_client_and_refused_once_spent() {
    let (uc, geocoder, limiter) = use_case(None, false);
    assert!(uc.geocode("1.2.3.4", "praça").await.unwrap().is_some());
    assert!(matches!(
        uc.geocode("1.2.3.4", "praça").await,
        Err(DestinationError::OverBudget)
    ));
    // Another client has its own budget.
    assert!(uc.geocode("5.6.7.8", "praça").await.unwrap().is_some());
    assert_eq!(geocoder.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        limiter.keys.lock().unwrap().first().map(String::as_str),
        Some("geocode:ip:1.2.3.4")
    );
}

#[tokio::test]
async fn searches_that_need_no_geocode_are_always_admitted() {
    let (uc, _geocoder, limiter) = use_case(None, false);
    assert!(uc.admit_search("ip", &search(Some("x"), true)).await);
    assert!(uc.admit_search("ip", &search(None, false)).await);
    assert!(uc.admit_search("ip", &search(Some("   "), false)).await);
    assert!(limiter.keys.lock().unwrap().is_empty());

    assert!(uc.admit_search("ip", &search(Some("x"), false)).await);
    assert!(!uc.admit_search("ip", &search(Some("x"), false)).await);
}

#[tokio::test]
async fn suggestions_and_their_resolution_are_always_charged() {
    let (uc, geocoder, _limiter) = use_case(Some("rua xv"), false);
    assert_eq!(
        uc.suggest("ip", "rua xv", 10, None, "en")
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(matches!(
        uc.resolve_suggestion("ip", "place-id", None).await,
        Err(DestinationError::OverBudget)
    ));
    assert_eq!(geocoder.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_limiter_error_counts_as_over_budget() {
    let (uc, geocoder, _limiter) = use_case(None, true);
    assert!(matches!(
        uc.geocode("ip", "anything").await,
        Err(DestinationError::OverBudget)
    ));
    assert!(
        !uc.admit_search("ip", &search(Some("anything"), false))
            .await
    );
    assert_eq!(geocoder.calls.load(Ordering::SeqCst), 0);
}
