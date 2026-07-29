//! The registry of management-API endpoints and which command reaches each.
//!
//! `tests/coverage.rs` reads this alongside a checked-in snapshot of
//! `/v1/openapi.json` and fails the build when the two disagree. That covers
//! the failure that actually matters — the API grows an endpoint and no
//! command can reach it — without generating the CLI from the spec, which
//! would cost the ergonomics that are the whole point of a CLI over `curl`.
//!
//! Adding a command and registering it here are the same edit, so the
//! registry cannot quietly fall behind the commands.

/// `(method, path, command)` for every endpoint a command reaches.
pub const COVERED: &[(&str, &str, &str)] = &[
    // auth
    ("POST", "/v1/login/device", "login"),
    ("POST", "/v1/login/device/token", "login"),
    ("POST", "/v1/logout", "logout"),
    ("GET", "/v1/tokens/verify", "auth status"),
    // products
    ("GET", "/v1/products", "products list"),
    ("POST", "/v1/products", "products create"),
    ("GET", "/v1/products/{productId}", "products get"),
    ("PATCH", "/v1/products/{productId}", "products update"),
    (
        "POST",
        "/v1/products/{productId}/free-tier",
        "products free-tier set",
    ),
    (
        "POST",
        "/v1/products/{productId}/test-mode",
        "products test-mode enable",
    ),
    (
        "DELETE",
        "/v1/products/{productId}/test-mode",
        "products test-mode disable",
    ),
    (
        "POST",
        "/v1/products/{productId}/integrations/{provider}",
        "products integrations set",
    ),
    // products key-types
    (
        "GET",
        "/v1/products/{productId}/key-types",
        "products key-types list",
    ),
    (
        "POST",
        "/v1/products/{productId}/key-types",
        "products key-types create",
    ),
    (
        "PATCH",
        "/v1/products/{productId}/key-types/{keyTypeId}",
        "products key-types update",
    ),
    (
        "DELETE",
        "/v1/products/{productId}/key-types/{keyTypeId}",
        "products key-types delete",
    ),
    (
        "POST",
        "/v1/products/{productId}/key-types/verify-prices",
        "products key-types verify-prices",
    ),
    // licenses
    ("GET", "/v1/licenses", "licenses list"),
    ("POST", "/v1/licenses", "licenses create"),
    ("GET", "/v1/licenses/{licenseId}", "licenses get"),
    ("POST", "/v1/licenses/{licenseId}/revoke", "licenses revoke"),
    ("POST", "/v1/licenses/{licenseId}/remint", "licenses remint"),
    (
        "POST",
        "/v1/licenses/{licenseId}/deactivate-device",
        "licenses deactivate-device",
    ),
    ("POST", "/v1/licenses/import", "licenses import"),
    ("GET", "/v1/licenses/export", "licenses export"),
];

/// Endpoints that are deliberately not commands, each with the reason.
///
/// An explicit list rather than a silent omission: a gap should be a decision
/// someone made, not something nobody noticed.
pub const NOT_COMMANDS: &[(&str, &str, &str)] = &[
    (
        "GET",
        "/v1/openapi.json",
        "the spec itself — read by this coverage test, not by a command",
    ),
    (
        "GET",
        "/v1/confirmations/{id}",
        "internal to the confirm-gated flow in confirmation.rs; polled, never invoked directly",
    ),
];

/// Endpoints whose command group has not been built yet.
///
/// Listed explicitly so the coverage test still fails on a *new* endpoint
/// appearing in the spec, and so the remaining work is visible in the test
/// output rather than being silently uncounted. This list must reach empty.
pub const PENDING: &[(&str, &str, &str)] = &[
    ("GET", "/v1/customers", "customers list"),
    ("POST", "/v1/customers", "customers create"),
    ("GET", "/v1/customers/{customerId}", "customers get"),
    (
        "PATCH",
        "/v1/customers/{customerId}/entitlements",
        "customers entitlements set",
    ),
    (
        "POST",
        "/v1/customers/{customerId}/link-license",
        "customers link-license",
    ),
    ("GET", "/v1/integrations", "integrations list"),
    (
        "GET",
        "/v1/integrations/stripe/status",
        "integrations status",
    ),
    (
        "POST",
        "/v1/integrations/stripe/connect",
        "integrations connect --provider stripe",
    ),
    (
        "POST",
        "/v1/integrations/{provider}/connect",
        "integrations connect",
    ),
    (
        "DELETE",
        "/v1/integrations/{provider}",
        "integrations disconnect",
    ),
    (
        "POST",
        "/v1/integrations/{provider}/rotate-secret",
        "integrations rotate-secret",
    ),
    ("GET", "/v1/sdk-key", "sdk-key get"),
    ("POST", "/v1/sdk-key/rotate", "sdk-key rotate"),
    ("GET", "/v1/webhook", "webhook get"),
    ("PUT", "/v1/webhook", "webhook set"),
    ("GET", "/v1/usage", "usage"),
    ("POST", "/v1/test-purchase", "test-purchase create"),
    ("GET", "/v1/test-purchase/{runId}", "test-purchase get"),
];
