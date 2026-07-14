// CLI front-end. Preview by default; --apply renames and records the batch
// in a dated history for selective undo. Planning, validation, execution,
// and history live in batch.rs, shared with the GUI.

use crate::{
    batch::{self, Op},
    engine::*,
    presets,
};
use std::{fs, path::PathBuf, process::exit};

const USAGE: &str = "\
iron_renamer — batch file renamer (run with no arguments for the GUI)

USAGE:
  iron_renamer [RULES] [OPTIONS] <files or globs>...
  iron_renamer history         list applied batches
  iron_renamer undo [ID]       revert a batch (the latest if no ID)

RULES (applied in order given). Every rule flag takes suffix mods, e.g.
-r:ci:first — ':name' or ':ext' limits a rule to the stem or the extension
(default: the whole name).

  -r, --replace <OLD> <NEW>    literal replace; mods: ci (ignore case),
                               first | last | n<N> (occurrence; default all)
  -e, --regex <PAT> <REPL>     regex replace ($1, $2 for groups)
  -c, --case <MODE>            lower | upper | title | first | invert
  -p, --pattern <PAT>          rebuild the name from a tag template
  -i, --insert <TEXT> <POS>    insert text (tags ok) at POS
      --remove <WHAT>          TEXT | re:PAT | pos:START,LEN | chars:LIST |
                               digits | upper | lower | diacritics
  -t, --trim <CHARS>           trim chars ('' = whitespace); mods: start |
                               end | both (default) | all, inv (inverse set)
      --renumber <NTH> <SPEC>  change the NTH number: +N | -N (shift) or
                               START[/STEP] (resequence); mod: pad<N>
      --move <PAT> <POS>       move first match (re:PAT for regex) to POS
      --swap <SEP>             swap around first separator: 'a - b' -> 'b - a'
      --names <FILE>           one new name per line, matched to list order
      --if <COND> <VALUE>      condition on the previous rule:
                               [not:]<name|new|ext|path>:<has|starts|ends|eq|re>

  POS:  start | end | N | -N | before:TEXT | after:TEXT | rbefore:PAT | rafter:PAT
  TAGS (in pattern/insert/replacement text; <tag[:args][|mod]...>):
        <name> <ext> <oname> <oext> <index> <parent> <path> <size[:kb|mb]>
        <crc32> <rand[:MIN[:MAX]]> <rands[:LEN]>
        counters, all take :START:STEP -- <num> <hex> <alpha> <roman>
        <dirnum> (resets per folder)
        dates (UTC) -- <now|created|modified[:FMT[:OFFSET]]>, FMT tokens
        yyyy yy MM dd HH mm ss, OFFSET like +3d -12h
        metadata (needs ExifTool on PATH or IRON_RENAMER_EXIFTOOL) --
        <exif:TAG> plus <width> <height> <datetaken> <artist> <album>
        <track> <title> <duration> <author>
  MODS: |upper |lower |title |sub:START[,LEN] |pad:N |trim[:CHARS]
        |replace:OLD[,NEW] |fallback:TEXT |+N |-N |*N |/N

