// CLI front-end. Preview by default; --apply renames and records the batch
// in a dated history for selective undo. Planning, validation, execution,
// and history live in batch.rs, shared with the GUI.

use crate::{
    batch::{self, Op},
    engine::*,
    presets,
};
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use std::{collections::HashMap, fs, path::PathBuf, process::exit};

const LONG_HELP: &str = "\
RULES are applied in order given. Every rule flag takes suffix mods, e.g.
-r:ci:first — ':name' or ':ext' limits a rule to the stem or the extension
(default: the whole name).

  POS:  start | end | N | -N | before:TEXT | after:TEXT | rbefore:PAT | rafter:PAT
  TAGS (in pattern/insert/replacement text; <tag[:args][|mod]...>):
        <name> <ext> <oname> <oext> <index> <total> <parent> <path>
        <subfolder[:N]> (Nth ancestor folder, 1 = parent)
        <size[:kb|mb|gb|tb|h]> ('h' = \"1.4 GB\") <crc32> <md5> <sha1>
        <rand[:MIN[:MAX]]> <rands[:LEN]> <csv:COL> (see --csv)
        counters, all take :START:STEP -- <num> <hex> <alpha> <roman>
        <dirnum> (resets per folder)
        dates (UTC) -- <now|created|modified|accessed[:FMT[:OFFSET]]>,
        FMT tokens yyyy yy MM dd HH mm ss or the literal FMT unix,
        OFFSET like +3d -12h
        metadata (needs ExifTool on PATH or IRON_RENAMER_EXIFTOOL) --
        <exif:TAG> plus <width> <height> <datetaken> <artist> <album>
        <track> <title> <duration> <author> <lat> <lon>
  JS:   --js runs a sandboxed script per item (no file/network access);
        globals name ext stem original path index num; the script's last
        expression becomes the new name; globals persist across the batch
  MODS: |upper |lower |title |sub:START[,LEN] |pad:N |trim[:CHARS]
        |replace:OLD[,NEW] |split:SEP,N (empty SEP = whitespace, N<0 from
        the end) |fallback:TEXT |+N |-N |*N |/N

EXAMPLES:
  iron_renamer -r \" \" \"_\" *.mp3
  iron_renamer -c:ext lower -p \"photo_<num>.<ext>\" --pad 3 *.jpg -x
  iron_renamer -i:name \"<parent>_\" start --if ext:eq jpg *.* -x
  iron_renamer --renumber 1 +100 --remove:name \" copy\" *.mkv";

#[derive(Parser)]
#[command(
    version,
    about = "Batch file renamer (run with no arguments for the GUI)",
    after_long_help = LONG_HELP
)]
struct Cli {
    #[command(subcommand)]
    command: Option<CliCommand>,

