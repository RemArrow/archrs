#!/usr/bin/env bash
set -euo pipefail
BIN="./target/debug/examples/vercmp_check"
mapfile -t VERSIONS < /tmp/real_versions.txt
FAIL=0
COUNT=0
N=${#VERSIONS[@]}
for ((k=0; k<3000; k++)); do
  a=${VERSIONS[$((RANDOM % N))]}
  b=${VERSIONS[$((RANDOM % N))]}
  expected=$(vercmp "$a" "$b")
  got=$("$BIN" "$a" "$b")
  COUNT=$((COUNT+1))
  if [[ "$expected" != "$got" ]]; then
    echo "MISMATCH: vercmp($a, $b) real=$expected ours=$got"
    FAIL=$((FAIL+1))
  fi
done
echo "checked $COUNT real-world pairs, $FAIL mismatches"
exit $((FAIL > 0))
