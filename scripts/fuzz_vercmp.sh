#!/usr/bin/env bash
# Cross-check alpm-rs's vercmp against the real pacman vercmp binary
# over a broad set of generated version-string pairs.
set -euo pipefail

BIN="./target/debug/examples/vercmp_check"
FAIL=0
COUNT=0

FRAGMENTS=(
  "1" "2" "10" "0" "1.0" "1.0.0" "1.0.1" "1.1" "2.0" "1.0-1" "1.0-2" "1.0-10"
  "1.0a" "1.0b" "1.0alpha" "1.0beta" "1.0rc1" "1.0.rc1" "1:1.0" "2:0.5"
  "1.0-1.1" "20221231" "1.0_1" "1.0+git1" "01.0" "1.00" "1.0.0.0" "r100"
  "1.0.0-1" "1.0.0-2" "0.9.9" "1.0-beta1" "3.14.159" "1.0.0a" "1.0.0.a"
)

for a in "${FRAGMENTS[@]}"; do
  for b in "${FRAGMENTS[@]}"; do
    expected=$(vercmp "$a" "$b")
    got=$("$BIN" "$a" "$b")
    COUNT=$((COUNT + 1))
    if [[ "$expected" != "$got" ]]; then
      echo "MISMATCH: vercmp($a, $b) real=$expected ours=$got"
      FAIL=$((FAIL + 1))
    fi
  done
done

echo "checked $COUNT pairs, $FAIL mismatches"
exit $((FAIL > 0))
