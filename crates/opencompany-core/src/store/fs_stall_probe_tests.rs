use super::{arm, arm_commit, maybe_block, maybe_block_commit, wait_blocked};
use std::path::PathBuf;

fn test_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("stall-probe-{name}-{}", std::process::id()))
}

#[tokio::test]
async fn legacy_stall_probe_apis_still_rendezvous() {
    let path = test_path("stage");
    let release = arm(&path);
    let blocked_path = path.clone();
    let blocked = tokio::task::spawn_blocking(move || maybe_block(&blocked_path));
    wait_blocked().await;
    release.send(()).expect("legacy stage gate still open");
    blocked.await.expect("legacy stage stall completes");

    let path = test_path("commit");
    let gate = arm_commit(&path);
    let blocked_path = path.clone();
    let blocked = tokio::task::spawn_blocking(move || maybe_block_commit(&blocked_path));
    gate.wait_blocked().await;
    gate.send(()).expect("legacy commit gate still open");
    blocked.await.expect("legacy commit stall completes");
}
