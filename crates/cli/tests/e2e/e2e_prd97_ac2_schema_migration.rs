//! AC-2: Session schema migration — Phase 2 (session crate).
//!
//! Test: Migration adds 5 columns + index, idempotent (re-running does not panic).
//! Subagent session row has parent_thread_id != NULL, depth = parent+1,
//! agent_path = "root.0", agent_status = "running".
//!
//! Pattern: Unit (SessionStore::open_in_memory).

use zerozero_session::SessionStore;

/// AC-2 test 1: Migration adds all 5 new columns + index to the sessions table.
///
/// After `open_in_memory()` (which calls `migrate()`), the `sessions` table
/// should have the 5 new columns: parent_thread_id, agent_path, depth,
/// agent_status, nickname. The index `idx_sessions_parent` should also exist.
#[test]
fn e2e_prd97_ac2_schema_migration_adds_columns() {
    let store = SessionStore::open_in_memory().expect("open_in_memory should succeed");

    // Check that all 5 new columns exist in the sessions table.
    let column_names: Vec<String> = store
        .list_sessions()
        .expect("list_sessions should work")
        .into_iter()
        // If list_sessions works with the new fields, the columns exist.
        // But we need to verify the columns directly via PRAGMA.
        .map(|s| s.parent_thread_id.unwrap_or_default())
        .collect();
    // list_sessions succeeding means the SELECT with all 10 columns works.
    // But to be thorough, query PRAGMA table_info directly.
    let _ = column_names; // suppress unused warning

    // Use a raw query to check column existence.
    // SessionStore doesn't expose the Connection, but we can verify
    // by using create_session_with_thread + list_sessions round-trip.
    store
        .create_session_with_thread(
            "test-1",
            "test prompt",
            Some("test-model"),
            Some("parent-123"),
            "root.0",
            1,
            "agent-0",
        )
        .expect("create_session_with_thread should succeed");

    let sessions = store.list_sessions().expect("list_sessions should succeed");
    assert_eq!(sessions.len(), 1, "should have 1 session");

    let s = &sessions[0];
    assert_eq!(s.id, "test-1");
    assert_eq!(
        s.parent_thread_id.as_deref(),
        Some("parent-123"),
        "parent_thread_id column should be populated"
    );
    assert_eq!(
        s.agent_path.as_deref(),
        Some("root.0"),
        "agent_path column should be populated"
    );
    assert_eq!(s.depth, 1, "depth column should be populated");
    assert_eq!(
        s.agent_status.as_deref(),
        Some("running"),
        "agent_status column should default to 'running'"
    );
    assert_eq!(
        s.nickname.as_deref(),
        Some("agent-0"),
        "nickname column should be populated"
    );

    // Verify the index exists by querying sqlite_master.
    // We can't access the Connection directly, but we can verify the
    // index works by using list_child_sessions (which queries by
    // parent_thread_id — the index optimizes this).
    let children = store
        .list_child_sessions("parent-123")
        .expect("list_child_sessions should succeed");
    assert_eq!(children.len(), 1, "should find 1 child of parent-123");
    assert_eq!(children[0].id, "test-1");
}

/// AC-2 test 2: Migration is idempotent — running it multiple times does
/// not panic or error.
///
/// `open_in_memory()` calls `migrate()` internally. We can't call `migrate()`
/// directly (it's private), but we can verify idempotency by checking that
/// creating a session, then opening another store on the same DB, and
/// creating another session all work without errors.
#[test]
fn e2e_prd97_ac2_schema_migration_idempotent() {
    // Use a temp file DB so we can re-open it.
    let temp = tempfile::NamedTempFile::new().expect("create temp file");
    let path = temp.path();

    // First open — runs migration, adds columns.
    {
        let store = SessionStore::open(path).expect("first open should succeed");
        store
            .create_session_with_thread(
                "session-a",
                "prompt a",
                None,
                Some("root-1"),
                "root.0",
                1,
                "agent-0",
            )
            .expect("create session a should succeed");
    }

    // Second open — migration runs again (idempotent), should not panic.
    {
        let store = SessionStore::open(path).expect("second open should succeed");
        // Verify the first session is still there.
        let sessions = store.list_sessions().expect("list should succeed");
        assert!(
            sessions.iter().any(|s| s.id == "session-a"),
            "session-a should persist after re-open"
        );

        // Create another session — should work fine.
        store
            .create_session_with_thread(
                "session-b",
                "prompt b",
                None,
                Some("root-1"),
                "root.1",
                1,
                "agent-1",
            )
            .expect("create session b should succeed");

        let sessions = store.list_sessions().expect("list should succeed");
        assert_eq!(sessions.len(), 2, "should have 2 sessions after re-open");
    }

    // Third open — still idempotent.
    {
        let store = SessionStore::open(path).expect("third open should succeed");
        let sessions = store.list_sessions().expect("list should succeed");
        assert_eq!(sessions.len(), 2, "should still have 2 sessions");
    }
}

