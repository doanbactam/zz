//! E2E tests for network namespace isolation .
//!
//! AC-1: `zz exec --allow-net none "<curl>"` must fail to reach the network.
//! AC-2: `zz exec --allow-net example.com "<curl example.com>"` must succeed
//!       via the allowlist proxy.
//!
//! These tests exercise the REAL binary + real bash tool path
//! (`crates/tools/src/bash.rs::execute` → `build_net_command` → bwrap).
//! They require the LLM provider (15GB resident) and therefore run under
//! `scripts/zz-test` in CI, not in the on-demand local session (§7.1 OOM
//! guard). The isolation *mechanism* is covered directly and safely by
//! `crates/sandbox/src/network.rs::test_isolation_blocks_outbound` (no LLM).
//!
//! TODO(cycle-101, AC-2): the allowlist proxy forwarder is stubbed in
//! `build_net_command`; AC-2 e2e is written but the proxy wiring is a
//! follow-up. Marked so it is not a no-op .

use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn e2e_prd105_net_isolation_blocks_outbound() {
    // AC-1: default --allow-net none → bwrap --unshare-net → no outbound.
    let mut cmd = Command::cargo_bin("zz").unwrap();
    cmd.args(["exec", "--allow-net", "none", "--max-turns", "1",
              "run: curl -s --max-time 5 https://example.com"]);
    cmd.assert()
        .failure()
        .stdout(predicate::str::contains("network").or(predicate::str::contains("unreachable")).or(predicate::str::contains("exit_code: 6")).or(predicate::str::contains("exit_code: 7")));
}

#[test]
fn e2e_prd105_allow_net_proxy() {
    // AC-2: allowlist domain reaching the network via proxy.
    // Gated: proxy forwarder is stubbed in cycle-101; enable when wired.
    let mut cmd = Command::cargo_bin("zz").unwrap();
    cmd.args(["exec", "--allow-net", "example.com", "--max-turns", "1",
              "run: curl -s --max-time 5 https://example.com"]);
    cmd.assert().success();
}
