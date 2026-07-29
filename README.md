<a href="https://keylight.dev">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/keylight-wordmark-white.svg">
    <img alt="Keylight" src="assets/keylight-wordmark-black.svg" width="176">
  </picture>
</a>

# Keylight CLI

[![License: Apache 2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![keylight.dev](https://img.shields.io/badge/keylight.dev-171717)](https://keylight.dev)

The open-source command-line client for the [Keylight](https://keylight.dev) management API —
think `wrangler`, but for your Keylight tenant: apps, key types, licenses, and payment
integrations, from the terminal instead of the dashboard.

> **Status: alpha.** Every endpoint the management API exposes
> (`https://api.keylight.dev/v1` — see the public
> [OpenAPI spec](https://api.keylight.dev/v1/openapi.json)) is reachable from a command, and a
> coverage test fails the build if that stops being true. No release binaries yet, and flag
> names may still change — nothing here is a stability promise.

## Commands

| Group | What it does |
|---|---|
| `keylight login` / `logout` / `auth` | Browser login (RFC 8628 device flow), token storage |
| `keylight products` | Apps: list, create, update, free tier, Stripe test mode, per-provider prices |
| `keylight products key-types` | License key types on an app |
| `keylight licenses` | Issue, inspect, revoke, remint, deactivate a device, bulk import, CSV export |
| `keylight customers` | Customers, their entitlements, and license links |
| `keylight integrations` | Connect/disconnect payment providers, rotate webhook secrets |
| `keylight webhook` | The outbound webhook URL and event subscriptions |
| `keylight sdk-key` | Read or rotate the SDK key |
| `keylight usage` | Activation and validation volume |
| `keylight test-purchase` | Run and inspect an end-to-end test purchase |

Two rules hold across every command:

**No command ever requires an interactive prompt.** Every input is available as a flag, so an
agent or a CI job can drive the whole surface. Secrets are the one exception to "as a flag" —
they are read from environment variables you name (`--api-key-env`, `--secret-key-env`), because
argv lands in shell history and is visible to other users via `ps`.

**Pagination is explicit.** `list` commands take `--limit` and `--cursor` and print the next
cursor when there is one. Nothing auto-pages: one command should mean one request.

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

### `keylight login`

```bash
keylight login
```

Prints a short code, opens your browser to approve it, and stores the token when
you do. Uses [RFC 8628 device authorization](https://datatracker.ietf.org/doc/html/rfc8628),
so it works over SSH and on headless machines — the browser never needs to reach
the CLI.

Requests every non-destructive permission by default. Narrow it with `--scope`,
repeated:

```bash
keylight login --scope licenses:read --scope usage:read
```

You can also uncheck permissions on the approval page itself. `danger`
(destructive operations) is never requested by default and must be opted into
on that page.

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

Seven operations are gated: `licenses revoke`, `licenses export`, `products key-types delete`,
`integrations rotate-secret`, `sdk-key get`, `sdk-key rotate`, and `webhook set`. You never have
to know which — the CLI walks the flow whenever the server asks for it.

The approval notice goes to **stderr** in both output modes, so `--json` stdout stays valid JSON
and `keylight licenses export > licenses.csv` writes CSV to the file rather than an approval URL.

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

Apache-2.0 — see [LICENSE](LICENSE).
