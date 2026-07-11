//! Library target for `zerozero-cli`.
//!
//! The `zz` binary lives in `main.rs`; this lib crate exposes the pure,
//! network-free helpers so they can be unit-tested via `cargo test --lib`
//! no network in tests).

pub mod output;

/// `zerozero-cli` library surface .
///
/// Exposes the pure, unit-testable pieces of the `zz config feature`
/// subcommand so they can be exercised without spawning the full binary:
///
/// * [`format_feature_list`] — render the `feature list` output from a config.
/// * [`apply_feature`] — validate + persist a feature enable/disable.
/// * [`validate_feature_name`] — reject unknown flag names.
use zerozero_core::{ZeroZeroConfig, check_supported_feature};

/// Label printed above the `feature list` entries.
pub const FEATURE_LIST_HEADER: &str = "Supported feature flags:";

/// Render the canonical `feature list` output for the given config:
/// a header line followed by every supported flag and its on/off state.
pub fn format_feature_list(config: &ZeroZeroConfig) -> String {
    let mut out = String::new();
    out.push_str(FEATURE_LIST_HEADER);
    out.push('\n');
    out.push_str(&config.format_feature_list());
    out
}

/// Validate a feature flag name against the supported set. Returns the
/// error from the core validator (which lists the supported flags) so that
/// `feature enable <unknown>` / `disable <unknown>` fail with a clear
/// message before touching the config file.
pub fn validate_feature_name(name: &str) -> anyhow::Result<()> {
    check_supported_feature(name)
}

/// Validate, set, and persist a feature flag in the central config file.
///
/// Validation runs *before* any filesystem write, so an unknown flag name
/// is rejected without mutating the config (see [`validate_feature_name`]).
/// Returns the human-facing confirmation message.
pub fn apply_feature(name: &str, enabled: bool) -> anyhow::Result<String> {
    validate_feature_name(name)?;
    let mut cfg = ZeroZeroConfig::load();
    cfg.set_feature(name, enabled);
    cfg.save()?;
    Ok(format!(
        "Feature '{}' {} (saved to config.toml).",
        name,
        if enabled { "enabled" } else { "disabled" }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_list_output_includes_known_flag() {
        let cfg = ZeroZeroConfig::default();
        let out = format_feature_list(&cfg);
        assert!(out.starts_with(FEATURE_LIST_HEADER));
        assert!(out.contains("image-composer"), "output was:\n{out}");
    }

    #[test]
    fn enabling_unknown_flag_errors() {
        // Validation must reject before any save/IO happens.
        let err = apply_feature("not-a-real-flag", true).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown feature flag 'not-a-real-flag'"));
        // The supported list is surfaced so the user can self-correct.
        assert!(msg.contains("image-composer"));
        assert!(msg.contains("mcp"));
    }

    #[test]
    fn validate_feature_name_accepts_known() {
        assert!(validate_feature_name("mcp").is_ok());
        assert!(validate_feature_name("compact-on-token-budget").is_ok());
    }
}