OPTIONS:
  --start <N>                  counter start (default 1)
  --pad <N>                    zero-pad width (default: fits the largest number)
  --copy-to <DEST>             copy instead of renaming; DEST is a folder
                               template (tags ok, e.g. \"sorted\\<ext>\"),
                               relative to each file; dirs are created
  --move-to <DEST>             move instead of renaming (same DEST rules)
  --collide <POLICY>           on name collision: fail (default) | number
                               (\"name (2)\") | letter (\"name_b\") | any other
                               value = tag pattern appended to the stem
  --preset <FILE|NAME>         load rules and settings from a saved preset
                               (bare names look in the preset folder)
  --export <FILE>              write the preview — or, with --apply, the
                               results — to FILE (.csv, .json, or text)
  --in <DIR>                   take files from DIR (repeatable)
  --recurse                    make --in recursive
  --mask <MASKS>               filter --in files: \"*.jpg;*.png;!*thumb*\"
  --list <FILE>                take files from a list, one path per line
                               (keeps list order unless --sort is given)
  --sort <name|ext|size|date|none>   sort order (default: natural by name)
  --desc                       reverse the sort order
  --pairs                      file-pair mode: same-stem sidecars (img1.jpg +
                               img1.xmp) take the same new stem
  --touch <WHICH=VALUE>        set timestamps after the batch; WHICH: comma
                               list of created|modified|accessed or all;
                               VALUE: \"2024-05-01 10:30\" | +3d | -2h |
                               name | parent | exif (dates are UTC)
  -d, --dirs                   rename folders instead of files
  -x, --apply                  actually rename (otherwise preview only)

EXAMPLES:
  iron_renamer -r \" \" \"_\" *.mp3
  iron_renamer -c:ext lower -p \"photo_<num>.<ext>\" --pad 3 *.jpg -x
  iron_renamer -i:name \"<parent>_\" start --if ext:eq jpg *.* -x
  iron_renamer --renumber 1 +100 --remove:name \" copy\" *.mkv";

pub fn run(args: Vec<String>) {
    match args[0].as_str() {
        "undo" => return undo(args.get(1)),
        "history" => return history(),
        _ => {}
    }

    let mut rules: Vec<RuleEntry> = Vec::new();
    let mut files: Vec<PathBuf> = Vec::new();
    let mut apply = false;
    let mut start: usize = 1;
    let mut pad: usize = 0;
    let mut mode = batch::Mode::Rename;
    let mut dest = String::new();
    let mut collision = batch::Collision::Fail;
    let mut export: Option<PathBuf> = None;
    let mut sort: Option<String> = None;
    let mut desc = false;
    let mut from_list = false;
    let mut pairs = false;
    let mut touch: Option<batch::TouchSpec> = None;
    // Pre-scan flags that must apply regardless of argument order.
    let dirs = args.iter().any(|a| a == "-d" || a == "--dirs");
    let recurse = args.iter().any(|a| a == "--recurse");
    let masks = Masks::parse(
        args.iter()
            .position(|a| a == "--mask")
            .and_then(|i| args.get(i + 1))
            .map(String::as_str)
            .unwrap_or(""),
    );

    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let need = |n: usize, it: &mut dyn Iterator<Item = String>| -> Vec<String> {
            let v: Vec<String> = it.take(n).collect();
            if v.len() < n {
                die(&format!("'{a}' needs {n} argument(s)"));
            }
            v
        };
        // Rule flags carry suffix mods: -r:ci:first, --case:ext, ...
        let (flag, mod_str) = a.split_once(':').unwrap_or((a.as_str(), ""));
        let mods: Vec<&str> = mod_str.split(':').filter(|m| !m.is_empty()).collect();
        let mut rule = |kind: &str, a: &str, b: &str| match build_rule(kind, &mods, a, b) {
            Ok((rule, part)) => rules.push(RuleEntry {
                rule,
                part,
                cond: None,
            }),
            Err(e) => die(&e),
        };
        match flag {
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            "-x" | "--apply" => apply = true,
            "-d" | "--dirs" => {}
            "-r" | "--replace" => {
                let v = need(2, &mut it);
                rule("replace", &v[0], &v[1]);
            }
            "-e" | "--regex" => {
                let v = need(2, &mut it);
                rule("regex", &v[0], &v[1]);
            }
            "-c" | "--case" => {
                let v = need(1, &mut it);
                rule("case", &v[0], "");
            }
            "-p" | "--pattern" => {
                let v = need(1, &mut it);
                rule("pattern", &v[0], "");
            }
            "-i" | "--insert" => {
                let v = need(2, &mut it);
                rule("insert", &v[0], &v[1]);
            }
            "--remove" => {
                let v = need(1, &mut it);
                rule("remove", &v[0], "");
            }
            "-t" | "--trim" => {
                let v = need(1, &mut it);
                rule("trim", &v[0], "");
            }
            "--renumber" => {
                let v = need(2, &mut it);
                rule("renumber", &v[0], &v[1]);
            }
            "--move" => {
                let v = need(2, &mut it);
                rule("move", &v[0], &v[1]);
            }
            "--swap" => {
                let v = need(1, &mut it);
                rule("swap", &v[0], "");
            }
            "--names" => {
                let v = need(1, &mut it);
                let list = fs::read_to_string(&v[0])
                    .unwrap_or_else(|e| die(&format!("cannot read names file '{}': {e}", v[0])));
                rule("names", &list, "");
            }
            "--if" => {
                let v = need(2, &mut it);
                let cond = build_cond(&v[0], &v[1]).unwrap_or_else(|e| die(&e));
                match rules.last_mut() {
                    Some(entry) => entry.cond = Some(cond),
                    None => die("--if must follow the rule it applies to"),
                }
            }
            "--preset" => {
                let v = need(1, &mut it);
                let preset = presets::load(&presets::resolve(&v[0])).unwrap_or_else(|e| die(&e));
                for (kind, mod_str, a, b) in &preset.rules {
                    let m: Vec<&str> = mod_str.split(':').filter(|x| !x.is_empty()).collect();
                    match build_rule(kind, &m, a, b) {
                        Ok((rule, part)) => rules.push(RuleEntry {
                            rule,
                            part,
                            cond: None,
                        }),
                        Err(e) => die(&format!("preset rule: {e}")),
                    }
                }
                let get = |k: &str| preset.settings.get(k).map(String::as_str).unwrap_or("");
                if let Ok(n) = get("start").parse() {
                    start = n;
                }
                if let Ok(n) = get("pad").parse() {
                    pad = n;
                }
                match get("mode") {
                    "copy" => mode = batch::Mode::Copy,
                    "move" => mode = batch::Mode::Move,
                    _ => {}
                }
                if !get("dest").is_empty() {
                    dest = get("dest").to_string();
                }
                collision = match get("collide") {
                    "number" => batch::Collision::Number,
                    "letter" => batch::Collision::Letter,
                    "pattern" => {
                        let p = get("collide_pattern");
                        batch::Collision::Pattern(if p.is_empty() {
                            "_<num>".into()
                        } else {
                            p.into()
                        })
                    }
                    _ => collision,
                };
            }
            "--export" => {
                let v = need(1, &mut it);
                export = Some(PathBuf::from(&v[0]));
            }
            "--in" => {
                let v = need(1, &mut it);
                let dir = PathBuf::from(&v[0]);
                if !dir.is_dir() {
                    die(&format!("--in: '{}' is not a folder", v[0]));
                }
                collect_dir(&dir, recurse, &masks, &mut files);
            }
            "--recurse" => {}
            "--mask" => {
                need(1, &mut it); // consumed in the pre-scan
            }
            "--list" => {
                let v = need(1, &mut it);
                let body = fs::read_to_string(&v[0])
                    .unwrap_or_else(|e| die(&format!("cannot read list '{}': {e}", v[0])));
                for line in body.lines().map(str::trim).filter(|l| !l.is_empty()) {
                    let p = PathBuf::from(line);
                    if p.exists() {
                        files.push(p);
                    } else {
                        eprintln!("warning: '{line}' not found, skipped");
                    }
                }
                from_list = true;
            }
            "--sort" => {
                let v = need(1, &mut it);
                if !matches!(v[0].as_str(), "name" | "ext" | "size" | "date" | "none") {
                    die("--sort takes name|ext|size|date|none");
                }
                sort = Some(v[0].clone());
            }
            "--desc" => desc = true,
            "--pairs" => pairs = true,
            "--touch" => {
                let v = need(1, &mut it);
                touch = Some(batch::parse_touch(&v[0]).unwrap_or_else(|e| die(&e)));
            }
            "--copy-to" => {
                let v = need(1, &mut it);
                (mode, dest) = (batch::Mode::Copy, v[0].clone());
            }
            "--move-to" => {
                let v = need(1, &mut it);
                (mode, dest) = (batch::Mode::Move, v[0].clone());
            }
            "--collide" => {
                let v = need(1, &mut it);
                collision = match v[0].as_str() {
                    "fail" => batch::Collision::Fail,
                    "number" => batch::Collision::Number,
                    "letter" => batch::Collision::Letter,
                    pat => batch::Collision::Pattern(pat.to_string()),
                };
            }
            "--start" => {
                let v = need(1, &mut it);
                start = v[0]
                    .parse()
                    .unwrap_or_else(|_| die("--start needs a number"));
            }
            "--pad" => {
                let v = need(1, &mut it);
                pad = v[0].parse().unwrap_or_else(|_| die("--pad needs a number"));
            }
            _ => files.extend(expand(&a, dirs)),
        }
    }

    if rules.is_empty() && dest.is_empty() && touch.is_none() {
        die("no rules given (see --help)");
    }
    if files.is_empty() {
        die(if dirs {
            "no folders matched"
        } else {
            "no files matched"
        });
    }

    // One batch never mixes files and folders.
    for f in &files {
        if dirs && !f.is_dir() {
            die(&format!(
                "'{}' is not a folder (drop --dirs to rename files)",
                f.display()
            ));
        }
        if !dirs && f.is_dir() {
            die(&format!(
                "'{}' is a folder (use --dirs to rename folders)",
                f.display()
            ));
        }
        if !dirs && !f.is_file() {
            die(&format!("'{}' not found", f.display()));
        }
    }

    // Natural name sort by default so photo_9 numbers before photo_10;
    // a --list keeps its own order unless a sort is asked for.
    let sort = sort.unwrap_or_else(|| {
        if from_list {
            "none".into()
        } else {
            "name".into()
        }
    });
    match sort.as_str() {
        "name" => files.sort_by_key(|f| natural_key(&name_of(f))),
        "ext" => files.sort_by_key(|f| split_ext(&name_of(f)).1.to_lowercase()),
        "size" => files.sort_by_key(|f| fs::metadata(f).map(|m| m.len()).unwrap_or(0)),
        "date" => files.sort_by_key(|f| fs::metadata(f).and_then(|m| m.modified()).ok()),
        _ => {}
    }
    if desc {
        files.reverse();
    }
    let mut seen = std::collections::HashSet::new();
    files.retain(|f| seen.insert(f.clone()));

    if pad == 0 {
        pad = (start + files.len() - 1).to_string().len();
    }

    let overrides = std::collections::HashMap::new();
    let cfg = batch::BatchCfg {
        rules: &rules,
        start,
        pad,
        overrides: &overrides,
        mode,
        dest: &dest,
        collision,
        pairs,
    };
    let items = batch::plan(&files, &cfg);
    // With --apply the export becomes a result log, written after execution.
    if let Some(path) = &export
        && !apply
    {
        match batch::export_preview(&items, path) {
            Ok(_) => println!("preview exported to {}", path.display()),
            Err(e) => die(&format!("export failed: {e}")),
        }
    }
    let (verb, done) = match mode {
        batch::Mode::Rename => ("rename", "renamed"),
        batch::Mode::Copy => ("copy", "copied"),
        batch::Mode::Move => ("move", "moved"),
    };
    let mut ops: Vec<Op> = Vec::new();
    let mut conflicts = 0;
    for item in items.iter().filter(|i| i.changed) {
        // Renames stay in place, so the name is enough; copy/move show the target path.
        let shown = if mode == batch::Mode::Rename && dest.is_empty() {
            item.new_name.clone()
        } else {
            item.target.display().to_string()
        };
        match &item.issue {
            Some(e) => {
                conflicts += 1;
                println!("{}  ->  {shown}   [{e}]", item.from.display());
            }
            None => {
                println!("{}  ->  {shown}", item.from.display());
                ops.push(item.op());
            }
        }
    }

    if ops.is_empty() && conflicts == 0 && touch.is_none() {
        println!("nothing to {verb} ({} item(s) unchanged)", files.len());
        return;
    }

    if !apply {
        println!(
            "\npreview only — re-run with --apply to {verb} {} item(s)",
            ops.len()
        );
        if touch.is_some() {
            println!("timestamps will be set on {} item(s)", files.len());
        }
        if conflicts > 0 {
            eprintln!("{conflicts} conflict(s) must be fixed first");
        }
        return;
    }
    if conflicts > 0 {
        die(&format!("{conflicts} conflict(s) above — nothing {done}"));
    }

    let planned = ops.len();
    let res = batch::execute(ops, mode);
    if mode != batch::Mode::Copy
        && let Err(e) = batch::record(&res.renamed)
    {
        eprintln!("warning: could not write batch history: {e}");
    }
    for (op, e) in &res.failed {
        eprintln!("FAILED {} -> {}: {e}", op.from.display(), op.to.display());
    }
    if let Some(path) = &export {
        match batch::export_results(&res, path) {
            Ok(_) => println!("result log written to {}", path.display()),
            Err(e) => eprintln!("warning: could not write result log: {e}"),
        }
    }
    // Timestamps go on every item at its final location (copies included).
    if let Some(spec) = &touch {
        let finals: Vec<PathBuf> = items
            .iter()
            .map(|it| {
                res.renamed
                    .iter()
                    .find(|op| op.from == it.from)
                    .map(|op| op.to.clone())
                    .unwrap_or_else(|| it.from.clone())
            })
            .collect();
        let (n, errors) = batch::apply_touch(&finals, spec);
        println!("timestamps set on {n} item(s)");
        for e in &errors {
            eprintln!("TOUCH FAILED {e}");
        }
    }
    if planned > 0 {
        println!("\n{done} {} of {planned} item(s)", res.renamed.len());
    }
    if !res.failed.is_empty() {
        eprintln!(
            "{} failed — re-run the same command to retry them",
            res.failed.len()
        );
        exit(1);
    }
    if planned > 0 && mode != batch::Mode::Copy {
        println!("'iron_renamer undo' reverts this batch");
    }
}

fn undo(id_arg: Option<&String>) {
    let id = id_arg.map(|s| {
        s.parse()
            .unwrap_or_else(|_| die("undo ID must be a number (see 'iron_renamer history')"))
    });
    match batch::undo(id) {
        Ok((reverted, errors)) => {
            println!("reverted {} item(s)", reverted.len());
            for e in &errors {
                eprintln!("FAILED {e}");
            }
            if !errors.is_empty() {
                eprintln!("{} kept in history — run undo again to retry", errors.len());
                exit(1);
            }
        }
        Err(e) => die(&e),
    }
}

fn history() {
    let batches = batch::history();
    if batches.is_empty() {
        println!("no batch history");
        return;
    }
    println!("{:<15} {:<18} ITEMS", "ID", "DATE");
    for (id, date, n) in batches {
        println!("{id:<15} {date:<18} {n}");
    }
    println!("\n'iron_renamer undo <ID>' reverts a batch");
}

fn die(msg: &str) -> ! {
    eprintln!("error: {msg}");
    exit(1)
}