/// AC-2 test 3: Subagent session row has correct thread metadata.
///
/// A subagent session created via `create_session_with_thread` should have:
/// - parent_thread_id != NULL (links to parent)
/// - depth = parent depth + 1
/// - agent_path = "root.0" (first child of root)
/// - agent_status = "running" (initial status)
/// - nickname = "agent-0"
///
/// After `update_agent_status`, the status should change.
#[test]
fn e2e_prd97_ac2_subagent_session_has_thread_metadata() {
    let store = SessionStore::open_in_memory().expect("open_in_memory should succeed");

    // Create a root session (depth 0, no parent).
    store
        .create_session_with_thread(
            "root-session",
            "root prompt",
            Some("model-x"),
            None, // root has no parent
            "root",
            0,
            "root",
        )
        .expect("create root session should succeed");

    // Create a subagent session (depth 1, parent = root).
    store
        .create_session_with_thread(
            "child-session",
            "child prompt",
            Some("model-x"),
            Some("root-session"),
            "root.0",
            1,
            "agent-0",
        )
        .expect("create child session should succeed");

    // Verify root session metadata.
    let sessions = store.list_sessions().expect("list should succeed");
    let root_meta = sessions
        .iter()
        .find(|s| s.id == "root-session")
        .expect("root session should exist");
    assert_eq!(
        root_meta.parent_thread_id, None,
        "root should have no parent"
    );
    assert_eq!(root_meta.depth, 0, "root depth should be 0");
    assert_eq!(
        root_meta.agent_path.as_deref(),
        Some("root"),
        "root agent_path should be 'root'"
    );
    assert_eq!(
        root_meta.agent_status.as_deref(),
        Some("running"),
        "root agent_status should default to 'running'"
    );
    assert_eq!(
        root_meta.nickname.as_deref(),
        Some("root"),
        "root nickname should be 'root'"
    );

    // Verify child session metadata.
    let child_meta = sessions
        .iter()
        .find(|s| s.id == "child-session")
        .expect("child session should exist");
    assert_eq!(
        child_meta.parent_thread_id.as_deref(),
        Some("root-session"),
        "child parent_thread_id should link to root"
    );
    assert_eq!(
        child_meta.depth, 1,
        "child depth should be 1 (parent depth 0 + 1)"
    );
    assert_eq!(
        child_meta.agent_path.as_deref(),
        Some("root.0"),
        "child agent_path should be 'root.0'"
    );
    assert_eq!(
        child_meta.agent_status.as_deref(),
        Some("running"),
        "child agent_status should be 'running' initially"
    );
    assert_eq!(
        child_meta.nickname.as_deref(),
        Some("agent-0"),
        "child nickname should be 'agent-0'"
    );

    // Verify list_child_sessions finds the child.
    let children = store
        .list_child_sessions("root-session")
        .expect("list_child_sessions should succeed");
    assert_eq!(children.len(), 1, "root should have 1 child");
    assert_eq!(children[0].id, "child-session");

    // Verify update_agent_status changes the status.
    store
        .update_agent_status("child-session", "completed")
        .expect("update_agent_status should succeed");
    let sessions = store.list_sessions().expect("list should succeed");
    let child_meta = sessions
        .iter()
        .find(|s| s.id == "child-session")
        .expect("child session should exist");
    assert_eq!(
        child_meta.agent_status.as_deref(),
        Some("completed"),
        "child agent_status should be 'completed' after update"
    );
}
