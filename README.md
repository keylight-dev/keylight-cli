<a href="https://keylight.dev">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/keylight-wordmark-white.svg">
    <img alt="Keylight" src="assets/keylight-wordmark-black.svg" width="176">
  </picture>
</a>

# Keylight CLI

[![crates.io](https://img.shields.io/crates/v/keylight-cli.svg)](https://crates.io/crates/keylight-cli)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![keylight.dev](https://img.shields.io/badge/keylight.dev-171717)](https://keylight.dev)

The open-source command-line client for the [Keylight](https://keylight.dev) management API —
think `wrangler`, but for your Keylight tenant: apps, key types, licenses, customers, and
payment integrations, from the terminal instead of the dashboard.

Every endpoint the API exposes ([OpenAPI spec](https://api.keylight.dev/v1/openapi.json)) is
reachable from a command, and a coverage test fails the build if that stops being true.

> **Status: 0.x.** Flag names and human-readable output may change in a minor release. The
> `--json` output and the exit codes are the parts to script against.

## Install

```bash
cargo install keylight-cli
```

Installs a `keylight` binary. Requires Rust 1.85+ (edition 2024).

Or download a prebuilt binary from [Releases](https://github.com/keylight-dev/keylight-cli/releases)
for macOS (Apple Silicon), Linux (x86_64), or Windows (x86_64).

> **macOS:** the prebuilt binaries are not yet code-signed or notarized, so Gatekeeper will
> block them on first run. Either clear the quarantine flag with
> `xattr -d com.apple.quarantine ./keylight`, or use `cargo install`, which builds locally and
> avoids the issue entirely.

## Quick start

```bash
keylight login                    # approve in your browser
keylight products list            # confirm it works
keylight licenses list --limit 10
```

## Authenticating

`keylight login` is the easy path. For CI and agents, set an environment variable instead.

### `keylight login`

```bash
keylight login
```

Prints a short code, opens your browser to approve it, and stores the token when you do. Uses
[RFC 8628 device authorization](https://datatracker.ietf.org/doc/html/rfc8628), so it works
over SSH and on headless machines — the browser never needs to reach the CLI.

Requests every non-destructive permission by default. Narrow it with `--scope`, repeated:

```bash
keylight login --scope licenses:read --scope usage:read
```

You can also uncheck permissions on the approval page. `danger` (destructive operations) is
never requested by default and must be opted into there.

**Check the code on the approval page matches the one in your terminal.** That match is what
proves the request came from the session you started, and it is the reason the flow is safe on
a machine you do not control.

### Token precedence

| Source | Notes |
|---|---|
| `KEYLIGHT_API_TOKEN` | Always wins. Nothing is read from or written to disk. Use this in CI. |
| Stored config file | Written by `keylight login` or `keylight auth set-token`. |

```bash
export KEYLIGHT_API_TOKEN=klm_...
keylight auth status
```

To store a token by hand:

```bash
keylight auth set-token                       # paste, then Enter
echo "$KEYLIGHT_TOKEN" | keylight auth set-token   # or pipe it
```

Read from **stdin**, never a command-line argument — arguments land in shell history and are
visible to other users via `ps`. Stored at your platform's config directory
(`~/.config/keylight/config.toml` on Linux, `~/Library/Application Support/keylight/config.toml`
on macOS) with `0600` permissions.

Create tokens from the dashboard under **Settings → API tokens**.

```bash
keylight auth status   # which tenant, token, and scopes the current auth resolves to
keylight logout        # revoke the token server-side and delete the local copy
```

`logout` revokes server-side and removes the local file. If the revoke call fails — offline, or
the token was already revoked — the local copy is still deleted and the output says so, rather
than leaving a credential on disk you believe is gone.

## Commands

### products

```bash
keylight products list
keylight products get my-app
keylight products create --display-name "My App" --key-prefix MYAP --support-email help@myapp.com
keylight products update my-app --purchase-url https://myapp.com/buy
keylight products free-tier set my-app --enabled
keylight products test-mode disable my-app
```

```
ID        NAME         PREFIX  FREE TIER  TEST MODE
my-app    My App       MYAP    true       false
other     Other App    OTHR    false      true
```

Enabling Stripe test mode needs two secrets, read from environment variables you name:

```bash
export KEYLIGHT_STRIPE_TEST_SECRET_KEY=sk_test_...
export KEYLIGHT_STRIPE_TEST_WEBHOOK_SECRET=whsec_...
keylight products test-mode enable my-app
```

### products key-types

```bash
keylight products key-types list --product my-app
keylight products key-types create --product my-app \
  --key-type-id pro --display-name "Pro" --activation-limit 3 --duration-days 365
keylight products key-types update pro --product my-app --activation-limit 5
keylight products key-types delete pro --product my-app     # needs browser approval
keylight products key-types verify-prices --product my-app
```

`--duration-days 0` means **perpetual**. Any other number is a real expiry in days.

### licenses

```bash
keylight licenses list --product my-app --status active --limit 20
keylight licenses get <license-id>
keylight licenses create --product my-app --key-type pro \
  --customer-email buyer@example.com --send-email
keylight licenses remint <license-id> --send-email
keylight licenses deactivate-device <license-id> --instance-id <instance>
keylight licenses revoke <license-id>                       # needs browser approval
```

```
KEY                        APP     TYPE  STATUS  DEVICES  EXPIRES
MYAP-****-****-****-7K2QP  my-app  pro   active  1/3      2027-06-18
```

Raw license keys are never returned by the API — lists and detail views show the masked form.
The one exception is `remint`, which returns the plaintext replacement key **once**; the command
prints it and says so.

`create` and `import` always send an `Idempotency-Key`, so a retry after a timeout cannot issue
a second license to the same customer. Pass `--idempotency-key` to control it yourself.

Bulk import takes a JSON file — either an array of rows, or an object with a `rows` array:

```bash
keylight licenses import --product my-app --file rows.json --migration-source gumroad
```

Export writes CSV to stdout so it composes, or to a file with `--output`:

```bash
keylight licenses export > licenses.csv     # needs browser approval
keylight licenses export --output licenses.csv
```

### customers

```bash
keylight customers list --limit 20
keylight customers get <customer-id>
keylight customers create --email buyer@example.com --name "Buyer"
keylight customers link-license <customer-id> --license <license-id>
keylight customers entitlements set <customer-id> \
  --license <license-id> --entitlements '["sync","export"]'
```

A customer id is a SHA-256 hash of their email, so `get` takes the id that `list` prints.

### integrations

```bash
keylight integrations list
keylight integrations status                       # Stripe connection detail
keylight integrations connect stripe --mode live   # returns an OAuth URL to open
keylight integrations disconnect paddle --environment sandbox
keylight integrations rotate-secret paddle         # needs browser approval
```

Non-Stripe providers take credentials from environment variables you name:

```bash
export PADDLE_KEY=pdl_live_...
keylight integrations connect paddle --environment live --api-key-env PADDLE_KEY
```

### webhook, sdk-key, usage, test-purchase

```bash
keylight webhook get
keylight webhook set --url https://example.com/keylight   # needs browser approval
keylight webhook set --url ""                             # clears it; delivery stops

keylight sdk-key get                                      # needs browser approval
keylight sdk-key rotate                                   # needs browser approval

keylight usage --days 30

keylight test-purchase create --product my-app --key-type pro
keylight test-purchase get <run-id>
```

`webhook set` returns the signing secret — use it to verify the signature on every delivery.
`webhook get` only reports whether a secret exists, never its value.

## `--json`

Every command supports `--json`, which prints the raw API response — that is the contract to
script against, and it is stable in a way the human output is not.

```bash
keylight --json licenses list --product my-app | jq '.items[].displayKey'
```

Errors follow the same rule: `--json` prints `{"error": "..."}` to stdout; otherwise the message
goes to stderr. Progress output (approval URLs, waiting notices) always goes to stderr, so
stdout stays parseable and redirects stay clean.

## Exit codes

| Code | Meaning |
|---|---|
| `0` | Success |
| `1` | Everything else |
| `2` | Not authenticated — no token, or the server rejected it. Run `keylight login`. |
| `3` | A confirmation was denied or expired |

`2` is distinct because "you need to authenticate" is the one failure an agent can act on
without a human.

## Confirm-gated operations

Some operations are dangerous enough that a bearer token alone cannot run them — a human has to
approve the specific call in a browser (same shape as npm's web-based 2FA). When a command hits
one, the CLI:

1. Prints an approval URL (and opens it, unless `--no-browser` or stdout is not a terminal).
2. Polls until you approve or deny it, or it expires after 10 minutes.
3. Retries the original call automatically once approved.

Seven operations are gated: `licenses revoke`, `licenses export`, `products key-types delete`,
`integrations rotate-secret`, `sdk-key get`, `sdk-key rotate`, and `webhook set`. You never have
to know which — the CLI walks the flow whenever the server asks.

These block and wait by design. They are human-gated operations, and failing fast for unattended
callers would misrepresent what they are; an agent can surface the URL to its user.

## Configuration

| Env var | Effect |
|---|---|
| `KEYLIGHT_API_TOKEN` | Bearer token; wins over the stored config file. |
| `KEYLIGHT_API_URL` | API base URL (default `https://api.keylight.dev`). For local dev and staging. |

Global flags: `--json`, `--no-browser` (never auto-open a browser; URLs are still printed),
`--api-url`.

## Design notes

**No command ever requires an interactive prompt.** Every input is available as a flag, so an
agent or CI job can drive the whole surface. Secrets are the one exception to "as a flag": they
are read from environment variables you name, because argv lands in shell history and is visible
to other users via `ps`.

**Pagination is explicit.** `list` commands take `--limit` and `--cursor` and print the next
cursor when one exists. Nothing auto-pages — one command means one request, and a silent loop
over a large tenant is both surprising and expensive.

**`--product` is required wherever the endpoint is product-scoped**, never inferred from config.
Inferring it would make the same command mean different things on different machines.

**Commands are hand-written, not generated from the OpenAPI spec.** Help text, flag names, output
formatting and error messages are the entire value over `curl`. Drift protection comes from the
coverage test instead, which catches the failure that actually matters — an endpoint no command
can reach.

## Development

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Tests mock the HTTP transport (no network needed) and isolate the config path, so `cargo test`
never reads or writes your real config.

The coverage test reads a checked-in snapshot of the API spec at `tests/fixtures/openapi.json`
rather than fetching it, so the suite is deterministic and works offline. Refresh it when the
API changes:

```bash
scripts/refresh-openapi.sh
```

Drift then shows up as a reviewable diff instead of a surprise failure, and the test reports any
endpoint that no command reaches.

## Related

- [`keylight-swift`](https://github.com/keylight-dev/keylight-swift) — Swift SDK
- [`keylight-rust`](https://github.com/keylight-dev/keylight-rust) — Rust SDK + Tauri plugin
- [`keylight-js`](https://github.com/keylight-dev/keylight-js) — TypeScript/JavaScript SDK
- [`keylight-csharp`](https://github.com/keylight-dev/keylight-csharp) — C#/.NET SDK

## License

Apache-2.0 — see [LICENSE](LICENSE).
