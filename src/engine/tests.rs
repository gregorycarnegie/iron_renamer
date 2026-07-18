use super::*;
use proptest::prelude::*;

proptest! {
    #[test]
    fn split_and_join_extension_roundtrip(
        stem in "[A-Za-z0-9_ -]+",
        ext in "[A-Za-z0-9_ -]+",
    ) {
        let name = format!("{stem}.{ext}");
        prop_assert_eq!(split_ext(&name), (stem.as_str(), ext.as_str()));
    }

    #[test]
    fn wildcard_matches_itself_and_star_matches_all(s in any::<String>()) {
        prop_assert!(mask_re("*").is_match(&s));
        if !s.contains(['*', '?']) {
            prop_assert!(mask_re(&s).is_match(&s));
        }
    }
}

fn entry(kind: &str, mods: &[&str], a: &str, b: &str) -> RuleEntry {
    let (rule, part) = build_rule(kind, mods, a, b).unwrap();
    RuleEntry {
        rule,
        part,
        cond: None,
    }
}

fn run(e: &RuleEntry, name: &str) -> String {
    let path = Path::new("C:/photos/trip").join(name);
    let ctx = Ctx {
        index: 6,
        num: 7,
        pad: 3,
        folder_num: 1,
        total: 0,
        csv: &[],
        path: &path,
        original: name,
    };
    apply_entry(e, name, &ctx)
}

// Run: cargo test --release -- --ignored bench_rule_application --nocapture
#[test]
#[ignore]
fn bench_rule_application() {
    const N: usize = 100_000;
    let path = Path::new("C:/photos/trip/IMG_img_photo.jpg");
    let ctx = Ctx {
        index: 0,
        num: 1,
        pad: 0,
        folder_num: 1,
        total: N,
        csv: &[],
        path,
        original: "IMG_img_photo.jpg",
    };
    for (label, rule) in [
        ("literal", entry("replace", &[], "IMG", "pic")),
        ("case-insensitive", entry("replace", &["ci"], "IMG", "pic")),
    ] {
        let start = std::time::Instant::now();
        let bytes: usize = (0..N)
            .map(|_| apply_entry(&rule, std::hint::black_box("IMG_img_photo.jpg"), &ctx).len())
            .sum();
        assert_eq!(bytes, N * 17);
        println!("{label}: {:?}", start.elapsed());
    }
}

#[test]
fn replace_options() {
    assert_eq!(
        run(&entry("replace", &[], " ", "_"), "a b c.txt"),
        "a_b_c.txt"
    );
    assert_eq!(
        run(&entry("replace", &["first"], " ", "_"), "a b c.txt"),
        "a_b c.txt"
    );
    assert_eq!(
        run(&entry("replace", &["last"], " ", "_"), "a b c.txt"),
        "a b_c.txt"
    );
    assert_eq!(
        run(&entry("replace", &["n2"], "o", "0"), "foo woof.txt"),
        "fo0 woof.txt"
    );
    assert_eq!(
        run(&entry("replace", &["ci"], "IMG", "pic"), "img_Img.jpg"),
        "pic_pic.jpg"
    );
    assert_eq!(
        run(&entry("replace", &[], "É", "E"), "cafÉ.txt"),
        "cafE.txt"
    );
}

#[test]
fn apply_to_parts() {
    assert_eq!(
        run(&entry("case", &["ext"], "lower", ""), "Photo.JPG"),
        "Photo.jpg"
    );
    assert_eq!(
        run(&entry("case", &["name"], "upper", ""), "photo.jpg"),
        "PHOTO.jpg"
    );
    assert_eq!(
        run(&entry("replace", &["name"], "o", "0"), "photo.mov"),
        "ph0t0.mov"
    );
    assert_eq!(
        run(&entry("case", &[], "lower", ""), "Photo.JPG"),
        "photo.jpg"
    );
    // no extension: ext rules are a no-op, name rules hit the whole thing
    assert_eq!(
        run(&entry("case", &["ext"], "upper", ""), "readme"),
        "readme"
    );
    assert_eq!(
        run(&entry("case", &["name"], "upper", ""), "readme"),
        "README"
    );
}

#[test]
fn case_modes_and_scopes() {
    assert_eq!(
        run(&entry("case", &[], "title", ""), "my file.txt"),
        "My File.Txt"
    );
    assert_eq!(
        run(&entry("case", &["name"], "first", ""), "my file.txt"),
        "My file.txt"
    );
    assert_eq!(run(&entry("case", &[], "invert", ""), "aBc.TXT"), "AbC.txt");
    assert_eq!(
        run(&entry("case", &["name"], "upper", "at:0,2"), "abcdef.txt"),
        "ABcdef.txt"
    );
    assert_eq!(
        run(&entry("case", &[], "upper", "img"), "img_img.jpg"),
        "IMG_IMG.jpg"
    );
}

