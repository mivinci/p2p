# SDK tests

All SDK test cases and fixtures live in this flat directory.

- `sdk.rs`: Cargo integration target using only the public `q2q` API.
- `client.rs`, `download_store.rs`, `transfer.rs`, `scheduler.rs`, `cdn.rs`, `reader.rs`, `online.rs`, `peer_identity.rs`, `punch_transport.rs`, `webrtc_transport.rs`, `signal_router.rs`, `upload.rs`, `upload_rate.rs`, `upload_progress.rs`, `upload_session.rs`, `admission.rs`, `source_file.rs`, `upload_source.rs`, `upload_cache.rs`, `query.rs`: unit test modules loaded from their source modules through `#[cfg(test)]` and `#[path]`, preserving access to private implementation details.
- `peer.rs`: authenticated local WebRTC pair fixture, compiled only for unit tests.
- `destination.rs`: private storage and fault-injection fixtures.
- `http.rs`: shared local HTTP fixture for unit and integration tests.

```sh
cargo test -p q2q
cargo test -p q2q --test sdk
cargo test -p q2q --lib
```

`autotests = false` prevents helper/unit files from becoming accidental integration targets. `sdk` is explicitly registered in Cargo.toml. Add public API cases to `sdk.rs`, or register another `[[test]]` target when adding a separate integration suite. The peer transfer tests are loaded by `peer_transfer.rs`; paced upload/accounting tests are loaded by `upload_session.rs`; the subprocess recovery test retains its original module path.

## Local Track/Punch regression

`local_upload.rs` is an explicit, ignored integration target. Start local Track/Punch with a memory store, a reachable Punch address and permissive local TLS settings. Use fresh Track and Punch instances between scenarios to reset their per-IP announce/join rate limits:

```sh
Q2Q_TEST_TRACK_URL=http://127.0.0.1:18086 cargo test -p q2q --test local_upload -- --ignored --nocapture
```

It covers concurrent handshakes and downloads under a shared 256 KiB/s upload budget, Busy admission and slot reuse, independent VOD seeks and idle VOD capacity, publication shutdown and republishing, source-change failure and withdrawal, publication snapshots, send accounting and final watch updates after close/Drop. It also verifies that low-level accepts cannot take over managed admission. No Redis or external deployment is required.


`local_sharing.rs` is another explicit ignored integration target. Run each case with freshly started local Track/Punch:

```sh
Q2Q_TEST_TRACK_URL=http://127.0.0.1:18086 cargo test -p q2q --test local_sharing three_nodes -- --ignored --nocapture
Q2Q_TEST_TRACK_URL=http://127.0.0.1:18086 cargo test -p q2q --test local_sharing multiple_contents -- --ignored --nocapture
Q2Q_TEST_TRACK_URL=http://127.0.0.1:18086 cargo test -p q2q --test local_sharing existing_vod -- --ignored --nocapture
Q2Q_TEST_TRACK_URL=http://127.0.0.1:18086 cargo test -p q2q --test local_sharing closing_sharing -- --ignored --nocapture
```

These cover A→B→C partial relay on one connection, partial/complete announcements, continued seeding after download completion, multiple content routes, duplicate-hash rejection, VOD sharing and cancellation, discovering a later partial source without reopening the reader, closing sharing independently of downloads, and ordinary transfer cancellation withdrawal. Unit tests cover committed-proof indexing and recovery, uncommitted-block exclusion, source tampering, Have updates and bounds, availability-aware exclusive leases and blocking-read cleanup.

`local_sharing` also includes `query_options_paginate_candidates_and_exclude_self`: run it with the `query_options` filter against fresh local Track/Punch to verify QueryOptions, fewer-than-requested responses, opaque pagination, self exclusion and withdrawal.


`upload_cache.rs` covers LRU byte accounting, cross-publication budgets/isolation, shared successful reads, zero/oversized budgets, source and proof-file changes, partial destinations, cancellation before/after blocking work and no late backfill after close. `upload_cache_bench.rs` is an ignored warm-filesystem microbenchmark loaded by lib.rs:

```sh
cargo test -p q2q source_read_workloads -- --ignored --nocapture
```

It compares the uncached read path with the managed cache/coalescing path for 128 requests from eight workers, using hotspot, distinct, deterministic seek and growing-destination workloads. Timings are informational; deterministic read-call counts and sharing/limit behavior are asserted. It does not measure physical disk reads, cold storage or network throughput.
