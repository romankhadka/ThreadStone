# ThreadStone – CPU Benchmark Suite

_Mission_: a transparent, cross‑platform measurement of raw CPU throughput and scalability.

## Crate layout

| Crate     | Description                          |
| --------- | ------------------------------------ |
| core      | Benchmark kernel, stats, JSON output |
| workloads | Test implementations                 |
| cli       | End‑user command‑line interface      |

Usage:

### Export the JSON Schema

```bash
# print to stdout
threadstone-cli schema

# or write to file
threadstone-cli schema -o result.schema.json
```

## Quickstart

```bash
cargo install --path cli
threadstone run -w dhrystone -t 4 -s 3 -o results.json
threadstone verify results.json
```

## Benchmark Scripts

### Parallel Dhrystone

The `run_dhry_parallel.sh` script runs Dhrystone benchmarks in parallel to fully utilize all CPU cores:

```bash
# Run with default 5 samples per core
./run_dhry_parallel.sh

# Run with custom samples per core
./run_dhry_parallel.sh 10
```

This script automatically detects available CPU cores and spawns one benchmark instance per core to achieve maximum CPU utilization. It measures and reports total execution time and CPU time used.

### Memory Bandwidth

```bash
# Run STREAM benchmark using all cores
cargo run -p threadstone-cli -- run -w stream --threads 0 --samples 5
```
