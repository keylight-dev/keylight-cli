# Changelog

All notable changes to the Keylight CLI. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[semver](https://semver.org/spec/v2.0.0.html).

While the CLI is `0.x`, flag names and output formatting may change in a minor
release. The `--json` shape and the exit codes are the parts to script against.

## [0.1.0] - 2026-07-29

First release. Every endpoint the Keylight management API exposes is reachable
from a command, and a test fails the build if that stops being true.

### Added

- `login` / `logout` / `auth` — browser login over the RFC 8628 device flow,
  which works on headless boxes and over SSH. `logout` revokes the token
  server-side, not just locally.
- `products` and `products key-types` — apps, their key types, the free tier,
  Stripe test mode, and per-provider price mapping.
- `licenses` — issue, inspect, revoke, remint, deactivate a device, bulk
  import, and CSV export.
- `customers` — customers, their entitlements, and license links.
- `integrations` — connect and disconnect payment providers, rotate webhook
  secrets.
- `webhook`, `sdk-key`, `usage`, `test-purchase`.
- `--json` on every command for machine-readable output.

### Conventions

- **No command requires an interactive prompt.** Every input is available as a
  flag, so agents and CI can drive the whole surface.
- **Secrets are never flags.** `--*-env` flags name an *environment variable*
  to read, because argv lands in shell history and is visible to other users
  via `ps`.
- **Pagination is explicit.** `list` commands take `--limit` and `--cursor` and
  print the next cursor; nothing auto-pages.
- **Confirm-gated operations block and poll.** Seven operations need a human to
  approve in a browser; the CLI prints the URL, waits, and retries on approval.
  The approval notice goes to stderr in both modes, so `--json` stdout stays
  valid JSON and `licenses export > out.csv` writes CSV to the file.
- **Exit codes:** `0` success, `2` not authenticated, `3` confirmation denied
  or expired, `1` everything else.

### Known limitations

- Prebuilt macOS binaries are **not signed or notarized**, so Gatekeeper warns
  on first run. Build from source with `cargo install keylight-cli` to avoid it.
- Only read paths have been exercised against production. The write and
  confirm-gated paths are covered by tests against the API's documented
  contract, but have not been run live.

[0.1.0]: https://github.com/keylight-dev/keylight-cli/releases/tag/v0.1.0
