use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;

#[test]
fn schema_subcommand_produces_valid_json() {
    // run `threadstone-cli schema` and capture stdout
    let mut cmd = Command::cargo_bin("threadstone-cli").unwrap();
    let assert = cmd.arg("schema").assert().success();
    let output = String::from_utf8(assert.get_output().stdout.clone()).unwrap();

    // parse it as JSON and do a very basic sanity check
    let v: Value = serde_json::from_str(&output).expect("should be valid JSON");
    // for example, it should have a "properties" object
    assert!(v.get("properties").is_some(), "schema must have properties");
}