    #[arg(short = 'r', long, num_args = 2, value_names = ["OLD", "NEW"], help = "Literal replace (mods: ci, first, last, n<N>)")]
    replace: Vec<String>,
    #[arg(short = 'e', long, num_args = 2, value_names = ["PAT", "REPL"], help = "Regex replace ($1, $2 for groups)")]
    regex: Vec<String>,
    #[arg(
        short = 'c',
        long = "case",
        value_name = "MODE",
        help = "Change case: lower, upper, title, first, or invert"
    )]
    case_rule: Vec<String>,
    #[arg(
        short = 'p',
        long,
        value_name = "PAT",
        help = "Rebuild the name from a tag template"
    )]
    pattern: Vec<String>,
    #[arg(short = 'i', long, num_args = 2, value_names = ["TEXT", "POS"], help = "Insert text (tags allowed) at a position")]
    insert: Vec<String>,
    #[arg(
        long,
        value_name = "WHAT",
        help = "Remove text, regex, position, character class, or diacritics"
    )]
    remove: Vec<String>,
    #[arg(
        short = 't',
        long,
        value_name = "CHARS",
        help = "Trim characters (mods: start, end, both, all, inv)"
    )]
    trim: Vec<String>,
    #[arg(long, num_args = 2, value_names = ["NTH", "SPEC"], allow_hyphen_values = true, help = "Shift or resequence the Nth number")]
    renumber: Vec<String>,
    #[arg(long = "move", num_args = 2, value_names = ["PAT", "POS"], help = "Move the first match to a position")]
    move_rule: Vec<String>,
    #[arg(long, value_name = "SEP", help = "Swap around the first separator")]
    swap: Vec<String>,
    #[arg(long, value_name = "FILE", help = "Use one new name per line")]
    names: Vec<String>,
    #[arg(
        long = "replace-list",
        value_name = "FILE",
        help = "Apply OLD=NEW pairs from a file, one per line (mods: ci)"
    )]
    replace_list: Vec<String>,
    #[arg(
        long,
        value_name = "SCRIPT|FILE",
        help = "JavaScript rule (sandboxed, no file access); result = new name"
    )]
    js: Vec<String>,
    #[arg(long = "if", num_args = 2, value_names = ["COND", "VALUE"], help = "Condition the previous rule")]
    conditions: Vec<String>,
    #[arg(
        long,
        value_name = "FILE|NAME",
        help = "Load rules and settings from a preset"
    )]
    preset: Vec<String>,

    #[arg(long, value_name = "N", help = "Counter start (default: 1)")]
    start: Option<usize>,
    #[arg(long, value_name = "N", help = "Zero-pad width (default: automatic)")]
    pad: Option<usize>,
    #[arg(
        long,
        value_name = "DEST",
        conflicts_with = "move_to",
        help = "Copy into a destination folder template"
    )]
    copy_to: Option<String>,
    #[arg(
        long,
        value_name = "DEST",
        conflicts_with = "copy_to",
        help = "Move into a destination folder template"
    )]
    move_to: Option<String>,
    #[arg(
        long,
        value_name = "POLICY",
        help = "Collision policy: fail, number, letter, or a tag pattern"
    )]
    collide: Option<String>,
    #[arg(
        long,
        value_name = "FILE",
        help = "Export the preview or applied results"
    )]
    export: Option<PathBuf>,
    #[arg(
        long = "in",
        value_name = "DIR",
        help = "Take files from a directory (repeatable)"
    )]
    in_dirs: Vec<PathBuf>,
    #[arg(long, help = "Recurse into --in directories")]
    recurse: bool,
    #[arg(
        long,
        default_value = "",
        value_name = "MASKS",
        help = "Filter --in files, e.g. *.jpg;!*thumb*"
    )]
    mask: String,
    #[arg(
        long,
        value_name = "FILE",
        help = "Take paths from a file, one per line"
    )]
    list: Option<PathBuf>,
    #[arg(
        long,
        value_name = "FILE",
        help = "Load CSV rows for the <csv:COL> tag (row = list position)"
    )]
    csv: Option<PathBuf>,
    #[arg(long, value_parser = ["name", "ext", "size", "date", "none"], help = "Sort order")]
    sort: Option<String>,
    #[arg(long, help = "Reverse the sort order")]
    desc: bool,
    #[arg(long, help = "Give same-stem sidecars the same new stem")]
    pairs: bool,
    #[arg(
        long,
        value_name = "WHICH=VALUE",
        allow_hyphen_values = true,
        help = "Set timestamps after the batch"
    )]
    touch: Option<String>,
    #[arg(
        long = "set-meta",
        value_name = "TAG=VALUE",
        allow_hyphen_values = true,
        help = "Write a metadata tag after the batch (needs ExifTool; repeatable)"
    )]
    set_meta: Vec<String>,
    #[arg(short = 'd', long, help = "Rename folders instead of files")]
    dirs: bool,
    #[arg(short = 'x', long, help = "Apply changes (default: preview)")]
    apply: bool,
    #[arg(value_name = "FILE_OR_GLOB")]
    files: Vec<String>,
}

#[derive(Subcommand)]
enum CliCommand {
    /// List applied batches.
    History,
    /// Revert a batch (the latest if no ID is given).
    Undo { id: Option<u64> },
    /// Open the GUI, preloading files, folders, or a .preset.
    Gui { paths: Vec<PathBuf> },
    /// Add "Rename with Iron Renamer" to Explorer menus and associate
    /// .preset files (current user only, no admin needed).
    Register,
    /// Remove the Explorer integration added by register.
    Unregister,
}

enum RuleEvent {
    Rule {
        pos: usize,
        kind: &'static str,
        mods: Vec<String>,
        values: Vec<String>,
    },
    Condition {
        pos: usize,
        values: Vec<String>,
    },
    Preset {
        pos: usize,
        name: String,
    },
}

impl RuleEvent {
    fn pos(&self) -> usize {
        match self {
            Self::Rule { pos, .. } | Self::Condition { pos, .. } | Self::Preset { pos, .. } => *pos,
        }
    }
}

