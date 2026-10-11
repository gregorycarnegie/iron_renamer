// Native metadata — no external tools required. Pure-Rust readers/writers:
//   images: nom-exif (EXIF read) + imagesize (dimensions) + little_exif (write)
//   audio/video: lofty (tags, duration, write) + nom-exif (mp4/mov track info)
// All fields of a file are read in one pass and cached for the session.

use std::{
    cell::RefCell,
    collections::HashMap,
    ffi::OsStr,
    path::{Path, PathBuf},
    rc::Rc,
};

use lofty::{
    file::{AudioFile, TaggedFileExt},
    tag::{Accessor, ItemKey, Tag, TagExt},
};
use nom_exif::EntryValue;

/// A metadata field (case-insensitive tag name) for a file.
/// None = the file is unreadable; Some("") = tag absent.
pub fn get(path: &Path, tag: &str) -> Option<String> {
    Some(
        fields(path)?
            .get(&tag.to_ascii_lowercase())
            .cloned()
            .unwrap_or_default(),
    )
}

type Fields = Rc<HashMap<String, String>>;

fn fields(path: &Path) -> Option<Fields> {
    thread_local! {
        static CACHE: RefCell<HashMap<PathBuf, Option<Fields>>> = RefCell::new(HashMap::new());
    }
    CACHE.with(|c| {
        if let Some(hit) = c.borrow().get(path) {
            return hit.clone();
        }
        let val = read_fields(path);
        c.borrow_mut().insert(path.to_path_buf(), val.clone());
        val
    })
}

// ponytail: up to three opens per file (imagesize, nom-exif, lofty); cached
// per session, still far cheaper than the exiftool process spawn it replaced.
fn read_fields(path: &Path) -> Option<Fields> {
    std::fs::metadata(path).ok()?; // unreadable file keeps the old None contract
    let mut map = HashMap::new();
    if let Some(ext) = path.extension().and_then(OsStr::to_str) {
        map.insert("filetype".into(), ext.to_ascii_uppercase());
    }
    if let Ok(dim) = imagesize::size(path) {
        map.insert("imagewidth".into(), dim.width.to_string());
        map.insert("imageheight".into(), dim.height.to_string());
    }
    match nom_exif::read_metadata(path) {
        Ok(nom_exif::Metadata::Exif(exif)) => {
            for e in exif.iter() {
                let name = match e.tag.tag() {
                    Some(tag) => tag.to_string().to_ascii_lowercase(),
                    // codes nom-exif doesn't name but the aliases need
                    None => match e.tag {
                        nom_exif::TagOrCode::Unknown(0x013b) => "artist".into(),
                        _ => continue,
                    },
                };
                if let Some(v) = fmt_value(e.value) {
                    map.insert(name, v);
                }
            }
            insert_gps(&mut map, exif.gps_info());
        }
        Ok(nom_exif::Metadata::Track(t)) => {
            use nom_exif::TrackInfoTag as T;
            for (tag, v) in t.iter() {
                let (name, v) = match tag {
                    T::Make => ("make", fmt_value(v)),
                    T::Model => ("model", fmt_value(v)),
                    T::Software => ("software", fmt_value(v)),
                    T::CreateDate => ("createdate", fmt_value(v)),
                    T::DurationMs => (
                        "duration",
                        match v {
                            EntryValue::U64(ms) => Some(fmt_duration(ms / 1000)),
                            _ => None,
                        },
                    ),
                    T::Width => ("imagewidth", fmt_value(v)),
                    T::Height => ("imageheight", fmt_value(v)),
                    T::Author => ("author", fmt_value(v)),
                    _ => continue, // GpsIso6709: parsed form below
                };
                if let Some(v) = v {
                    map.insert(name.into(), v);
                }
            }
            insert_gps(&mut map, t.gps_info());
        }
        Err(_) => {}
    }
    if let Ok(tf) = lofty::read_from_path(path) {
        let secs = tf.properties().duration().as_secs();
        if secs > 0 {
            map.insert("duration".into(), fmt_duration(secs));
        }
        if let Some(tag) = tf.primary_tag().or_else(|| tf.first_tag()) {
            let mut put = |k: &str, v: Option<String>| {
                if let Some(v) = v.filter(|v| !v.is_empty()) {
                    map.insert(k.into(), v);
                }
            };
            put("artist", tag.artist().map(|v| v.into_owned()));
            put("album", tag.album().map(|v| v.into_owned()));
            put("title", tag.title().map(|v| v.into_owned()));
            put("genre", tag.genre().map(|v| v.into_owned()));
            put("comment", tag.comment().map(|v| v.into_owned()));
            put("track", tag.track().map(|n| n.to_string()));
            put(
                "year",
                tag.get_string(ItemKey::Year)
                    .or_else(|| tag.get_string(ItemKey::RecordingDate))
                    .map(str::to_string),
            );
            put(
                "albumartist",
                tag.get_string(ItemKey::AlbumArtist).map(str::to_string),
            );
        }
    }
    Some(Rc::new(map))
}

