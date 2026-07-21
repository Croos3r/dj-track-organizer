// SPDX-License-Identifier: GPL-3.0-only
//! Filename normalization to the `Artist - Title (Mix).ext` scheme.
//!
//! Faithful port of `skills/normalize-music-filenames/scripts/normalize_filenames.py`;
//! behavior is pinned by fixtures generated from that script (see
//! `tests/normalize_parity.rs`). Comments referencing "Python" point at the
//! oracle implementation.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use unicode_properties::{GeneralCategoryGroup, UnicodeGeneralCategory};

pub const AUDIO_EXT: [&str; 7] = [".mp3", ".wav", ".aiff", ".aif", ".flac", ".m4a", ".ogg"];

const MIX_KW: &str = "(mix|remix|edit|bootleg|rework|version|vip|flip|radio|extended|\
                      original|cut|remaster|dub|refix|mashup|remake|anthem|instrumental)";

static WS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
static ILLEGAL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"[\\/:*?"<>|]"#).unwrap());
static FEAT_WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(feat(?:uring|s)?|ft)\b\.?").unwrap());
static FEAT_SPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\s*\bfeat\.\s*").unwrap());
static FEAT_SPLIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\s+feat\.\s+").unwrap());
static PARENS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\([^)]*\)").unwrap());
static SLUG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9]+(-[a-z0-9.]+)+$").unwrap());
static DJ_WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bDj\b").unwrap());
static EXT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.[^.]+$").unwrap());
// leading track number: only clearly numbered forms ("01 ", "1. ", "1 - "),
// so numeric artist names like "12 Inch" or "4.20" survive
static TRACK_NO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:0\d[\s_.\-]+|\d{1,2}(?:\.\s+|\s*-\s+|_+))").unwrap());
static MIX_UNIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)-(?:original|extended|radio)-mix").unwrap());
static LABEL_CODE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*-\s*[A-Z]{2,6}\d{2,4}\s*-\s*").unwrap());
static SEG_TRIM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^-+\s*|\s*-+$").unwrap());
static COMPILATION_TRACK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{1,2}\s+\S").unwrap());
static COMPILATION_STRIP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{1,2}\s+").unwrap());
static MIX_PAREN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"(?i)\([^)]*{MIX_KW}[^)]*\)")).unwrap());
static MIX_ANY: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!("(?i){MIX_KW}")).unwrap());

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Tags,
    Filename,
}

