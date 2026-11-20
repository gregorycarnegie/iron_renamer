use super::*;
use googletest::prelude::*;
use proptest::prelude::*;
use rstest::rstest;
use std::path::PathBuf;

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

// ───────────────────────── properties
//
// Plain random numbers almost never hit the interesting boundaries, so the
// numeric strategies mix small values with the extremes on purpose.

fn edge_usize() -> impl Strategy<Value = usize> {
    prop_oneof![
        0usize..12,
        Just(usize::MAX),
        Just(usize::MAX - 1),
        any::<usize>()
    ]
}

fn edge_i64() -> impl Strategy<Value = i64> {
    prop_oneof![
        -20i64..20,
        Just(i64::MAX),
        Just(i64::MIN + 1), // i64::MIN has no "+N"/"-N" spelling
        Just(i64::MAX / 6),
        any::<i64>().prop_filter("spellable", |n| *n != i64::MIN),
    ]
}

/// What a START + STEP*index counter should produce, computed in i128.
fn counter_ref(start: i64, step: i64, i: usize) -> Option<i64> {
    let v = start as i128 + step as i128 * i as i128;
    (v <= i64::MAX as i128).then(|| v.max(0) as i64)
}

fn arg_value() -> impl Strategy<Value = String> {
    prop_oneof![
        "[ -~]{0,8}",
        edge_usize().prop_map(|n| n.to_string()),
        edge_i64().prop_map(|n| n.to_string()),
        (edge_usize(), edge_usize()).prop_map(|(a, b)| format!("pos:{a},{b}")),
        (edge_usize(), edge_usize()).prop_map(|(a, b)| format!("at:{a},{b}")),
        (edge_i64(), edge_i64()).prop_map(|(a, b)| format!("{a}/{b}")),
        edge_i64().prop_map(|n| format!("{n:+}")),
        "(before|after|rbefore|rafter|chars|re):[a-z.]{0,3}",
    ]
}

