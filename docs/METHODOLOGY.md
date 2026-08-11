# Methodology

How ThreadStone measures, and why each choice was made. Where a decision could
reasonably have gone the other way, the alternative is stated along with the
reason it was rejected.

---

## 1. The measurement window

### Calibration

Iteration counts are discovered at run time, not fixed in the source. A count
that fills 300 ms on a laptop fills 3 ms on a fast server, and at 3 ms the
clock's own granularity becomes a material fraction of the reading.

The runner starts at one iteration and grows the count until a round reaches 90%
of the target window (250 ms by default), capping growth at 20× per step so a
single anomalously fast round cannot overshoot catastrophically. If no count
below `u64::MAX` reaches the target, the run fails rather than reporting a
number — in practice that means the kernel's inner loop was optimised away, and
a benchmark that silently measures an empty loop is worse than one that stops.

**Calibration runs with every thread active.** This matters more than it
sounds. A memory-bandwidth count calibrated against an idle machine will take
several times longer once fourteen threads are contending for the same memory
controller, so the "250 ms" window becomes two seconds and the run takes eight
times longer than intended.

### Threads start together

The obvious way to benchmark N threads is to hand N samples to a work-stealing
pool. That measures the wrong thing, three separate ways:

1. **Threads do not start together.** The pool spawns work as slots free up, so
   early threads run against an idle machine and late threads against a loaded
   one. The samples come from different conditions; averaging them is
   meaningless.
2. **Work stealing is inside the timed region.** The scheduler's own load
   balancing becomes part of the measurement.
3. **Per-sample setup lands in the window.** Allocating and first-touching a
   working set is often slower than the kernel itself.

ThreadStone spawns its threads once for the whole workload. Each allocates its
state before any clock starts, then executes rounds in lockstep: a barrier
releases every thread simultaneously, and a second barrier collects them. The
measured window is the span between those barriers — exactly "time for all N
threads to complete their work, having started at the same instant."

Barrier overhead is a few microseconds against a 250 ms window: under one part
in fifty thousand.

### Warmup

Two rounds are discarded before measurement. That is enough to fault in the
working set, fill the branch predictors and TLB, and let the CPU reach its boost
frequency. Warmup rounds use the calibrated count, so they are representative of
what follows.

---

## 2. Statistics

### Median, not mean

Benchmark noise is one-sided. Interrupts, migrations, and thermal events only
ever make a sample *slower*, so the distribution has a hard floor and a long
right tail. The mean chases that tail; the median does not. Every headline
figure in a ThreadStone result is a median.

### Outlier rejection

Samples are filtered by median absolute deviation, scaled to a
standard-deviation equivalent (×1.4826) with a three-sigma threshold. MAD is
used rather than standard deviation because the standard deviation is itself
inflated by the outliers it is supposed to detect.

Rejection is skipped below four samples, where MAD is not estimable and
discarding data would be arbitrary. The count of rejected samples is reported —
a run that threw away three of seven samples is telling you something about the
machine.

### Stability verdict

Each pass is classified on its coefficient of variation:

| Verdict | CV | Meaning |
|---|---|---|
| `stable` | < 1% | Differences of a few percent are meaningful |
| `acceptable` | < 3% | Usable; trust only differences larger than the spread |
| `noisy` | < 10% | The machine was busy or thermally constrained |
| `unreliable` | ≥ 10% | Do not draw conclusions from this run |

The terminal output prints a warning naming any workload that did not reach
`acceptable`. A benchmark that reports an unreliable number as though it were
solid is worse than one that reports nothing.

### Comparison significance

`threadstone compare` combines both measurements' 95% confidence intervals in
quadrature and only calls a change significant if it exceeds them. A 3%
difference between runs that each vary by 5% is noise; the same 3% between runs
that vary by 0.2% is a real regression. Each measurement's relative uncertainty
is floored at 0.5%, so a single-sample pass cannot claim infinite precision.

---

## 3. Timing

Two clocks, for two different jobs.

**Measurement** uses `std::time::Instant`, which is already
`clock_gettime(CLOCK_MONOTONIC)` on Linux and `mach_absolute_time` on macOS.

**Cycle-level reporting** uses the raw architectural counter — `CNTVCT_EL0` on
aarch64, `RDTSC` on x86 — and only for figures that need it.