#[test]
fn insert_positions() {
    assert_eq!(
        run(&entry("insert", &["name"], "new_", "start"), "a.txt"),
        "new_a.txt"
    );
    assert_eq!(
        run(&entry("insert", &["name"], "_old", "end"), "a.txt"),
        "a_old.txt"
    );
    assert_eq!(
        run(&entry("insert", &["name"], "-", "2"), "abcd.txt"),
        "ab-cd.txt"
    );
    assert_eq!(
        run(&entry("insert", &["name"], "-", "-1"), "abcd.txt"),
        "abc-d.txt"
    );
    assert_eq!(
        run(&entry("insert", &[], "X", "before:cd"), "abcd.txt"),
        "abXcd.txt"
    );
    assert_eq!(
        run(&entry("insert", &[], "X", "after:cd"), "abcd.txt"),
        "abcdX.txt"
    );
    assert_eq!(
        run(&entry("insert", &[], "X", "rbefore:\\d+"), "ab12.txt"),
        "abX12.txt"
    );
    assert_eq!(
        run(&entry("insert", &[], "X", "before:zzz"), "abcd.txt"),
        "abcd.txt"
    );
    // tags in inserted text
    assert_eq!(
        run(&entry("insert", &["name"], "_<num>", "end"), "a.txt"),
        "a_007.txt"
    );
}

#[test]
fn remove_kinds() {
    assert_eq!(
        run(&entry("remove", &["name"], "pos:1,2", ""), "abcd.txt"),
        "ad.txt"
    );
    assert_eq!(
        run(&entry("remove", &[], "chars:_-", ""), "a_b-c.txt"),
        "abc.txt"
    );
    assert_eq!(
        run(&entry("remove", &["name"], "digits", ""), "a1b2.txt"),
        "ab.txt"
    );
    assert_eq!(
        run(&entry("remove", &["name"], "upper", ""), "aXbY.txt"),
        "ab.txt"
    );
    assert_eq!(
        run(&entry("remove", &["name"], "lower", ""), "aXbY.txt"),
        "XY.txt"
    );
    assert_eq!(
        run(&entry("remove", &[], "diacritics", ""), "café_señor.txt"),
        "cafe_senor.txt"
    );
    assert_eq!(
        run(&entry("remove", &["name"], "re:\\(\\d+\\)", ""), "a(1).txt"),
        "a.txt"
    );
    assert_eq!(
        run(&entry("remove", &[], "copy", ""), "a copy.txt"),
        "a .txt"
    );
}

#[test]
fn trim_kinds() {
    assert_eq!(
        run(&entry("trim", &["name"], "", ""), " a b .txt"),
        "a b.txt"
    );
    assert_eq!(
        run(&entry("trim", &["name", "start"], "_", ""), "__a__.txt"),
        "a__.txt"
    );
    assert_eq!(
        run(&entry("trim", &["name", "all"], "_", ""), "_a_b_.txt"),
        "ab.txt"
    );
    // inverse: keep only underscores and letters a/b at the edges trimmed away
    assert_eq!(
        run(&entry("trim", &["name", "inv"], "ab", ""), "xxabyy.txt"),
        "ab.txt"
    );
}

#[test]
fn renumber_modes() {
    // ctx.index is 6
    assert_eq!(
        run(&entry("renumber", &[], "1", "+10"), "img005.jpg"),
        "img015.jpg"
    );
    assert_eq!(
        run(&entry("renumber", &[], "1", "-9"), "ep12.mkv"),
        "ep03.mkv"
    );
    assert_eq!(
        run(&entry("renumber", &[], "2", "+1"), "s01e04.mkv"),
        "s01e05.mkv"
    );
    assert_eq!(
        run(&entry("renumber", &["pad4"], "1", "100/10"), "img5.jpg"),
        "img0160.jpg"
    );
    assert_eq!(
        run(&entry("renumber", &[], "3", "+1"), "a1b2.txt"),
        "a1b2.txt"
    );
}

#[test]
fn move_and_swap() {
    assert_eq!(
        run(&entry("move", &["name"], "re:\\d+", "start"), "abc123.txt"),
        "123abc.txt"
    );
    assert_eq!(
        run(&entry("move", &["name"], "CD", "end"), "abCDef.txt"),
        "abefCD.txt"
    );
    assert_eq!(
        run(&entry("swap", &["name"], " - ", ""), "Artist - Title.mp3"),
        "Title - Artist.mp3"
    );
    assert_eq!(
        run(&entry("swap", &["name"], " - ", ""), "NoSep.mp3"),
        "NoSep.mp3"
    );
}

