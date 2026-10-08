# B09a correction handoff

Baseline: `513da0b`. This correction remains uncommitted and requires the same
independent reviewer to recheck it.

## Correction

- Removed the inert `hx-history="false"` attribute from the shared layout and
  browser fixture, plus the source-string assertion that treated it as a
  control.
- Corrected the architecture and implementation handoff: the pinned and shipped
  htmx 4.0.0 itself does not implement a localStorage history cache and performs
  network restoration. This is a version-dependent library property, not an
  application attribute.
- Retained the installed-library browser regression as an upgrade guard. It
  observes the back-navigation request, waits for restored page content, and
  verifies no `htmx-history-cache` localStorage entry appears.

No history feature, cleanup behavior, CSP work, migration, provider action, or
deployment action was added.

## Verification

- Complete browser suite: **14 passed**.
- Focused HTTP dynamic cache matrix: **1 passed**.
- Formatting, workspace all-target locked check, strict locked Clippy for the
  touched Rust crates, and `git diff --check 513da0b`: passed.
