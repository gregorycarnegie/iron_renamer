// CLI front-end. Preview by default; --apply renames and records the batch
// in a dated history for selective undo. Planning, validation, execution,
// and history live in batch.rs, shared with the GUI.

use crate::batch::{self, Op};
use crate::engine::*;
use std::fs;
use std::path::PathBuf;
use std::process::exit;

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
  MODS: |upper |lower |title |sub:START[,LEN] |pad:N |trim[:CHARS]
        |replace:OLD[,NEW] |fallback:TEXT |+N |-N |*N |/N

OPTIONS:
  --start <N>                  counter start (default 1)
  --pad <N>                    zero-pad width (default: fits the largest number)
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
    // Pre-scan so --dirs applies to globs regardless of argument order.
    let dirs = args.iter().any(|a| a == "-d" || a == "--dirs");

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
            Ok((rule, part)) => rules.push(RuleEntry { rule, part, cond: None }),
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
            "--start" => {
                let v = need(1, &mut it);
                start = v[0].parse().unwrap_or_else(|_| die("--start needs a number"));
            }
            "--pad" => {
                let v = need(1, &mut it);
                pad = v[0].parse().unwrap_or_else(|_| die("--pad needs a number"));
            }
            _ => files.extend(expand(&a, dirs)),
        }
    }

    if rules.is_empty() {
        die("no rules given (see --help)");
    }
    if files.is_empty() {
        die(if dirs { "no folders matched" } else { "no files matched" });
    }

    // One batch never mixes files and folders.
    for f in &files {
        if dirs && !f.is_dir() {
            die(&format!("'{}' is not a folder (drop --dirs to rename files)", f.display()));
        }
        if !dirs && f.is_dir() {
            die(&format!("'{}' is a folder (use --dirs to rename folders)", f.display()));
        }
        if !dirs && !f.is_file() {
            die(&format!("'{}' not found", f.display()));
        }
    }

    // Natural sort so photo_9 numbers before photo_10.
    files.sort_by_key(|f| natural_key(&name_of(f)));

    if pad == 0 {
        pad = (start + files.len() - 1).to_string().len();
    }

    let items = batch::plan(&files, &rules, start, pad, &std::collections::HashMap::new());
    let mut ops: Vec<Op> = Vec::new();
    let mut conflicts = 0;
    for item in items.iter().filter(|i| i.changed) {
        match &item.issue {
            Some(e) => {
                conflicts += 1;
                println!("{}  ->  {}   [{e}]", item.from.display(), item.new_name);
            }
            None => {
                println!("{}  ->  {}", item.from.display(), item.new_name);
                ops.push(item.op());
            }
        }
    }

    if ops.is_empty() && conflicts == 0 {
        println!("nothing to rename ({} item(s) unchanged)", files.len());
        return;
    }

    if !apply {
        println!("\npreview only — re-run with --apply to rename {} item(s)", ops.len());
        if conflicts > 0 {
            eprintln!("{conflicts} conflict(s) must be fixed first");
        }
        return;
    }
    if conflicts > 0 {
        die(&format!("{conflicts} conflict(s) above — nothing renamed"));
    }

    let planned = ops.len();
    let res = batch::execute(ops);
    if let Err(e) = batch::record(&res.renamed) {
        eprintln!("warning: could not write batch history: {e}");
    }
    for (op, e) in &res.failed {
        eprintln!("FAILED {} -> {}: {e}", op.from.display(), name_of(&op.to));
    }
    println!("\nrenamed {} of {planned} item(s)", res.renamed.len());
    if !res.failed.is_empty() {
        eprintln!("{} failed — re-run the same command to retry them", res.failed.len());
        exit(1);
    }
    println!("'iron_renamer undo' reverts this batch");
}

fn undo(id_arg: Option<&String>) {
    let id = id_arg.map(|s| {
        s.parse().unwrap_or_else(|_| die("undo ID must be a number (see 'iron_renamer history')"))
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
