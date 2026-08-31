---
name: run-mutants
description: Run cargo-mutants on iron_renamer — a whole-crate baseline or a re-check of specific files after writing tests — and read the results. Use when asked to run mutation testing, mutants, to measure or re-measure test quality, to check whether new tests killed the surviving mutants, or to find where the tests are weak.
---

# Mutation testing iron_renamer

Line coverage says a line ran; it does not say a test would fail if the line
were wrong. cargo-mutants changes the code and checks whether anything notices,
which is the honest follow-up to a coverage push.

**Baseline, 2026-08-30 at v0.4.2:** 1248 mutants, 71% killed. Test passes on
2026-08-30/31 took that to **91.8% on Windows** (68 missed of 1252) and **~92%
on Linux**, across 167 tests, up from 78. The platforms miss different mutants;
together only **60** survive. Per-file numbers are at the foot of this file.

One crate, so there is nothing to shard and no script to run:

```powershell
cargo mutants -j8                                   # whole crate, hours
cargo mutants -j8 --file tags.rs --file engine.rs   # re-check after writing tests
```

Run both with `run_in_background: true` — even a two-file check pays a tree
copy and a baseline build, and this crate links slint, so each rebuild is tens
of seconds even when the tests themselves take under two.

`--file` matches on the **basename**, and `src/engine/` and `src/batch/` each
contain a `tests.rs` — `--file tests.rs` would hit both. Name the module file
you mean.

## Before a run

`cargo test` must be green. A dirty tree is fine — the copy is of the working
tree, so uncommitted work is what gets measured. The `#[ignore]`d benches
(`bench_rule_application`, `bench_plan_preview`,
`bench_chain_execute_and_finals`) stay ignored under mutants, which is what you
want — they are ~35s of pure loop and would time out every mutant that touches
them.

Nothing else needs setting up: no database, no fixtures, no nested repo to work
around.

## Running it on Linux (WSL)

Some code only exists on one platform, so a Windows-only run can never be the
whole picture. WSL Ubuntu is set up for this:

```powershell
wsl -- bash ~/run_mutants.sh     # history.rs, touch.rs, files.rs -- ~12 min
wsl -- bash ~/full_linux.sh      # everything -- ~55 min
```

The source lives at `~/iron_renamer` in the WSL filesystem (not `/mnt/c`, which
is slow). Re-sync before a run, because nothing watches for edits:

```bash
cd /mnt/c/Users/grego/rust_projects/iron_renamer
tar cf - --exclude=./target --exclude=./.git . | (cd ~/iron_renamer && tar xf -)
```

Three things that cost time the first time:

- **`/tmp` in WSL is a 16 GB tmpfs.** cargo-mutants puts a full source+target
  copy there per job, and eight slint builds overflow it -- the run dies with
  `No space left on device` while `/` still has hundreds of GB free. The
  scripts export `TMPDIR=$HOME/mtmp`. Do not raise `-j` past 8 for this reason,
  whatever `nproc` says.
- **Invoke scripts by name from the WSL home**, with `MSYS_NO_PATHCONV=1`. Git
  Bash rewrites a `/mnt/c/...` argument into `C:/Program Files/Git/mnt/c/...`,
  and nested `$VAR` inside `wsl -- bash -lc '...'` gets mangled. A script file
  in `~` sidesteps both.
- **Do not background with `nohup ... &` inside `bash -lc`.** The `&` binds the
  whole `&&` chain, bash exits, and the run is killed before it writes a log.
  Let the foreground call hold the process open instead.

**Run clippy there too.** CI runs `cargo clippy --locked --all-targets -D
warnings` on ubuntu-latest, and a helper used only by a `#[cfg(windows)]` test
is dead code on Linux. That exact break shipped once and Windows clippy could
not see it.

## While a run is in flight

**Do not compile.** cargo-mutants copies the source tree at the start and
rebuilds inside it; a concurrent `cargo build` at `-j8` inflates the timeout
count and can fail the baseline. Reading and drafting are free.

The log prints a line per *non-caught* mutant only, so a healthy run looks
stalled. Progress is in `mutants.out/*.txt`.

## Reading the results

`mutants.out/` holds `missed.txt`, `caught.txt`, `timeout.txt` and
`unviable.txt`. Rank the misses by file to decide where to go next:

```powershell
Get-Content mutants.out/missed.txt |
    ForEach-Object { ($_ -split ':')[0] } | Group-Object | Sort-Object Count -Desc |
    Select-Object -First 15 Count, Name
```

"failed in an unmutated tree" means the **baseline** failed — a real bug under
parallel load, not flake. Fix it before rerunning.

Compare **percentages**, never raw mutant counts, across dates: the count moves
with the tree.

## Fixture hygiene — the commonest cause of a surviving mutant

Most survivors are not missing tests. They are tests whose *numbers* make two
different operators agree, so an exact-value assertion still fails to
discriminate:

| fixture value | what silently collapses |
|---|---|
| `start = 1`, `step = 1` | `start + i * step`, `start * i` and `i` all agree |
| `pad = 0` or `pad = 1` | width arithmetic degenerates |
| a position of `0` | `pos - 1`, `pos + 1` and `pos` are hard to tell apart |
| a two-item list | `len - 1`, `len / 2` and `1` are the same number |
| an index of `1` | `index * n` and `index` agree |
| one side of a difference `= 0` | `a - b` and `a + b` agree |

**The rule:** every number that flows through arithmetic should be non-zero,
non-unit, and pairwise non-dividing. A counter starting at 7 stepping by 3, not
1 by 1. Five files, not two. A pad of 4, not 1.

**The ordering that matters:** when an arithmetic mutant survives a test that
already asserts an exact value, the first hypothesis is "my fixture is
degenerate", and only the second is "equivalent mutant". Getting that backwards
wastes a re-check cycle and can retire a real gap as unreachable.

For a parameterised relationship — a counter sequence, an insert position, a
substring mod — a `proptest` over many parameter pairs avoids the collision by
construction where a single hand-picked pair cannot. `proptest` is already a
dev-dependency.

## Where it stands (2026-08-31)

Windows and Linux miss different mutants, so the union is the real number.

| file | baseline | Windows | Linux | union |
|---|---:|---:|---:|---:|
| `src/meta.rs` | 59 | **0** | **0** | **0** |
| `src/gui/preview.rs` | 13 | **0** | **0** | **0** |
| `src/gui/callbacks.rs` | 10 | **0** | **0** | **0** |
| `src/presets.rs` | 3 | **0** | **0** | **0** |
| `src/engine/apply.rs` | 1 | **0** | **0** | **0** |
| `src/batch/export.rs` | 1 | **0** | **0** | **0** |
| `src/batch/touch.rs` | 2 | 1 | 1 | **0** |
| `src/tags.rs` | 69 | 2 | 2 | **2** |
| `src/engine/files.rs` | 21 | 4 | 1 | **1** |
| `src/batch/history.rs` | 13 | 5 | 6 | **1** |
| `src/gui/model.rs` | 16 | 1 | 1 | **1** |
| `src/batch.rs` | 26 | 13 | 13 | **13** |
| `src/cli.rs` | 49 | 14 | 14 | **14** |
| `src/gui.rs` | 28 | 28 | 28 | **28** |
| **total** | **311** | **68** | **66** | **60** |

Read the union column, not either platform's. `history.rs` shows 5 and 6 but
intersects at 1: each platform only misses the other's dead `#[cfg]` body.
`touch.rs` shows 1 and 1 and intersects at **zero** — the platforms kill
opposite arms of the same guard.

Note the mutant count moved from 1248 to 1252 when `xorshift64` and `seed_from`
were extracted. Compare percentages across dates, never raw counts.

### Two flaky mutants, and what they meant

A mutant that survives *intermittently* is a test-design problem, not noise.
Both cases here were the test unable to reach an invariant deterministically,
and both were fixed without changing behaviour:

- **`rng`'s `| 1` to `& 1` was clock-flaky.** It died only on an even
  nanosecond reading. `rng()` bundled clock seeding, the never-zero guard and
  the xorshift step into one function, so the only available assertions were
  statistical. Splitting out `seed_from(nanos) -> nanos | 1` and
  `xorshift64(x)` made both directly testable: `seed_from(2) == 3` kills
  `| -> &`, `seed_from(1) != 0` kills `| -> ^`, and a known-answer test against
  Marsaglia's published 13/7/17 sequence pins all ten shift/xor mutants. That
  took `tags.rs` from 14 survivors to 2.
- **`civil_utc`'s `+ doe/36524` to `-` was proptest-flaky.** Only ~0.8% of days
  discriminate that era-correction term, so `civil_dates_roundtrip` killed it
  only when its random draw happened to land on one, and its range stops at
  2100-01-01. Pinned with **2100-03-01** — a century year that is not a leap
  year, which is exactly what the correction exists for.

### What the runs actually found

Two real bugs-in-waiting, both invisible to every other kind of test:

- **`FsKinds::warm` and `warm_parents` survived being replaced with `()`.**
  Pure cache warming: deleting them leaves every result correct and merely
  restores the serial round trips that 0.3.3 and 0.3.4 existed to remove. No
  output assertion can catch that. Fixed by asserting on the private
  `FsKinds::map` (so those tests live in `files.rs`, not `engine/tests.rs`) and
  by deleting a file behind the cache to prove `exists()` does not re-stat.
- **Deleting the `"date"` arm of `sort_files` survived.** It falls through to
  `_ => return false`, so `--sort date` would silently not sort and report that
  it did nothing.

Plus one design question, left as-is and pinned by a test: `--set-meta` and
`--touch` failures print to stderr but **do not change the exit code**
(`cli.rs`, `METADATA FAILED` / `TOUCH FAILED`). A scripted caller checking `$?`
sees success.

