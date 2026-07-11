//! E2E test: `zz config set <key> <value>` persists a top-level setting to
//! the user config.toml and `config show` reflects it parity with
//! Codex `configure`).

use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn e2e_config_set_persists_model_and_provider() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();

    // Set the model.
    Command::cargo_bin("zz")
        .unwrap()
        .env("HOME", home)
        .args(["config", "set", "model", "grok-5"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Set model = 'grok-5'"));

    // Set the provider.
    Command::cargo_bin("zz")
        .unwrap()
        .env("HOME", home)
        .args(["config", "set", "provider", "anthropic"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Set provider = 'anthropic'"));

    // The config file must exist on disk at the expected path.
    let cfg_path = home.join(".config").join("zerozero").join("config.toml");
    assert!(cfg_path.exists(), "config.toml should be written");
    let contents = std::fs::read_to_string(&cfg_path).unwrap();
    assert!(contents.contains("model = \"grok-5\""), "got: {contents}");
    assert!(
        contents.contains("provider = \"anthropic\""),
        "got: {contents}"
    );

    // `config show` must reflect the persisted values.
    Command::cargo_bin("zz")
        .unwrap()
        .env("HOME", home)
        .env_remove("ZZ_MODEL")
        .env_remove("ZZ_PROVIDER")
        .args(["config", "show"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Model:"))
        .stdout(predicate::str::contains("grok-5"))
        .stdout(predicate::str::contains("Provider:"))
        .stdout(predicate::str::contains("anthropic"));
}

#[test]
fn e2e_config_set_rejects_unknown_key() {
    let tmp = tempfile::tempdir().unwrap();
    Command::cargo_bin("zz")
        .unwrap()
        .env("HOME", tmp.path())
        .args(["config", "set", "bogus", "x"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown config key 'bogus'"));
}
