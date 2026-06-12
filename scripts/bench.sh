#!/usr/bin/env bash
#
# End-to-end benchmark harness for `rep`, modelled on ripgrep's benchsuite:
# two deliberately different corpora - "many small files" and "few large
# files" - exercise the per-file overhead and the raw-throughput paths
# separately. Timing is wall-clock over the real binary via `hyperfine`.
#
# Usage:
#   scripts/bench.sh                      # build release, bench it
#   scripts/bench.sh BIN_A BIN_B ...      # compare two or more rep binaries
#   REGEN=1 scripts/bench.sh              # force corpus regeneration
#   WRITE=1 scripts/bench.sh              # also run the (destructive) write bench
#
# Env knobs: SMALL_FILES, LARGE_FILES, LARGE_LINES (corpus sizing).

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
corpus_root="$repo_root/target/bench-corpus"
many_dir="$corpus_root/many"
large_dir="$corpus_root/large"

SMALL_FILES="${SMALL_FILES:-4000}"
LARGE_FILES="${LARGE_FILES:-4}"
LARGE_LINES="${LARGE_LINES:-200000}"

need() { command -v "$1" >/dev/null 2>&1 || { echo "error: '$1' not found on PATH" >&2; exit 1; }; }
need hyperfine
need python3

# --- Corpus generation -------------------------------------------------------
# Generated once and cached under target/ (gitignored). The pattern "needle"
# is sprinkled at a fixed rate so match density is stable across runs; the
# replacement keeps the byte length close so diffs stay realistic.
gen_corpus() {
    if [[ -d "$corpus_root" && -z "${REGEN:-}" ]]; then
        echo "corpus: reusing $corpus_root (REGEN=1 to rebuild)"
        return
    fi
    echo "corpus: generating (small=$SMALL_FILES large=$LARGE_FILES x ${LARGE_LINES} lines)"
    rm -rf "$corpus_root"
    mkdir -p "$many_dir" "$large_dir"
    python3 - "$many_dir" "$large_dir" "$SMALL_FILES" "$LARGE_FILES" "$LARGE_LINES" <<'PY'
import os, sys
many, large, n_small, n_large, large_lines = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4]), int(sys.argv[5])

LINE = "let value_{i} = compute(some_input, other_input);  // routine work here\n"
HIT  = "let needle_{i} = compute(some_input, other_input);  // contains the pattern\n"

# Many small files: ~40 lines each, nested 2 dirs deep, ~1 in 5 lines a hit.
for f in range(n_small):
    d = os.path.join(many, f"d{f % 50}", f"s{(f // 50) % 50}")
    os.makedirs(d, exist_ok=True)
    with open(os.path.join(d, f"file_{f}.rs"), "w") as fh:
        for i in range(40):
            fh.write((HIT if i % 5 == 0 else LINE).format(i=i))

# Few large files: LARGE_LINES lines each, ~1 in 20 a hit.
for f in range(n_large):
    with open(os.path.join(large, f"big_{f}.txt"), "w") as fh:
        for i in range(large_lines):
            fh.write((HIT if i % 20 == 0 else LINE).format(i=i))
PY
    echo "corpus: $(find "$corpus_root" -type f | wc -l | tr -d ' ') files, $(du -sh "$corpus_root" | cut -f1) total"
}

# --- Binaries under test -----------------------------------------------------
bins=()
if [[ $# -gt 0 ]]; then
    bins=("$@")
else
    echo "build: cargo build --release"
    ( cd "$repo_root" && cargo build --release --bin rep >/dev/null )
    bins=("$repo_root/target/release/rep")
fi
for b in "${bins[@]}"; do
    [[ -x "$b" ]] || { echo "error: not executable: $b" >&2; exit 1; }
done

# Build a hyperfine invocation that benches one shell command across every
# binary, labelling each run with the binary's basename + parent dir.
run() {
    local name="$1"; shift
    local tmpl="$1"; shift   # command template, with @BIN@ as the binary slot
    echo
    echo "### $name"
    local args=()
    for b in "${bins[@]}"; do
        local label; label="$(basename "$(dirname "$b")")/$(basename "$b")"
        args+=( -n "$label" "${tmpl//@BIN@/$b}" )
    done
    hyperfine --warmup 2 --min-runs 8 "$@" "${args[@]}"
}

gen_corpus

# Non-destructive read/render scenarios (stdout sent to /dev/null so the
# terminal is never the bottleneck - we are timing rep, not the tty).
run "many-small: quiet scan only" \
    'env REP_COLOR=never @BIN@ -q needle replaced '"$many_dir"

run "many-small: plain patch output" \
    'sh -c '\''env REP_COLOR=never '"'"'@BIN@'"'"' needle replaced '"$many_dir"' >/dev/null'\'''

run "many-small: colored diff render" \
    'sh -c '\''env REP_COLOR=always '"'"'@BIN@'"'"' needle replaced '"$many_dir"' >/dev/null'\'''

run "many-small: list files (-l)" \
    'sh -c '\''@BIN@ -l needle '"$many_dir"' >/dev/null'\'''

run "many-small: multi-expression" \
    'sh -c '\''env REP_COLOR=always '"'"'@BIN@'"'"' -e needle replaced -e compute derive '"$many_dir"' >/dev/null'\'''

run "few-large: colored diff render" \
    'sh -c '\''env REP_COLOR=always '"'"'@BIN@'"'"' needle replaced '"$large_dir"' >/dev/null'\'''

run "few-large: colored, file-only hyperlinks" \
    'sh -c '\''env REP_COLOR=always REP_HYPERLINK_FORMAT=file '"'"'@BIN@'"'"' needle replaced '"$large_dir"' >/dev/null'\'''

# Destructive write bench: restore the corpus from a pristine copy before each
# timed run via hyperfine --prepare. Opt-in because the copy dominates timing.
if [[ -n "${WRITE:-}" ]]; then
    pristine="$corpus_root/.pristine-many"
    [[ -d "$pristine" ]] || cp -R "$many_dir" "$pristine"
    run "many-small: write to disk" \
        'sh -c '\''env REP_COLOR=never '"'"'@BIN@'"'"' --write needle replaced '"$many_dir"' >/dev/null'\''' \
        --prepare "rm -rf '$many_dir' && cp -R '$pristine' '$many_dir'"
fi

echo
echo "done."
