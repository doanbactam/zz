use assert_cmd::Command;

#[test]
fn e2e_prd6_ac13_sessions_list() {
    // Use a temp DB that we pre-populate.
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("test_sessions.db");

    // Pre-populate the session DB.
    let store = zerozero_session::SessionStore::open(&db_path).unwrap();
    store
        .create_session("test-session-1", "hello world", Some("gpt-4o-mini"))
        .unwrap();
    store
        .create_session("test-session-2", "fix the bug", None)
        .unwrap();
    drop(store);

    // Run `zz sessions list` with the pre-populated DB.
    let output = Command::cargo_bin("zz")
        .unwrap()
        .env("ZZ_SESSION_DB", db_path.to_str().unwrap())
        .arg("sessions")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let s = String::from_utf8(output).expect("stdout is valid utf-8");
    assert!(
        s.contains("test-session-1"),
        "should list test-session-1: {s}"
    );
    assert!(
        s.contains("test-session-2"),
        "should list test-session-2: {s}"
    );
    assert!(s.contains("hello world"), "should show prompt preview");
    assert!(s.contains("fix the bug"), "should show second prompt");
}

#[test]
fn e2e_prd6_ac13_sessions_list_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("empty_sessions.db");

    let output = Command::cargo_bin("zz")
        .unwrap()
        .env("ZZ_SESSION_DB", db_path.to_str().unwrap())
        .arg("sessions")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let s = String::from_utf8(output).expect("stdout is valid utf-8");
    assert!(
        s.contains("No sessions found"),
        "should show 'No sessions found': {s}"
    );
}
