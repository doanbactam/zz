//! E2E test: `zz config` shows resolved config and supports feature/profile.

use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn e2e_config_show_default_provider() {
    // Run from a temp dir so the project's .env file (which sets
    // XAI_API_KEY) is not loaded by dotenvy::dotenv().
    let tmp = tempfile::tempdir().unwrap();
    Command::cargo_bin("zz")
        .unwrap()
        .current_dir(tmp.path())
        .env_remove("ZZ_PROVIDER")
        .env_remove("XAI_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("GEMINI_API_KEY")
        .env_remove("OPENROUTER_API_KEY")
        .env_remove("GROQ_API_KEY")
        .env_remove("DEEPSEEK_API_KEY")
        .env_remove("TOGETHER_API_KEY")
        .env_remove("FIREWORKS_API_KEY")
        .env_remove("MISTRAL_API_KEY")
        .args(["config", "show"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Provider:"))
        .stdout(predicate::str::contains("xai (default)"))
        .stdout(predicate::str::contains("XAI_API_KEY"))
        .stdout(predicate::str::contains("not set"));
}

#[test]
fn e2e_config_show_with_provider_env() {
    let tmp = tempfile::tempdir().unwrap();
    Command::cargo_bin("zz")
        .unwrap()
        .current_dir(tmp.path())
        .env("XAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "xai")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("GEMINI_API_KEY")
        .env_remove("OPENROUTER_API_KEY")
        .env_remove("GROQ_API_KEY")
        .env_remove("DEEPSEEK_API_KEY")
        .env_remove("TOGETHER_API_KEY")
        .env_remove("FIREWORKS_API_KEY")
        .env_remove("MISTRAL_API_KEY")
        .args(["config", "show"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Provider:"))
        .stdout(predicate::str::contains("xai"))
        .stdout(predicate::str::contains("Model:"))
        .stdout(predicate::str::contains("grok-4"))
        .stdout(predicate::str::contains("API Keys (env or auth.json):"))
        .stdout(predicate::str::contains("XAI_API_KEY"))
        .stdout(predicate::str::contains("set (env)"));
}

#[test]
fn e2e_config_feature_enable_disable() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();

    // enable a feature
    Command::cargo_bin("zz")
        .unwrap()
        .env("HOME", home)
        .args(["config", "feature", "telemetry", "enable"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Feature 'telemetry' enabled"));

    // verify it was written to the config file
    let cfg_path = home.join(".config").join("zerozero").join("config.toml");
    assert!(cfg_path.exists());
    let contents = std::fs::read_to_string(&cfg_path).unwrap();
    assert!(contents.contains("telemetry") && contents.contains("true"));

    // disable it
    Command::cargo_bin("zz")
        .unwrap()
        .env("HOME", home)
        .args(["config", "feature", "telemetry", "disable"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Feature 'telemetry' disabled"));

    let contents = std::fs::read_to_string(&cfg_path).unwrap();
    assert!(contents.contains("telemetry") && contents.contains("false"));
}

#[test]
fn e2e_config_use_profile() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();

    Command::cargo_bin("zz")
        .unwrap()
        .env("HOME", home)
        .args(["config", "use", "dev"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Activated profile 'dev'"));

    let cfg_path = home.join(".config").join("zerozero").join("config.toml");
    assert!(cfg_path.exists());
    let contents = std::fs::read_to_string(&cfg_path).unwrap();
    assert!(contents.contains("active_profile") && contents.contains("dev"));
}

#[test]
fn e2e_config_permissions_add_list_remove() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();

    // list with no rules -> friendly empty message
    Command::cargo_bin("zz")
        .unwrap()
        .env("HOME", home)
        .args(["config", "permissions", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("no permission rules"));

    // add a deny rule
    Command::cargo_bin("zz")
        .unwrap()
        .env("HOME", home)
        .args(["config", "permissions", "add", "Deny(Bash(rm -rf *))"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Added permission rule 'Deny(Bash(rm -rf *))'",
        ));

    // it must be persisted to config.toml
    let cfg_path = home.join(".config").join("zerozero").join("config.toml");
    assert!(cfg_path.exists());
    let contents = std::fs::read_to_string(&cfg_path).unwrap();
    assert!(contents.contains("Deny(Bash(rm -rf *))"));

    // list now shows it
    Command::cargo_bin("zz")
        .unwrap()
        .env("HOME", home)
        .args(["config", "permissions", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Deny(Bash(rm -rf *))"));

    // remove it
    Command::cargo_bin("zz")
        .unwrap()
        .env("HOME", home)
        .args(["config", "permissions", "remove", "Deny(Bash(rm -rf *))"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Removed permission rule 'Deny(Bash(rm -rf *))'",
        ));

    // back to empty
    Command::cargo_bin("zz")
        .unwrap()
        .env("HOME", home)
        .args(["config", "permissions", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("no permission rules"));
}
