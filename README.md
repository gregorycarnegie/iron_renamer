# Iron Renamer

[![Rust 2024](https://img.shields.io/badge/Rust-2024-orange?logo=rust)](https://www.rust-lang.org/)
[![Slint 1.17](https://img.shields.io/badge/GUI-Slint_1.17-2379f4)](https://slint.dev/)
![CLI and GUI](https://img.shields.io/badge/interfaces-CLI_%2B_GUI-555)

Batch file renamer in Rust.
One binary, two faces: run with no arguments for the GUI (Slint), with arguments for the CLI.

## GUI

Run the binary with no arguments:

```
cargo run --release          # from the repo
iron_renamer                 # or the built exe / after cargo install --path .
```

- Load files with **＋ Files** / **＋ From folder** (with optional recursion and
  `*.jpg;!*thumb*` masks), drag-and-drop from Explorer, or rename folders
  themselves with **＋ Folders** (a batch is either all files or all folders,
  never mixed). Save/load the list as plain text.
- Click a row to select it: reorder with ▲/▼, remove it, or type a manual
  new name that bypasses the rules. Search filters the view; sort by
  name/ext/size/date in either direction. Numbering follows list order.
- Click a rule in the stack to edit it in the form (the button becomes
  **✓ Save rule**); click it again or **✕ cancel edit** to back out.
- Preview is live — every edit recomputes the table. Conflicts (duplicate targets,
  name already on disk, reserved Windows names, over-long paths) show per-row
  in red and are skipped on rename.
- Output modes: rename in place, or **Copy**/**Move** to a tag-expanded
  destination folder (created automatically). Collision policy: fail,
  "name (2)", "name_b", or a tag pattern. Save/load the rule stack as a
  preset (the CLI runs the same files via `--preset`), import a CSV of
  old/new names, or export the preview as text/CSV/JSON.
- **Rename N files** applies the batch; **Undo last batch** reverts it
  (moves included; copies are not undoable). Batches are recorded in a
  dated history, so undo works across restarts too.
- Shortcuts (when no text field has focus): Ctrl+O add files · Ctrl+F search ·
  ↑/↓ select · Ctrl+↑/↓ reorder · Del remove · F2 manual override ·
  Ctrl+Z undo · Ctrl+Enter start · Esc deselect.

## CLI

Preview by default; nothing is touched until `-x/--apply`.

```
iron_renamer [RULES] [OPTIONS] <files or globs>...
iron_renamer history         # list applied batches with dates and IDs
iron_renamer undo [ID]       # revert a batch (the latest if no ID)

RULES (applied in order given). Every rule flag takes suffix mods, e.g.
-r:ci:first — ':name' or ':ext' limits a rule to the stem or the extension
(default: the whole name).

  -r, --replace <OLD> <NEW>    literal replace; mods: ci, first|last|n<N>
  -e, --regex <PAT> <REPL>     regex replace ($1, $2 for groups)
  -c, --case <MODE>            lower | upper | title | first | invert
  -p, --pattern <PAT>          rebuild the name from a tag template
  -i, --insert <TEXT> <POS>    insert text (tags ok) at POS
      --remove <WHAT>          TEXT | re:PAT | pos:START,LEN | chars:LIST |
                               digits | upper | lower | diacritics
  -t, --trim <CHARS>           mods: start|end|both|all, inv ('' = whitespace)
      --renumber <NTH> <SPEC>  +N | -N (shift) or START[/STEP]; mod: pad<N>
      --move <PAT> <POS>       move first match (re:PAT for regex) to POS
      --swap <SEP>             swap around first separator: 'a - b' -> 'b - a'
      --names <FILE>           one new name per line, matched to list order
      --replace-list <FILE>    OLD=NEW pairs, one per line, applied in order;
                               mod: ci (tab also splits, so OLD may hold '=')
      --js <SCRIPT|FILE>       sandboxed JavaScript; last expression = new name
      --if <COND> <VALUE>      condition on the previous rule:
                               [not:]<name|new|ext|path>:<has|starts|ends|eq|re>

  POS:  start | end | N | -N | before:TEXT | after:TEXT | rbefore:PAT | rafter:PAT
  TAGS: <tag[:args][|modifier]...> in pattern/insert/replacement text
        <name> <ext> <oname> <oext> <index> <total> <parent> <path>
        <subfolder[:N]> (Nth ancestor folder, 1 = parent)
        <size[:kb|mb|gb|tb|h]> ('h' = "1.4 GB") <crc32> <md5> <sha1>
        <rand[:MIN[:MAX]]> <rands[:LEN]> <csv:COL> (see --csv)
        counters (:START:STEP): <num> <hex> <alpha> <roman> <dirnum>
        dates (UTC): <now|created|modified|accessed[:FMT[:OFFSET]]>
          FMT tokens yyyy yy MM dd HH mm ss, or the literal FMT unix
          for epoch seconds · OFFSET like +3d -12h
        metadata (needs ExifTool on PATH or IRON_RENAMER_EXIFTOOL):
          <exif:TAG> plus <width> <height> <datetaken> <artist> <album>
          <track> <title> <duration> <author> <lat> <lon>
  MODS: |upper |lower |title |sub:START[,LEN] |pad:N |trim[:CHARS]
        |replace:OLD[,NEW] |split:SEP,N (empty SEP = whitespace, N<0
        from the end) |fallback:TEXT |+N |-N |*N |/N

OPTIONS:
  --start <N>                  counter start (default 1)
  --pad <N>                    zero-pad width (default: fits the largest number)
  --copy-to / --move-to <DEST> copy or move; DEST is a folder template
                               (tags ok, relative to each file), dirs created
  --collide <POLICY>           fail | number ("name (2)") | letter ("name_b")
                               | anything else = tag pattern suffix
  --preset <FILE|NAME>         load rules/settings from a saved preset
  --export <FILE>              write the preview — or, with --apply, the
                               result log (.csv, .json, or text)
  --in <DIR> [--recurse]       take files from DIR; --mask "*.jpg;!*thumb*"
  --list <FILE>                take files from a list (keeps its order)
  --csv <FILE>                 rows for the <csv:COL> tag; COL is a 1-based
                               column number (row = list position) or a
                               header name (row 0 = headers)
  --sort <name|ext|size|date|none> [--desc]
  --pairs                      same-stem sidecars share the generated stem
  --touch <WHICH=VALUE>        set created|modified|accessed|all timestamps:
                               "2024-05-01 10:30" | +3d | name | parent | exif
  --set-meta <TAG=VALUE>       write a metadata tag after the batch
                               (needs ExifTool; repeatable)
  -d, --dirs                   rename folders instead of files
  -x, --apply                  actually rename (otherwise preview only)
```

Examples:

```
iron_renamer -r " " "_" *.mp3
iron_renamer -e "(\d+)" "ep$1" *.mkv
iron_renamer -c:ext lower -p "trip_<num>.<ext>" --pad 3 *.jpg -x
iron_renamer -i:name "<parent>_" start --if ext:eq jpg *.* -x
iron_renamer --renumber 1 +100 --remove:name " copy" *.mkv
```

Globs (`*`, `?`) are expanded internally, so they work from PowerShell too.
Files are natural-sorted (`img9` before `img10`) before numbering.

Names are validated before anything is renamed (empty/invalid names, reserved
Windows names like `CON`, trailing dots/spaces, over-long paths); case-only
renames are allowed. Chains (`1→2, 2→3`) are ordered automatically and swap
cycles are broken with temporary names. A failed item leaves its file
untouched — re-run the same command to retry. Applied batches are appended to
`%LOCALAPPDATA%\iron_renamer\history.tsv` for `history`/`undo`.

## Build

```
cargo build --release        # -> target/release/iron_renamer.exe
cargo test
```

## Layout

| Path             | What                                        |
|------------------|---------------------------------------------|
| `src/engine.rs`  | Rule engine (rules, shared rule parsing, natural sort, globbing) + tests |
| `src/tags.rs`    | Tag parser shared by pattern/insert/replacement text + tests |
| `src/batch.rs`   | Shared planner/executor: validation, collision policies, chain/swap-safe rename/copy/move, dated undo history, preview export + tests |
| `src/presets.rs` | Preset files and CSV/JSON helpers + tests          |
| `src/cli.rs`     | CLI front-end                                |
| `src/gui.rs`     | GUI state and callbacks                      |
| `ui/main.slint`  | All UI markup and styling                    |

## Not included (on purpose)

GPS reverse geocoding (city/country names need an online database — `<lat>`,
`<lon>`, and `<exif:TAG>` cover the offline part).
The rule engine is the extension point — a new `Rule` variant in `engine.rs`
shows up in both front-ends.
