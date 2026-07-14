// CLI front-end. Preview by default; --apply renames and records the batch
// in a dated history for selective undo. Planning, validation, execution,
// and history live in batch.rs, shared with the GUI.

use crate::batch::{self, Op};
use crate::engine::*;
use std::path::PathBuf;
use std::process::exit;

const USAGE: &str = "\
iron_renamer — batch file renamer (run with no arguments for the GUI)

USAGE:
  iron_renamer [RULES] [OPTIONS] <files or globs>...
  iron_renamer history         list applied batches
  iron_renamer undo [ID]       revert a batch (the latest if no ID)

RULES (applied in order given, to the full file name):
  -r, --replace <OLD> <NEW>    literal text replace
  -e, --regex <PAT> <REPL>     regex replace ($1, $2 for groups)
  -c, --case <lower|upper|title>
  -p, --pattern <PAT>          rebuild name: <name> = stem, <ext> = extension,
                               <num> = counter   e.g. \"trip_<num>.<ext>\"

OPTIONS:
  --start <N>                  counter start (default 1)
  --pad <N>                    zero-pad width (default: fits the largest number)
  -d, --dirs                   rename folders instead of files
  -x, --apply                  actually rename (otherwise preview only)

EXAMPLES:
  iron_renamer -r \" \" \"_\" *.mp3
  iron_renamer -c lower -p \"photo_<num>.<ext>\" --pad 3 *.jpg -x";

pub fn run(args: Vec<String>) {
    match args[0].as_str() {
        "undo" => return undo(args.get(1)),
        "history" => return history(),
        _ => {}
    }

    let mut rules: Vec<Rule> = Vec::new();
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
        match a.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            "-x" | "--apply" => apply = true,
            "-d" | "--dirs" => {}
            "-r" | "--replace" => {
                let v = need(2, &mut it);
                rules.push(Rule::Replace(v[0].clone(), v[1].clone()));
            }
            "-e" | "--regex" => {
                let v = need(2, &mut it);
                match regex::Regex::new(&v[0]) {
                    Ok(re) => rules.push(Rule::Regex(re, v[1].clone())),
                    Err(e) => die(&format!("bad regex '{}': {e}", v[0])),
                }
            }
            "-c" | "--case" => {
                let v = need(1, &mut it);
                let mode = match v[0].as_str() {
                    "lower" => CaseMode::Lower,
                    "upper" => CaseMode::Upper,
                    "title" => CaseMode::Title,
                    other => die(&format!("unknown case '{other}' (lower|upper|title)")),
                };
                rules.push(Rule::Case(mode));
            }
            "-p" | "--pattern" => {
                let v = need(1, &mut it);
                rules.push(Rule::Pattern(v[0].clone()));
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
    files.sort_by(|a, b| natural_key(&name_of(a)).cmp(&natural_key(&name_of(b))));

    if pad == 0 {
        pad = (start + files.len() - 1).to_string().len();
    }

    let items = batch::plan(&files, &rules, start, pad);
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
