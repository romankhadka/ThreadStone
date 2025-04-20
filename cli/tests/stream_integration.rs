use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;

#[test]
fn stream_generates_valid_json() {
    // Create the target directory if it doesn't exist
    let target_dir = Path::new("target");
    if !target_dir.exists() {
        fs::create_dir_all(target_dir).expect("Failed to create target directory");
    }
    
    let mut cmd = Command::cargo_bin("threadstone-cli").unwrap();
    cmd.args(&["run", "-w", "stream", "--threads", "1", "--samples", "1", "-o", "target/stream.json"])
        .assert().success();
    // load and verify schema...
}