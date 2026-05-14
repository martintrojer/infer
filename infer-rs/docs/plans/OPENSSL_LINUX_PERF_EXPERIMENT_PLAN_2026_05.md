# OpenSSL Linux perf experiment plan (2026-05)

This is a doc-only, speculative experiment sequence for the next OpenSSL Linux
performance worker.  Run it only after worker-1 has delivered the post-fix state
where the 454-procedure corpus completes, and after worker-2 has delivered the
Linux time-wrapper fix for the benchmark script.  The formula-substitution panic
and the GNU/BSD time mismatch are prerequisites, not experiments here.

Common context for all experiments:

- Corpus: `/home/mtrojer/infer-rs-bench/openssl-20260514-121752`
  (`74` `.sil` files, `454` procedures).
- Textual input glob:
  `/home/mtrojer/infer-rs-bench/openssl-20260514-121752/textual-out/*.sil`.
- Required sentinels: `obj_dat.sil:OBJ_bsearch_ex_`,
  `cfb64ede.sil:DES_ede3_cfb_encrypt`, `apps.sil:set_multi_opts`.
- Full-corpus entrypoint: `scripts/bench_openssl_partial.sh`, which must be the
  post-worker-2 Linux-capable version and must pass
  `--pulse-max-heap-mb 2048 --pulse-max-wall-secs 60` on every run.
- Per-procedure profiling entrypoint: one `.sil` file, `-j 1`, explicit caps
  `--pulse-max-heap-mb 2048 --pulse-max-wall-secs 60`, matching the bench
  script caps.
- Host guard: abort and document any full-corpus attempt with wall `>600s` or
  total RSS `>30 GiB`.
- Cross-reference while interpreting results:
  `docs/plans/OPENSSL_LINUX_PERF_ATTACK_SURFACE_2026_05.md` and
  `docs/STATUS.md` / "OpenSSL benchmark dashboard".  The historical dashboard
  has Rust default `244.70s`, Rust formula-GC `238.56s`, OCaml `42.9s`, and a
  Rust/OCaml wall ratio of about `5.7x`.

Every result note should record the post-fix Rust SHA, any prototype SHA,
command, output directory, wall/RSS/footprint, analyzed count, aborts,
max-visit count, exit distribution, and top wall/RSS procedures.  Do not update
`docs/STATUS.md` from exploratory single-shot data.

## Experiment 1: reproduce the macOS-style 5.7x wall ratio on Linux

Hypothesis: with worker-1's post-fix analyzer state, worker-2's fixed
`/usr/bin/time` handling, and the new corpus at
`/home/mtrojer/infer-rs-bench/openssl-20260514-121752`, Linux Rust should land
near the historical OpenSSL dashboard shape because the analyzer is the same
code: `200-300s` Rust wall at `JOBS=4`, and a Rust/OCaml wall ratio around
`4-7x` when compared to the `STATUS.md` OCaml reference unless a same-corpus
OCaml number is also available.  Method: run the full corpus only through
`scripts/bench_openssl_partial.sh` with its fixed caps
`--pulse-max-heap-mb 2048 --pulse-max-wall-secs 60`, `RUNS=3 JOBS=4`, on the
single worker-1 post-fix SHA.  Success: median Rust wall `200-300s`, analyzed
count `454/454` or clearly explained cap aborts, max RSS below the `30 GiB`
host guard, and ratio `4-7x`; refutation is ratio `>>10x` or wall far above
`300s` without a corpus/count explanation, suggesting a Linux-specific syscall,
allocator, or scheduling hotspot.  Cost: about `10-15` minutes of bench wall and
expected max RSS `~15-25 GiB`.  Decision tree: if successful, use this as the
Linux STATUS.md candidate and continue to Experiment 2; if `>>10x`, run
Experiment 4 early and add `strace -c` as a side probe; if unexpectedly fast,
verify the procedure count and cap-abort accounting before claiming a win.

```sh
cd /home/mtrojer/.local/state/mu/workspaces/infer-rs/worker-leak/infer-rs
OUT_DIR="/tmp/openssl-linux-exp1-j4-$(date +%Y%m%d-%H%M%S)" \
  BENCH_DIR=/home/mtrojer/infer-rs-bench/openssl-20260514-121752 \
  RUNS=3 JOBS=4 STRICT=1 \
  scripts/bench_openssl_partial.sh 2>&1 | tee /tmp/openssl-linux-exp1-j4.log
```