#[derive(Clone, Debug)]
pub struct Options {
    pub source: Source,
    pub alphabetical: bool,
    pub keep_mix: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            source: Source::Tags,
            alphabetical: true,
            keep_mix: true,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PlanRow {
    pub old: String,
    pub new: String,
    /// "tags" | "mixed" | "filename" | "manual"
    pub origin: String,
}

/// Python `strip_junk`: drop control/format characters, normalise spaces.
fn strip_junk(s: &str) -> String {
    let s = s.replace('\u{a0}', " ");
    let s: String = s
        .chars()
        .filter(|c| c.general_category_group() != GeneralCategoryGroup::Other)
        .collect();
    WS.replace_all(&s, " ").trim().to_string()
}

/// Python `clean_ws` (tag values only): underscores to spaces, then strip_junk.
fn clean_ws(s: &str) -> String {
    strip_junk(&s.replace('_', " "))
}

/// Python `std_feat`: unify featuring / ft / ft. into a single 'feat.' form.
fn std_feat(s: &str) -> String {
    let s = FEAT_WORD.replace_all(s, "feat.");
    let s = FEAT_SPACE.replace_all(&s, " feat. ");
    WS.replace_all(&s, " ").trim().to_string()
}

/// Python `_sort_names`: comma-split, trim, drop empties, stable sort by lowercase.
fn sort_names(s: &str) -> String {
    let mut parts: Vec<&str> = s
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    parts.sort_by_key(|p| p.to_lowercase());
    parts.join(", ")
}

/// Python `sort_artists`: sort comma-separated artists; '&' groups stay intact.
fn sort_artists(a: &str, alphabetical: bool) -> String {
    let a = std_feat(a);
    if !alphabetical {
        return a;
    }
    if let Some(m) = FEAT_SPLIT.find(&a) {
        format!(
            "{} feat. {}",
            sort_names(&a[..m.start()]),
            sort_names(&a[m.end()..])
        )
    } else {
        sort_names(&a)
    }
}

/// Python `dedupe_parens`: remove repeated parenthetical groups.
fn dedupe_parens(t: &str) -> String {
    // Python does re.split with a captured group; reconstruct the alternating
    // outside-text / paren-group sequence manually.
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = String::new();
    let mut last = 0;
    for m in PARENS.find_iter(t) {
        out.push_str(&t[last..m.start()]);
        let p = m.as_str();
        let k: String = p
            .to_lowercase()
            .chars()
            .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            .collect();
        let dup = !k.is_empty() && seen.contains(&k);
        seen.insert(k);
        if !dup {
            out.push_str(p);
        }
        last = m.end();
    }
    out.push_str(&t[last..]);
    WS.replace_all(&out, " ").trim().to_string()
}

/// Python `sanitize`: remove filesystem-illegal characters, then strip junk and
/// trailing dots/spaces.
fn sanitize(name: &str) -> String {
    let cleaned = strip_junk(&ILLEGAL.replace_all(name, ""));
    cleaned.trim_end_matches(['.', ' ']).to_string()
}

/// Python `titlecase_slug`.
fn titlecase_slug(s: &str) -> String {
    let words: Vec<String> = s
        .replace('-', " ")
        .split_whitespace()
        .map(|w| {
            let mut cs = w.chars();
            match cs.next() {
                Some(first) => {
                    first.to_uppercase().collect::<String>() + &cs.as_str().to_lowercase()
                }
                None => String::new(),
            }
        })
        .collect();
    DJ_WORD.replace_all(&words.join(" "), "DJ").to_string()
}

fn is_slug(b: &str) -> bool {
    SLUG.is_match(b)
}

/// Python: `re.sub(r"(-(?:original|extended|radio)-mix)\1", r"\1", base, flags=I)`.
/// The regex crate has no backreferences; collapse adjacent case-insensitively
/// equal units pairwise, left to right, like the oracle does.
fn collapse_doubled_mix(base: &str) -> String {
    let ms: Vec<regex::Match> = MIX_UNIT.find_iter(base).collect();
    if ms.len() < 2 {
        return base.to_string();
    }
    let mut out = String::new();
    let mut last = 0;
    let mut i = 0;
    while i < ms.len() {
        let m = ms[i];
        out.push_str(&base[last..m.start()]);
        out.push_str(m.as_str());
        last = m.end();
        if i + 1 < ms.len()
            && ms[i + 1].start() == m.end()
            && ms[i + 1].as_str().to_lowercase() == m.as_str().to_lowercase()
        {
            last = ms[i + 1].end(); // skip the duplicate unit
            i += 2;
        } else {
            i += 1;
        }
    }
    out.push_str(&base[last..]);
    out
}

/// Python `parse_from_filename`: best-effort (artist, title, confident).
fn parse_from_filename(fn_: &str) -> (Option<String>, String, bool) {
    let base = strip_junk(&EXT_RE.replace(fn_, ""));
    let base = TRACK_NO.replace(&base, "").to_string();
    let base = collapse_doubled_mix(&base);
    if is_slug(&base) {
        return (None, titlecase_slug(&base), false);
    }
    // drop label codes like -VCU017- inside compilation names
    let b = LABEL_CODE.replace_all(&base, " - ").to_string();
    let segs: Vec<String> = b
        .split(" - ")
        .map(|s| SEG_TRIM.replace_all(s, "").trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    // compilation: "Artist - Album - NN Track Title"
    if segs.len() >= 3 && COMPILATION_TRACK.is_match(segs.last().unwrap()) {
        let title = COMPILATION_STRIP
            .replace(segs.last().unwrap(), "")
            .to_string();
        return (Some(segs[0].clone()), title, true);
    }
    if let Some((a, t)) = base.split_once(" - ") {
        return (Some(a.trim().to_string()), t.trim().to_string(), true);
    }
    (None, base.trim().to_string(), false)
}

/// Python `add_missing_mix`: if the tag title has no mix suffix but the
/// filename does, keep it.
fn add_missing_mix(title: &str, fn_title: &str) -> String {
    if fn_title.is_empty() || MIX_PAREN.is_match(title) {
        return title.to_string();
    }
    let mut title = title.to_string();
    for m in PARENS.find_iter(fn_title) {
        if MIX_ANY.is_match(m.as_str()) {
            title = format!("{} {}", title, m.as_str()).trim().to_string();
        }
    }
    title
}

/// `os.path.splitext` semantics: leading dots never start an extension.
pub(crate) fn splitext(name: &str) -> (&str, &str) {
    let first_non_dot = match name.find(|c| c != '.') {
        Some(i) => i,
        None => return (name, ""),
    };
    match name[first_non_dot..].rfind('.') {
        Some(i) => name.split_at(first_non_dot + i),
        None => (name, ""),
    }
}

fn first_nonempty(a: Option<&str>, b: &str) -> String {
    match a {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => b.to_string(),
    }
}

/// Python `build_name`: derive the normalized file name for one file.
///
/// `tags` carries the embedded (artist, title) when available — the oracle read
/// them with ffprobe; the app reads them with lofty. `None` reproduces the
/// filenames-only mode used to generate the fixtures.
pub fn build_name(file_name: &str, tags: Option<(&str, &str)>, opts: &Options) -> (String, String) {
    let ext = splitext(file_name).1.to_lowercase();
    let (art, tit) = match tags {
        Some((a, t)) => (clean_ws(a), clean_ws(t)),
        None => (String::new(), String::new()),
    };
    let (fa, ft, _confident) = parse_from_filename(file_name);
    let fa = fa.unwrap_or_default();

    let (a, t, origin) = match opts.source {
        Source::Filename => {
            let a = if !fa.is_empty() { fa } else { art.clone() };
            let t = if !ft.is_empty() { ft } else { tit.clone() };
            (a, t, "filename")
        }
        Source::Tags => {
            if !art.is_empty() && !tit.is_empty() {
                let t = if opts.keep_mix {
                    add_missing_mix(&tit, &ft)
                } else {
                    tit.clone()
                };
                (art.clone(), t, "tags")
            } else {
                let a = first_nonempty(Some(&art), &fa);
                let t = first_nonempty(Some(&tit), &ft);
                let origin = if !art.is_empty() || !tit.is_empty() {
                    "mixed"
                } else {
                    "filename"
                };
                (a, t, origin)
            }
        }
    };

    let a = if a.is_empty() {
        a
    } else {
        sort_artists(&a, opts.alphabetical)
    };
    let t = std_feat(&t);
    if !a.is_empty() && !t.is_empty() {
        (
            sanitize(&dedupe_parens(&format!("{a} - {t}"))) + &ext,
            origin.to_string(),
        )
    } else {
        (String::new(), "manual".to_string())
    }
}

fn has_audio_ext(name: &str) -> bool {
    let lower = name.to_lowercase();
    AUDIO_EXT.iter().any(|e| lower.ends_with(e))
}

/// Python `build_plan` minus the directory walk: plan rows for a list of file
/// names, resolving duplicate targets with " (2)", " (3)" suffixes.
pub fn build_plan_for_names<F>(files: &[String], mut read_tags: F, opts: &Options) -> Vec<PlanRow>
where
    F: FnMut(&str) -> Option<(String, String)>,
{
    let mut names: Vec<&String> = files.iter().filter(|f| has_audio_ext(f)).collect();
    names.sort(); // byte order == Python codepoint order for valid UTF-8
    let mut rows: Vec<PlanRow> = names
        .into_iter()
        .map(|fn_| {
            let tags = read_tags(fn_);
            let (new, origin) = build_name(
                fn_,
                tags.as_ref().map(|(a, t)| (a.as_str(), t.as_str())),
                opts,
            );
            PlanRow {
                old: fn_.clone(),
                new,
                origin,
            }
        })
        .collect();

    // resolve duplicate target names with " (2)", " (3)" suffixes
    let mut counts: HashMap<String, usize> = HashMap::new();
    for r in &rows {
        if !r.new.is_empty() {
            *counts.entry(r.new.to_lowercase()).or_insert(0) += 1;
        }
    }
    let mut seen: HashMap<String, usize> = HashMap::new();
    for r in &mut rows {
        if r.new.is_empty() {
            continue;
        }
        let k = r.new.to_lowercase();
        if counts[&k] > 1 {
            let n = seen.entry(k).or_insert(0);
            *n += 1;
            if *n > 1 {
                let (root, ext) = splitext(&r.new);
                r.new = format!("{root} ({n}){ext}");
            }
        }
    }
    rows
}

/// Directory-walking wrapper (Python `build_plan`).
pub fn build_plan<F>(folder: &Path, read_tags: F, opts: &Options) -> std::io::Result<Vec<PlanRow>>
where
    F: FnMut(&str) -> Option<(String, String)>,
{
    let mut files = Vec::new();
    for entry in std::fs::read_dir(folder)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type()?.is_file() && has_audio_ext(&name) {
            files.push(name);
        }
    }
    Ok(build_plan_for_names(&files, read_tags, opts))
}

/// Result of applying a rename plan.
#[derive(Debug, Default)]
pub struct RenameOutcome {
    /// (old, new) pairs actually renamed.
    pub done: Vec<(String, String)>,
    /// (old, new) pairs skipped because the target already existed.
    pub skipped: Vec<(String, String)>,
}

/// Python `two_phase_rename`: rename via temp names first so no target can
/// overwrite another file.
///
/// Deviations from the oracle (safety fixes, covered by unit tests):
/// - every source is checked before anything is touched, so a stale plan
///   cannot abort halfway with temp files on disk;
/// - each rename retries briefly on a transient Windows sharing/lock violation
///   (antivirus / search indexer holding the file), see [`crate::retry`];
/// - if a source rename still fails after retries, every temp file already
///   created is rolled back to its original name before returning the error,
///   so a locked file never leaves the folder in a half-renamed state;
/// - if a skipped file cannot be restored to its old name because another
///   rename claimed it, it is restored to "old (2)" instead of crashing.
pub fn two_phase_rename(
    folder: &Path,
    changes: &[(String, String)],
) -> std::io::Result<RenameOutcome> {
    for (old, _) in changes {
        if !folder.join(old).is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("source file missing, aborting before any rename: {old}"),
            ));
        }
    }
    let rename = |from: &Path, to: &Path| crate::retry::on_lock(|| std::fs::rename(from, to));

    // Phase 1: source -> temp. On failure, undo the temps done so far.
    let ts = chrono::Utc::now().timestamp();
    let mut tmp = Vec::new();
    for (i, (old, new)) in changes.iter().enumerate() {
        let t = folder.join(format!(".__norm_{i}_{ts}.tmp"));
        if let Err(e) = rename(&folder.join(old), &t) {
            for (done_t, _, done_old) in tmp.iter().rev() {
                let _ = std::fs::rename(done_t, folder.join(done_old));
            }
            return Err(std::io::Error::new(
                e.kind(),
                format!(
                    "could not rename {old:?} (in use by another process?); \
                         no files were changed: {e}"
                ),
            ));
        }
        tmp.push((t, new.clone(), old.clone()));
    }

    // Phase 2: temp -> final. A hard failure here restores the remaining temps
    // to their original names so nothing is left as a .tmp.
    let mut out = RenameOutcome::default();
    let mut pending = tmp.into_iter();
    while let Some((t, new, old)) = pending.next() {
        let dst = folder.join(&new);
        let result = if dst.exists() {
            let mut restore = folder.join(&old);
            let mut n = 2;
            while restore.exists() {
                let (root, ext) = splitext(&old);
                restore = folder.join(format!("{root} ({n}){ext}"));
                n += 1;
            }
            rename(&t, &restore).map(|()| out.skipped.push((old.clone(), new.clone())))
        } else {
            rename(&t, &dst).map(|()| out.done.push((old.clone(), new.clone())))
        };
        if let Err(e) = result {
            let _ = std::fs::rename(&t, folder.join(&old));
            for (rt, _, ro) in pending {
                let _ = std::fs::rename(&rt, folder.join(ro));
            }
            return Err(std::io::Error::new(
                e.kind(),
                format!(
                    "could not finish renaming {old:?} (in use by another \
                         process?); other files were restored: {e}"
                ),
            ));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), b"").unwrap();
    }

    #[test]
    fn two_phase_handles_name_swap() {
        let td = tempfile::tempdir().unwrap();
        touch(td.path(), "X - A.mp3");
        touch(td.path(), "X - B.mp3");
        let changes = vec![
            ("X - A.mp3".to_string(), "X - B.mp3".to_string()),
            ("X - B.mp3".to_string(), "X - A.mp3".to_string()),
        ];
        let out = two_phase_rename(td.path(), &changes).unwrap();
        assert_eq!(out.done.len(), 2);
        assert!(td.path().join("X - A.mp3").exists());
        assert!(td.path().join("X - B.mp3").exists());
    }

    #[test]
    fn two_phase_skips_existing_target_and_restores() {
        let td = tempfile::tempdir().unwrap();
        touch(td.path(), "a.mp3");
        touch(td.path(), "taken.mp3");
        let changes = vec![("a.mp3".to_string(), "taken.mp3".to_string())];
        let out = two_phase_rename(td.path(), &changes).unwrap();
        assert!(out.done.is_empty());
        assert_eq!(out.skipped.len(), 1);
        assert!(td.path().join("a.mp3").exists(), "skipped file restored");
    }

    #[test]
    fn two_phase_restore_collision_gets_suffix() {
        // The oracle quirk: "Artist  -  Title.mp3" claims "Artist - Title.mp3",
        // whose own rename to "(2)" is blocked. Restore must not crash.
        let td = tempfile::tempdir().unwrap();
        touch(td.path(), "Artist  -  Title.mp3");
        touch(td.path(), "Artist - Title.mp3");
        touch(td.path(), "Artist - Title (2).mp3");
        let changes = vec![
            (
                "Artist  -  Title.mp3".to_string(),
                "Artist - Title.mp3".to_string(),
            ),
            (
                "Artist - Title.mp3".to_string(),
                "Artist - Title (2).mp3".to_string(),
            ),
        ];
        let out = two_phase_rename(td.path(), &changes).unwrap();
        assert_eq!(out.done.len(), 1);
        assert_eq!(out.skipped.len(), 1);
        assert!(
            td.path().join("Artist - Title (2) (2).mp3").exists()
                || td.path().join("Artist - Title (3).mp3").exists()
                || td.path().join("Artist - Title (2).mp3").exists()
        );
        // exactly three files remain, nothing lost
        assert_eq!(std::fs::read_dir(td.path()).unwrap().count(), 3);
    }

    #[test]
    fn two_phase_aborts_before_touching_anything_on_missing_source() {
        let td = tempfile::tempdir().unwrap();
        touch(td.path(), "real.mp3");
        let changes = vec![
            ("real.mp3".to_string(), "renamed.mp3".to_string()),
            ("ghost.mp3".to_string(), "other.mp3".to_string()),
        ];
        assert!(two_phase_rename(td.path(), &changes).is_err());
        assert!(td.path().join("real.mp3").exists(), "nothing was renamed");
    }

    #[test]
    fn splitext_matches_python() {
        assert_eq!(splitext("a.b.mp3"), ("a.b", ".mp3"));
        assert_eq!(splitext("noext"), ("noext", ""));
        assert_eq!(splitext(".hidden"), (".hidden", ""));
        assert_eq!(splitext("..mp3"), ("..mp3", ""));
        assert_eq!(splitext("x.mp3"), ("x", ".mp3"));
    }
}
