//! `keylight branding get|set` — the workspace customer identity.

use super::{Context, Deps, json_body, run_gated};
use crate::error::Result;
use crate::output::{self, emit};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum BrandingCommand {
    /// Show customer branding and available plan capabilities.
    Get,
    /// Partially update customer branding. Omitted values stay unchanged.
    Set {
        #[arg(long)]
        business_name: Option<String>,
        #[arg(long)]
        logo_url: Option<String>,
        /// Clear the saved logo URL.
        #[arg(long, conflicts_with = "logo_url")]
        clear_logo: bool,
        #[arg(long)]
        accent_color: Option<String>,
        #[arg(long)]
        support_email: Option<String>,
        /// Explicitly clear the saved support address.
        #[arg(long, conflicts_with = "support_email")]
        clear_support_email: bool,
        #[arg(long, conflicts_with = "hide_keylight_branding")]
        show_keylight_branding: bool,
        #[arg(long)]
        hide_keylight_branding: bool,
    },
}

pub fn run(ctx: &Context, deps: &Deps, cmd: BrandingCommand) -> Result<()> {
    let client = ctx.client(deps)?;
    match cmd {
        BrandingCommand::Get => {
            let value = run_gated(ctx, deps, &client, "GET", "/v1/branding", None)?;
            emit(ctx.json, &value, |v| {
                output::kv(&[
                    ("Business name", output::cell(v.pointer("/branding/displayName"))),
                    ("Logo", output::cell(v.pointer("/branding/logoUrl"))),
                    ("Accent colour", output::cell(v.pointer("/branding/accentColor"))),
                    ("Support email", output::cell(v.get("supportEmail"))),
                    ("Show Keylight", output::cell(v.get("showKeylightBranding"))),
                    ("Can configure branding", output::cell(v.pointer("/capabilities/canBrand"))),
                    ("Can remove attribution", output::cell(v.pointer("/capabilities/canHideAttribution"))),
                ]);
            });
            Ok(())
        }
        BrandingCommand::Set { business_name, logo_url, clear_logo, accent_color, support_email, clear_support_email, show_keylight_branding, hide_keylight_branding } => {
            let body = json_body(vec![
                ("displayName", business_name.map(Into::into)),
                ("logoUrl", if clear_logo { Some(serde_json::Value::Null) } else { logo_url.map(Into::into) }),
                ("accentColor", accent_color.map(Into::into)),
                ("supportEmail", if clear_support_email { Some(serde_json::Value::Null) } else { support_email.map(Into::into) }),
                ("showKeylightBranding", if show_keylight_branding { Some(serde_json::Value::Bool(true)) } else if hide_keylight_branding { Some(serde_json::Value::Bool(false)) } else { None }),
            ]);
            if body.as_object().is_some_and(|o| o.is_empty()) {
                return Err(crate::error::CliError::Config("pass at least one branding setting".into()));
            }
            let value = run_gated(ctx, deps, &client, "PATCH", "/v1/branding", Some(&body))?;
            emit(ctx.json, &value, |v| {
                output::kv(&[
                    ("Business name", output::cell(v.pointer("/branding/displayName"))),
                    ("Logo", output::cell(v.pointer("/branding/logoUrl"))),
                    ("Accent colour", output::cell(v.pointer("/branding/accentColor"))),
                    ("Support email", output::cell(v.get("supportEmail"))),
                    ("Show Keylight", output::cell(v.get("showKeylightBranding"))),
                ]);
            });
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::testing::{cleanup, ctx, deps, request_at, transport, RecordingOpener};
    use crate::test_support::{resp, ScriptedTransport};
    use std::sync::Arc;

    fn run_cmd(t: &Arc<ScriptedTransport>, cmd: BrandingCommand) -> Result<()> {
        let opener = RecordingOpener::new();
        let deps = deps(t, &opener);
        let ctx = ctx(true);
        let result = run(&ctx, &deps, cmd);
        cleanup(&ctx);
        result
    }

    #[test]
    fn get_reads_the_tenant_scoped_branding_endpoint() {
        let t = transport(vec![resp(200, r#"{"branding":{},"capabilities":{"canBrand":true}}"#, None)]);
        run_cmd(&t, BrandingCommand::Get).unwrap();
        let (method, url, _) = request_at(&t, 0);
        assert_eq!(method, "GET");
        assert!(url.ends_with("/v1/branding"));
    }

    #[test]
    fn set_sends_only_supplied_fields_and_preserves_support_address_when_omitted() {
        let t = transport(vec![resp(200, r#"{"branding":{"displayName":"Stemlin"}}"#, None)]);
        run_cmd(&t, BrandingCommand::Set {
            business_name: Some("Stemlin".into()), logo_url: None, clear_logo: false,
            accent_color: None, support_email: None, clear_support_email: false,
            show_keylight_branding: false, hide_keylight_branding: false,
        }).unwrap();
        let (method, url, body) = request_at(&t, 0);
        assert_eq!(method, "PATCH");
        assert!(url.ends_with("/v1/branding"));
        assert_eq!(serde_json::from_str::<serde_json::Value>(&body.unwrap()).unwrap(), serde_json::json!({"displayName":"Stemlin"}));
    }

    #[test]
    fn set_uses_explicit_null_to_clear_the_logo() {
        let t = transport(vec![resp(200, r#"{"branding":{"logoUrl":null}}"#, None)]);
        run_cmd(&t, BrandingCommand::Set {
            business_name: None, logo_url: None, clear_logo: true,
            accent_color: None, support_email: None, clear_support_email: false,
            show_keylight_branding: false, hide_keylight_branding: false,
        }).unwrap();
        let (_, _, body) = request_at(&t, 0);
        assert_eq!(serde_json::from_str::<serde_json::Value>(&body.unwrap()).unwrap(), serde_json::json!({"logoUrl":null}));
    }
}
