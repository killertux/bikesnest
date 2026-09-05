# Testing

How BikesNest is tested and how to write a test.

## Running tests

```bash
cargo test                    # domain + application only (no database needed)
docker compose up -d db       # required once, before DB-backed tests
cargo test --workspace        # everything, including #[db_test] integration/HTTP tests
cargo test -p bikesnest-web    # a single crate
cargo test search_            # filter by name substring
```

`cargo build` itself needs **no database** (queries are runtime-checked), but
the DB-backed tests do — they connect to the compose database
(`TEST_DATABASE_URL`, falling back to `DATABASE_URL`).

Every `#[db_test]` installs a `tracing` subscriber (see "Tracing in tests"
below), so a repository's error-classification logging is visible with:

```bash
RUST_LOG=info cargo test -- --nocapture
```

## The four layers

| Layer | Where | Kind | Needs DB? |
|---|---|---|---|
| Domain | `crates/domain/src/*.rs`, inline `#[cfg(test)] mod tests` | pure unit tests (hours/DST, freshness, cost, security, scoring, confidence rules) | no |
| Application | `crates/application/tests/*.rs` | use-case tests with in-memory/fake ports | no |
| Infrastructure | `crates/infrastructure/tests/*.rs` | real-Postgres integration tests against `Sqlx*` repositories | yes |
| Web | `crates/web/tests/*.rs` | HTTP tests through the real router | yes |

## The `#[db_test]` harness

`#[db_test]` (from `bikesnest_test_support`) turns an `async fn` into a test that
runs against **real PostgreSQL** inside a transaction that is **automatically
rolled back** at the end. All `#[db_test]`s share one multi-threaded tokio
runtime, one connection pool, and one migration pass — so the suite is fast and
tests are isolated with zero cleanup code.

```rust
use bikesnest_test_support::db_test;

#[db_test]
async fn my_test(tx: &mut TestTx) {
    // tx.executor() runs SQL inside the test's transaction.
    sqlx::query("INSERT INTO ...").execute(tx.executor()).await.unwrap();
    // ...assert...
    // tx drops here → rollback; nothing persists.
}
```

Rules:

- The function must take exactly one parameter: `tx: &mut TestTx`.
- Use `tx.executor()` for any SQL that must be visible to code running inside
  the same transaction.
- **Never** `block_on` inside a `#[db_test]` body — await `pool()` instead.
- An open **savepoint** simulates an inner application transaction committing:
  `let mut sp = tx.savepoint().await;` … `sp.commit().await;` (or
  `sp.rollback().await`).

### Repository tests: inject the outer transaction (preferred)

Use `tx.db().await` **before any fixture queries**. It transfers ownership of the
test transaction to a cloneable, transaction-backed `Db`. Build fixtures using
`db.acquire()`, release the connection lease, then inject `db.clone()` into real
repositories. Their SQLx transactions become savepoints: repository commit means
`RELEASE SAVEPOINT`, not a commit visible outside the test.

```rust
#[db_test]
async fn repository_behavior(tx: &mut TestTx) {
    let db = tx.db().await;
    let mut conn = db.acquire().await.unwrap();
    let user = UserBuilder::new().create(&mut *conn).await.unwrap();
    drop(conn); // do not hold a lease while invoking a repository

    let accounts = SqlxAccountRepository::new(db.clone());
    accounts.set_public_contribution_name(user.id, true).await.unwrap();
    assert!(accounts.public_contribution_name(user.id).await.unwrap());
    // No commit_fixture(), fixture tags or cleanup DELETE statements.
}
```

The harness **awaits outer rollback on success and on panic**, then invalidates
all surviving `Db` clones. A failed repository transaction rolls back its own
savepoint without aborting the test's outer transaction. Scope tests run at
REPEATABLE READ so snapshot-export transactions can use a savepoint too;
PostgreSQL cannot change transaction isolation inside a savepoint.

