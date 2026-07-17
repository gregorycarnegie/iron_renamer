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

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("iron_meta_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    // Minimal valid WAV: RIFF header + "fmt " + empty "data" chunk.
    fn write_wav(p: &Path) {
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&44u32.to_le_bytes());
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
        b.extend_from_slice(&8u32.to_le_bytes());
        b.extend_from_slice(&[128u8; 8]); // 1ms of silence
        std::fs::write(p, b).unwrap();
    }

    #[test]
    fn audio_tag_roundtrip() {
        let p = tmp("roundtrip.wav");
        write_wav(&p);
        set(
            &[p.clone()],
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

    #[test]
    fn formats_duration() {
        assert_eq!(fmt_duration(0), "0:00:00");
        assert_eq!(fmt_duration(205), "0:03:25");
        assert_eq!(fmt_duration(3661), "1:01:01");
    }
}
