# CI test phases

The `DB-free tests` job intentionally measures test-binary compilation and test
execution as separate commands for `bikesnest-domain` and
`bikesnest-application`. It does not select the virtual workspace, so it needs
no database, object store, cache, browser, or service container.

In CI, `cargo test --no-run --locked` is the **compile phase**. The following
`cargo test --locked` is the **execution phase** in the same runner, after that
compile phase. GitHub Actions step durations are the recorded CI measurements.
The Rust cache may make the compile phase warm; it is not labeled or treated as
a cold build.

For a comparable local measurement, use a disposable target directory:

```bash
export CARGO_TARGET_DIR="$(mktemp -d)"
/usr/bin/time -f 'cold compile: %E elapsed, %M KiB peak' \
  cargo test -p bikesnest-domain -p bikesnest-application --no-run --locked
/usr/bin/time -f 'warm execution: %E elapsed, %M KiB peak' \
  cargo test -p bikesnest-domain -p bikesnest-application --locked
rm -rf "$CARGO_TARGET_DIR"
```

Here “cold compile” means only that temporary target directory had no project
artifacts; downloads and shared compiler caches can still affect it. “Warm
execution” means the immediately preceding compile in the same local target.
Neither measurement is a request-latency, load-test, production, or cross-runner
comparison. The full service-backed workspace lane and the browser lanes remain
separate CI gates.