### The two fixtures

`meta.rs` reached 100%, but only once both file formats were real.

`tests/fixtures/1x1.jpg` (634 bytes) is what makes the image half of `meta.rs`
testable, and it is not optional: little_exif will happily write a valid APP1
into a `FF D8 FF D9` stub, but **nom-exif only reads EXIF back out of a
structurally valid JPEG**. With it, `set_image`'s arms, `fmt_value`, and
`insert_gps` all became reachable — including GPS written directly through
little_exif, and rationals that are signed, non-unit-denominator, and
zero-denominator.

Watch the denominators: a first attempt used `n/1` throughout, which makes
`/` and `*` the same operation and left the mutants alive. The fixture hygiene
table above is not theoretical.

The video half needs no committed file — `write_mp4` in the tests builds a
577-byte MP4 (`ftyp + moov{mvhd, trak{tkhd, mdia{hdlr}}, meta{keys, ilst}}`)
from struct packing. Two details cost two attempts and are worth keeping in
mind if it ever needs extending: the track needs `mdia/hdlr` = `"vide"` before
nom-exif looks at `tkhd` for dimensions, and QuickTime's `moov/meta` carries
**no version/flags word** (unlike ISO `meta`), without which the `keys`/`ilst`
lookup silently finds nothing. lofty cannot parse this file, which is why the
`DurationMs` arm dies rather than being masked by lofty's own duration.

### What is left, and why it stays

- **`src/gui.rs`, all 28** — the winit block in `run`: backend selector, drag,
  resize, `handle_drop`'s timer. None of it executes under the testing backend.
- **Platform-gated, and now measured on both sides.** `history.rs`'s
  `encode_path`/`decode_path` survivors are perfectly symmetric: each platform
  leaves the *other's* `#[cfg]` body alive (Windows misses lines 82/97, Linux
  misses 87/102), and the live one is caught. Only `unhex`'s `<<4 | x` to `^`
  survives on both, and that is equivalent. `touch.rs`'s `CREATED_OK` guard is
  the same story in miniature: Windows leaves `guard -> false` alive, Linux
  leaves `guard -> true` alive, and the pair kills both.
- **Symlinks need Linux, or a junction.** Windows refuses file symlinks without
  Developer Mode, but a **directory junction** needs no privilege and Rust
  reports it as a symlink -- a *dangling* junction is the discriminating case
  (still a link, no longer a folder), which kills the folder halves of those
  branches. The file halves need real symlinks, so they are covered by a
  `#[cfg(unix)]` test under WSL. Between the two, only
  `files.rs:109:35` survives.
- **`files.rs:109:35`** (`exists`'s empty-parent guard) is killable only by
  resolving a bare name against the process working directory. `set_current_dir`
  is global and tests run in parallel, so this one mutant is not worth a flaky
  suite.
- **Environment-bound** — `transfer`'s `CrossesDevices` fallback needs two
  volumes; `cli.rs`'s `registry` writes to HKCU and is deliberately left alone
  (it survives on both platforms for different reasons: on Linux the Windows
  body is dead, on Windows the stub is).
- **One flaky mutant**: `tags.rs:482:17 | with &` reseeds the RNG from
  `nanos & 1`, so it dies only when the clock happens to be even. It shows as
  caught on some runs and missed on others; do not read a per-run change there
  as progress.
- **`execute`'s cycle breaker** (~10) — cycles, three-way rotations and
  duplicate sources are now covered by contract tests, but the remaining
  mutants look equivalent on inspection: `tmp_n += 1` to `*=` still yields a
  unique temp name while only one file is aside, and `sources.entry(..) += 1`
  to `*=` leaves 0 vs 1, which `vacate` treats through the same arm.
- **Confirmed equivalent mutants** — read the code before chasing any of these:
  `alpha`'s `n < 1` to `<=`, `nibble`'s `<<4 | x` to `^`, `modify`'s `n < 0` to
  `<=`, `remove_rule`'s `e > i` to `>=`, `civil_utc`'s `+ doe/36524` to `-`,
  `plan`'s `> MAX_WINDOWS_PATH_LEN` to `>=`.
- **`rng`'s bit twiddles** — the `<<`/`>>` swaps still produce a varying
  stream, so no distribution assertion separates them. The stuck and saturating
  cases are killed by asserting a 32-character `<rands>` has five distinct
  characters.

Also normal and not worth chasing: clamps nothing reaches, `<` vs `<=` where
both agree at the boundary, and print/`eprintln` bodies mutated to `()` where
output is not captured. If a printer's content matters, splitting `print_x(..)`
into `format_x(..) -> String` plus a one-line printer turns it into an ordinary
value test.

After writing tests, re-check with one `--file`-per-file invocation listing
every file at once, and expect two or three rounds before a file settles.
