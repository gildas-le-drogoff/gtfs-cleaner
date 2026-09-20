## What does this PR do?


## How was it tested?


## Checklist

- [ ] `cargo fmt` — no formatting issues
- [ ] `cargo clippy --all-targets -- -D warnings` — no warnings
- [ ] `cargo test` — all tests pass
- [ ] Tests assert concrete values (not just `is_ok()`)
- [ ] Run on a real GTFS feed (e.g. `make fetch && make run`) and the output zip is valid