Building the measurement clock on `RDTSC` was considered and rejected. The TSC
is not guaranteed invariant across sockets or power states, calibrating it
requires stalling process startup, and `Instant` is already a vDSO read of the
same underlying hardware.

Each result records the clock's **measured** resolution and per-call overhead,
determined empirically at startup rather than assumed. This is worth knowing:
`CNTVCT_EL0` on Apple silicon ticks at 24 MHz, giving ~41.7 ns granularity —
coarser than most people expect. A reader can check whether a window was long
enough to trust rather than taking it on faith.

---

## 4. Workloads

Each measures a capability the others cannot see.

### Dhrystone 2.1 — integer ALU, branches, calls, strings

A faithful Rust port of Weicker's C version, verified against the reference
implementation's published final values: `Int_Glob`, `Arr_2_Glob[8][7]`, both
records' fields, and every local the original prints. Those constants come from
the published expected output, so the test is a real check rather than a
snapshot of whatever the code happens to do.

**Why port rather than link the C.** The original's state is entirely file-scope
globals, so two threads corrupt each other — the previous version of this suite
worked around that with a process-wide mutex, which meant `--threads 14`
serialised and measured nothing parallel. The C also carries its own `main` and
timing loop, and compiling it needs `-std=gnu89` plus eight warning
suppressions.

Two documented departures: the two records live in a two-element array addressed
by index rather than by raw pointer (they form a self-referential cycle that
safe references cannot express), and the three-way `variant` union is flattened
to a struct, which moves a slightly larger record on each structure assignment.

Every `Proc_` and `Func_` is `#[inline(never)]`. This enforces Dhrystone's ground
rule that procedures not be merged into their callers — call overhead is part of
what the benchmark measures — and stops LLVM collapsing the loop after proving
that iterations past the first reach a fixed point.

Dhrystone is a tiny working set that lives entirely in L1 and says nothing about
memory, floating point, or vector units. That is exactly why five other
workloads sit next to it.

### SGEMM 256³ — floating-point throughput

`C += A · B` over square f64 matrices, using `i-k-j` order so the inner loop is
a contiguous AXPY that LLVM vectorises into FMAs. The textbook `i-j-k` order
accumulates a dot product, creating a serial dependency through the accumulator
that stalls on FMA latency. Four rows of `C` are updated per pass over a row of
`B`, so each `B` load feeds four FMAs.

`N = 256` puts three matrices at 1.5 MiB: past L1, inside L2. A working set
inside L1 would measure peak FLOPs with no memory system involvement; one beyond
L2 would measure bandwidth, which STREAM already covers.

### SHA-256 — dependent-chain integer ALU

Implemented here rather than imported, for two reasons. A benchmark whose result
depends on which version of a dependency resolved is not reproducible; and the
implementation is verified against the NIST test vectors, so it provably
computes SHA-256 rather than something of similar cost.

Deliberately the portable software path — no ARMv8 or x86 SHA extensions. Those
are roughly an order of magnitude faster, so the workload would stop measuring
integer throughput and start measuring the presence of one instruction.

### Sort 1Mi u64 — branch mispredicts and irregular access

The closest thing here to what ordinary application code does to a CPU:
unpredictable branches at every comparison and a recursive access pattern no
prefetcher models.

Each iteration copies a pristine array over the working buffer before sorting,
because sorting an already-sorted array is a different and much easier problem.
That copy is inside the measurement window, and the reported rate is therefore
refill-plus-sort. The alternative — stopping and restarting the clock around
every iteration — would add timer overhead to a loop that runs thousands of
times. An 8 MiB sequential copy against roughly 20 million comparisons is well
under 1% of the window.

### STREAM Triad — memory bandwidth

`a[i] = b[i] + scalar · c[i]` over three 64 MiB arrays, following McCalpin.
Triad is the most demanding of the four STREAM kernels and the one reported
here.

Arrays are **partitioned** across threads, not replicated, so the footprint stays
192 MiB at every thread count. Replicating would grow the working set with the
thread count and change what is being measured partway up the scaling curve.
Partitioning also gives correct NUMA placement for free: each thread allocates
and first-touches its own slice, so pages land in the memory attached to the
socket that will read them.