#[test]
fn list_names_by_index() {
    let names = "zero\none\ntwo\nthree\nfour\nfive\nsix\nseven";
    // ctx.index is 6 -> "six"; applied to the stem it keeps the extension
    assert_eq!(run(&entry("names", &[], names, ""), "old.txt"), "six");
    assert_eq!(
        run(&entry("names", &["name"], names, ""), "old.txt"),
        "six.txt"
    );
}

#[test]
fn pairs_replace_in_order() {
    // '=' separated, applied top to bottom; tags expand in NEW.
    let pairs = " =_\nIMG=photo";
    assert_eq!(
        run(&entry("pairs", &[], pairs, ""), "img a b.jpg"),
        "img_a_b.jpg" // case-sensitive by default
    );
    assert_eq!(
        run(&entry("pairs", &["ci"], pairs, ""), "img a b.jpg"),
        "photo_a_b.jpg"
    );
    // tab wins over '=' so either side may contain '='
    assert_eq!(
        run(&entry("pairs", &[], "a=b\tx=y", ""), "a=b.txt"),
        "x=y.txt"
    );
    assert!(build_rule("pairs", &[], "no separator here", "").is_err());
    assert!(build_rule("pairs", &["bogus"], "a=b", "").is_err());
}

#[test]
fn conditions_gate_rules() {
    let mut e = entry("case", &[], "upper", "");
    e.cond = Some(build_cond("ext:eq", "jpg").unwrap());
    assert_eq!(run(&e, "photo.jpg"), "PHOTO.JPG");
    assert_eq!(run(&e, "photo.png"), "photo.png");

    e.cond = Some(build_cond("not:name:has", "draft").unwrap());
    assert_eq!(run(&e, "draft_1.jpg"), "draft_1.jpg");
    assert_eq!(run(&e, "final_1.jpg"), "FINAL_1.JPG");

    e.cond = Some(build_cond("name:re", r"^\d").unwrap());
    assert_eq!(run(&e, "1st.jpg"), "1ST.JPG");
    assert_eq!(run(&e, "first.jpg"), "first.jpg");

    e.cond = Some(build_cond("path:has", "trip").unwrap());
    assert_eq!(run(&e, "a.jpg"), "A.JPG");
}

#[test]
fn pattern_uses_shared_tags() {
    assert_eq!(
        run(&entry("pattern", &[], "x_<num>.<ext>", ""), "old.jpg"),
        "x_007.jpg"
    );
    assert_eq!(
        run(&entry("pattern", &[], "<parent>_<name>!", ""), "noext"),
        "trip_noext!"
    );
    // pattern applied to the stem only keeps the real extension
    assert_eq!(
        run(&entry("pattern", &["name"], "<name>_<num>", ""), "a.jpg"),
        "a_007.jpg"
    );
}

#[test]
fn natural_sort_and_glob() {
    let mut v = vec!["img10.jpg", "img9.jpg", "img1.jpg"];
    v.sort_by_key(|a| natural_key(a));
    assert_eq!(v, vec!["img1.jpg", "img9.jpg", "img10.jpg"]);
    assert!(mask_re("*.jpg").is_match("photo.jpg"));
    assert!(mask_re("img?.png").is_match("img1.png"));
    assert!(!mask_re("*.jpg").is_match("photo.png"));
}

#[test]
fn js_rule_sandboxed_eval() {
    // Result of the last expression becomes the new name.
    assert_eq!(
        run(&entry("js", &[], "name.toUpperCase()", ""), "a.jpg"),
        "A.JPG"
    );
    // Globals: stem/ext/index/num; apply-to :name keeps the extension.
    assert_eq!(
        run(&entry("js", &["name"], "stem + '_' + num", ""), "a.jpg"),
        "a_7.jpg"
    );
    // undefined and runtime errors leave the name unchanged.
    assert_eq!(run(&entry("js", &[], "undefined", ""), "a.jpg"), "a.jpg");
    assert_eq!(run(&entry("js", &[], "nope()", ""), "a.jpg"), "a.jpg");
    // Syntax errors are caught when the rule is built.
    assert!(build_rule("js", &[], "this is not js", "").is_err());
    // Sandbox: no file or process access is exposed.
    assert_eq!(
        run(
            &entry("js", &[], "typeof require + '-' + typeof process", ""),
            "a.jpg"
        ),
        "undefined-undefined"
    );
    // Script globals persist across items until reset_js (pre-batch state).
    let e = entry(
        "js",
        &["name"],
        "if (typeof n == 'undefined') n = 0; n += 1; stem + n",
        "",
    );
    assert_eq!(run(&e, "a.jpg"), "a1.jpg");
    assert_eq!(run(&e, "a.jpg"), "a2.jpg");
    reset_js();
    assert_eq!(run(&e, "a.jpg"), "a1.jpg");
}