Adapters must use `Db::acquire()` for reads and acquire a connection before
calling `conn.begin()` for writes. Do not call `Db::pool()` on an injected scope:
it deliberately fails rather than accidentally committing outside the test.
Account, review and export adapters support this now; migrate other adapters
before injecting them into a transaction-scoped HTTP router. Do not mix
`tx.executor()` / `tx.commit_fixture()` with `tx.db()` in one test.

One scoped `Db` means **one connection**, with exclusive leases. This tests real
SQL, constraints, rollback and repository commit behavior, but not independent
concurrent transactions, snapshot visibility between connections, or lock races.
Those tests need a dedicated isolated database and real connections; do not
serialize them on a scoped `Db` and claim concurrency coverage.

Regression examples: `infrastructure/tests/transaction_scope_test.rs` verifies
repository commit isolation, failed-savepoint recovery, outer rollback after
success/panic, and invalidation of surviving clones. `public_attribution_test.rs`
tests the real account/review/export adapters without committing fixtures.

### Legacy pooled tests: the committed-fixture pattern

Existing tests below predate transaction injection. Prefer the scoped pattern
above for new sequential repository tests and migrate these incrementally.

Read-model tests query through *other* pool connections, which cannot see the
uncommitted rows of the test transaction. For those, commit a **tagged**
fixture, assert against the real readers, then delete by tag. See
`crates/infrastructure/tests/parking_test.rs` for the canonical example:

```rust
const MARK: &str = "fix-within-radius";

#[db_test]
async fn within_radius_ordered_by_distance(tx: &mut TestTx) {
    cleanup_fixture(MARK).await;                       // delete leftover rows by seed_key
    ParkingBuilder::new()
        .with_fixture_tag(MARK)
        .at(lat, lon)
        .create(tx.executor()).await.unwrap();
    tx.commit_fixture().await;                         // commit, then start a fresh tx

    let page = real_search(&request).await.unwrap();   // reads via the pool
    assert!(/* ... */);

    cleanup_fixture(MARK).await;                       // leave no trace
}
```

`ParkingBuilder::with_fixture_tag(marker)` writes the marker into the
`seed_key` column; `cleanup_fixture` deletes by it. Give each test a unique tag
and a geographically separated origin so crashed runs can't cross-contaminate.

The canonical example of this pattern at scale is
`crates/infrastructure/tests/parking_test.rs::keyset_pagination_is_stable_across_inserts`:
it commits **25** fixture rows (tagged `fix-keyset`, spread along a line so
distance order is unambiguous), pages through the real search reader 5 rows at
a time via the keyset cursor, and asserts every one of the 25 ids is seen
exactly once across the pages before deleting the fixture by tag.

## Tracing in tests

`bikesnest_test_support::init_test_tracing()` installs a `tracing_subscriber::fmt()`
writer once per test binary (`OnceLock` + `try_init`, so calling it from many
tests is harmless), directed at the test harness's own captured output
(`with_test_writer()`) and filtered by `RUST_LOG` (default `warn`). `run_db_test`
calls it before every `#[db_test]`, so it is automatic — nothing to opt into.

Without this, `tracing::warn!`/`error!` lines a repository logs through
`bikesnest_infrastructure::classify_and_log` (the single place a `sqlx::Error`
becomes a `DbFailure`) had **no destination at all**: not suppressed, simply
dropped, because no subscriber was ever installed. That made a repository's
error classification unverifiable from the test suite. Now:

```bash
RUST_LOG=info cargo test -p bikesnest-infrastructure --test db_error_test -- --nocapture
```

prints a structured line per classified failure (`context`, `kind`, `sqlstate`,
`constraint`, `error`) — `RUST_LOG=warn` (the default) is enough for anything
`classify_and_log` actually logs today, since it only ever logs at `warn` or
`error`.

## Job-queue test isolation

`SqlxJobRepository::claim` claims across every job `kind` — the right thing in
production (one worker, every registered handler) but wrong for a test that
wants to control exactly which rows it can see: a large, unscoped batch claim
picks up *any* concurrently-running test's due rows just as readily as its own.

