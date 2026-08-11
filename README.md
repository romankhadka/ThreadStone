# ThreadStone

A CPU benchmark suite that shows its work.

Six workloads, each measuring something the others cannot see. Every result
records the machine it ran on, how much it varied, and whether it should be
believed. No workload reports a number the methodology cannot support.

**[threadstone.romn.dev](https://threadstone.romn.dev)** — results, methodology,
and what each workload measures.

```
ThreadStone 2.0.0 · Apple M4 Pro (10P+4E) · macos · aarch64-apple-darwin

Workload            Unit         1 thread  14 threads  scaling  cv
────────────────────────────────────────────────────────────────────
Dhrystone 2.1       Dhry/s          68.0M        680M    10.0x  ~ 1.9%
SGEMM 256³          GFLOP/s          19.1         186     9.7x  ~ 2.8%
SHA-256             MiB/s             422       3.75k     8.9x  ~ 2.7%
Sort 1Mi u64        Melem/s           102       1.06k    10.4x  ~ 2.2%
STREAM Triad        GiB/s             110         214     1.9x  ~ 1.1%
Memory Latency      ns                118           —        —  = 0.9%
────────────────────────────────────────────────────────────────────
ThreadStone Score  single-core 2216   multi-core 19373
```

## Install

```bash
cargo install --path threadstone-cli
```

Requires Rust 1.75 or newer. There is no C toolchain, no build script beyond
capturing the compiler version, and three third-party crates in the runtime
binary: `clap`, `serde`, and `ring`.

## Use

```bash
threadstone run                          # the full suite, both passes
threadstone run -w sgemm -w stream       # only these workloads
threadstone run --out result.json        # save the full document
threadstone list                         # what each workload measures
threadstone sweep                        # map the cache hierarchy
threadstone compare before.json after.json
threadstone verify result.json
threadstone sign result.json --key ~/.threadstone/threadstone.key
```

Progress goes to stderr, so `threadstone run --format json > result.json` gives
a clean document.

## The workloads

| Workload | What it exposes | Unit |
|---|---|---|
| `dhrystone` | Integer ALU, branch prediction, call overhead | Dhrystones/s |
| `sgemm` | Floating-point and SIMD throughput out of L2 | GFLOP/s |
| `sha256` | Dependent-chain integer ALU with no memory traffic | MiB/s |
| `sort` | Branch mispredicts and irregular access, as real code produces | Melem/s |
| `stream` | Sustained DRAM bandwidth | GiB/s |
| `latency` | Unhidden memory latency — the one number caches cannot fix | ns |

A CPU that is fast at all six is fast. One that is fast at a single one is fast
at that one thing, and six numbers side by side make that impossible to hide.

## What makes a result trustworthy

Every design decision here follows from one idea: a benchmark number is a claim,
and a claim nobody can check is worthless.

**Iteration counts are calibrated, not fixed.** A count that takes 300 ms on a
laptop takes 3 ms on a server, and 3 ms is close enough to the clock's
granularity to be noise. Calibration happens with every thread running, because
a count tuned on an idle machine overshoots wildly once memory is contended.

**Threads start together.** A work-stealing pool hands out samples as slots free
up, so early threads run against an idle machine and late ones against a loaded
one. ThreadStone spawns its threads once, allocates their state once, and
releases them all from a barrier — so the measured window is exactly "time for
all N threads to finish, having started at the same instant."

**Nothing unmeasured is inside the window.** Allocation and page-faulting happen
before the clock starts.

**Every number carries its uncertainty.** Results report median, standard
deviation, coefficient of variation, and a stability verdict. Samples disturbed
by an interrupt are rejected by median absolute deviation and counted.

**Every number carries its provenance.** CPU model, P/E core split, cache sizes,
OS, compiler version, target triple, optimisation flags, and the measured
resolution of the clock itself.

**Numbers that cannot be measured well are not reported.** Memory latency is
measured single-threaded only. Splitting a 256 MiB chase buffer across sixteen
threads would give each a slice that fits in last-level cache, so the
"multi-threaded latency" would be an LLC hit time — several times better than
reality, and a straightforward lie about the machine.

`docs/METHODOLOGY.md` covers all of this in detail, including the reference core
the score is defined against.

## Scores

The score is the geometric mean of each workload's ratio to a fixed reference,
scaled so that matching the reference scores 1000.

The reference — the *ThreadStone Reference Core v1* — is a definition, not a
machine anyone owns: a nominal 3.0 GHz out-of-order core with 256-bit SIMD and
one DDR4-3200 channel. Deriving it from whatever hardware the author had would
make the author's machine score exactly 1000 and everything else look like a
deviation from it.

The geometric mean is not decoration. The arithmetic mean of ratios depends on
which machine you put in the denominator, so A can beat B under one reference
and lose under another. The geometric mean is invariant to that choice, which is
the entire point of having a normalised score.

## Signing

```bash
threadstone keygen --dir ~/.threadstone
threadstone run --sign-key ~/.threadstone/threadstone.key --out result.json
threadstone sign result.json --key ~/.threadstone/threadstone.key   # or after the fact
threadstone verify result.json --require-signature
```

Ed25519 over the report's canonical JSON — sorted keys, no whitespace,
`signature` removed before hashing. The public key travels inside the file, so
verification needs nothing else.

A signature proves **integrity, not authority**: that a result has not been
edited since signing. It does not prove the number is honest or the machine is
what the file claims. Anyone can sign with their own key.

## Layout

| Crate | Contents |
|---|---|
| `threadstone-core` | Timing, statistics, thread orchestration, environment capture, scoring |
| `threadstone-workloads` | The six kernels |
| `threadstone-cli` | `threadstone` |

`threadstone-core` knows how to measure and contains no workloads; a workload
implements one trait and the engine handles everything around it.

## Result format

Schema version 2. The JSON Schema is committed at `v2/result.schema.json` and CI
fails if it drifts from what the code emits.

```bash
threadstone schema -o result.schema.json
```

## Contributing

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

A new workload needs a `Kernel` impl, a correctness test that checks it computes
what it claims, and a reference value with a written justification.

`Cargo.lock` is deliberately kept at format version 3. Version 4 requires Cargo
1.78, which would raise the effective minimum above what the code needs.

## Licence

MIT OR Apache-2.0.

Dhrystone 2.1 is by Reinhold P. Weicker (1984, C version 1988). STREAM is by
John D. McCalpin. Both are reimplemented here rather than vendored; see the
module documentation for what changed and why.