proptest! {
    #[test]
    fn remove_range_keeps_exactly_the_chars_outside_it(
        s in "\\PC{0,12}", start in edge_usize(), len in edge_usize(),
    ) {
        let chars: Vec<char> = s.chars().collect();
        let n = chars.len();
        let from = start.min(n);
        let to = (start as u128 + len as u128).min(n as u128) as usize;
        let expected: String = chars[..from].iter().chain(&chars[to..]).collect();
        let e = entry("remove", &[], &format!("pos:{start},{len}"), "");
        prop_assert_eq!(run(&e, &s), expected);
    }

    #[test]
    fn case_range_leaves_the_chars_outside_it_alone(
        s in "[a-zß ]{0,12}", start in edge_usize(), len in edge_usize(),
    ) {
        let chars: Vec<char> = s.chars().collect();
        let n = chars.len();
        let from = start.min(n);
        let to = (start as u128 + len as u128).min(n as u128) as usize;
        let head: String = chars[..from].iter().collect();
        let tail: String = chars[to..].iter().collect();
        let out = run(&entry("case", &[], "upper", &format!("at:{start},{len}")), &s);
        prop_assert!(out.starts_with(&head) && out.ends_with(&tail), "{out:?}");
    }

    #[test]
    fn insert_lands_at_the_clamped_char_position(
        s in "\\PC{0,12}", n in edge_usize(), from_end in any::<bool>(),
    ) {
        let chars: Vec<char> = s.chars().collect();
        let k = n.min(chars.len());
        let pos = if from_end { chars.len() - k } else { k };
        let expected: String = chars[..pos]
            .iter()
            .copied()
            .chain(['@'])
            .chain(chars[pos..].iter().copied())
            .collect();
        let at = if from_end { format!("-{n}") } else { n.to_string() };
        prop_assert_eq!(run(&entry("insert", &[], "@", &at), &s), expected);
    }

    #[test]
    fn renumber_delta_matches_wide_arithmetic(old in 0u32.., d in edge_i64()) {
        let name = format!("ep{old}.mkv");
        let expected = match counter_ref(old as i64, d, 1) {
            Some(v) => format!("ep{v:0w$}.mkv", w = old.to_string().len()),
            None => name.clone(),
        };
        prop_assert_eq!(run(&entry("renumber", &[], "1", &format!("{d:+}")), &name), expected);
    }

    #[test]
    fn renumber_sequence_matches_wide_arithmetic(start in 0i64.., step in edge_i64()) {
        // run() uses index 6.
        let expected = match counter_ref(start, step, 6) {
            Some(v) => format!("ep{v}.mkv"),
            None => "ep5.mkv".to_string(),
        };
        let e = entry("renumber", &[], "1", &format!("{start}/{step}"));
        prop_assert_eq!(run(&e, "ep5.mkv"), expected);
    }

    #[test]
    fn title_and_sentence_case_are_idempotent(s in "[ -~]{0,16}") {
        for mode in [CaseMode::Title, CaseMode::First, CaseMode::Upper, CaseMode::Lower] {
            let once = change_case(&s, mode);
            prop_assert_eq!(change_case(&once, mode), once.clone());
        }
    }

    #[test]
    fn natural_order_follows_the_number(prefix in "[a-z_ ]{0,4}", a in any::<u32>(), b in any::<u32>()) {
        let (ka, kb) = (natural_key(&format!("{prefix}{a}.jpg")), natural_key(&format!("{prefix}{b}.jpg")));
        prop_assert_eq!(ka.cmp(&kb), a.cmp(&b));
    }

    // Catch-all: whatever a user types into a rule, building and applying it
    // must not panic. Targeted properties above pin down the actual results.
    #[test]
    fn any_rule_on_any_name_never_panics(
        kind in prop::sample::select(vec![
            "replace", "regex", "case", "pattern", "insert", "remove", "trim",
            "renumber", "move", "swap", "names", "pairs",
        ]),
        mods in prop::collection::vec(
            prop_oneof![
                prop::sample::select(vec!["name", "ext", "ci", "first", "last", "all", "inv"])
                    .prop_map(String::from),
                edge_usize().prop_map(|n| format!("n{n}")),
                edge_usize().prop_map(|n| format!("pad{n}")),
            ],
            0..3,
        ),
        a in arg_value(),
        b in arg_value(),
        name in "\\PC{0,12}",
    ) {
        let mods: Vec<&str> = mods.iter().map(String::as_str).collect();
        if let Ok((rule, part)) = build_rule(kind, &mods, &a, &b) {
            let _ = run(&RuleEntry { rule, part, cond: None }, &name);
        }
    }
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

#[rstest]
#[case::all(&[], " ", "_", "a b c.txt", "a_b_c.txt")]
#[case::first(&["first"], " ", "_", "a b c.txt", "a_b c.txt")]
#[case::last(&["last"], " ", "_", "a b c.txt", "a b_c.txt")]
#[case::nth(&["n2"], "o", "0", "foo woof.txt", "fo0 woof.txt")]
#[case::case_insensitive(&["ci"], "IMG", "pic", "img_Img.jpg", "pic_pic.jpg")]
#[case::unicode(&[], "É", "E", "cafÉ.txt", "cafE.txt")]
fn replace_options(
    #[case] mods: &[&str],
    #[case] a: &str,
    #[case] b: &str,
    #[case] input: &str,
    #[case] expected: &str,
) {
    assert_eq!(run(&entry("replace", mods, a, b), input), expected);
}

#[rstest]
#[case::extension("case", &["ext"], "lower", "", "Photo.JPG", "Photo.jpg")]
#[case::name("case", &["name"], "upper", "", "photo.jpg", "PHOTO.jpg")]
#[case::replace_name("replace", &["name"], "o", "0", "photo.mov", "ph0t0.mov")]
#[case::whole("case", &[], "lower", "", "Photo.JPG", "photo.jpg")]
#[case::missing_extension("case", &["ext"], "upper", "", "readme", "readme")]
#[case::extensionless_name("case", &["name"], "upper", "", "readme", "README")]
fn apply_to_parts(
    #[case] kind: &str,
    #[case] mods: &[&str],
    #[case] a: &str,
    #[case] b: &str,
    #[case] input: &str,
    #[case] expected: &str,
) {
    assert_eq!(run(&entry(kind, mods, a, b), input), expected);
}

#[rstest]
#[case::title(&[], "title", "", "my file.txt", "My File.Txt")]
#[case::first(&["name"], "first", "", "my file.txt", "My file.txt")]
#[case::invert(&[], "invert", "", "aBc.TXT", "AbC.txt")]
#[case::positions(&["name"], "upper", "at:0,2", "abcdef.txt", "ABcdef.txt")]
#[case::matching_text(&[], "upper", "img", "img_img.jpg", "IMG_IMG.jpg")]
#[case::length_to_usize_max(&["name"], "upper", "at:1,18446744073709551615", "abc.txt", "aBC.txt")]
#[case::start_past_end(&["name"], "upper", "at:9,1", "abc.txt", "abc.txt")]
fn case_modes_and_scopes(
    #[case] mods: &[&str],
    #[case] a: &str,
    #[case] b: &str,
    #[case] input: &str,
    #[case] expected: &str,
) {
    assert_eq!(run(&entry("case", mods, a, b), input), expected);
}

#[rstest]
#[case::start(&["name"], "new_", "start", "a.txt", "new_a.txt")]
#[case::end(&["name"], "_old", "end", "a.txt", "a_old.txt")]
#[case::position(&["name"], "-", "2", "abcd.txt", "ab-cd.txt")]
#[case::negative_position(&["name"], "-", "-1", "abcd.txt", "abc-d.txt")]
#[case::before(&[], "X", "before:cd", "abcd.txt", "abXcd.txt")]
#[case::after(&[], "X", "after:cd", "abcd.txt", "abcdX.txt")]
#[case::regex_before(&[], "X", "rbefore:\\d+", "ab12.txt", "abX12.txt")]
#[case::missing_match(&[], "X", "before:zzz", "abcd.txt", "abcd.txt")]
#[case::tags(&["name"], "_<num>", "end", "a.txt", "a_007.txt")]
fn insert_positions(
    #[case] mods: &[&str],
    #[case] a: &str,
    #[case] b: &str,
    #[case] input: &str,
    #[case] expected: &str,
) {
    assert_eq!(run(&entry("insert", mods, a, b), input), expected);
}

#[rstest]
#[case::positions(&["name"], "pos:1,2", "", "abcd.txt", "ad.txt")]
#[case::characters(&[], "chars:_-", "", "a_b-c.txt", "abc.txt")]
#[case::digits(&["name"], "digits", "", "a1b2.txt", "ab.txt")]
#[case::uppercase(&["name"], "upper", "", "aXbY.txt", "ab.txt")]
#[case::lowercase(&["name"], "lower", "", "aXbY.txt", "XY.txt")]
#[case::diacritics(&[], "diacritics", "", "café_señor.txt", "cafe_senor.txt")]
#[case::regex(&["name"], "re:\\(\\d+\\)", "", "a(1).txt", "a.txt")]
#[case::literal(&[], "copy", "", "a copy.txt", "a .txt")]
#[case::length_to_usize_max(&["name"], "pos:1,18446744073709551615", "", "abcd.txt", "a.txt")]
#[case::start_past_end(&["name"], "pos:9,1", "", "abcd.txt", "abcd.txt")]
fn remove_kinds(
    #[case] mods: &[&str],
    #[case] a: &str,
    #[case] b: &str,
    #[case] input: &str,
    #[case] expected: &str,
) {
    assert_eq!(run(&entry("remove", mods, a, b), input), expected);
}

#[rstest]
#[case::whitespace(&["name"], "", "", " a b .txt", "a b.txt")]
#[case::start(&["name", "start"], "_", "", "__a__.txt", "a__.txt")]
#[case::all(&["name", "all"], "_", "", "_a_b_.txt", "ab.txt")]
#[case::inverse(&["name", "inv"], "ab", "", "xxabyy.txt", "ab.txt")]
fn trim_kinds(
    #[case] mods: &[&str],
    #[case] a: &str,
    #[case] b: &str,
    #[case] input: &str,
    #[case] expected: &str,
) {
    assert_eq!(run(&entry("trim", mods, a, b), input), expected);
}

#[rstest]
#[case::add(&[], "1", "+10", "img005.jpg", "img015.jpg")]
#[case::subtract(&[], "1", "-9", "ep12.mkv", "ep03.mkv")]
#[case::second_run(&[], "2", "+1", "s01e04.mkv", "s01e05.mkv")]
#[case::sequence(&["pad4"], "1", "100/10", "img5.jpg", "img0160.jpg")]
#[case::missing_run(&[], "3", "+1", "a1b2.txt", "a1b2.txt")]
// Past i64::MAX the name is left alone; below 0 it clamps (index is 6).
#[case::delta_overflows(&[], "1", "+9223372036854775807", "a5.jpg", "a5.jpg")]
#[case::sequence_overflows(&[], "1", "1/9223372036854775807", "a5.jpg", "a5.jpg")]
#[case::sequence_clamps(&[], "1", "1/-9223372036854775807", "a5.jpg", "a0.jpg")]
#[case::widest_pad(&["pad65535"], "1", "+0", "a5.jpg", &format!("a{:065535}.jpg", 5))]
fn renumber_modes(
    #[case] mods: &[&str],
    #[case] a: &str,
    #[case] b: &str,
    #[case] input: &str,
    #[case] expected: &str,
) {
    assert_eq!(run(&entry("renumber", mods, a, b), input), expected);
}

#[gtest]
fn renumber_pad_wider_than_the_formatter_is_rejected() {
    // Formatter widths panic past u16::MAX, so the option itself is refused.
    expect_that!(
        build_rule("renumber", &["pad65536"], "1", "+1").err(),
        some(contains_substring("unknown renumber option 'pad65536'"))
    );
}

#[rstest]
#[case::move_regex("move", &["name"], "re:\\d+", "start", "abc123.txt", "123abc.txt")]
#[case::move_literal("move", &["name"], "CD", "end", "abCDef.txt", "abefCD.txt")]
#[case::swap("swap", &["name"], " - ", "", "Artist - Title.mp3", "Title - Artist.mp3")]
#[case::missing_separator("swap", &["name"], " - ", "", "NoSep.mp3", "NoSep.mp3")]
fn move_and_swap(
    #[case] kind: &str,
    #[case] mods: &[&str],
    #[case] a: &str,
    #[case] b: &str,
    #[case] input: &str,
    #[case] expected: &str,
) {
    assert_eq!(run(&entry(kind, mods, a, b), input), expected);
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

#[gtest]
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
    expect_that!(
        build_rule("pairs", &[], "no separator here", ""),
        err(anything())
    );
    expect_that!(build_rule("pairs", &["bogus"], "a=b", ""), err(anything()));
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

#[rstest]
#[case::number_and_extension(&[], "x_<num>.<ext>", "", "old.jpg", "x_007.jpg")]
#[case::parent_and_name(&[], "<parent>_<name>!", "", "noext", "trip_noext!")]
#[case::stem_only(&["name"], "<name>_<num>", "", "a.jpg", "a_007.jpg")]
fn pattern_uses_shared_tags(
    #[case] mods: &[&str],
    #[case] a: &str,
    #[case] b: &str,
    #[case] input: &str,
    #[case] expected: &str,
) {
    assert_eq!(run(&entry("pattern", mods, a, b), input), expected);
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

#[gtest]
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
    expect_that!(build_rule("js", &[], "this is not js", ""), err(anything()));
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

#[gtest]
fn masks_include_exclude_case_insensitive() {
    let m = Masks::parse("*.jpg; *.PNG ;!*thumb*");
    expect_true!(m.pass("photo.jpg"));
    expect_true!(m.pass("PHOTO.JPG"));
    expect_true!(m.pass("img.png"));
    expect_false!(m.pass("photo.gif"));
    expect_false!(m.pass("photo_THUMB.jpg"), "exclude wins over include");
    // empty mask passes everything; exclude-only mask includes the rest
    expect_true!(Masks::parse("").pass("anything.xyz"));
    let only_exc = Masks::parse("!*.bak");
    expect_true!(only_exc.pass("a.txt"));
    expect_false!(only_exc.pass("a.bak"));
}

#[gtest]
fn natural_key_orders_numbers_and_ignores_case() {
    let mut v = ["IMG10.jpg", "img2.jpg", "Img3.jpg"];
    v.sort_by_key(|s| natural_key(s));
    expect_that!(
        v,
        elements_are![eq("img2.jpg"), eq("Img3.jpg"), eq("IMG10.jpg")]
    );
    // digit runs too long for u64 sort after every valid number
    let mut v = ["a99999999999999999999999", "a1"];
    v.sort_by_key(|s| natural_key(s));
    expect_that!(v, elements_are![eq("a1"), anything()]);
}

#[gtest]
fn sort_files_by_name_ext_size() {
    let d = tmpdir("sort");
    std::fs::write(d.join("b10.txt"), "xxx").unwrap();
    std::fs::write(d.join("b9.txt"), "x").unwrap();
    std::fs::write(d.join("a.zip"), "xx").unwrap();
    let orig = vec![d.join("b10.txt"), d.join("b9.txt"), d.join("a.zip")];
    let names = |f: &[PathBuf]| f.iter().map(|p| name_of(p)).collect::<Vec<_>>();

    let mut f = orig.clone();
    expect_true!(sort_files(&mut f, "name"));
    expect_that!(
        names(&f),
        elements_are![eq("a.zip"), eq("b9.txt"), eq("b10.txt")]
    );

    let mut f = orig.clone();
    expect_true!(sort_files(&mut f, "ext"));
    expect_that!(
        names(&f),
        elements_are![anything(), anything(), eq("a.zip")],
        "txt before zip"
    );

    let mut f = orig.clone();
    expect_true!(sort_files(&mut f, "size"));
    expect_that!(
        names(&f),
        elements_are![eq("b9.txt"), eq("a.zip"), eq("b10.txt")]
    );

    let mut f = orig.clone();
    expect_false!(sort_files(&mut f, "none"), "unknown kind leaves order");
    expect_that!(f, eq(&orig));
}

#[gtest]
fn expand_globs_files_and_dirs() {
    let d = tmpdir("glob");
    std::fs::write(d.join("a.jpg"), "").unwrap();
    std::fs::write(d.join("b.jpg"), "").unwrap();
    std::fs::write(d.join("c.png"), "").unwrap();
    std::fs::create_dir(d.join("sub1")).unwrap();

    let pat = d.join("*.jpg");
    expect_that!(
        expand(&pat.to_string_lossy(), false),
        unordered_elements_are![eq(&d.join("a.jpg")), eq(&d.join("b.jpg"))]
    );

    // ? matches exactly one char; case-insensitive like Windows
    expect_that!(
        expand(&d.join("?.PNG").to_string_lossy(), false),
        elements_are![eq(&d.join("c.png"))]
    );

    // dirs=true matches folders only
    expect_that!(
        expand(&d.join("sub?").to_string_lossy(), true),
        elements_are![eq(&d.join("sub1"))]
    );
    expect_that!(expand(&d.join("sub?").to_string_lossy(), false), is_empty());

    // no wildcard: passed through untouched, even if it doesn't exist
    expect_that!(
        expand("no_such_file.txt", false),
        elements_are![eq(&PathBuf::from("no_such_file.txt"))]
    );
}

#[gtest]
fn collect_dir_recursion_and_masks() {
    let d = tmpdir("collect");
    std::fs::write(d.join("a.jpg"), "").unwrap();
    std::fs::write(d.join("skip.txt"), "").unwrap();
    std::fs::create_dir(d.join("sub")).unwrap();
    std::fs::write(d.join("sub").join("b.jpg"), "").unwrap();

    let jpg = Masks::parse("*.jpg");
    let mut flat = Vec::new();
    collect_dir(&d, false, &jpg, &mut flat);
    expect_that!(flat, elements_are![eq(&d.join("a.jpg"))]);

    let mut deep = Vec::new();
    collect_dir(&d, true, &jpg, &mut deep);
    expect_that!(
        deep,
        unordered_elements_are![eq(&d.join("a.jpg")), eq(&d.join("sub").join("b.jpg"))]
    );

    let mut everything = Vec::new();
    collect_dir(&d, true, &Masks::parse(""), &mut everything);
    expect_that!(everything, len(eq(3)));
}

// ───────────────────────── rule syntax (parse.rs)
//
// Rules carry Regex values, so outcomes are pinned through their Debug form:
// it names every variant and field and shows each regex's source pattern.

fn parsed(kind: &str, mods: &[&str], a: &str, b: &str) -> String {
    match build_rule(kind, mods, a, b) {
        Ok((rule, _)) => format!("{rule:?}"),
        Err(e) => panic!("{kind} {mods:?} {a:?} {b:?} was refused: {e}"),
    }
}

fn refusal(kind: &str, mods: &[&str], a: &str, b: &str) -> String {
    match build_rule(kind, mods, a, b) {
        Ok((rule, _)) => panic!("{kind} {mods:?} {a:?} {b:?} parsed as {rule:?}"),
        Err(e) => e,
    }
}

#[rstest]
#[case::start("start", "Pos(0)")]
#[case::end("end", "FromEnd(0)")]
#[case::empty_means_end("", "FromEnd(0)")]
#[case::from_start("3", "Pos(3)")]
#[case::leading_zeros("007", "Pos(7)")]
#[case::from_end("-2", "FromEnd(2)")]
#[case::minus_zero("-0", "FromEnd(0)")]
#[case::widest("18446744073709551615", "Pos(18446744073709551615)")]
#[case::widest_from_end("-18446744073709551615", "FromEnd(18446744073709551615)")]
#[case::before_is_literal("before:a.b", r#"Before(Regex("a\\.b"))"#)]
#[case::after_is_literal("after:a.b", r#"After(Regex("a\\.b"))"#)]
#[case::rbefore_is_a_regex("rbefore:\\d+", r#"Before(Regex("\\d+"))"#)]
#[case::rafter_is_a_regex("rafter:\\d+", r#"After(Regex("\\d+"))"#)]
#[case::before_nothing("before:", r#"Before(Regex(""))"#)]
#[case::keyword_prefix_wins("before:start", r#"Before(Regex("start"))"#)]
#[gtest]
fn positions_parse(#[case] spec: &str, #[case] expected: &str) {
    expect_that!(
        parse::parse_pos(spec).map(|p| format!("{p:?}")),
        ok(eq(expected))
    );
}

#[rstest]
#[case::word("middle")]
#[case::lone_minus("-")]
#[case::plus_sign("+3")]
#[case::minus_plus("-+3")]
#[case::double_minus("--1")]
#[case::leading_space(" 3")]
#[case::trailing_space("3 ")]
#[case::decimal("3.0")]
#[case::past_usize_max("18446744073709551616")]
#[case::past_usize_max_from_end("-18446744073709551616")]
#[case::case_sensitive_keyword("START")]
#[gtest]
fn bad_positions_are_refused(#[case] spec: &str) {
    expect_that!(
        parse::parse_pos(spec).map(|p| format!("{p:?}")),
        err(eq(&format!("bad position '{spec}'")))
    );
}

#[rstest]
#[case::rbefore("rbefore:(")]
#[case::rafter("rafter:[")]
#[gtest]
fn position_regexes_must_compile(#[case] spec: &str) {
    expect_that!(
        parse::parse_pos(spec).map(|p| format!("{p:?}")),
        err(starts_with("bad regex"))
    );
}

#[rstest]
#[case::range("remove", "pos:1,2", "", "Remove(Range { start: 1, len: 2 })")]
#[case::empty_range("remove", "pos:0,0", "", "Remove(Range { start: 0, len: 0 })")]
#[case::chars("remove", "chars:x.y", "", r#"Remove(Chars("x.y"))"#)]
#[case::regex("remove", "re:\\d", "", r#"Remove(Match(Regex("\\d")))"#)]
#[case::digits("remove", "digits", "", "Remove(Digits)")]
#[case::upper("remove", "upper", "", "Remove(Upper)")]
#[case::lower("remove", "lower", "", "Remove(Lower)")]
#[case::diacritics("remove", "diacritics", "", "Remove(Diacritics)")]
#[case::literal_text("remove", "a.b", "", r#"Remove(Match(Regex("a\\.b")))"#)]
#[case::case_everywhere("case", "upper", "", "Case { mode: Upper, scope: All }")]
#[case::case_lower("case", "lower", "", "Case { mode: Lower, scope: All }")]
#[case::case_first("case", "first", "", "Case { mode: First, scope: All }")]
#[case::case_invert("case", "invert", "", "Case { mode: Invert, scope: All }")]
#[case::case_at(
    "case",
    "title",
    "at:1,2",
    "Case { mode: Title, scope: Pos { start: 1, len: 2 } }"
)]
#[case::case_regex(
    "case",
    "title",
    "re:\\d+",
    r#"Case { mode: Title, scope: Match(Regex("\\d+")) }"#
)]
#[case::case_literal(
    "case",
    "title",
    "a.b",
    r#"Case { mode: Title, scope: Match(Regex("a\\.b")) }"#
)]
#[case::delta_up("renumber", "1", "+3", "Renumber { nth: 1, mode: Delta(3), pad: 0 }")]
#[case::delta_down("renumber", "2", "-3", "Renumber { nth: 2, mode: Delta(-3), pad: 0 }")]
#[case::delta_zero("renumber", "1", "+0", "Renumber { nth: 1, mode: Delta(0), pad: 0 }")]
#[case::delta_max(
    "renumber",
    "1",
    "+9223372036854775807",
    "Renumber { nth: 1, mode: Delta(9223372036854775807), pad: 0 }"
)]
#[case::delta_min(
    "renumber",
    "1",
    "-9223372036854775808",
    "Renumber { nth: 1, mode: Delta(-9223372036854775808), pad: 0 }"
)]
#[case::sequence(
    "renumber",
    "1",
    "5",
    "Renumber { nth: 1, mode: Sequence { start: 5, step: 1 }, pad: 0 }"
)]
#[case::sequence_step(
    "renumber",
    "1",
    "5/2",
    "Renumber { nth: 1, mode: Sequence { start: 5, step: 2 }, pad: 0 }"
)]
#[case::sequence_down(
    "renumber",
    "1",
    "5/-2",
    "Renumber { nth: 1, mode: Sequence { start: 5, step: -2 }, pad: 0 }"
)]
#[case::sequence_leading_zeros(
    "renumber",
    "1",
    "007/0",
    "Renumber { nth: 1, mode: Sequence { start: 7, step: 0 }, pad: 0 }"
)]
#[case::insert("insert", "x", "3", r#"Insert { text: "x", at: Pos(3) }"#)]
#[case::move_literal(
    "move",
    "a.b",
    "",
    r#"MoveText { pat: Regex("a\\.b"), to: FromEnd(0) }"#
)]
#[case::move_regex(
    "move",
    "re:\\d+",
    "start",
    r#"MoveText { pat: Regex("\\d+"), to: Pos(0) }"#
)]
#[case::pattern("pattern", "<num>", "", r#"Pattern("<num>")"#)]
#[case::regex_rule("regex", "\\d", "#", r##"Regex(Regex("\\d"), "#")"##)]
#[case::swap("swap", " - ", "", r#"Swap(" - ")"#)]
#[case::names_trim_line_ends("names", "a  \nb\t\n", "", r#"ListNames(["a", "b"])"#)]
#[case::names_empty("names", "", "", "ListNames([])")]
#[case::js("js", "name.toUpperCase()", "", r#"Js("name.toUpperCase()")"#)]
#[gtest]
fn rule_arguments_parse(
    #[case] kind: &str,
    #[case] a: &str,
    #[case] b: &str,
    #[case] expected: &str,
) {
    expect_that!(parsed(kind, &[], a, b), eq(expected));
}