pub fn run(args: Vec<String>) {
    let (normalized, mods) = normalize_rule_flags(&args);
    let matches = Cli::command().get_matches_from(normalized);
    let cli = Cli::from_arg_matches(&matches).expect("clap matches its own schema");
    match cli.command {
        Some(CliCommand::Undo { id }) => return undo(id),
        Some(CliCommand::History) => return history(),
        Some(CliCommand::Gui { paths }) => {
            if let Err(e) = crate::gui::run(paths) {
                die(&format!("GUI error: {e}"));
            }
            return;
        }
        Some(CliCommand::Register) => return registry(true),
        Some(CliCommand::Unregister) => return registry(false),
        None => {}
    }

    let mut rules: Vec<RuleEntry> = Vec::new();
    let mut start: usize = 1;
    let mut pad: usize = 0;
    let mut mode = batch::Mode::Rename;
    let mut dest = String::new();
    let mut collision = batch::Collision::Fail;
    let mut from_list = false;
    let mut events = rule_events(&cli, &matches, &mods);
    events.sort_by_key(RuleEvent::pos);
    for event in events {
        match event {
            RuleEvent::Rule {
                kind,
                mods,
                mut values,
                ..
            } => {
                if kind == "names" || kind == "pairs" {
                    values[0] = fs::read_to_string(&values[0]).unwrap_or_else(|e| {
                        die(&format!("cannot read {kind} file '{}': {e}", values[0]))
                    });
                }
                // --js takes inline script text or a path to a script file.
                if kind == "js"
                    && std::path::Path::new(&values[0]).is_file()
                    && let Ok(body) = fs::read_to_string(&values[0])
                {
                    values[0] = body;
                }
                let mods: Vec<&str> = mods.iter().map(String::as_str).collect();
                let b = values.get(1).map(String::as_str).unwrap_or("");
                let (rule, part) =
                    build_rule(kind, &mods, &values[0], b).unwrap_or_else(|e| die(&e));
                rules.push(RuleEntry {
                    rule,
                    part,
                    cond: None,
                });
            }
            RuleEvent::Condition { values, .. } => {
                let cond = build_cond(&values[0], &values[1]).unwrap_or_else(|e| die(&e));
                match rules.last_mut() {
                    Some(entry) => entry.cond = Some(cond),
                    None => die("--if must follow the rule it applies to"),
                }
            }
            RuleEvent::Preset { name, .. } => {
                let preset = presets::load(&presets::resolve(&name)).unwrap_or_else(|e| die(&e));
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
                if !get("collide").is_empty() {
                    collision = batch::Collision::parse(get("collide"), get("collide_pattern"));
                }
            }
        }
    }

    start = cli.start.unwrap_or(start);
    pad = cli.pad.unwrap_or(pad);
    if let Some(path) = cli.copy_to {
        (mode, dest) = (batch::Mode::Copy, path);
    } else if let Some(path) = cli.move_to {
        (mode, dest) = (batch::Mode::Move, path);
    }
    if let Some(policy) = cli.collide {
        collision = batch::Collision::parse(&policy, "");
    }
    let touch = cli
        .touch
        .as_deref()
        .map(|value| batch::parse_touch(value).unwrap_or_else(|e| die(&e)));
    let masks = Masks::parse(&cli.mask);
    let mut files: Vec<PathBuf> = cli
        .files
        .iter()
        .flat_map(|path| expand(path, cli.dirs))
        .collect();
    for dir in &cli.in_dirs {
        if !dir.is_dir() {
            die(&format!("--in: '{}' is not a folder", dir.display()));
        }
        collect_dir(dir, cli.recurse, &masks, &mut files);
    }
    if let Some(path) = &cli.list {
        let body = fs::read_to_string(path)
            .unwrap_or_else(|e| die(&format!("cannot read list '{}': {e}", path.display())));
        for line in body.lines().map(str::trim).filter(|line| !line.is_empty()) {
            let path = PathBuf::from(line);
            if path.exists() {
                files.push(path);
            } else {
                eprintln!("warning: '{line}' not found, skipped");
            }
        }
        from_list = true;
    }

    for a in &cli.set_meta {
        if !a.contains('=') {
            die(&format!("--set-meta '{a}' must be TAG=VALUE"));
        }
    }
    if rules.is_empty() && dest.is_empty() && touch.is_none() && cli.set_meta.is_empty() {
        die("no rules given (see --help)");
    }
    if files.is_empty() {
        die(if cli.dirs {
            "no folders matched"
        } else {
            "no files matched"
        });
    }

    // One batch never mixes files and folders.
    for f in &files {
        if cli.dirs && !f.is_dir() {
            die(&format!(
                "'{}' is not a folder (drop --dirs to rename files)",
                f.display()
            ));
        }
        if !cli.dirs && f.is_dir() {
            die(&format!(
                "'{}' is a folder (use --dirs to rename folders)",
                f.display()
            ));
        }
        if !cli.dirs && !f.is_file() {
            die(&format!("'{}' not found", f.display()));
        }
    }

    // Natural name sort by default so photo_9 numbers before photo_10;
    // a --list keeps its own order unless a sort is asked for.
    let sort = cli.sort.unwrap_or_else(|| {
        if from_list {
            "none".into()
        } else {
            "name".into()
        }
    });
    sort_files(&mut files, &sort);
    if cli.desc {
        files.reverse();
    }
    let mut seen = std::collections::HashSet::new();
    files.retain(|f| seen.insert(f.clone()));

    if pad == 0 {
        pad = (start + files.len() - 1).to_string().len();
    }

    let csv_rows: Vec<Vec<String>> = match &cli.csv {
        Some(path) => fs::read_to_string(path)
            .unwrap_or_else(|e| die(&format!("cannot read csv '{}': {e}", path.display())))
            .lines()
            .map(presets::csv_split)
            .collect(),
        None => Vec::new(),
    };

    let overrides = std::collections::HashMap::new();
    let cfg = batch::BatchCfg {
        rules: &rules,
        start,
        pad,
        overrides: &overrides,
        mode,
        dest: &dest,
        collision,
        pairs: cli.pairs,
        csv: &csv_rows,
    };
    let items = batch::plan(&files, &cfg);
    // With --apply the export becomes a result log, written after execution.
    if let Some(path) = &cli.export
        && !cli.apply
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

    if ops.is_empty() && conflicts == 0 && touch.is_none() && cli.set_meta.is_empty() {
        println!("nothing to {verb} ({} item(s) unchanged)", files.len());
        return;
    }

    if !cli.apply {
        println!(
            "\npreview only — re-run with --apply to {verb} {} item(s)",
            ops.len()
        );
        if touch.is_some() {
            println!("timestamps will be set on {} item(s)", files.len());
        }
        if !cli.set_meta.is_empty() {
            println!("metadata will be written on {} item(s)", files.len());
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
    if let Some(path) = &cli.export {
        match batch::export_results(&res, path) {
            Ok(_) => println!("result log written to {}", path.display()),
            Err(e) => eprintln!("warning: could not write result log: {e}"),
        }
    }
    // Timestamps and metadata go on every item at its final location
    // (copies included).
    if touch.is_some() || !cli.set_meta.is_empty() {
        let finals = batch::finals(&items, &res);
        if let Some(spec) = &touch {
            let (n, errors) = batch::apply_touch(&finals, spec);
            println!("timestamps set on {n} item(s)");
            for e in &errors {
                eprintln!("TOUCH FAILED {e}");
            }
        }
        if !cli.set_meta.is_empty() {
            match crate::meta::set(&finals, &cli.set_meta) {
                Ok(msg) => println!("metadata: {msg}"),
                Err(e) => eprintln!("METADATA FAILED: {e}"),
            }
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

fn normalize_rule_flags(args: &[String]) -> (Vec<String>, HashMap<String, Vec<Vec<String>>>) {
    let mut normalized = vec!["iron_renamer".to_string()];
    let mut modifiers: HashMap<String, Vec<Vec<String>>> = HashMap::new();
    for arg in args {
        let (flag, suffix) = arg.split_once(':').unwrap_or((arg, ""));
        if let Some(id) = rule_id(flag) {
            normalized.push(flag.to_string());
            modifiers.entry(id.to_string()).or_default().push(
                suffix
                    .split(':')
                    .filter(|part| !part.is_empty())
                    .map(str::to_string)
                    .collect(),
            );
        } else {
            normalized.push(arg.clone());
        }
    }
    (normalized, modifiers)
}

fn rule_id(flag: &str) -> Option<&'static str> {
    Some(match flag {
        "-r" | "--replace" => "replace",
        "-e" | "--regex" => "regex",
        "-c" | "--case" => "case_rule",
        "-p" | "--pattern" => "pattern",
        "-i" | "--insert" => "insert",
        "--remove" => "remove",
        "-t" | "--trim" => "trim",
        "--renumber" => "renumber",
        "--move" => "move_rule",
        "--swap" => "swap",
        "--names" => "names",
        "--replace-list" => "replace_list",
        "--js" => "js",
        _ => return None,
    })
}

fn rule_events(
    cli: &Cli,
    matches: &clap::ArgMatches,
    modifiers: &HashMap<String, Vec<Vec<String>>>,
) -> Vec<RuleEvent> {
    let mut events = Vec::new();
    macro_rules! add {
        ($id:literal, $kind:literal, $arity:literal, $values:expr) => {
            add_rule_events(&mut events, matches, modifiers, $id, $kind, $arity, $values)
        };
    }
    add!("replace", "replace", 2, &cli.replace);
    add!("regex", "regex", 2, &cli.regex);
    add!("case_rule", "case", 1, &cli.case_rule);
    add!("pattern", "pattern", 1, &cli.pattern);
    add!("insert", "insert", 2, &cli.insert);
    add!("remove", "remove", 1, &cli.remove);
    add!("trim", "trim", 1, &cli.trim);
    add!("renumber", "renumber", 2, &cli.renumber);
    add!("move_rule", "move", 2, &cli.move_rule);
    add!("swap", "swap", 1, &cli.swap);
    add!("names", "names", 1, &cli.names);
    add!("replace_list", "pairs", 1, &cli.replace_list);
    add!("js", "js", 1, &cli.js);

    let positions: Vec<usize> = matches
        .indices_of("conditions")
        .into_iter()
        .flatten()
        .collect();
    for (values, positions) in cli.conditions.chunks(2).zip(positions.chunks(2)) {
        events.push(RuleEvent::Condition {
            pos: positions[0],
            values: values.to_vec(),
        });
    }
    let positions: Vec<usize> = matches.indices_of("preset").into_iter().flatten().collect();
    for (name, pos) in cli.preset.iter().zip(positions) {
        events.push(RuleEvent::Preset {
            pos,
            name: name.clone(),
        });
    }
    events
}

fn add_rule_events(
    events: &mut Vec<RuleEvent>,
    matches: &clap::ArgMatches,
    modifiers: &HashMap<String, Vec<Vec<String>>>,
    id: &str,
    kind: &'static str,
    arity: usize,
    values: &[String],
) {
    let positions: Vec<usize> = matches.indices_of(id).into_iter().flatten().collect();
    for (occurrence, (values, positions)) in values
        .chunks(arity)
        .zip(positions.chunks(arity))
        .enumerate()
    {
        events.push(RuleEvent::Rule {
            pos: positions[0],
            kind,
            mods: modifiers
                .get(id)
                .and_then(|sets| sets.get(occurrence))
                .cloned()
                .unwrap_or_default(),
            values: values.to_vec(),
        });
    }
}

fn undo(id: Option<u64>) {
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

// Explorer integration: per-user context-menu verbs on files and folders,
// plus a .preset file association, written with reg.exe (no new dependency).
// Explorer's classic verbs launch one GUI instance per selected item.
#[cfg(windows)]
fn registry(add: bool) {
    use std::process::Command;
    let exe = std::env::current_exe()
        .unwrap_or_else(|e| die(&format!("cannot locate this executable: {e}")));
    let open = format!("\"{}\" gui \"%1\"", exe.display());
    let menu = "Rename with Iron Renamer";
    const KEYS: [&str; 4] = [
        r"HKCU\Software\Classes\*\shell\IronRenamer",
        r"HKCU\Software\Classes\Directory\shell\IronRenamer",
        r"HKCU\Software\Classes\.preset",
        r"HKCU\Software\Classes\IronRenamer.Preset",
    ];
    let sets: [(String, &str); 7] = [
        (KEYS[0].into(), menu),
        (format!(r"{}\command", KEYS[0]), &open),
        (KEYS[1].into(), menu),
        (format!(r"{}\command", KEYS[1]), &open),
        (KEYS[2].into(), "IronRenamer.Preset"),
        (KEYS[3].into(), "Iron Renamer preset"),
        (format!(r"{}\shell\open\command", KEYS[3]), &open),
    ];
    if add {
        for (key, value) in &sets {
            let ok = Command::new("reg")
                .args(["add", key, "/ve", "/d", value, "/f"])
                .output()
                .is_ok_and(|o| o.status.success());
            if !ok {
                die(&format!("could not write registry key {key}"));
            }
        }
        println!("Explorer context menu and .preset association added for the current user");
        println!("('iron_renamer unregister' removes them)");
    } else {
        for key in KEYS {
            // Absent keys are fine — unregister is idempotent.
            let _ = Command::new("reg").args(["delete", key, "/f"]).output();
        }
        println!("Explorer integration removed");
    }
}

#[cfg(not(windows))]
fn registry(_add: bool) {
    die("Explorer integration is Windows-only");
}

fn die(msg: &str) -> ! {
    eprintln!("error: {msg}");
    exit(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clap_preserves_rule_order_and_suffix_modifiers() {
        let args = [
            "-c",
            "upper",
            "-r:ci:first",
            "A",
            "b",
            "--if",
            "ext:eq",
            "toml",
            "Cargo.toml",
        ]
        .map(str::to_string);
        let (normalized, modifiers) = normalize_rule_flags(&args);
        let matches = Cli::command().try_get_matches_from(normalized).unwrap();
        let cli = Cli::from_arg_matches(&matches).unwrap();
        let mut events = rule_events(&cli, &matches, &modifiers);
        events.sort_by_key(RuleEvent::pos);

        assert_eq!(cli.files, ["Cargo.toml"]);
        assert!(matches!(
            &events[..],
            [
                RuleEvent::Rule { kind: "case", .. },
                RuleEvent::Rule { kind: "replace", mods, .. },
                RuleEvent::Condition { .. }
            ] if mods == &["ci", "first"]
        ));
    }

    #[test]
    fn clap_rejects_invalid_and_conflicting_options() {
        for args in [
            vec!["iron_renamer", "--sort", "random"],
            vec!["iron_renamer", "--copy-to", "a", "--move-to", "b"],
            vec!["iron_renamer", "undo", "not-a-number"],
        ] {
            assert!(Cli::command().try_get_matches_from(args).is_err());
        }
    }

    #[test]
    fn normalize_leaves_non_rule_colons_alone() {
        // Windows paths and option values contain ':' but are not rule flags.
        let args = [
            "-r:ci",
            "a",
            "b",
            r"C:\photos\img.jpg",
            "--touch",
            "modified=2024-05-01 10:30",
        ]
        .map(str::to_string);
        let (normalized, mods) = normalize_rule_flags(&args);
        assert_eq!(
            normalized[1..],
            [
                "-r",
                "a",
                "b",
                r"C:\photos\img.jpg",
                "--touch",
                "modified=2024-05-01 10:30"
            ]
            .map(str::to_string)
        );
        assert_eq!(mods["replace"], vec![vec!["ci".to_string()]]);
    }

    #[test]
    fn suffix_mods_track_their_own_occurrence() {
        // Two -r rules: only the second has mods; each keeps its own set.
        let args = ["-r", "a", "b", "-e", "x", "y", "-r:ci:last", "c", "d"].map(str::to_string);
        let (normalized, mods) = normalize_rule_flags(&args);
        assert_eq!(
            mods["replace"],
            vec![Vec::<String>::new(), vec!["ci".into(), "last".into()]]
        );
        let matches = Cli::command().try_get_matches_from(normalized).unwrap();
        let cli = Cli::from_arg_matches(&matches).unwrap();
        let mut events = rule_events(&cli, &matches, &mods);
        events.sort_by_key(RuleEvent::pos);
        assert!(matches!(
            &events[..],
            [
                RuleEvent::Rule { kind: "replace", mods: m1, .. },
                RuleEvent::Rule { kind: "regex", .. },
                RuleEvent::Rule { kind: "replace", mods: m2, .. },
            ] if m1.is_empty() && m2 == &["ci", "last"]
        ));
    }

    #[test]
    fn presets_and_conditions_keep_command_line_position() {
        let args = ["--preset", "mine", "-c", "upper", "--if", "ext:eq", "jpg"].map(str::to_string);
        let (normalized, mods) = normalize_rule_flags(&args);
        let matches = Cli::command().try_get_matches_from(normalized).unwrap();
        let cli = Cli::from_arg_matches(&matches).unwrap();
        let mut events = rule_events(&cli, &matches, &mods);
        events.sort_by_key(RuleEvent::pos);
        assert!(matches!(
            &events[..],
            [
                RuleEvent::Preset { name, .. },
                RuleEvent::Rule { kind: "case", .. },
                RuleEvent::Condition { values, .. },
            ] if name == "mine" && values == &["ext:eq", "jpg"]
        ));
    }
}