/// GPS coordinates as signed decimal degrees (file-name friendly).
fn insert_gps(map: &mut HashMap<String, String>, gps: Option<&nom_exif::GPSInfo>) {
    let Some(g) = gps else { return };
    let dec = |ll: &nom_exif::LatLng, sign: f64| {
        let r = |r: nom_exif::URational| {
            if r.denominator() == 0 {
                0.0
            } else {
                r.numerator() as f64 / r.denominator() as f64
            }
        };
        sign * (r(ll.degrees) + r(ll.minutes) / 60.0 + r(ll.seconds) / 3600.0)
    };
    let lat = dec(&g.latitude, g.latitude_ref.sign());
    let lon = dec(&g.longitude, g.longitude_ref.sign());
    map.insert("gpslatitude".into(), format!("{lat:+.6}"));
    map.insert("gpslongitude".into(), format!("{lon:+.6}"));
}

/// EXIF entry value as file-name material; None for binary/array values.
fn fmt_value(v: &EntryValue) -> Option<String> {
    use EntryValue::*;
    Some(match v {
        Text(s) => s.trim().to_string(),
        URational(r) if r.denominator() != 0 => {
            (r.numerator() as f64 / r.denominator() as f64).to_string()
        }
        IRational(r) if r.denominator() != 0 => {
            (r.numerator() as f64 / r.denominator() as f64).to_string()
        }
        U8(_) | U16(_) | U32(_) | U64(_) | I8(_) | I16(_) | I32(_) | I64(_) | F32(_) | F64(_)
        | DateTime(_) | NaiveDateTime(_) => v.to_string(),
        _ => return None,
    })
}

fn fmt_duration(secs: u64) -> String {
    format!("{}:{:02}:{:02}", secs / 3600, secs % 3600 / 60, secs % 60)
}

/// Write metadata tags ("TAG=VALUE" each) on files.
/// Audio tags go via lofty, image EXIF via little_exif, chosen per file.
/// Returns a summary line (e.g. "2 files updated").
pub fn set(paths: &[PathBuf], assigns: &[String]) -> Result<String, String> {
    let assigns: Vec<(String, &str)> = assigns
        .iter()
        .map(|a| {
            a.split_once('=')
                .map(|(t, v)| (t.trim().to_ascii_lowercase(), v))
                .ok_or_else(|| format!("bad assignment {a:?}: expected TAG=VALUE"))
        })
        .collect::<Result<_, _>>()?;
    let mut updated = 0usize;
    let mut errs = Vec::new();
    for p in paths {
        match set_one(p, &assigns) {
            Ok(()) => updated += 1,
            Err(e) => errs.push(format!("{}: {e}", p.display())),
        }
    }
    if errs.is_empty() {
        Ok(format!(
            "{updated} file{} updated",
            if updated == 1 { "" } else { "s" }
        ))
    } else {
        Err(errs.join("\n"))
    }
}

const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "jxl", "png", "tif", "tiff", "webp", "heif", "heic", "hif", "avif",
];

fn set_one(path: &Path, assigns: &[(String, &str)]) -> Result<(), String> {
    let ext = path
        .extension()
        .and_then(OsStr::to_str)
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if IMAGE_EXTS.contains(&ext.as_str()) {
        set_image(path, assigns)
    } else {
        set_audio(path, assigns)
    }
}