**Byte accounting.** 24 bytes per element — two 8-byte reads and one 8-byte
write — following STREAM's convention of ignoring read-for-ownership traffic.
Real DRAM traffic is up to a third higher. The convention is kept because every
published STREAM number uses it; a differently-accounted number would not be
comparable to any of them.

**On unusually high single-thread numbers.** Apple silicon sustains over
100 GiB/s of triad bandwidth from a single core, which makes its 1→N scaling
factor look poor (around 2×). That is not a measurement error: one core already
saturates a large fraction of the memory controller, so there is little headroom
left for the others. This figure was cross-checked against an independent C
implementation and agreed within 4%.

### Memory latency — the number caches cannot fix

A dependent-load pointer chase over 256 MiB. Each step's address is the previous
step's value, so the CPU cannot issue the next load until the current one
returns: no prefetcher helps, no memory-level parallelism applies.

Two properties defeat the prefetchers. Nodes are one cache line apart, so every
hop is a fresh miss. And the chase is a single Hamiltonian cycle over a shuffled
permutation, not independent random jumps — uniformly random indices would
revisit nodes and touch only about 63% of the buffer, whereas a single cycle
visits every node exactly once per lap. That is what makes the working set
exactly the buffer size, and what makes the cache-hierarchy sweep meaningful.

**Measured single-threaded only.** This is the suite's most consequential
omission and deserves to be stated plainly. Partitioning the buffer across
sixteen threads would give each a 16 MiB slice that fits in last-level cache, so
the "multi-threaded latency" would be an LLC hit time — several times better
than reality. Replicating the full buffer per thread would instead measure
*loaded* latency, a legitimate but entirely different metric needing 4 GiB on a
sixteen-thread machine. Rather than report a flattering wrong number or silently
change the metric, the suite measures one honest figure and excludes it from the
multi-core score.

`threadstone sweep` walks the same chase across working-set sizes from 4 KiB to
256 MiB. The curve makes the cache hierarchy directly visible: latency sits flat
inside each level and steps up at every boundary, so the plateaus name the cache
sizes and the step heights name their costs.

---

## 5. Scoring

### The reference core

Every workload is normalised against a fixed reference value, and the geometric
mean of those ratios, scaled by 1000, is the score.

The **ThreadStone Reference Core v1** is a definition, not a machine anyone
owns: a nominal 3.0 GHz out-of-order core with 256-bit SIMD and a single
DDR4-3200 channel.

| Workload | Reference | Reasoning |
|---|---|---|
| `dhrystone` | 22,000,000 Dhry/s | ~135 cycles per loop at 3 GHz |
| `sgemm` | 12 GFLOP/s | ~25% of a 48 GFLOP/s 256-bit FMA peak |
| `sha256` | 250 MiB/s | ~12 cycles per byte, software path |
| `sort` | 50 Melem/s | ~20 comparisons per element, ~1 cycle each |
| `stream` | 12 GiB/s | ~half of one DDR4-3200 channel's 23.8 GiB/s |
| `latency` | 90 ns | Typical DDR4 random-access latency |

Each reference is what the **reference core would achieve running these exact
kernels**, not what ideal code would achieve on that hardware. The distinction
matters most for SGEMM: this suite ships a blocked but deliberately
unvectorised kernel, so a reference derived from hand-tuned BLAS throughput
would put every machine below 1.0 and would be measuring the kernel rather than
the CPU.

Fixing the reference by fiat, in round numbers, published here, means the
author's machine lands wherever it lands. A reference derived from whatever
hardware the author happened to own would make that machine score exactly 1000
and everything else look like a deviation from it.

These values are frozen for the lifetime of schema version 2. Changing one would
silently invalidate every previously published score, so a revision would ship
as "Reference Core v2" alongside a schema bump.

### Why the geometric mean

The arithmetic mean of ratios is not a meaningful composite: it depends on which
machine you designate as the denominator, so A can beat B under one choice of
reference and lose under another. The geometric mean is invariant to that
choice — the ratio of two machines' scores is the same whatever the reference —
which is the entire point of having a normalised score.

### Direction

Latency is reported in nanoseconds, where lower is better. It is normalised as
`reference / measured` rather than `measured / reference`, putting it on the
same footing as everything else so it can join the geometric mean without
special-casing.

---

## 6. Provenance and integrity