## Experiment 2: `-j` scaling curve at 1, 4, 16, and 64 workers

Hypothesis: on the same worker-1 post-fix SHA and corpus
`/home/mtrojer/infer-rs-bench/openssl-20260514-121752`, Rust scales
sublinearly because retained-state comparison, summaries, allocator pressure,
and memory bandwidth become shared bottlenecks; the likely sweet spot is
`JOBS=8-16`.  Method: run the capped benchmark script
`scripts/bench_openssl_partial.sh` (`--pulse-max-heap-mb 2048
--pulse-max-wall-secs 60`) once for each `JOBS=N` in `{1,4,16,64}` with
`RUNS=1`, then compute `parallelism_factor = wall_at_j1 / wall_at_jN` from the
summary files.  Success: speedup improves through `j4`, continues but flattens
by `j16`, and has little or negative gain at `j64`; refutation is linear scaling
to `j64` (good news or corpus too small) or anti-scaling before `j16` (contention
pathology).  Cost: roughly `20-35` minutes total if Experiment 1 is in-band;
`j64` is the main RSS risk, so enforce the `30 GiB` stop guard.  Decision tree:
if the curve plateaus near `j16`, keep `j4` as the dashboard comparator and use
`j16` as the high-throughput ceiling; if linear to `j64`, repeat suspicious
high-j points with `RUNS=3` before declaring no contention; if anti-scaling,
profile the hottest slow-proc with Experiment 4.

```sh
: > /tmp/openssl-linux-exp2-jscale.log
for j in 1 4 16 64; do
  echo "=== JOBS=$j ===" | tee -a /tmp/openssl-linux-exp2-jscale.log
  OUT_DIR="/tmp/openssl-linux-exp2-j${j}-$(date +%Y%m%d-%H%M%S)" \
    BENCH_DIR=/home/mtrojer/infer-rs-bench/openssl-20260514-121752 \
    RUNS=1 JOBS=$j STRICT=1 \
    scripts/bench_openssl_partial.sh 2>&1 | tee -a /tmp/openssl-linux-exp2-jscale.log
  # Stop before the next point if wall >600s or total RSS >30 GiB.
done
```

## Experiment 3: `--pulse-intermediate-formula-gc` on/off

Hypothesis: formula GC remains roughly neutral on Linux, matching the
`STATUS.md` OpenSSL dashboard where it was only a `~2.5%` wall win with RSS
inside noise; the new worker-1 post-fix 454-procedure corpus at
`/home/mtrojer/infer-rs-bench/openssl-20260514-121752` might still refute this
if it weights DES/formula-heavy procedures more strongly.  Method: run two
`RUNS=3 JOBS=4` blocks through `scripts/bench_openssl_partial.sh` with the same
fixed caps (`--pulse-max-heap-mb 2048 --pulse-max-wall-secs 60`): default first,
then `EXTRA_ARGS="--pulse-intermediate-formula-gc"`; require the post-worker-2
script flag preflight to validate the extra flag.  Success: median wall changes
by `<5%`, max RSS/peak footprint remain within run noise, and abort counts do
not materially improve; refutation is a `>=10%` wall or RSS win without higher
abort count or issue churn.  Cost: two Experiment-1-sized blocks, about
`20-30` minutes bench wall, expected RSS below `30 GiB`.  Decision tree: if
neutral, keep the flag opt-in and proceed to attribution; if it wins, open a
default-enablement decision task and add focused DES A/B profiles; if it trades
lower RSS for worse wall, keep it as a pressure-relief knob only.

```sh
OUT_DIR="/tmp/openssl-linux-exp3-default-$(date +%Y%m%d-%H%M%S)" \
  BENCH_DIR=/home/mtrojer/infer-rs-bench/openssl-20260514-121752 \
  RUNS=3 JOBS=4 STRICT=1 \
  scripts/bench_openssl_partial.sh 2>&1 | tee /tmp/openssl-linux-exp3-default.log
OUT_DIR="/tmp/openssl-linux-exp3-formulagc-$(date +%Y%m%d-%H%M%S)" \
  BENCH_DIR=/home/mtrojer/infer-rs-bench/openssl-20260514-121752 \
  RUNS=3 JOBS=4 STRICT=1 EXTRA_ARGS="--pulse-intermediate-formula-gc" \
  scripts/bench_openssl_partial.sh 2>&1 | tee /tmp/openssl-linux-exp3-formulagc.log
```

## Experiment 4: per-procedure flamegraph of the top three wall offenders

