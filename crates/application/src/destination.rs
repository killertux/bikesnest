//! Use case: resolve a free-text destination within a per-client budget.
//!
//! Every web entry point that can make the geocoding provider bill us — the
//! search page, the address autocomplete, the suggestion resolver and the
//! picker's geocode fallback — follows the same rule: an answer the adapter
//! already holds is free and is never charged; anything else first spends one
//! unit of the caller's budget. That rule lives here once, over the
//! [`Geocoder`] and [`RateLimiter`] ports, so no handler repeats it.

use std::sync::Arc;
use std::time::Duration;

use crate::ports::{AddressSuggestion, GeoHit, GeocodeError, Geocoder, SearchInput};
use crate::rate_limit::RateLimiter;

/// How many billable geocodes one client may spend per window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeocodeBudget {
    pub per_client: u32,
    pub window: Duration,
}

#[derive(Debug, thiserror::Error)]
pub enum DestinationError {
    /// The client has spent its budget for the current window.
    #[error("geocode budget exhausted")]
    OverBudget,
    #[error(transparent)]
    Geocode(#[from] GeocodeError),
}

pub struct ResolveDestination {
    geocoder: Arc<dyn Geocoder>,
    limiter: Arc<dyn RateLimiter>,
    budget: GeocodeBudget,
}

impl ResolveDestination {
    pub fn new(
        geocoder: Arc<dyn Geocoder>,
        limiter: Arc<dyn RateLimiter>,
        budget: GeocodeBudget,
    ) -> Self {
        Self {
            geocoder,
            limiter,
            budget,
        }
    }

    /// May this search run? A search that needs no billable geocode always
    /// may: it carries coordinates (they win over the query), it has no query,
    /// or the adapter already holds the answer. Anything else spends budget.
    pub async fn admit_search(&self, client: &str, input: &SearchInput) -> bool {
        if input.lat.is_some() && input.lon.is_some() {
            return true;
        }
        match input.query.as_deref().map(str::trim) {
            Some(query) if !query.is_empty() && self.geocoder.peek(query).is_none() => {
                self.charge(client).await
            }
            _ => true,
        }
    }

    /// Resolve `query`: a held answer is returned free of charge, otherwise
    /// the call is charged first. `Ok(None)` when nothing matches.
    pub async fn geocode(
        &self,
        client: &str,
        query: &str,
    ) -> Result<Option<GeoHit>, DestinationError> {
        if let Some(hit) = self.geocoder.peek(query) {
            return Ok(Some(hit));
        }
        self.require_budget(client).await?;
        Ok(self.geocoder.geocode(query).await?)
    }

    /// Provider-ranked predictions for an incomplete query. Always charged:
    /// autocomplete answers are never cached.
    pub async fn suggest(
        &self,
        client: &str,
        query: &str,
        limit: usize,
        session_token: Option<&str>,
        language_code: &str,
    ) -> Result<Vec<AddressSuggestion>, DestinationError> {
        self.require_budget(client).await?;
        Ok(self
            .geocoder
            .suggest(query, limit, session_token, language_code)
            .await?)
    }

    /// Resolve the opaque reference of a chosen prediction. Always charged.
    pub async fn resolve_suggestion(
        &self,
        client: &str,
        reference: &str,
        session_token: Option<&str>,
    ) -> Result<Option<GeoHit>, DestinationError> {
        self.require_budget(client).await?;
        Ok(self
            .geocoder
            .resolve_suggestion(reference, session_token)
            .await?)
    }

    async fn require_budget(&self, client: &str) -> Result<(), DestinationError> {
        if self.charge(client).await {
            Ok(())
        } else {
            Err(DestinationError::OverBudget)
        }
    }

    /// Spend one unit of `client`'s budget. A limiter error counts as *over*
    /// budget: the limiter applies its own fail-open policy, so an error that
    /// reaches here means the operator asked to refuse rather than let calls
    /// through unmetered.
    async fn charge(&self, client: &str) -> bool {
        matches!(
            self.limiter
                .check(
                    &format!("geocode:ip:{client}"),
                    self.budget.per_client,
                    self.budget.window,
                )
                .await,
            Ok(true)
        )
    }
}