### What travels with a result

CPU model and vendor, physical and logical core counts, the performance and
efficiency core split on heterogeneous CPUs, cache sizes, cache line size,
installed memory, OS and version, target triple, compiler version, optimisation
level, `target-cpu`, enabled target features, whether debug assertions were on,
and the measured resolution and overhead of the clock.

"2300 Dhrystones/sec" is unfalsifiable. "63.5 million Dhrystones/sec on an Apple
M4 Pro, 10 performance cores plus 4 efficiency cores, macOS, rustc 1.83.0,
aarch64-apple-darwin, opt-level 3 with fat LTO" is a claim someone can reproduce
or refute.

Detection is dependency-free and degrades gracefully — macOS through `sysctl`,
Linux through `/proc` and `/sys`, Windows through environment variables. Every
field is optional; an unrecognised platform yields fewer fields, never an error.

### Reproducible builds

`codegen-units = 1` and fat LTO, because parallel codegen is nondeterministic in
ways that perturb hot loops between builds. The optimisation settings travel
with the result, since two binaries built differently produce numbers that are
not comparable.

### Signing

Ed25519 over the report's canonical JSON: sorted keys, no insignificant
whitespace, `signature` removed before hashing.

**Canonicalisation goes through JSON text, not straight to a value.** This is
subtle and it matters. `serde_json`'s float parser is not correctly rounded — it
writes the shortest round-tripping representation, but reading that text back
can land on the adjacent double:

```
99.0 * 0.8 + 99.5 * 0.2  ->  bits 4058c66666666667
serialised               ->  "99.10000000000001"
parsed back              ->  bits 4058c66666666666   (different)
re-serialised            ->  "99.1"
```

A signer canonicalising its in-memory struct would produce different bytes from
a verifier canonicalising the same document after reading it from disk, and
every signature would fail. Percentile statistics land on values of exactly this
kind routinely. Round-tripping through text first puts both sides on the same
footing.

**What a signature proves: integrity, not authority.** It shows a result has not
been modified since it was signed by the holder of a particular key. It does not
show the number is honest, that the machine is what the file claims, or that the
key belongs to anyone in particular — a signer can always run the benchmark
under favourable conditions and sign the result truthfully. The public key
travels inside the file so verification needs nothing else; deciding whether to
trust that key is left to the reader.

### Verification

`threadstone verify` asks three separate questions and reports each answer
rather than collapsing them:

1. **Does it parse?** Serde enforces the shape strictly.
2. **Is it internally coherent?** A file can parse and still be nonsense: a
   negative value, a median outside its own min/max, statistics counting more
   samples than are recorded, a schema version from the future. These are the
   checks a JSON Schema cannot express.
3. **Is it unmodified?** The Ed25519 signature.

Validating a report against a schema generated from the very type it was just
deserialised into — as an earlier version did — can never fail. The useful
checks are the semantic ones.

---

## 7. Known limitations

Stated because a methodology document that only lists strengths is marketing.

**No thread pinning.** Threads are not bound to specific cores. On macOS this is
not really available — Apple silicon ignores affinity hints — and on a
heterogeneous CPU the scheduler may move a thread between performance and
efficiency cores mid-measurement. This shows up as elevated variance rather than
bias, and the stability verdict will say so.

**Frequency is not controlled.** Boost behaviour, thermal throttling, and power
state are whatever the machine decides. Warmup reaches a boost state, but a
sustained run on a thermally limited laptop will drift downward. The
coefficient of variation exposes this.

**Dhrystone is Dhrystone.** It is a 1984 benchmark that modern compilers
optimise aggressively, its working set fits in L1, and small changes in compiler
version can move the number by double digits. It is included for continuity and
because integer/branch/call performance is worth measuring, not because it is a
good benchmark. Read it alongside `sort`, which is far more representative.

**The multi-core score excludes latency.** Five workloads contribute, not six.
This is deliberate (see above) but means single-core and multi-core scores are
not composed from the same set and should be compared machine-to-machine, not
to each other.

**Single-run scores are not the whole story.** A score summarises six numbers
into one and loses the shape. The per-workload table is the actual result; the
score is a convenience.

**No cross-machine calibration.** Reference values are reasoned estimates, not
measurements of a real reference machine. They fix the scale; they do not
certify it.
