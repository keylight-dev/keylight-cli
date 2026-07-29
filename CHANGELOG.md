# Changelog

All notable changes to the Keylight CLI. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[semver](https://semver.org/spec/v2.0.0.html).

While the CLI is `0.x`, flag names and human-readable output may change in a
minor release. The `--json` output and the exit codes are the parts to script
against.

## [Unreleased]

Nothing yet.

## [0.1.0] - 2026-07-29

First release. Every endpoint the Keylight management API exposes is reachable
from a command, and a coverage test fails the build if that stops being true.

### Added

**Authentication**

- `keylight login` — browser approval over
  [RFC 8628 device authorization](https://datatracker.ietf.org/doc/html/rfc8628),
  so it works over SSH and on headless machines. `--scope` narrows the request;
  `danger` is never requested by default.
- `keylight logout` — revokes the token server-side *and* deletes the local
  copy. If the revoke fails, the local copy is still removed and the output
  says so, rather than leaving a credential on disk you believe is gone.
- `keylight auth set-token` / `auth status` — token read from stdin, never
  argv. `KEYLIGHT_API_TOKEN` takes precedence over the stored file.

**Resources**

- `keylight products` — list, get, create, update, free tier, Stripe test mode,
  and per-provider price mapping.
- `keylight products key-types` — list, create, update, delete, verify-prices.
- `keylight licenses` — list, get, create, revoke, remint, deactivate-device,
  bulk import, CSV export.
- `keylight customers` — list, get, create, entitlements, link-license.
- `keylight integrations` — list, status, connect, disconnect, rotate-secret.
- `keylight webhook` — get and set, including clearing it.
- `keylight sdk-key` — get and rotate.
- `keylight usage` — activation and validation volume, with a summary.
- `keylight test-purchase` — create and get.

**Output**

- `--json` on every command, printing the raw API response.
- Aligned tables for lists, key/value blocks for single items, and empty
  results that name the command to run next.
- Progress output — approval URLs, waiting notices — always goes to stderr, so
  `--json` stdout stays parseable and `licenses export > out.csv` writes CSV.

### Behavior worth knowing

- **No command requires an interactive prompt.** Every input is available as a
  flag, so agents and CI can drive the whole surface.
- **Secrets are never flags.** `--*-env` flags name an *environment variable*
  to read from, because argv lands in shell history and is visible to other
  users via `ps`.
- **Pagination is explicit.** `list` commands take `--limit` and `--cursor` and
  print the next cursor; nothing auto-pages.
- **Confirm-gated operations block and poll.** Seven operations need a human to
  approve in a browser: `licenses revoke`, `licenses export`, `products
  key-types delete`, `integrations rotate-secret`, `sdk-key get`, `sdk-key
  rotate`, `webhook set`.
- **Mints are idempotent.** `licenses create` and `licenses import` always send
  an `Idempotency-Key`, so a retry after a timeout cannot issue a second
  license to the same customer.
- **`--duration-days 0` means perpetual**, which the API represents as `null`.
- **`licenses remint` prints the new plaintext key**, which the API returns
  exactly once.
- **Exit codes:** `0` success, `1` general failure, `2` not authenticated,
  `3` confirmation denied or expired.

### Known limitations

- **Prebuilt macOS binaries are not code-signed or notarized**, so Gatekeeper
  blocks them on first run. Clear the quarantine flag with
  `xattr -d com.apple.quarantine ./keylight`, or install with
  `cargo install keylight-cli`, which builds locally and avoids it.
- **Only read paths have been exercised against production.** Write and
  confirm-gated paths are covered by tests written against the API's documented
  request and response contracts, but have not been run live.
- No shell completions or man pages yet.

[Unreleased]: https://github.com/keylight-dev/keylight-cli/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/keylight-dev/keylight-cli/releases/tag/v0.1.0