#[rstest]
#[case::defaults(&[], r#"Replace { find: "a.b", repl: "x", ci: None, occ: All }"#)]
#[case::all(&["first", "all"], r#"Replace { find: "a.b", repl: "x", ci: None, occ: All }"#)]
#[case::first(&["first"], r#"Replace { find: "a.b", repl: "x", ci: None, occ: First }"#)]
#[case::last(&["last"], r#"Replace { find: "a.b", repl: "x", ci: None, occ: Last }"#)]
#[case::nth(&["n2"], r#"Replace { find: "a.b", repl: "x", ci: None, occ: Nth(2) }"#)]
#[case::ci(&["ci"], r#"Replace { find: "a.b", repl: "x", ci: Some(Regex("(?i)a\\.b")), occ: All }"#)]
#[gtest]
fn replace_options_parse(#[case] mods: &[&str], #[case] expected: &str) {
    expect_that!(parsed("replace", mods, "a.b", "x"), eq(expected));
}

#[rstest]
#[case::defaults(&[], r#"Trim { chars: "_", at: Both, invert: false }"#)]
#[case::start(&["start"], r#"Trim { chars: "_", at: Start, invert: false }"#)]
#[case::end(&["end"], r#"Trim { chars: "_", at: End, invert: false }"#)]
#[case::all(&["all"], r#"Trim { chars: "_", at: All, invert: false }"#)]
#[case::last_wins(&["start", "end"], r#"Trim { chars: "_", at: End, invert: false }"#)]
// "both" is the shared apply-to mod, so it never reaches trim; Both is
// already trim's default end.
#[case::both_is_the_apply_to_mod(&["start", "both"], r#"Trim { chars: "_", at: Start, invert: false }"#)]
#[case::invert(&["inv", "end"], r#"Trim { chars: "_", at: End, invert: true }"#)]
#[gtest]
fn trim_options_parse(#[case] mods: &[&str], #[case] expected: &str) {
    expect_that!(parsed("trim", mods, "_", ""), eq(expected));
}

