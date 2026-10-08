# B16b rendered-browser and execution baseline

Baseline checkpoint: `a8b6058`. This is lead validation, not independent approval.
All application traffic stayed on the ephemeral loopback Axum servers. Browser
routes reject requests to other origins. Database commands unset `DATABASE_URL`
and use only `bikesnest_test_audit_b15b_20260914` at `127.0.0.1:55439`.
The search harness owns a disposable child database; the CSRF harness uses a
rollback-scoped real router. No production asset or provider was changed.

## Real behavior

The CSRF journey now follows the actual logout redirect to the rendered home
page. The old manual-redirect workaround is removed. A second real tab retains
the old session form and receives a CSRF rejection after logout. The journey
also exercises boosted form/head divergence, explicit headers, bad-token
recovery without replay, history, first-cookie racing responses, registration,
login, and a JavaScript-disabled login form with its rendered token.

The existing real search journey covers repeated filters, sort/filter intent
composition, delayed older server responses, main and out-of-band state, clear
actions, history restoration, pagination, changed destinations, and native
JavaScript-disabled forms. It uses actual server responses, not invented result
HTML.

| Check | Result | Test execution | Warm Cargo preparation |
| --- | --- | --- | --- |
| Final CSRF browser | 1 passed | 4.42s | 0.28s |
| Search browser | 1 passed | 5.75s | 0.28s |
| HTTP, 8 threads, repeat 1 | 181 passed | 5.40s | 0.26s |
| HTTP, 8 threads, repeat 2 | 181 passed | 5.47s | 0.25s |
| HTTP, 8 threads, repeat 3 | 181 passed | 5.33s | 0.26s |

The separate warm HTTP `--no-run` command reported 0.38s. These are local
Rust/Cargo 1.98.1 debug-profile test measurements with an existing build cache,
not cold build costs, CI/pinned-1.95 evidence, request latency, or load-test
percentiles. The three execution samples span 5.33–5.47s (median 5.40s).
The preceding integrated B16a run passed 753 tests with 6 explicitly ignored;
its image-heavy dev-data test took 75.70s and is not a fast-lane test.

## Mutation sensitivity

`tests/browser/mutation.cjs` provides two explicit opt-in probes. It changes only
the script response delivered to the test browser, verifies the replacement
anchor occurs exactly once, and requires that the mutation was actually applied.
It never rewrites the repository asset or application server. Normal runs leave
all script responses unchanged.

| Browser-only mutation | Expected failing result observed |
| --- | --- |
| `csrf-stale-head`: prefer retained head metadata over current form token | Applied mutation; actual reset POST 403 versus required 303, 1.32s |
| `search-stale-response`: remove document-lifetime stale-response rejection | Applied mutation; committed type filters became only `rack` instead of `rack,indoor`, 3.56s |

Both probe commands exited 101 because their browser tests correctly failed.
Normal runs with the mutation variable unset then passed. These failures are
evidence that the regressions detect the targeted bugs, not product failures.

Run the ignored browsers with the normal explicit audit test environment:

```bash
cargo test -p bikesnest-web --test csrf_browser_test --locked -- --ignored --nocapture
cargo test -p bikesnest-web --test search_browser_test --locked -- --ignored --nocapture
```

For a deliberately failing probe, additionally set
`BIKESNEST_BROWSER_MUTANT=csrf-stale-head` for the CSRF harness, or
`BIKESNEST_BROWSER_MUTANT=search-stale-response` for search. Always unset it for
normal validation. These are not default CI success commands.

During test authoring, a fourth password-reset submission hit the real limit of
three per IP; the native case now uses login and asserts the exact invalid-
credentials field message, without weakening the limiter. Its initial broad
alert locator was narrowed to the specific email-field alert. Neither correction
changed application behavior.

Independent review, new approval/suspension/reset race evidence, and final
combined checks remain required before accepting B16b. Broader browser/release
matrix and external deployment evidence remain B19.

Independent Sol subsequently accepted this bounded browser slice with no
material findings: default CSRF passed in 4.62s, search in 5.52s; the applied
CSRF and search mutants failed at the same expected assertions in 1.18s and
2.97s. JavaScript syntax checks passed. This does not constitute full B16b
approval.

Lead browser-fixture rerun: `npm run test:browser` passed all 28 tests in
23.12s. The initial sandboxed attempt could not bind loopback (`EPERM`);
the permitted local-server rerun above is the validation result.

The explicit DB-free domain/application command also passed 182 tests with
both database URL variables unset (0.46s warm total command time). This is
not a cold-build measurement.
