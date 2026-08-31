---
name: find-hotspots
description: Profile iron_renamer with samply and rank the hottest functions by CPU time. Use when asked where the time goes, what is slow, to find or confirm a performance hotspot, or to check whether an optimisation actually helped rather than guessing from a benchmark number alone.
---

# Finding hotspots

`samply` records a sampling profile; `hotspots.mjs` turns it into a ranked list
of function names. The list is the point — samply's own output is a browser UI
and a `profile.json.gz` full of raw hex addresses.

Paths below are relative to the repo root.

## Before you reach for a profiler

**This app's worst slowdowns have been I/O latency, not CPU.** 0.3.3 and 0.3.4
were both fixed by batching per-folder filesystem calls and moving them off the
UI thread — on a network share each `metadata()` costs ~25ms and the CPU is
parked in a wait stub the whole time. A sampling profile of that shows an idle
thread and tells you nothing. If the complaint involves a network path or a
large drop, count filesystem calls first (`Grep` for `metadata`/`read_dir` on
the path in question); profile only once you know the work is real CPU.

Genuine CPU targets here: rule application over a long list (`src/engine/`),
preview planning and collision resolution (`src/batch.rs`), tag expansion and
hashes (`src/tags.rs`, `<md5>`/`<sha1>`/`<crc32>` read every byte).

## 1. Build with symbols

`[profile.release]` is cargo's default here — no `strip`, but also `debug =
false`, so release frames resolve to addresses. Add a profiling profile to
`Cargo.toml`:

```toml
[profile.profiling]
inherits = "release"
debug = 1
```

```powershell
cargo build --profile profiling
```

Its own target dir means switching back to `cargo build --release` costs no
rebuild. For a genuine one-off, `$env:CARGO_PROFILE_RELEASE_DEBUG=1; cargo
build --release` avoids touching `Cargo.toml` at the price of rebuilding
release twice.

## 2. Record

```powershell
samply record --save-only -o hot.json.gz -- ./target/profiling/iron_renamer.exe -p "<num>_<md5>.<ext>" --in D:\big-folder --recurse
```

Profile the thing you actually suspect, over enough files that it dominates
startup — a preview of thousands of items, not a directory of six. A run that
does not exercise the suspect path produces a profile of `clap` and process
startup.

The bench tests are ready-made CPU-bound targets. They are `#[ignore]`d, so
build the test binary and drive it directly:

```powershell
cargo test --profile profiling --no-run            # prints the binary path
samply record --save-only -o hot.json.gz -- ./target/profiling/deps/iron_renamer-abc123.exe --ignored --nocapture bench_plan_preview
```

`bench_rule_application`, `bench_plan_preview` and
`bench_chain_execute_and_finals` are the three that exist.

The GUI can be recorded the same way, but a profile of it is mostly the
renderer waiting for vsync. Reproduce the slow operation through the CLI where
you can; only profile the GUI when the cost is provably in `src/gui/`.

## 3. Rank

```bash
node .claude/skills/find-hotspots/hotspots.mjs hot.json.gz --module iron_renamer.exe --top 20
```

```
hot.json.gz — 12.4 s of CPU across 5 threads

CPU by thread
     9820 ms  79.2%  iron_renamer.exe (49620)

Self time — top 20 in iron_renamer.exe
      172 ms   2.3%  regex_automata::meta::regex::Regex::find_iter()  (regex.rs:1163)
      ...
```

- **Self time** is where the CPU actually is — start here.
- **Total (inclusive) time** says which caller owns it. Near-100% entries at
  the top are just `main` and the thread starts.
- `--module iron_renamer.exe` filters by binary — without it the top of the
  list is `ntdll` wait stubs.
- `--thread <substr>` restricts everything to one thread. The GUI's folder
  scan runs off the UI thread, so check the thread table before aggregating.
- Weights are CPU microseconds, not sample counts: most samples in a GUI or
  I/O-bound profile are parked in `NtWaitForMultipleObjects`, and counting
  them would rank the parked threads first.

A worked example: a 6000-file preview with a regex rule, a case rule and a
`<num>`/`<rands>` pattern spends **0.6 s of CPU in total**, and its top self-time
entries are `File::open_native`, `EncodeWide` and `PathBuf::push` — Windows
path and syscall overhead, not rule matching. That is the shape to expect. It
is also the reminder above in miniature: on a local disk this app is already
fast enough that the profile is mostly syscall entry, and on a share the same
run is dominated by wait time the profiler will not show you at all.

To compare before and after, record twice over the same input and diff the two
self-time lists. Absolute ms only compares across runs of equal length;
percentages are safer.

## Gotchas

- **Symbolicate before rebuilding.** samply stores raw addresses and resolves
  them lazily against the PDB. Rebuild the binary and the build id no longer
  matches, so every name in an older profile falls back to `17:18439557`
  forever. If that is what you see, the profile is stale — re-record.
- **Regex and `walkdir`-shaped work near the top is expected.** Masks compile
  to regexes once (0.3.4) and rules match per file; before "optimising" one,
  check it is not simply being called in a loop that could hoist it.
- **System DLLs stay unresolved** unless you pass a symbol server:
  `--windows-symbol-server https://msdl.microsoft.com/download/symbols` on
  `samply load`/`record`. Rarely worth it; `--module` is the answer to
  `ntdll.dll` noise.
- **`hotspots.mjs` starts `samply load` on port 3579** to reach its
  `/symbolicate/v5` endpoint, and kills it on the way out. `--port` if that
  clashes. It never opens a browser. It also passes `--symbol-dir` for every
  directory the profile loaded a module from — the profile names each PDB by
  bare filename, so without that samply looks in the cwd and resolves nothing.
- **Inlined callees are attributed to the caller.** A "missing" function in a
  release build was almost certainly inlined; look for its caller.
- **`samply record` wedges if a recording is orphaned.** When a record dies
  without its child, every later `samply record` hangs at startup or fails with
  `The handle is invalid`. Kill both leftovers and retry:

  ```powershell
  Stop-Process -Name samply,iron_renamer -Force -ErrorAction SilentlyContinue
  samply record --save-only -o probe.json.gz -- cmd /c ver
  ```

- **Numbers from the first run after a build are not a baseline.** A first
  measurement can come out several times slower than the same binary measured
  minutes later. Always take before/after back to back, and repeat the baseline
  once.

## Human path

```powershell
samply record ./target/profiling/iron_renamer.exe --in D:\big-folder -p "<num>.<ext>"
```

Opens the Firefox Profiler UI with a flame graph, which is better than any
text list for *shape* — useless to an agent, which cannot read it.
