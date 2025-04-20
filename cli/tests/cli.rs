use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn schema_subcommand_emits_schema() {
    let mut cmd = Command::cargo_bin("threadstone-cli").unwrap();
    cmd.arg("schema")
       .arg("-o")
       .arg("tests/tmp.schema.json");
    cmd.assert()
       .success();
    
    // Read the generated schema file and check its content
    let schema_content = std::fs::read_to_string("tests/tmp.schema.json").unwrap();
    assert!(schema_content.contains("\"$schema\""), "Schema should contain $schema field");
    
    std::fs::remove_file("tests/tmp.schema.json").unwrap();
}

#[test]
fn run_and_verify_roundtrip() {
    // generate a small result file
    let mut run = Command::cargo_bin("threadstone-cli").unwrap();
    run.args(&["run", "-w", "dhrystone", "-t", "0", "-s", "1", "-o", "tests/tmp.json"]);
    run.assert().success();

    // verify should pass
    let mut verify = Command::cargo_bin("threadstone-cli").unwrap();
    verify.args(&["verify", "tests/tmp.json"]);
    verify.assert().success();

    std::fs::remove_file("tests/tmp.json").unwrap();
}