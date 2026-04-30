# NetcodePlus Launcher

A small, signed, auditable update launcher for the NetcodePlus UT4
plugin and competitive pak channels.

> Status: pre-alpha. Not yet ready for general use.

## What it does

- Fetches an offline-signed update manifest from GitHub Pages.
- Verifies the manifest signature against a compiled-in Ed25519 public
  key before parsing any JSON.
- Checks each pak's SHA-256 against the signed manifest before
  installing.
- Detects the local UT4 install and atomically replaces paks.
- Launches UT4 once paks are up to date.

It does **not** distribute the UT4 base game itself. UT4 is distributed
by [ut4ever.org](https://ut4ever.org).

## Trust model

See [SECURITY.md](SECURITY.md). The short version: the only thing you
need to trust is the launcher binary itself (signed via SignPath
Foundation, pending approval). Everything the launcher fetches is
verified against a key that lives in the binary; GitHub, the CDN, and
TLS are all untrusted.

## Project layout

```
.
├── Cargo.toml             Cargo workspace root
├── crates/
│   ├── manifest/          Signed manifest schema + verification (no I/O)
│   └── planner/           Pure update planning (no I/O)
├── src-tauri/             Tauri 2.x desktop shell (Rust backend)
├── src/                   Frontend (vanilla TypeScript)
├── index.html
└── package.json
```

The pure-logic crates (`ncp-manifest`, `ncp-planner`) deliberately have
no filesystem or network dependencies, so they can be unit-tested in
isolation without mocks.

## Build prerequisites

- [Rust](https://rustup.rs) stable (>= 1.85)
- [Node.js LTS](https://nodejs.org) (>= 24.x)
- Tauri 2.x platform deps:
  - **Linux:** see <https://tauri.app/start/prerequisites/>
  - **macOS:** Xcode CLI tools
  - **Windows:** WebView2 (preinstalled on Windows 11) and the MSVC
    build tools

## Build

```sh
npm install
npm run tauri dev      # development
npm run tauri build    # production bundle
```

The pure-logic crates can be built and tested without Tauri or Node:

```sh
cargo test -p ncp-manifest -p ncp-planner
```

## Contributing

1. Install [pre-commit](https://pre-commit.com):
   `pip install pre-commit && pre-commit install`
2. Run `cargo fmt --all` and
   `cargo clippy --workspace --all-targets -- -D warnings` before
   pushing.
3. One logical change per commit; conventional commit prefixes
   (`feat:`, `fix:`, `chore:`, `test:`, `docs:`).

## License

Apache License 2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