`SqlxJobRepository::claim_kinds(batch, worker_id, lease_ttl, kinds: &[&str])` is
the test-safe alternative — same claim, with `AND kind = ANY($kinds)` added to
the query. `claim` is unchanged (still delegates to the same SQL with no kind
filter), so production behavior is untouched.

Every job test that calls `claim`/`claim_kinds` directly (rather than
simulating a claim with a plain `UPDATE`, which most of
`crates/infrastructure/tests/job_test.rs` does) seeds its rows under a kind
unique to that test run —

```rust
let kind = format!("test.{}.{}.{}", module_path!(), "my_test_name", std::process::id());
```

— and claims through `claim_kinds(..., &[&kind])`. `concurrent_claims_are_disjoint`
is the test this fixes most visibly: with a kind-scoped claim, nothing else in
the suite can touch its 6 seeded rows, so its assertion is exact — *every*
seeded row is claimed by exactly `worker-a` or `worker-b`, not merely "claimed
by someone" — where before it had to tolerate a foreign claimer stealing a row
out from under it.

## CSP / rendered-asset consistency

`crates/web/tests/csp_test.rs` (`#[db_test]`) renders a representative page set
(`/`, `/search`, a parking page with a published photo, `/login`, `/about`, and
`/moderation/photos` as an admin), extracts every `<img src>`, `<script src>`,
and `<link rel="stylesheet"|"preconnect" href>` origin from each response body,
and asserts each is covered by that same response's own
`Content-Security-Policy` header — a relative URL by `'self'`, an absolute one
by its exact origin appearing in the matching directive (`img-src`/`script-src`/
`style-src`; a `preconnect` origin only has to appear *somewhere* in the CSP,
per the task this codifies).

