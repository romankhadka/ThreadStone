#[test]
fn dhrystone_runs() {
    let dps = workloads::dhrystone::run_dhry(50_000);
    assert!(dps > 1_000.0);  // should be non‑trivial
}