// ───────────────────────── file discovery (files.rs)

fn tmpdir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("iron_files_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn masks_include_exclude_case_insensitive() {
    let m = Masks::parse("*.jpg; *.PNG ;!*thumb*");
    assert!(m.pass("photo.jpg"));
    assert!(m.pass("PHOTO.JPG"));
    assert!(m.pass("img.png"));
    assert!(!m.pass("photo.gif"));
    assert!(!m.pass("photo_THUMB.jpg"), "exclude wins over include");
    // empty mask passes everything; exclude-only mask includes the rest
    assert!(Masks::parse("").pass("anything.xyz"));
    let only_exc = Masks::parse("!*.bak");
    assert!(only_exc.pass("a.txt"));
    assert!(!only_exc.pass("a.bak"));
}

#[test]
fn natural_key_orders_numbers_and_ignores_case() {
    let mut v = ["IMG10.jpg", "img2.jpg", "Img3.jpg"];
    v.sort_by_key(|s| natural_key(s));
    assert_eq!(v, ["img2.jpg", "Img3.jpg", "IMG10.jpg"]);
    // digit runs too long for u64 sort after every valid number
    let mut v = ["a99999999999999999999999", "a1"];
    v.sort_by_key(|s| natural_key(s));
    assert_eq!(v[0], "a1");
}

#[test]
fn sort_files_by_name_ext_size() {
    let d = tmpdir("sort");
    std::fs::write(d.join("b10.txt"), "xxx").unwrap();
    std::fs::write(d.join("b9.txt"), "x").unwrap();
    std::fs::write(d.join("a.zip"), "xx").unwrap();
    let orig = vec![d.join("b10.txt"), d.join("b9.txt"), d.join("a.zip")];

    let mut f = orig.clone();
    assert!(sort_files(&mut f, "name"));
    assert_eq!(
        f,
        vec![d.join("a.zip"), d.join("b9.txt"), d.join("b10.txt")]
    );

    let mut f = orig.clone();
    assert!(sort_files(&mut f, "ext"));
    assert_eq!(name_of(&f[2]), "a.zip", "txt before zip");

    let mut f = orig.clone();
    assert!(sort_files(&mut f, "size"));
    assert_eq!(
        f,
        vec![d.join("b9.txt"), d.join("a.zip"), d.join("b10.txt")]
    );

    let mut f = orig.clone();
    assert!(!sort_files(&mut f, "none"), "unknown kind leaves order");
    assert_eq!(f, orig);
}

#[test]
fn expand_globs_files_and_dirs() {
    let d = tmpdir("glob");
    std::fs::write(d.join("a.jpg"), "").unwrap();
    std::fs::write(d.join("b.jpg"), "").unwrap();
    std::fs::write(d.join("c.png"), "").unwrap();
    std::fs::create_dir(d.join("sub1")).unwrap();

    let pat = d.join("*.jpg");
    let mut hits = expand(&pat.to_string_lossy(), false);
    hits.sort();
    assert_eq!(hits, vec![d.join("a.jpg"), d.join("b.jpg")]);

    // ? matches exactly one char; case-insensitive like Windows
    assert_eq!(expand(&d.join("?.PNG").to_string_lossy(), false).len(), 1);

    // dirs=true matches folders only
    let dirs = expand(&d.join("sub?").to_string_lossy(), true);
    assert_eq!(dirs, vec![d.join("sub1")]);
    assert!(expand(&d.join("sub?").to_string_lossy(), false).is_empty());

    // no wildcard: passed through untouched, even if it doesn't exist
    assert_eq!(
        expand("no_such_file.txt", false),
        vec![std::path::PathBuf::from("no_such_file.txt")]
    );
}

#[test]
fn collect_dir_recursion_and_masks() {
    let d = tmpdir("collect");
    std::fs::write(d.join("a.jpg"), "").unwrap();
    std::fs::write(d.join("skip.txt"), "").unwrap();
    std::fs::create_dir(d.join("sub")).unwrap();
    std::fs::write(d.join("sub").join("b.jpg"), "").unwrap();

    let jpg = Masks::parse("*.jpg");
    let mut flat = Vec::new();
    collect_dir(&d, false, &jpg, &mut flat);
    assert_eq!(flat, vec![d.join("a.jpg")]);

    let mut deep = Vec::new();
    collect_dir(&d, true, &jpg, &mut deep);
    deep.sort();
    assert_eq!(deep, vec![d.join("a.jpg"), d.join("sub").join("b.jpg")]);

    let all = Masks::parse("");
    let mut everything = Vec::new();
    collect_dir(&d, true, &all, &mut everything);
    assert_eq!(everything.len(), 3);
}
