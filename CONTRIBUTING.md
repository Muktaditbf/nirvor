# Contributing to nirvor

Thanks for helping make DNS in Bangladesh more resilient. Bug reports, operator feedback and pull requests are all welcome.

## Set up

You need Rust (stable, edition 2024) and Node.js 24 or newer.

```sh
(cd web && npm ci && npm run build)   # dashboard, embedded into the binary
cargo run -- -c config/config.toml    # DNS on 127.0.0.1:5300, dashboard on 127.0.0.1:8053
(cd web && npm run dev)               # optional: dashboard with hot reload, proxies /api to :8053
```

## Before opening a pull request

CI runs these and they must pass:

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all
(cd web && npm run build)
```

Also:

- **Follow the RFCs.** For DNS behaviour, cite the RFC section in a comment or the PR (1034/1035, 2308, 6891, 7766, 8767, 8806).
- **Never panic on network input.** No `unwrap()` or `expect()` on data from the network. New parsing code needs a test, and a property test if practical.
- **Don't guess performance.** Don't claim a speed-up you haven't measured. Run `bench/run.sh` and include the numbers.
- **Never make an open resolver.** Nothing may relax the default deny ACL.

## Good first contributions

- Bangla translations in `web/src/lib/i18n.ts`
- More OS connectivity-check paths in `crates/api/src/connectivity.rs`
- Docs: clearer operator guides, especially for small ISPs and BDIX setups

Open an issue before larger changes (new protocols such as DoT/DoH, DNSSEC validation, storage changes), so we can agree on the approach first.
