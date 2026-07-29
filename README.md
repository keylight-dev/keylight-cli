# Keylight CLI

[![License: Apache 2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)

The open-source command-line client for the [Keylight](https://keylight.dev) management API —
think `wrangler`, but for your Keylight tenant: apps, key types, licenses, and payment
integrations, from the terminal instead of the dashboard.

> **Status: alpha.** The management API is live (`https://api.keylight.dev/v1`, 37 endpoints —
> see the public [OpenAPI spec](https://api.keylight.dev/v1/openapi.json)), but this CLI is
> still the **foundation only**: auth, the HTTP core, and the confirm-gated approval flow.
> Resource commands (`keylight apps`, `keylight licenses`, `keylight integrations`, ...) are
> not built yet. Expect the command tree to grow; nothing here is a stability promise.

## Install

No release binaries yet — build from source:

```bash
git clone https://github.com/keylight-dev/keylight-cli.git
cd keylight-cli
cargo install --path . --locked
```

This installs a `keylight` binary. Requires Rust 1.85+ (edition 2024).

## Authenticating

Two ways to give the CLI a token, in precedence order:

### 1. Environment variable (agents / CI)

```bash
export KEYLIGHT_API_TOKEN=klm_...
keylight auth status
```

Always wins when set — nothing is ever read from or written to disk.

### 2. Stored token (interactive)

```bash
keylight auth set-token
```

Reads the token from **stdin**, never a command-line argument — arguments land in shell
history and are visible to other users on the machine via `ps`. Paste it and press Enter, or
pipe it:

```bash
echo "$KEYLIGHT_TOKEN" | keylight auth set-token
```

The token is stored at your platform's config directory (e.g. `~/.config/keylight/config.toml`
on Linux, `~/Library/Application Support/keylight/config.toml` on macOS) with `0600`
permissions — owner read/write only.

Create a token from the dashboard: **Settings → API tokens**.

```bash
keylight auth status   # who am I: tenant, token name, scopes
keylight logout         # remove the local token (see note below)
```

`keylight logout` removes the local copy. The server also exposes `POST /v1/logout` for
self-revocation; wiring the two together is pending (see below). To be certain a compromised
token is dead, revoke it from the dashboard (Settings → API tokens).

### `keylight login` — not working yet

**Use `keylight auth set-token` today.**

The server side shipped, but as a different flow than this client was built against. Keylight
implements [RFC 8628 device authorization](https://datatracker.ietf.org/doc/html/rfc8628) —
`POST /v1/login/device` returns a short user code, you approve it in the browser at
`https://app.keylight.dev/login/device`, and the CLI polls `POST /v1/login/device/token` until
it's granted. That works anywhere, including headless boxes and over SSH.

The `login` command in this repo still implements the earlier *loopback callback* design (a
localhost listener waiting for a browser redirect), so it will hang and time out. Rewriting it
against the device flow is the next piece of work.

## `--json`

Every command supports `--json` for machine-readable output (the raw API response, or a small
structured summary for local-only commands like `logout`). Omit it for human-formatted text.
Errors follow the same rule: `--json` prints `{"error": "..."}` to stdout, otherwise the message
goes to stderr.

```bash
keylight --json auth status
```

## Confirm-gated operations

Some management API operations are dangerous enough that a bearer token alone can't run them —
revoking a license, rotating the SDK key, changing the webhook URL, and a few others require a
human to approve the specific call from the dashboard (same shape as `npm`'s web-based 2FA).
When a command hits one of these, the CLI will:

1. Print (and, unless `--no-browser` or you're not at an interactive terminal, open) an approval
   URL.
2. Poll until you approve or deny it in the browser (or it expires — 10 minutes).
3. Automatically retry the original call once approved.

This is built as reusable infrastructure now so every resource command that needs it (once
those commands exist) gets it for free.

## Configuration

| Env var | Effect |
|---|---|
| `KEYLIGHT_API_TOKEN` | Bearer token; wins over the stored config file. |
| `KEYLIGHT_API_URL` | Override the API base URL (default `https://api.keylight.dev`). Useful for local dev. |

`--no-browser` (global flag) suppresses automatic browser-opening for `login` and confirm-gated
approval URLs; the URL is always printed regardless.

## Development

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Tests mock the HTTP transport (no real network access needed) and isolate `HOME`/`XDG_CONFIG_HOME`
for anything that touches the config file — `cargo test` never reads or writes your real
`~/.config/keylight`.

## Related

- [`keylight-swift`](https://github.com/keylight-dev/keylight-swift) — Swift SDK
- [`keylight-rust`](https://github.com/keylight-dev/keylight-rust) — Rust SDK + Tauri plugin
- [`keylight-js`](https://github.com/keylight-dev/keylight-js) — TypeScript/JavaScript SDK

## License

MIT — see [LICENSE](LICENSE).
