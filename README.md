# ThreadStone – CPU Benchmark Suite

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
