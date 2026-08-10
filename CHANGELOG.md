# Changelog

All notable changes to the Keylight CLI. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[semver](https://semver.org/spec/v2.0.0.html).

While the CLI is `0.x`, flag names and human-readable output may change in a
minor release. The `--json` output and the exit codes are the parts to script
against.

## [Unreleased]

## [0.2.1] - 2026-08-10

### Fixed
- Destructive commands (`licenses revoke`, `licenses remint`, `licenses export`,
  `products key-types delete`, `sdk-key`, `webhook`) no longer dead-end with a
  raw `403 Missing scope: danger` when your login lacks the destructive-actions
  permission. The CLI now re-authorizes for it in your browser (keeping every
  scope you already had), then continues into the normal per-action approval —
  two quick browser steps instead of an opaque error.

## [0.2.0] - 2026-08-10

### Added

- **`keylight init`.** Sets Keylight up in the current project and forks on
  integration style. It inspects the project (Tauri/Xcode/Cargo → SDK;
  Next.js/Node → API), recommends a mode, and walks the chosen path — an SDK
  project is never asked about the API and vice versa. The recommendation is
  advice, never a gate: `--mode` wins, and a browser/client project that
  chooses API is warned against embedding the token, not blocked.
- **API-mode credential wiring.** `init --mode api` mints a `licenses:runtime`
  token through the browser device flow (scoped to exactly that one
  permission), writes it to `.env` as `KEYLIGHT_API_TOKEN` (owner-only, added
  to `.gitignore` in a git repo), and prints a copy-ready `curl`. The token is
  the app's server-side secret and is never stored in the CLI's own config.
- **`keylight setup verify-api`.** The HTTPS-only counterpart to
  `verify-license` — no SDK. Mints a test license with your CLI login, then
  activates and validates it through the `/v1/licenses` runtime endpoints with
  the app's runtime token, prints the JSON, and cleans up. The runtime token is
  read from `KEYLIGHT_RUNTIME_TOKEN`, kept distinct from the management auth so
  the two credentials never shadow each other.
- **`keylight products create --integration-mode <sdk|api>`.** Creates an app
  in the chosen integration mode (default `sdk`), forwarding the field to the
  same endpoint the dashboard uses.

## [0.1.3] - 2026-08-02

### Added

- **`keylight setup verify-license`.** A one-time terminal check that mints a
  recognizable `Setup check` license, activates it through the real Rust SDK,
  and verifies the returned lease against the tenant's Ed25519 keyset. The SDK
  key is read from `KEYLIGHT_SDK_KEY`, never from an argument.
- **Safe cleanup and retry reuse.** Setup state is held in a separate
  owner-only local file until `--clean` deactivates the device and revokes the
  test license. Non-interactive runs never delete it implicitly.

### Changed

- **Setup input errors exit `2`.** Missing SDK keys and an ambiguous app
  selection are actionable setup failures; lease and activation failures still
  exit `1`, and confirmation denial or expiry remains `3`.

## [0.1.2] - 2026-07-31

First release after running every command against production rather than
against a test double. The command surface was already complete; what had never
been checked was whether the *output* was right. Three commands were printing
the wrong thing, and one of them was losing a value that cannot be recovered.

### Fixed

- **`licenses create` no longer discards the license key.** The API returns the
  plaintext key once, on mint, and never again. `remint` printed it; `create`
  printed only the masked form, so a key issued without `--json` or
  `--send-email` was gone the moment the command returned. Both mint paths now
  print it the same way.
- **`products key-types create` and `update` print the key type again.** Both
  read a `keyType` field that no route sends — the API wraps the object in
  `created` and `updated` — so every field rendered as `-` and the output looked
  like a failure. The write had always succeeded; only the display was wrong.
- **`test-purchase create` and `get` show the key.** They read `displayKey`,
  which is the *licenses* routes' name for it; the test-purchase routes send
  `licenseKeyMasked`. Every run printed `Key: -`, including completed ones.

### Changed

- **A usage error now exits `1` instead of `2`.** `2` is documented as "not
  authenticated, or the token lacks the required scope — re-run `keylight
  login`", but argument parsing exited `2` as well, so a mistyped flag was
  indistinguishable from an auth failure. An unattended caller following the
  documented table would log in, retry the same broken command, and loop. `2`
  now means only what it says. `--help` and `--version` still exit `0`.

  This aligns the implementation with the published exit-code table rather than
  changing that table. A script keying on observed behaviour rather than the
  documented meaning may need adjusting.

### Notes

Each fix is behind a small pure function with tests against real response
bodies captured from `api.keylight.dev`. The previous tests asserted the same
assumed shapes the code used, which is why all three renderer bugs passed a
green suite.

## [0.1.1] - 2026-07-30

Packaging only — no change to any command, flag, or output.

### Added

- **Homebrew.** `brew tap keylight-dev/tap && brew trust keylight-dev/tap &&
  brew install keylight` installs a prebuilt binary: no Rust toolchain, and
  nothing to clear past Gatekeeper. Bottles mean the install pours rather than
  building, so it never runs Homebrew's build-environment checks — which reject
  an Xcode older than the minimum for your macOS, an error about Xcode on a
  machine that only wanted a licensing CLI.
- **An Intel macOS binary** (`x86_64-apple-darwin`), cross-compiled from the
  arm64 runner. Previously Intel Macs had no published binary and fell through
  to a source build.

### Fixed

- Release builds now pass `--target` explicitly. Artifacts were named for a
  triple the build never specified — correct only while each runner happened to
  be that architecture, and wrong the moment two targets shared a runner, which
  is what adding Intel macOS does.
- The `products list` sample in the README showed `false` under FREE TIER and
  TEST MODE. The API omits those flags when they are off rather than sending
  `false`, so the real output is `-`.

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
