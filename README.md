# Keylight CLI

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

The open-source command-line client for the [Keylight](https://keylight.dev) management API —
think `wrangler`, but for your Keylight tenant: apps, key types, licenses, and payment
integrations, from the terminal instead of the dashboard.

> **Status: alpha.** The management API this CLI talks to is still being built out
> (`https://api.keylight.dev/v1`). This release is the **foundation only** — auth, the HTTP
> core, and the confirm-gated approval flow. Resource commands (`keylight apps`, `keylight
> licenses`, `keylight integrations`, ...) land as the API surface ships. Expect the command
> tree to grow; nothing here is a stability promise yet.

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

`keylight logout` only removes the **local** copy. There's no server-side revocation endpoint
yet — revoke a compromised token from the dashboard (Settings → API tokens) as well.

### `keylight login` (browser flow) — not live yet

The design is a `wrangler login`-style flow: the CLI starts a localhost callback listener,
opens your browser to a dashboard grant page, and the page mints a scoped token and posts it
back to the CLI. **The dashboard grant page doesn't exist yet.** Running `keylight login` today
will wait for a callback that never arrives and time out with a message pointing you at `auth
set-token`. The client side is fully wired up — only the URL it opens needs to change once the
grant page ships.

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