Hypothesis: the post-fix baseline on
`/home/mtrojer/infer-rs-bench/openssl-20260514-121752` will surface a small
wall-heavy set, initially expected to include `DES_ede3_cfb_encrypt`,
`OBJ_bsearch_ex_`, and `set_multi_opts` or a caller on that path, and a focused
CPU profile will show one dominant function family (`Pulse::Operations`,
`formula::phi::propagate`, `state_cmp::Canonicalizer`,
`DisjunctiveDomain::leq`, or interproc summary application) above `30%` of CPU.
Method: select the top three wall offenders from the worker-1 post-fix
`RUNS=3 JOBS=4` bench-script logs and slow-proc tables, then run
`cargo flamegraph` and `perf record` per `docs/TESTING.md` on one `.sil` at a
time, `-j 1`, explicit caps matching `scripts/bench_openssl_partial.sh`
(`--pulse-max-heap-mb 2048 --pulse-max-wall-secs 60`).  Success: a single
function or tight stack family is `>30%` of CPU for at least one top offender;
refutation is an evenly spread profile with no obvious function-level lever.
Cost: at most `3 procedures x 2 profilers x 60s` analyzer cap, usually
`10-15` minutes including profiler overhead, expected focused RSS `<=3 GiB`.
Decision tree: if `state_cmp` dominates, run Experiment 6; if formula dominates,
correlate with Experiment 3; if interproc dominates, inspect summary shape; if
flat, prefer structural/representation work over micro-fixes.

```sh
PROC=DES_ede3_cfb_encrypt
SIL=/home/mtrojer/infer-rs-bench/openssl-20260514-121752/textual-out/cfb64ede.sil
cargo flamegraph --output "/tmp/openssl-linux-exp4-${PROC}.svg" -- \
  target/release/infer-rs --pulse-only --quiet --trace-ondemand \
  --procedures-filter "$PROC" --pulse-max-heap-mb 2048 --pulse-max-wall-secs 60 \
  -j 1 "$SIL"
perf record -F 997 -g --call-graph dwarf -o "/tmp/openssl-linux-exp4-${PROC}.perf.data" -- \
  target/release/infer-rs --pulse-only --quiet --trace-ondemand \
  --procedures-filter "$PROC" --pulse-max-heap-mb 2048 --pulse-max-wall-secs 60 \
  -j 1 "$SIL"
perf report -i "/tmp/openssl-linux-exp4-${PROC}.perf.data"
```

Fallback mappings: `OBJ_bsearch_ex_` -> `$TEXTUAL_DIR/obj_dat.sil`,
`DES_ede3_cfb_encrypt` -> `$TEXTUAL_DIR/cfb64ede.sil`, `set_multi_opts` ->
`$TEXTUAL_DIR/apps.sil`.

## Experiment 5: per-procedure heaptrack on the top three RSS contributors

Hypothesis: after the worker-1 post-fix baseline, Linux RSS on
`/home/mtrojer/infer-rs-bench/openssl-20260514-121752` will either be dominated
by one allocation site/retained-state structure in DES-like procedures or by
cumulative summary/invariant retention that does not attribute cleanly to one
procedure.  Method: choose the top three RSS contributors from the post-fix
bench-script run (`scripts/bench_openssl_partial.sh` with its `2048 MB / 60s`
caps), falling back to `DES_ede3_cfb_encrypt`, `OBJ_bsearch_ex_`, and
`set_multi_opts`, then run `heaptrack` on a single `.sil` at a time, `-j 1`,
with explicit caps.  Success: identify one allocation site or retained family
(`BaseMemory`, `BaseAddressAttributes`, `Phi` maps, formula caches, summary
storage, canonicalizer temporary vectors) dominating peak or retained bytes;
refutation is allocation cost spread broadly across many small sites.  Cost:
nominal analyzer cap `<=3` minutes for three procedures, but heaptrack overhead
may make wall `10-20` minutes; expected focused RSS `<=3-5 GiB`.  Decision tree:
if one retained structure dominates, open its representation/GC experiment; if
temporaries dominate, inspect canonicalizer sort keys first; if no owner
appears, correlate `/usr/bin/time -v` peaks with `live-fixpoint` heartbeats.