fn set_image(path: &Path, assigns: &[(String, &str)]) -> Result<(), String> {
    use little_exif::exif_tag::ExifTag;
    // A file with no EXIF block yet starts from an empty Metadata.
    let mut m = little_exif::metadata::Metadata::new_from_path(path)
        .unwrap_or_else(|_| little_exif::metadata::Metadata::new());
    for (tag, v) in assigns {
        let v = v.to_string();
        m.set_tag(match tag.as_str() {
            "artist" => ExifTag::Artist(v),
            "imagedescription" | "description" => ExifTag::ImageDescription(v),
            "copyright" => ExifTag::Copyright(v),
            "make" => ExifTag::Make(v),
            "model" => ExifTag::Model(v),
            "datetimeoriginal" => ExifTag::DateTimeOriginal(v),
            "createdate" => ExifTag::CreateDate(v),
            "software" => ExifTag::Software(v),
            _ => {
                return Err(format!(
                    "unsupported image tag {tag:?} (supported: artist, description, \
                     copyright, make, model, datetimeoriginal, createdate, software)"
                ));
            }
        });
    }
    m.write_to_file(path).map_err(|e| e.to_string())
}

fn set_audio(path: &Path, assigns: &[(String, &str)]) -> Result<(), String> {
    let mut tf = lofty::read_from_path(path).map_err(|e| e.to_string())?;
    if tf.primary_tag_mut().is_none() {
        tf.insert_tag(Tag::new(tf.primary_tag_type()));
    }
    let tag = tf.primary_tag_mut().unwrap();
    for (name, v) in assigns {
        let v = v.to_string();
        let num = || {
            v.parse::<u32>()
                .map_err(|_| format!("{name} needs a number"))
        };
        match name.as_str() {
            "artist" => tag.set_artist(v),
            "album" => tag.set_album(v),
            "title" => tag.set_title(v),
            "genre" => tag.set_genre(v),
            "comment" => tag.set_comment(v),
            "track" => tag.set_track(num()?),
            "year" | "date" => {
                tag.insert_text(ItemKey::RecordingDate, v);
            }
            "albumartist" => {
                tag.insert_text(ItemKey::AlbumArtist, v);
            }
            _ => {
                return Err(format!(
                    "unsupported audio tag {name:?} (supported: artist, album, title, \
                     genre, comment, track, year, albumartist)"
                ));
            }
        }
    }
    tag.save_to_path(path, lofty::config::WriteOptions::default())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("iron_meta_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    // Minimal valid WAV: RIFF header + "fmt " + a "data" chunk of `samples`
    // bytes of silence. 8000 Hz mono 8-bit, so 8000 samples is one second.
    fn write_wav_samples(p: &Path, samples: u32) {
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + samples).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        // PCM, mono, 8000 Hz, 8-bit
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&8000u32.to_le_bytes());
        b.extend_from_slice(&8000u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&8u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&samples.to_le_bytes());
        b.extend(std::iter::repeat_n(128u8, samples as usize));
        std::fs::write(p, b).unwrap();
    }

    fn write_wav(p: &Path) {
        write_wav_samples(p, 8); // 1ms of silence
    }

    #[test]
    fn audio_tag_roundtrip() {
        let p = tmp("roundtrip.wav");
        write_wav(&p);
        set(
            std::slice::from_ref(&p),
            &["artist=Iron Maiden".into(), "title=Run".into()],
        )
        .unwrap();
        // read back through the public API (cache is cold: set ran first)
        assert_eq!(get(&p, "Artist").as_deref(), Some("Iron Maiden"));
        assert_eq!(get(&p, "title").as_deref(), Some("Run"));
        assert_eq!(get(&p, "album").as_deref(), Some("")); // absent tag = ""
        assert_eq!(get(&p, "filetype").as_deref(), Some("WAV"));
    }

    #[test]
    fn unreadable_and_plain_files() {
        assert_eq!(get(Path::new("no_such_file.xyz"), "artist"), None);
        let p = tmp("plain.txt");
        std::fs::write(&p, "hello").unwrap();
        assert_eq!(get(&p, "FileType").as_deref(), Some("TXT"));
        assert_eq!(get(&p, "artist").as_deref(), Some(""));
    }

    #[rstest]
    #[case::zero(0, "0:00:00")]
    #[case::minutes(205, "0:03:25")]
    #[case::hours(3661, "1:01:01")]
    fn formats_duration(#[case] input: u64, #[case] expected: &str) {
        assert_eq!(fmt_duration(input), expected);
    }

    // ── mutation-testing gap fills (2026-08-30 baseline) ──

    // Header-only PNG. imagesize reads IHDR without needing pixel data, and
    // little_exif falls back to an empty block when it cannot parse a file, so
    // this is enough to exercise both routing and dimension reads.
    fn write_png(p: &Path, w: u32, h: u32) {
        let mut b = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&[8, 2, 0, 0, 0, 0, 0, 0, 0]);
        std::fs::write(p, b).unwrap();
    }

    #[test]
    fn set_counts_updated_files_and_collects_per_file_errors() {
        let (a, b) = (tmp("count_a.wav"), tmp("count_b.wav"));
        write_wav(&a);
        write_wav(&b);

        // Singular and plural are separate paths through the summary.
        assert_eq!(
            set(std::slice::from_ref(&a), &["artist=Solo".into()]).unwrap(),
            "1 file updated"
        );
        assert_eq!(
            set(&[a.clone(), b.clone()], &["artist=Both".into()]).unwrap(),
            "2 files updated"
        );

        // A failing file names itself in the error, and does not count.
        let missing = tmp("count_missing.wav");
        let _ = std::fs::remove_file(&missing);
        let err = set(&[a.clone(), missing.clone()], &["artist=X".into()]).unwrap_err();
        assert!(err.contains("count_missing.wav"), "{err}");
    }

    #[test]
    fn set_rejects_assignments_that_are_not_tag_equals_value() {
        let p = tmp("badassign.wav");
        write_wav(&p);
        let err = set(std::slice::from_ref(&p), &["no_equals_sign".into()]).unwrap_err();
        assert!(err.contains("expected TAG=VALUE"), "{err}");

        // An empty value is legal; only a missing '=' is not.
        assert!(set(std::slice::from_ref(&p), &["artist=".into()]).is_ok());
        // The tag half is trimmed and lowercased before matching.
        assert!(set(std::slice::from_ref(&p), &["  ARTIST  =x".into()]).is_ok());
    }

    #[test]
    fn unsupported_tags_report_the_format_they_were_routed_to() {
        let wav = tmp("route.wav");
        write_wav(&wav);
        let png = tmp("route.png");
        write_png(&png, 4, 5);

        // Extension picks the writer, and each writer names its own tag list.
        let audio_err = set(std::slice::from_ref(&wav), &["bogus=1".into()]).unwrap_err();
        assert!(audio_err.contains("unsupported audio tag"), "{audio_err}");
        assert!(audio_err.contains("albumartist"), "{audio_err}");

        let image_err = set(std::slice::from_ref(&png), &["bogus=1".into()]).unwrap_err();
        assert!(image_err.contains("unsupported image tag"), "{image_err}");
        assert!(image_err.contains("datetimeoriginal"), "{image_err}");

        // A tag that is valid for images is still rejected for audio.
        let err = set(std::slice::from_ref(&wav), &["copyright=me".into()]).unwrap_err();
        assert!(err.contains("unsupported audio tag"), "{err}");
    }

    #[test]
    fn every_audio_tag_round_trips_and_track_must_be_numeric() {
        let p = tmp("alltags.wav");
        write_wav_samples(&p, 8000); // exactly one second
        set(
            std::slice::from_ref(&p),
            &[
                "artist=The Artist".into(),
                "album=The Album".into(),
                "title=The Title".into(),
                "genre=Metal".into(),
                "comment=A comment".into(),
                "track=7".into(),
                "year=1999".into(),
                "albumartist=Various".into(),
            ],
        )
        .unwrap();

        for (tag, want) in [
            ("artist", "The Artist"),
            ("album", "The Album"),
            ("title", "The Title"),
            ("genre", "Metal"),
            ("comment", "A comment"),
            ("track", "7"),
            ("year", "1999"),
            ("albumartist", "Various"),
        ] {
            assert_eq!(get(&p, tag).as_deref(), Some(want), "tag {tag}");
        }
        // A file with real length reports a duration; the 1ms one does not.
        assert_eq!(get(&p, "duration").as_deref(), Some("0:00:01"));

        let short = tmp("alltags_short.wav");
        write_wav(&short);
        assert_eq!(get(&short, "duration").as_deref(), Some(""));

        let bad = tmp("alltags_bad.wav");
        write_wav(&bad);
        let err = set(std::slice::from_ref(&bad), &["track=seven".into()]).unwrap_err();
        assert!(err.contains("track needs a number"), "{err}");
    }

    #[test]
    fn image_dimensions_and_filetype_come_from_the_file_header() {
        let p = tmp("dims.png");
        write_png(&p, 7, 3);
        assert_eq!(get(&p, "imagewidth").as_deref(), Some("7"));
        assert_eq!(get(&p, "imageheight").as_deref(), Some("3"));
        // FileType is the extension, upper-cased, whatever the content is.
        assert_eq!(get(&p, "filetype").as_deref(), Some("PNG"));

        // A non-image keeps FileType but gains no dimensions.
        let t = tmp("dims.txt");
        std::fs::write(&t, "not an image").unwrap();
        assert_eq!(get(&t, "filetype").as_deref(), Some("TXT"));
        assert_eq!(get(&t, "imagewidth").as_deref(), Some(""));
    }

    #[test]
    fn tag_lookup_is_case_insensitive_and_distinguishes_absent_from_unreadable() {
        let p = tmp("lookup.wav");
        write_wav(&p);
        set(std::slice::from_ref(&p), &["artist=Case Test".into()]).unwrap();
        for spelling in ["artist", "Artist", "ARTIST", "ArTiSt"] {
            assert_eq!(
                get(&p, spelling).as_deref(),
                Some("Case Test"),
                "{spelling}"
            );
        }
        // Absent tag is Some(""), so |fallback applies; unreadable is None.
        assert_eq!(get(&p, "nosuchtag").as_deref(), Some(""));
        assert_eq!(get(Path::new("definitely_not_here.wav"), "artist"), None);
    }

    #[rstest]
    #[case::zero(0, "0:00:00")]
    #[case::last_second(59, "0:00:59")]
    #[case::minute_boundary(60, "0:01:00")]
    #[case::last_minute(3599, "0:59:59")]
    #[case::hour_boundary(3600, "1:00:00")]
    #[case::more_than_a_day(93_784, "26:03:04")]
    fn duration_formats_hours_minutes_and_seconds_separately(
        #[case] input: u64,
        #[case] expected: &str,
    ) {
        assert_eq!(fmt_duration(input), expected);
    }

    // A real 1x1 JPEG. little_exif will happily write an APP1 into a stub of
    // FF D8 FF D9, but nom-exif only reads EXIF back out of a structurally
    // valid file, so the round-trip tests need genuine bytes.
    const JPEG_1X1: &[u8] = include_bytes!("../tests/fixtures/1x1.jpg");

    fn write_jpeg(p: &Path) {
        std::fs::write(p, JPEG_1X1).unwrap();
    }

    #[test]
    fn every_image_tag_is_writable_and_reads_back() {
        let p = tmp("exifall.jpg");
        write_jpeg(&p);
        // A deleted arm here returns "unsupported image tag" instead of
        // writing, so a successful set is what proves each one is wired up.
        set(
            std::slice::from_ref(&p),
            &[
                "artist=The Photographer".into(),
                "description=A description".into(),
                "copyright=Copyright 2026".into(),
                "make=IronCorp".into(),
                "model=IronCam X".into(),
                "datetimeoriginal=2024:05:06 07:08:09".into(),
                "createdate=2024:05:06 07:08:09".into(),
                "software=Iron Renamer".into(),
            ],
        )
        .unwrap();

        // Artist is read back through a tag code nom-exif does not name, so
        // it exercises the Unknown(0x013b) fallback as well as the Text arm.
        assert_eq!(get(&p, "artist").as_deref(), Some("The Photographer"));
        assert_eq!(get(&p, "model").as_deref(), Some("IronCam X"));
        assert_eq!(get(&p, "make").as_deref(), Some("IronCorp"));
        assert_eq!(get(&p, "copyright").as_deref(), Some("Copyright 2026"));
        assert_eq!(get(&p, "software").as_deref(), Some("Iron Renamer"));
        // Dimensions still come from the image header, not EXIF.
        assert_eq!(get(&p, "imagewidth").as_deref(), Some("1"));

        // "imagedescription" and "description" are the same tag.
        let p2 = tmp("exifdesc.jpg");
        write_jpeg(&p2);
        set(
            std::slice::from_ref(&p2),
            &["imagedescription=Via the long name".into()],
        )
        .unwrap();
        assert_eq!(
            get(&p2, "imagedescription").as_deref(),
            Some("Via the long name")
        );
    }

    #[test]
    fn gps_coordinates_become_signed_decimal_degrees() {
        use little_exif::{exif_tag::ExifTag, metadata::Metadata, rational::uR64};
        let p = tmp("gps.jpg");
        write_jpeg(&p);

        // Denominators other than 1, so dividing and multiplying a rational
        // are not the same thing, and no two components share a value.
        let r = |n: u32, d: u32| uR64 {
            nominator: n,
            denominator: d,
        };
        let mut m = Metadata::new_from_path(&p).unwrap_or_else(|_| Metadata::new());
        // 51° 28.5' 38" N, 2° 17.4' 23" W.
        m.set_tag(ExifTag::GPSLatitudeRef("N".into()));
        m.set_tag(ExifTag::GPSLatitude(vec![r(51, 1), r(285, 10), r(380, 10)]));
        m.set_tag(ExifTag::GPSLongitudeRef("W".into()));
        m.set_tag(ExifTag::GPSLongitude(vec![r(2, 1), r(174, 10), r(230, 10)]));
        m.write_to_file(&p).unwrap();

        // 51 + 28.5/60 + 38/3600, signed by the hemisphere and fixed to 6 dp.
        assert_eq!(get(&p, "gpslatitude").as_deref(), Some("+51.485556"));
        assert_eq!(get(&p, "gpslongitude").as_deref(), Some("-2.296389"));
    }

    #[test]
    fn rational_exif_values_are_formatted_as_plain_numbers() {
        use little_exif::{exif_tag::ExifTag, metadata::Metadata, rational::uR64};
        let p = tmp("rational.jpg");
        write_jpeg(&p);
        let mut m = Metadata::new_from_path(&p).unwrap_or_else(|_| Metadata::new());
        // 180/2 divides to exactly 90, so a multiply or modulo is visible.
        m.set_tag(ExifTag::XResolution(vec![uR64 {
            nominator: 180,
            denominator: 2,
        }]));
        m.write_to_file(&p).unwrap();
        assert_eq!(get(&p, "xresolution").as_deref(), Some("90"));
    }

    #[test]
    fn signed_rationals_keep_their_sign_and_zero_denominators_are_dropped() {
        use little_exif::{exif_tag::ExifTag, metadata::Metadata, rational::iR64, rational::uR64};
        let p = tmp("signed.jpg");
        write_jpeg(&p);
        let mut m = Metadata::new_from_path(&p).unwrap_or_else(|_| Metadata::new());
        // -9/4 = -2.25: negative, and not a whole number.
        m.set_tag(ExifTag::ExposureCompensation(vec![iR64 {
            nominator: -9,
            denominator: 4,
        }]));
        // A zero denominator is undefined, not infinity: the field is skipped
        // so that |fallback applies rather than "inf" landing in a file name.
        m.set_tag(ExifTag::YResolution(vec![uR64 {
            nominator: 72,
            denominator: 0,
        }]));
        m.write_to_file(&p).unwrap();

        assert_eq!(get(&p, "exposurebiasvalue").as_deref(), Some("-2.25"));
        assert_eq!(get(&p, "yresolution").as_deref(), Some(""));

        // The same rule holds for signed rationals, not just unsigned ones.
        let q = tmp("signedzero.jpg");
        write_jpeg(&q);
        let mut m = Metadata::new_from_path(&q).unwrap_or_else(|_| Metadata::new());
        m.set_tag(ExifTag::ExposureCompensation(vec![iR64 {
            nominator: -9,
            denominator: 0,
        }]));
        m.write_to_file(&q).unwrap();
        assert_eq!(get(&q, "exposurebiasvalue").as_deref(), Some(""));
    }

    // A minimal but real MP4: ftyp + moov{mvhd, trak{tkhd, mdia{hdlr}}, meta}.
    // Everything nom-exif reads for a video lives in these boxes, so ~60 lines
    // of struct packing beats committing a binary clip. Two details matter:
    // the track needs mdia/hdlr = "vide" before tkhd is looked at, and
    // QuickTime's moov/meta carries no version/flags word (unlike ISO meta).
    fn write_mp4(path: &Path) {
        fn atom(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
            let mut v = ((8 + body.len()) as u32).to_be_bytes().to_vec();
            v.extend_from_slice(kind);
            v.extend_from_slice(body);
            v
        }
        const EPOCH_1904: u32 = 2_082_844_800; // MP4 times count from 1904
        let created: u32 = 1_551_657_600 + EPOCH_1904; // 2019-03-04 UTC
        let matrix: Vec<u8> = [0x0001_0000i32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000]
            .iter()
            .flat_map(|v| v.to_be_bytes())
            .collect();

        let mut mvhd = vec![0u8; 4]; // version + flags
        mvhd.extend(created.to_be_bytes());
        mvhd.extend(created.to_be_bytes());
        mvhd.extend(1000u32.to_be_bytes()); // timescale: ticks per second
        mvhd.extend(65_500u32.to_be_bytes()); // duration: 65.5 seconds
        mvhd.extend(0x0001_0000u32.to_be_bytes()); // rate
        mvhd.extend(0x0100u16.to_be_bytes()); // volume
        mvhd.extend([0u8; 10]);
        mvhd.extend_from_slice(&matrix);
        mvhd.extend([0u8; 24]);
        mvhd.extend(2u32.to_be_bytes());

        let mut tkhd = vec![0, 0, 0, 7]; // version 0, flags: enabled
        for v in [created, created, 1, 0, 65_500] {
            tkhd.extend(v.to_be_bytes());
        }
        tkhd.extend([0u8; 16]); // reserved, layer, group, volume, reserved
        tkhd.extend_from_slice(&matrix);
        tkhd.extend((640u32 << 16).to_be_bytes()); // width, 16.16 fixed point
        tkhd.extend((480u32 << 16).to_be_bytes()); // height

        let mut hdlr = vec![0u8; 8]; // version + flags, pre_defined
        hdlr.extend_from_slice(b"vide"); // this is what makes it a video track
        hdlr.extend([0u8; 12]);
        let trak = atom(
            b"trak",
            &[atom(b"tkhd", &tkhd), atom(b"mdia", &atom(b"hdlr", &hdlr))].concat(),
        );

        // moov/meta pairs a "keys" table with an "ilst" of values by index.
        let named = [
            ("com.apple.quicktime.make", "IronCorp"),
            ("com.apple.quicktime.model", "IronCam"),
            ("com.apple.quicktime.software", "IronOS"),
            ("com.apple.quicktime.author", "Iron Author"),
        ];
        let mut key_table = vec![0u8; 4];
        key_table.extend((named.len() as u32).to_be_bytes());
        for (k, _) in named {
            key_table.extend(((8 + k.len()) as u32).to_be_bytes());
            key_table.extend_from_slice(b"mdta");
            key_table.extend_from_slice(k.as_bytes());
        }
        let mut ilst = Vec::new();
        for (i, (_, v)) in named.iter().enumerate() {
            let data_len = (16 + v.len()) as u32;
            ilst.extend((8 + data_len).to_be_bytes());
            ilst.extend((i as u32 + 1).to_be_bytes()); // 1-based key index
            ilst.extend(data_len.to_be_bytes());
            ilst.extend_from_slice(b"data");
            ilst.extend([0, 0, 0, 1]); // type set 0, type code 1 = UTF-8
            ilst.extend([0u8; 4]); // locale
            ilst.extend_from_slice(v.as_bytes());
        }
        let meta = atom(
            b"meta",
            &[atom(b"keys", &key_table), atom(b"ilst", &ilst)].concat(),
        );

        let ftyp = atom(b"ftyp", b"isom\x00\x00\x02\x00isommp42");
        let moov = atom(b"moov", &[atom(b"mvhd", &mvhd), trak, meta].concat());
        std::fs::write(path, [ftyp, moov].concat()).unwrap();
    }

    #[test]
    fn video_track_metadata_is_read_from_the_container() {
        let p = tmp("clip.mp4");
        write_mp4(&p);

        // 65500 ms divided down to whole seconds, then split into h:mm:ss.
        // Multiplying would give 18 hours; a modulo would give 0:00:00.
        assert_eq!(get(&p, "duration").as_deref(), Some("0:01:05"));
        // Dimensions come from tkhd for a video, not from an image header.
        assert_eq!(get(&p, "imagewidth").as_deref(), Some("640"));
        assert_eq!(get(&p, "imageheight").as_deref(), Some("480"));
        // Each of these is a separate arm mapping a QuickTime key to a field.
        assert_eq!(get(&p, "make").as_deref(), Some("IronCorp"));
        assert_eq!(get(&p, "model").as_deref(), Some("IronCam"));
        assert_eq!(get(&p, "software").as_deref(), Some("IronOS"));
        assert_eq!(get(&p, "author").as_deref(), Some("Iron Author"));
        let created = get(&p, "createdate").unwrap();
        assert!(created.starts_with("2019-03-04"), "{created}");
        assert_eq!(get(&p, "filetype").as_deref(), Some("MP4"));
    }
}
