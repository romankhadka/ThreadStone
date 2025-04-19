use threadstone_core::time::now_nanos;
use workloads::dhrystone::run_dhry;

fn main() {
    let t0 = now_nanos();
    let score = run_dhry(10_000);
    let t1 = now_nanos();

    println!("Stub Dhrystone returned {score}; elapsed {} ns", t1 - t0);
}
