#!/bin/bash

# Default number of samples if not specified
SAMPLES=${1:-5}

# How many cores to use
CORES=$(sysctl -n hw.ncpu)

# Record start time
START_TIME=$(date +%s)
echo "Starting Dhrystone benchmark across $CORES cores with $SAMPLES samples per instance..."

# Run one benchmark process per core
for i in $(seq 1 $CORES); do
  echo "Starting Dhrystone instance $i"
  cargo run -p threadstone-cli -- run -w dhrystone --threads 1 --samples $SAMPLES &
done

# Wait for all background processes to complete
wait

# Record end time and calculate duration
END_TIME=$(date +%s)
DURATION=$((END_TIME - START_TIME))
MINUTES=$((DURATION / 60))
SECONDS=$((DURATION % 60))

echo "All Dhrystone instances completed in ${MINUTES}m ${SECONDS}s"
echo "Total CPU time: $((DURATION * CORES)) core-seconds" 