```sh
PROC=DES_ede3_cfb_encrypt
SIL=/home/mtrojer/infer-rs-bench/openssl-20260514-121752/textual-out/cfb64ede.sil
heaptrack --output "/tmp/openssl-linux-exp5-${PROC}.heaptrack.gz" -- \
  target/release/infer-rs --pulse-only --quiet --trace-ondemand \
  --procedures-filter "$PROC" --pulse-max-heap-mb 2048 --pulse-max-wall-secs 60 \
  -j 1 "$SIL"
heaptrack_print "/tmp/openssl-linux-exp5-${PROC}.heaptrack.gz" \
  > "/tmp/openssl-linux-exp5-${PROC}.heaptrack.txt"
```

## Experiment 6: `state_cmp.rs` canonicalizer sort-key cleanup measurement

Hypothesis: the cleanup tracked by `cleanup_state_cmp_canonicalizer_sort_keys`
can remove duplicate sorted-vector / partial-key construction in `state_cmp.rs`
and should improve DES/OBJ wall if Experiment 4 still shows canonicalizer CPU
on the worker-1 post-fix corpus
`/home/mtrojer/infer-rs-bench/openssl-20260514-121752`.  Method: branch from the
same post-fix baseline SHA, implement only the paired `propagate_*` /
`assign_remaining_*` sort-key cleanup, run the normal fast correctness gate,
then run `RUNS=3 JOBS=4` through `scripts/bench_openssl_partial.sh` with fixed
caps `--pulse-max-heap-mb 2048 --pulse-max-wall-secs 60`.  Success: `>=5%`
full-corpus median wall reduction or `>=10%` reduction in DES-family slow-proc
elapsed, with no RSS/analyzed/abort regression; refutation is `<3%` noise-level
movement, correctness fallout, or higher temporary allocation.  Cost:
implementation plus `10-15` minutes bench wall, max RSS under the `30 GiB`
guard.  Decision tree: if it wins and the profile agrees, prepare a scoped
cleanup PR; if neutral, leave it as style-only; if it regresses, revert and
capture the profile delta.

```sh
git switch -c exp/state-cmp-sort-key-cleanup <worker-1-post-fix-commit>
# Implement only cleanup_state_cmp_canonicalizer_sort_keys, then run the fast gate.
OUT_DIR="/tmp/openssl-linux-exp6-statecmp-$(date +%Y%m%d-%H%M%S)" \
  BENCH_DIR=/home/mtrojer/infer-rs-bench/openssl-20260514-121752 \
  RUNS=3 JOBS=4 STRICT=1 \
  scripts/bench_openssl_partial.sh 2>&1 | tee /tmp/openssl-linux-exp6-statecmp.log
```

## Experiment 7: structural sharing prototype re-validation

Hypothesis: the parked track in
`docs/plans/STRUCTURAL_SHARING_PROTOTYPE.md` may still reduce full-corpus peak
RSS on the worker-1 post-fix Linux corpus
`/home/mtrojer/infer-rs-bench/openssl-20260514-121752`, even if wall is neutral,
because historical Arc/persistent-state experiments reduced retained physical
storage in heap, attrs, stack, and `Phi`.  Method: find and revive an existing
structural-sharing prototype branch if one exists; do not rebuild the prototype
from scratch under this measurement task; rebase it onto the same post-fix
baseline SHA, then run `RUNS=3 JOBS=4` through `scripts/bench_openssl_partial.sh`
with fixed `2048 MB / 60s` caps.  Success: `>=15%` lower median max RSS or peak
footprint on the full corpus, without worse wall/analyzed/abort count;
refutation is no full-corpus RSS win, wall regression `>5%`, or correctness /
rebase complexity.  Cost: branch archaeology plus `10-15` minutes bench wall,
with the `30 GiB` host guard.  Decision tree: if it wins, update the structural
sharing plan with Linux numbers and open a scoped representation task; if
neutral, keep it parked behind smaller canonicalizer/formula work; if no branch
exists, record that and skip rather than recreating it here.

```sh
git branch --all --list '*structural*' '*sharing*'
STRUCT_BRANCH=<existing-structural-sharing-branch>
git worktree add /tmp/infer-rs-structural-sharing "$STRUCT_BRANCH"
cd /tmp/infer-rs-structural-sharing
git rebase <worker-1-post-fix-commit>
OUT_DIR="/tmp/openssl-linux-exp7-structural-$(date +%Y%m%d-%H%M%S)" \
  BENCH_DIR=/home/mtrojer/infer-rs-bench/openssl-20260514-121752 \
  RUNS=3 JOBS=4 STRICT=1 \
  scripts/bench_openssl_partial.sh 2>&1 | tee /tmp/openssl-linux-exp7-structural.log
```