#[rstest]
#[case::default(&[], "Renumber { nth: 3, mode: Delta(1), pad: 0 }")]
#[case::pad(&["pad4"], "Renumber { nth: 3, mode: Delta(1), pad: 4 }")]
#[case::widest_pad(&["pad65535"], "Renumber { nth: 3, mode: Delta(1), pad: 65535 }")]
#[case::last_pad_wins(&["pad4", "pad2"], "Renumber { nth: 3, mode: Delta(1), pad: 2 }")]
#[gtest]
fn renumber_options_parse(#[case] mods: &[&str], #[case] expected: &str) {
    expect_that!(parsed("renumber", mods, "3", "+1"), eq(expected));
}

#[gtest]
fn pairs_split_on_tab_before_equals_and_skip_blank_or_empty_lines() {
    expect_that!(
        parsed("pairs", &[], "a=b\tx\n\nk=v  \n=skipped\nno separator", ""),
        eq(r#"Pairs { pairs: [("a=b", "x", None), ("k", "v", None)] }"#)
    );
    expect_that!(
        parsed("pairs", &["ci"], "a.b=x", ""),
        eq(r#"Pairs { pairs: [("a.b", "x", Some(Regex("(?i)a\\.b")))] }"#)
    );
}

#[rstest]
#[case::default_is_both(&[], Part::Both)]
#[case::name(&["name"], Part::Name)]
#[case::stem_is_name(&["stem"], Part::Name)]
#[case::ext(&["ext"], Part::Ext)]
#[case::last_wins(&["name", "ext"], Part::Ext)]
#[case::back_to_both(&["ext", "both"], Part::Both)]
#[case::mixed_with_rule_options(&["ci", "ext", "first"], Part::Ext)]
#[gtest]
fn apply_to_part_mods(#[case] mods: &[&str], #[case] part: Part) {
    expect_that!(
        build_rule("replace", mods, "a", "b").map(|(_, p)| p),
        ok(eq(&part))
    );
}

#[rstest]
#[case::replace("replace", "a", "")]
#[case::regex("regex", "a", "")]
#[case::case("case", "upper", "")]
#[case::pattern("pattern", "<name>", "")]
#[case::insert("insert", "x", "")]
#[case::remove("remove", "digits", "")]
#[case::trim("trim", "", "")]
#[case::renumber("renumber", "1", "+1")]
#[case::move_text("move", "a", "")]
#[case::swap("swap", "-", "")]
#[case::names("names", "a", "")]
#[case::pairs("pairs", "a=b", "")]
#[case::js("js", "name", "")]
#[gtest]
fn every_kind_refuses_an_unknown_option(#[case] kind: &str, #[case] a: &str, #[case] b: &str) {
    expect_that!(
        refusal(kind, &["zz"], a, b),
        eq(&format!("unknown {kind} option 'zz'"))
    );
    // The apply-to mods are shared and never count as unknown.
    expect_that!(
        build_rule(kind, &["name", "ext", "both", "stem"], a, b),
        ok(anything())
    );
}

#[rstest]
#[case::unknown_kind("bogus", &[], "a", "", "unknown rule kind 'bogus'")]
#[case::replace_empty("replace", &[], "", "x", "replace needs text to find")]
#[case::replace_zeroth("replace", &["n0"], "a", "", "occurrence number is 1-based")]
#[case::replace_nth_word("replace", &["nx"], "a", "", "unknown replace option 'nx'")]
#[case::replace_nth_plus("replace", &["n+2"], "a", "", "unknown replace option 'n+2'")]
#[case::replace_nth_huge("replace", &["n18446744073709551616"], "a", "", "unknown replace option")]
#[case::regex_bad("regex", &[], "(", "", "bad regex '('")]
#[case::case_unknown("case", &[], "shout", "", "unknown case 'shout' (lower|upper|title|first|invert)")]
#[case::case_scope_no_len("case", &[], "upper", "at:1", "use at:START,LEN")]
#[case::case_scope_bad_start("case", &[], "upper", "at:x,1", "bad scope 'at:x,1'")]
#[case::case_scope_negative_len("case", &[], "upper", "at:1,-1", "bad scope 'at:1,-1'")]
#[case::case_scope_bad_regex("case", &[], "upper", "re:(", "bad regex '('")]
#[case::pattern_empty("pattern", &[], "", "", "pattern needs a template")]
#[case::insert_empty("insert", &[], "", "end", "insert needs text")]
#[case::insert_bad_position("insert", &[], "x", "nowhere", "bad position 'nowhere'")]
#[case::remove_empty("remove", &[], "", "", "remove needs something to remove")]
#[case::remove_range_no_len("remove", &[], "pos:1", "", "use pos:START,LEN")]
#[case::remove_range_bad_start("remove", &[], "pos:a,1", "", "bad range 'pos:a,1'")]
#[case::remove_range_bad_len("remove", &[], "pos:1,b", "", "bad range 'pos:1,b'")]
#[case::remove_range_three_parts("remove", &[], "pos:1,2,3", "", "bad range 'pos:1,2,3'")]
#[case::remove_range_signed("remove", &[], "pos:+1,2", "", "bad range 'pos:+1,2'")]
#[case::remove_bad_regex("remove", &[], "re:(", "", "bad regex '('")]
#[case::renumber_zeroth("renumber", &[], "0", "+1", "number position is 1-based")]
#[case::renumber_word("renumber", &[], "x", "+1", "bad number position 'x'")]
#[case::renumber_signed_position("renumber", &[], "+1", "+1", "bad number position '+1'")]
#[case::renumber_empty_pad("renumber", &["pad"], "1", "+1", "unknown renumber option 'pad'")]
#[case::renumber_signed_pad("renumber", &["pad+3"], "1", "+1", "unknown renumber option 'pad+3'")]
#[case::delta_plus_minus("renumber", &[], "1", "+-5", "bad delta '+-5'")]
#[case::delta_double_plus("renumber", &[], "1", "++5", "bad delta '++5'")]
#[case::delta_bare_plus("renumber", &[], "1", "+", "bad delta '+'")]
#[case::delta_bare_minus("renumber", &[], "1", "-", "bad delta '-'")]
#[case::delta_past_max("renumber", &[], "1", "+9223372036854775808", "bad delta")]
#[case::delta_past_min("renumber", &[], "1", "-9223372036854775809", "bad delta")]
#[case::negative_start("renumber", &[], "1", "-5/2", "bad delta '-5/2'")]
#[case::sequence_empty("renumber", &[], "1", "", "bad start ''")]
#[case::sequence_no_start("renumber", &[], "1", "/2", "bad start '/2'")]
#[case::sequence_no_step("renumber", &[], "1", "5/", "bad step '5/'")]
#[case::sequence_bad_step("renumber", &[], "1", "5/x", "bad step '5/x'")]
#[case::sequence_two_steps("renumber", &[], "1", "5/2/1", "bad step '5/2/1'")]
#[case::move_empty("move", &[], "", "end", "move needs text to move")]
#[case::move_bad_regex("move", &[], "re:(", "end", "bad regex '('")]
#[case::move_bad_position("move", &[], "a", "nowhere", "bad position 'nowhere'")]
#[case::swap_empty("swap", &[], "", "", "swap needs a separator")]
#[case::pairs_empty("pairs", &[], "", "", "pairs needs OLD=NEW lines")]
#[case::pairs_only_empty_old("pairs", &[], "=x\n\n", "", "pairs needs OLD=NEW lines")]
#[case::js_empty("js", &[], "", "", "js needs a script")]
#[case::js_syntax("js", &[], "(", "", "bad js")]
#[gtest]
fn bad_rules_are_refused_with_a_reason(
    #[case] kind: &str,
    #[case] mods: &[&str],
    #[case] a: &str,
    #[case] b: &str,
    #[case] reason: &str,
) {
    expect_that!(refusal(kind, mods, a, b), contains_substring(reason));
}

#[rstest]
#[case::replace("replace")]
#[case::pairs("pairs")]
#[gtest]
fn case_insensitive_text_past_the_regex_size_limit_is_refused_not_a_panic(#[case] kind: &str) {
    // Case folding turns each letter into a class; ~50k letters overflow the
    // compiled-size limit, which used to hit an unwrap.
    let text = "k".repeat(60_000);
    let a = if kind == "pairs" {
        format!("{text}=x")
    } else {
        text
    };
    expect_that!(
        refusal(kind, &["ci"], &a, ""),
        starts_with("text too long to match case-insensitively")
    );
}

#[rstest]
#[case::has(
    "name:has",
    "x",
    r#"Cond { field: Original, op: Has, value: "x", negate: false }"#
)]
#[case::starts(
    "new:starts",
    "x",
    r#"Cond { field: Current, op: Starts, value: "x", negate: false }"#
)]
#[case::ends(
    "ext:ends",
    "pg",
    r#"Cond { field: Ext, op: Ends, value: "pg", negate: false }"#
)]
#[case::eq(
    "path:eq",
    "",
    r#"Cond { field: Path, op: Eq, value: "", negate: false }"#
)]
#[case::regex(
    "name:re",
    "^\\d",
    r#"Cond { field: Original, op: Re(Regex("^\\d")), value: "^\\d", negate: false }"#
)]
#[case::negated(
    "not:ext:eq",
    "jpg",
    r#"Cond { field: Ext, op: Eq, value: "jpg", negate: true }"#
)]
#[gtest]
fn conditions_parse(#[case] spec: &str, #[case] value: &str, #[case] expected: &str) {
    expect_that!(
        build_cond(spec, value).map(|c| format!("{c:?}")),
        ok(eq(expected))
    );
}