`TestObjectStorage::presigned_get` (`crates/test-support/src/object_storage.rs`)
signs every URL under `bikesnest_infrastructure::TEST_MEDIA_ORIGIN`
(`http://media.test.invalid` — an RFC 2606 `.invalid` host, so it can never
resolve to anything real), and `Config::for_tests` puts that exact string in
`security.media_hosts` — one constant, so the two can never drift apart. This
means the rendered photo is a genuine *absolute-origin* URL, the same shape a
real S3/MinIO presigned URL has, not a same-origin `/media/...` placeholder —
the test additionally asserts (`assert_page_has_media_origin_img`) that the
moderation queue (while it still holds the test's own pending upload) and the
published parking page each render at least one `<img src>` at that origin, so
the CSP consistency check above is proven to be exercising a real absolute
origin, not trivially passing on `'self'` alone.

The test also separately asserts, independent of any one rendered page, that
every host in `test_config().security.media_hosts` appears in `img-src`.

**Mutation evidence** (empty `Config::for_tests`'s `security.media_hosts`,
`crates/infrastructure/src/config.rs`, then `cargo test -p bikesnest-web --test
csp_test`): the very first page checked (`/`, which renders a seeded featured
photo) now fails inside `assert_page_is_csp_consistent`, before the test even
reaches the parking/moderation pages:

```
/: asset http://media.test.invalid/seed/curitiba/hero-bike-parking.jpg?exp=...&sig=testsig
  (origin http://media.test.invalid) is not allowed by img-src:
  "'self' data: blob: https://demotiles.maplibre.org"
```

— i.e. the mutation is now caught by an ordinary rendered-page check, not only
by the dedicated `media_hosts`-list assertion at the end of the test (which
still also fails, redundantly). Reverting `media_hosts` makes the test pass
again with no other change.

## Hygiene / guard tests

A block of plain `#[test]`s (no database) at the bottom of `crates/web/tests/http_test.rs`
scans templates/CSS/JS on disk rather than exercising a route. Each protects a
specific regression:

| Test | Protects against |
|---|---|
| `no_error_colour_classes_remain_in_templates` | a `text\|bg\|border-error` utility surviving the `--color-error` → `--color-danger` rename (dead CSS, no matching class) |
| `no_undefined_tailwind_color_tokens_remain_in_templates` | the same class of bug, generalised to every colour-utility prefix (`ring`, `from`/`to`, `fill`, `stroke`, `placeholder`, `divide`, the `hover:`/`peer-checked:`/`focus-visible:` variants, …) against every `--color-*` token `web/static/css/input.css`'s `@theme` block actually defines |
| `base_layout_does_not_branch_on_csrf_presence` | the "signed in" check regressing onto CSRF-token presence (also true for anonymous auth pages) |
| `web_crate_never_reads_the_process_environment` | a stray `std::env::var` reintroducing per-request/second config path outside `Config` |
| `route_handlers_never_reach_for_infrastructure` | a handler reaching past `AppState`'s application ports for a concrete `Sqlx*`/pool/adapter |
| `no_web_source_file_is_longer_than_1200_lines` | the router regressing back into one giant module |
| `templates_reference_css_and_js_only_through_asset` | a literal `/static/...css\|js` path bypassing the content-hashed asset manifest |
| `no_focus_outline_none_remains_in_templates` | `focus:outline-none` silently killing the global `:focus-visible` ring |
| `no_tiny_text_xs_buttons_remain_in_templates` | a `text-xs` button under the WCAG 2.5.8 24×24px tap-target minimum |
| `every_button_has_visible_text_or_an_aria_label` | a button with no accessible name |
| `app_js_has_the_focus_trap_and_after_swap_focus_listener` | the dialog focus-trap/after-swap-focus JS regressing or moving |
| `no_hardcoded_english_sentences_in_static_js` | translatable UI copy hardcoded into static JS instead of read from a server-rendered, already-translated `data-*` attribute |

`no_undefined_tailwind_color_tokens_remain_in_templates`'s regex:

```
\b(text|bg|border|ring|outline|from|to|fill|stroke|placeholder|divide|hover:text|hover:bg|hover:border|peer-checked:bg|peer-checked:text|focus-visible:ring)-([a-z][a-z0-9-]*)(?:/\d+)?
```

Group 2 must be a token from `@theme` (e.g. `danger`, `accent-strong`), a
Tailwind colour keyword (`white`/`black`/`transparent`/`current`/`inherit` —
the numeric default palette is not allowlisted because nothing in this
codebase uses it), or one of a small, commented `NON_COLOR_SUFFIXES` list for
utilities that share a colour prefix without naming a colour (`text-sm`,
`border-t`, `divide-y`, `outline-none`, `border-dashed`, `bg-gradient-to-b`,
and the raw `stroke-width`/`stroke-linecap`/`stroke-linejoin` SVG attributes
this whole-file text scan also matches). A first offender's path and utility
are reported directly.

`no_hardcoded_english_sentences_in_static_js`'s heuristic is deliberately
narrow — a quoted, capitalised, exactly-two-word literal
(`"[A-Z][a-z]+ [a-z]+"`) — and does not even match `app.js`'s `/* "Copy to all
days" */` comment (four words, no quote right after the second). It does match
`search.js`'s `ds.labelDetails || "View details"`, allowlisted by (file,
exact text) with a comment: that string is the last-resort fallback for a
missing translated dataset attribute, not copy shown in the normal path.

## Builders and doubles

`bikesnest_test_support` provides domain-rich builders and fast fakes:

- **`UserBuilder`** — creates `users` rows and returns a domain `User`.
- **`ParkingBuilder`** — creates a full `parking_location` (hours, security
  tri-state, rating, moderation state, fixture tag, version). Chainable:
  `.with_cost(...)`, `.at(lat, lon)`, `.with_hours(1..=5, (6,0), (20,0))`,
  `.with_security("cctv", 1)`, `.verified_days_ago(3)`, …
- **`TestPasswordHasher`** — a non-cryptographic hash (prefix `test:`) so the
  web/HTTP suite never pays for argon2.
