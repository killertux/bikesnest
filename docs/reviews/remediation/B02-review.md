# B02 independent review — CSRF / htmx lifecycle

Verdict: **PASS**

Reviewed the frozen uncommitted candidate relative to checkpoint `7c927e3`, including every changed source/template/test/CI file and the untracked real-browser harness. No production operations, external providers, migrations, dependency changes, synthetic-review changes, or application-code edits were performed.

## Findings

No remaining material findings.

### Resolved from the first review

1. `expired_and_revoked_session_cookies_get_stale_form_recovery` now creates real live sessions in the scoped SQL router, expires or revokes their rows, deliberately continues presenting each session cookie, and proves the old session CSRF receives 403, `reload-required`, localized stale-form copy, and a safe GET recovery link. This closes the gap that the browser logout journey alone could not cover.

2. `csrf_token_from_another_live_session_cannot_mutate` crosses two genuine authenticated session contexts, proves 403, then proves the targeted session remains live. This covers cross-session rejection and the absence of the attempted logout mutation.

3. `resolve_auth` now distinguishes `Err` from `Ok(None)`. A session-store error returns localized 503 with `private, no-store`, does not mint an anonymous cookie, does not attach CSRF recovery semantics, and emits only the token-free category `auth_failure="session_store_unavailable"`. `session_store_failure_is_unavailable_not_stale_recovery` forces a real SQL resolution failure via transaction-local `search_path` and verifies this behavior. Genuine expired/revoked/unknown sessions continue through the stale-session recovery path.

## Confirmed behavior

- Unsafe methods remain fail-closed. Header precedence is preserved, so a stale/invalid header is not rescued by a valid body/query token.
- Multipart bodies are not consumed when the header/query fallback supplies the token; a non-upload multipart request without a token is rejected. The earlier broad multipart deferral is absent.
- The htmx hook ignores safe methods, preserves the currently used exact-case explicit `X-CSRF-Token`, prefers the initiating form/current body token over stale head metadata, performs no token fetch, and does not replay a rejected mutation.
- The 403 hook cancels the swap, preserves live form input, and offers explicit reload/login actions. Native recovery uses a safe GET source/parent fallback rather than the POST action.
- Anonymous tokens are random, validated through `CsrfToken::verify`, reused from one unambiguous well-formed `__Host-csrf` cookie, and set only on successful anonymous HTML. Anonymous HTML is `private, no-store`; static/plain health and unsuccessful 404 responses do not receive a newly minted token cookie by the middleware.
- Authentication changes to the session CSRF token; logout removes the session cookie and anonymous re-entry reuses the pre-session anonymous context. `Auth.user` remains the identity authority, so making `Auth.csrf` present for anonymous requests does not create an auditing actor.
- No migration, config, dependency, production-data, fake-review, or rating change is present. The unrelated test fixture/comment renames are non-functional but add review noise.

## Independent checks

- `git diff --check 7c927e3` — passed.
- Focused corrected HTTP regressions — **3 passed**: expired/revoked cookies, cross-session token, and session-store failure classification.
- `cargo test -p bikesnest-web --test http_test --locked` with the dedicated disposable loopback PostGIS target — **170 passed, 1 unrelated existing test failed** (`review_create_updates_aggregate`, a fixture FK race under the parallel full suite). Immediate isolated rerun of that unchanged test passed. The three B02 correction tests passed both focused and full-suite runs; the implementer's independent full run reported **171 passed**.
- `cargo test -p bikesnest-web --test csrf_browser_test --locked -- --ignored` with the same target — **1 passed** in 2.77s. The manual-redirect logout boundary remains an acceptable scoped harness workaround; the new direct middleware regression independently proves the client-presented revoked-cookie case.
- `npm run test:browser` — **13 passed**. The first sandboxed attempt could not bind loopback (`EPERM`); the approved loopback run passed.

Overall confidence: **high**. Both first-review findings are corrected with direct real-middleware evidence, and the implementation satisfies the B02 gate. Browser coverage remains a representative journey rather than proof of every mutation route.