#[rstest]
#[case::no_op("name", "x", "bad condition 'name' (use FIELD:OP, e.g. ext:eq)")]
#[case::bare_not("not:", "x", "bad condition ''")]
#[case::double_not(
    "not:not:name:has",
    "x",
    "unknown condition field 'not' (name|new|ext|path)"
)]
#[case::unknown_field("size:eq", "x", "unknown condition field 'size'")]
#[case::unknown_op(
    "name:like",
    "x",
    "unknown condition op 'like' (has|starts|ends|eq|re)"
)]
#[case::case_sensitive("Name:has", "x", "unknown condition field 'Name'")]
#[case::bad_regex("name:re", "(", "bad regex '('")]
#[gtest]
fn bad_conditions_are_refused_with_a_reason(
    #[case] spec: &str,
    #[case] value: &str,
    #[case] reason: &str,
) {
    expect_that!(
        build_cond(spec, value).map(|c| format!("{c:?}")),
        err(contains_substring(reason))
    );
}

fn edge_or_any_i64() -> impl Strategy<Value = i64> {
    prop_oneof![-20i64..20, Just(i64::MIN), Just(i64::MAX), any::<i64>()]
}

proptest! {
    // The verified kernels must agree with std on everything std reads the
    // same way. The one deliberate difference: std takes a leading '+'.
    #[test]
    fn number_kernels_match_std_without_a_plus(
        s in prop_oneof!["-?[0-9]{1,24}", "\\PC{0,6}", "-?0*[0-9]{18,20}"]
            .prop_filter("no leading plus", |s| !s.starts_with('+')),
    ) {
        prop_assert_eq!(crate::syntax::parse_u64(s.as_bytes()), s.parse::<u64>().ok());
        prop_assert_eq!(crate::syntax::parse_i64(s.as_bytes()), s.parse::<i64>().ok());
    }

    #[test]
    fn a_leading_plus_is_never_a_number(s in "\\+\\PC{0,8}") {
        prop_assert_eq!(crate::syntax::parse_u64(s.as_bytes()), None);
        prop_assert_eq!(crate::syntax::parse_i64(s.as_bytes()), None);
    }

    #[test]
    fn positions_roundtrip(n in edge_usize()) {
        prop_assert_eq!(format!("{:?}", parse::parse_pos(&n.to_string())), format!("Ok(Pos({n}))"));
        prop_assert_eq!(format!("{:?}", parse::parse_pos(&format!("-{n}"))), format!("Ok(FromEnd({n}))"));
    }

    #[test]
    fn ranges_and_scopes_roundtrip(start in edge_usize(), len in edge_usize()) {
        prop_assert_eq!(
            parsed("remove", &[], &format!("pos:{start},{len}"), ""),
            format!("Remove(Range {{ start: {start}, len: {len} }})")
        );
        prop_assert_eq!(
            parsed("case", &[], "upper", &format!("at:{start},{len}")),
            format!("Case {{ mode: Upper, scope: Pos {{ start: {start}, len: {len} }} }}")
        );
    }

    #[test]
    fn deltas_roundtrip_through_their_signed_spelling(d in edge_or_any_i64()) {
        prop_assert_eq!(
            parsed("renumber", &[], "1", &format!("{d:+}")),
            format!("Renumber {{ nth: 1, mode: Delta({d}), pad: 0 }}")
        );
    }

    #[test]
    fn sequences_roundtrip(start in 0i64.., step in edge_or_any_i64(), nth in 1usize..) {
        prop_assert_eq!(
            parsed("renumber", &[], &nth.to_string(), &format!("{start}/{step}")),
            format!("Renumber {{ nth: {nth}, mode: Sequence {{ start: {start}, step: {step} }}, pad: 0 }}")
        );
    }

    #[test]
    fn nth_occurrence_roundtrips(n in 1usize..) {
        prop_assert_eq!(
            parsed("replace", &[&format!("n{n}")], "a", ""),
            format!(r#"Replace {{ find: "a", repl: "", ci: None, occ: Nth({n}) }}"#)
        );
    }

    #[test]
    fn literal_text_matches_only_itself(text in "\\PC{1,8}", other in "\\PC{0,8}") {
        // before:/after:, plain remove text and plain case scopes are literal
        // even when the text is full of regex syntax.
        let InsertAt::Before(re) = parse::parse_pos(&format!("before:{text}")).unwrap() else {
            panic!("before: must parse to Before");
        };
        prop_assert!(re.is_match(&text));
        prop_assert_eq!(re.is_match(&other), other.contains(text.as_str()));
    }

    #[test]
    fn condition_values_are_kept_verbatim(
        negate in any::<bool>(),
        field in prop::sample::select(vec!["name", "new", "ext", "path"]),
        op in prop::sample::select(vec!["has", "starts", "ends", "eq"]),
        value in "\\PC{0,12}",
    ) {
        let spec = format!("{}{field}:{op}", if negate { "not:" } else { "" });
        let cond = build_cond(&spec, &value).unwrap();
        prop_assert_eq!(&cond.value, &value);
        prop_assert_eq!(cond.negate, negate);
    }

    #[test]
    fn parsing_any_text_never_panics(spec in "\\PC{0,16}", value in "\\PC{0,8}") {
        let _ = parse::parse_pos(&spec);
        let _ = build_cond(&spec, &value);
        for kind in ["remove", "case", "renumber"] {
            let _ = build_rule(kind, &[], &spec, &value);
            let _ = build_rule(kind, &[], &value, &spec);
        }
    }
}