- **`TestObjectStorage`** — in-memory object storage double.
- **`pool()`** — the shared, migrated connection pool, for wiring routers.

For HTTP tests, inject doubles through the test constructor:

```rust
let db = Db::from_pool(pool().await);
let app = bikesnest_web::app_router_with(
    db,
    std::time::Duration::from_secs(2),
    Box::new(FakeEmailProvider::default()),       // or a capture double
    bikesnest_infrastructure::FakeOAuthProvider::default(),
    TestPasswordHasher,
    Box::new(bikesnest_infrastructure::InMemoryRateLimiter::default()),
    std::sync::Arc::new(TestObjectStorage::default()),
);
let res = app.oneshot(Request::builder().uri("/").body(Body::empty()).unwrap()).await.unwrap();
```

## Writing a new test, step by step

### 1. Domain rule (no DB)

Put it inline next to the code, or in the crate's tests. Pure `assert_eq!` on
domain values.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paid_cost_with_price_roundtrips() {
        let money = Money::new(1250, CurrencyCode::BRL, PricingUnit::PerDay).unwrap();
        match Cost::paid(money) {
            Cost::Paid { price: Some(p) } => assert_eq!(p.cents(), 1250),
            _ => panic!("expected paid"),
        }
    }
}
```

### 2. Application use case (in-memory ports, no DB)

Construct the service with hand-rolled or in-memory port impls and assert on
the returned domain result. Look at `crates/application/tests/*.rs` for existing
double patterns.

### 3. Repository / persistence (DB-backed)

Use `#[db_test]` + builders + `tx.executor()`:

```rust
#[db_test]
async fn user_builder_persists_a_user(tx: &mut TestTx) {
    let user = UserBuilder::new()
        .with_email("ada@example.com")
        .create(tx.executor()).await.unwrap();
    assert_eq!(user.email().to_string(), "ada@example.com");
    // rollback happens automatically
}
```

### 4. HTTP endpoint (DB-backed, through the real router)

Use `#[db_test]` + `pool()` + `app_router(_with)` + `tower::ServiceExt::oneshot`:

```rust
#[db_test]
async fn healthz_is_alive(_tx: &mut TestTx) {
    let app = bikesnest_web::app_router(Db::from_pool(pool().await), std::time::Duration::from_secs(2));
    let res = app.oneshot(
        Request::builder().uri("/healthz").body(axum::body::Body::empty()).unwrap()
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}
```

Pin the locale for deterministic copy with `Accept-Language: en` (the default
falls back to pt-BR).

## Naming and behavior

- Test names describe the *behavior*, not the method: `within_radius_ordered_by_distance`,
  `readyz_returns_ready_with_real_database`, `security_headers_present_on_public_page`.
- One assertion or one coherent scenario per test; prefer many small tests over
  one large one.
- Prefer real infrastructure over mocks at the persistence layer (`#[db_test]`
  against Postgres) and fakes only at the *external-provider* boundary (email,
  OAuth, storage, geocoding, rate limiter).
- Keep argon2 and ValKey out of the suite with `TestPasswordHasher` and the
  in-memory limiter, so tests stay fast and deterministic.

## Continuous integration

`.github/workflows/ci.yml` runs on every pull request and every push to `main`.
It exposes independent checks so a formatting or lint failure is visible
without waiting for the integration suite:

- **Format** runs `cargo fmt --all -- --check`.
- **Clippy** runs `cargo clippy --workspace --all-targets --locked -- -D warnings`.
- **Tests** runs `cargo test --workspace --locked` against PostgreSQL/PostGIS,
  ValKey, and MinIO. The provider smoke tests remain opt-in and skip when their
  external API keys are absent.
- **Frontend assets** rebuilds the vendored JavaScript/CSS and Tailwind output,
  then fails if the generated files differ from the committed copies.
- **Docker image** builds the production image and checks that startup rejects
  an incomplete production environment.

All Rust jobs use Rust 1.95.0, matching the production Docker builder. Workflow
concurrency cancels an older run when a PR receives another push.
