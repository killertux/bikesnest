# B09b implementation handoff

Baseline: `3882b3a`. This batch is uncommitted and requires independent review.

## Outcome

- The known provider-compatible CSP remains enforced. A fresh cryptographic
  nonce is generated for every HTML response, applied to trusted script tags,
  and used by a nonce/`strict-dynamic` report-only candidate policy.
- Navigation and Google SDK loaders retain the original trusted document nonce.
  They do not copy a nonce from swapped manifests or accept request headers as
  nonce authority. Executable scripts in swapped responses are removed before
  installed htmx processes them; inert application/json map data remains.
  Manifest loading is restricted to the exact same-origin map application and
  vendor asset families, with no query, fragment or remote URL gadget.
- The candidate removes script `unsafe-inline`. Non-Google profiles are
  eval-free; Google alone retains the documented `unsafe-eval` compatibility
  exception. Inline styles remain allowed for map SDK and Alpine compatibility,
  so this batch does not claim style-injection prevention.
- Report-only is intentional pending live restricted-key Google SDK validation
  and an edge decision for the parser-inserted Cloudflare analytics beacon.
  Under `strict-dynamic`, its host allowlist is not a modern-browser fallback.

Primary references:

- [Google Maps CSP](https://developers.google.com/maps/documentation/javascript/content-security-policy)
- [Mapbox GL JS security/testing](https://docs.mapbox.com/mapbox-gl-js/guides/security-and-testing/)
- [htmx 4 CSP extension](https://four.htmx.org/extensions/hx-csp)

## Evidence and limits

- Security policy unit tests: **11 passed**.
- Complete local browser suite: **17 passed**. Under an enforcing copy of the
  candidate policy, trusted htmx/Alpine/navigation code and all provider loader
  lifecycles run; injected response script/event-handler and a remote manifest
  script do not. Actual vendored MapLibre and Mapbox runtimes each construct and
  reach `load` on a token-free local empty-style map, including their blob
  worker path.
- HTTP suite: **174 passed**. It verifies fresh per-response
  nonce/header/markup agreement, rejects a hostile nonce request header, and
  preserves the B02/B03 navigation and B09a cache behavior.
- Final gates: formatting, workspace all-target locked check, strict locked web
  Clippy, and `git diff --check 3882b3a` passed.

Google provider behavior is a loader stub plus official documentation only;
this does not constitute live restricted-key SDK/provider-console evidence.
No provider credential, console, production, asset build, deployment or release
action was used.
