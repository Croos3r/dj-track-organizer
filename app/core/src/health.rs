// SPDX-License-Identifier: GPL-3.0-only
//! Read-only helpers used by the Library Health dashboard.

/// Calculate the dashboard's deterministic 0-100 health score.
pub fn score(
    rename_issues: usize,
    file_duplicate_extras: usize,
    missing_rekordbox_files: usize,
    database_unavailable: bool,
    collection_duplicate_groups: usize,
) -> u8 {
    let rename_penalty = (rename_issues / 20).min(20);
    let file_duplicate_penalty = ((file_duplicate_extras / 10) * 2).min(25);
    let missing_file_penalty = ((missing_rekordbox_files / 10) * 2).min(30);
    let database_penalty = usize::from(database_unavailable) * 5;
    let collection_penalty = collection_duplicate_groups.min(15);
    let penalty = rename_penalty
        + file_duplicate_penalty
        + missing_file_penalty
        + database_penalty
        + collection_penalty;
    (100usize.saturating_sub(penalty)).min(100) as u8
}

#[cfg(feature = "rekordbox")]
/// Return the number of missing files and a bounded, deterministic sample.
///
/// The entries have already been loaded by the read-only database layer; this
/// helper only checks filesystem existence and never mutates either side.
pub fn missing_rekordbox_files(
    entries: &[crate::rekordbox::RbEntry],
    sample_limit: usize,
) -> (usize, Vec<String>) {
    let mut count = 0;
    let mut samples = Vec::new();
    for entry in entries {
        if !std::path::Path::new(&entry.path).is_file() {
            count += 1;
            if samples.len() < sample_limit {
                samples.push(entry.path.clone());
            }
        }
    }
    (count, samples)
}

#[cfg(test)]
mod tests {
    use super::score;

    #[test]
    fn score_applies_each_penalty_and_clamps() {
        assert_eq!(score(20, 10, 10, true, 1), 89);
        assert_eq!(
            score(usize::MAX, usize::MAX, usize::MAX, true, usize::MAX),
            5
        );
    }

    #[test]
    fn score_uses_complete_groups_of_twenty_or_ten() {
        assert_eq!(score(19, 9, 9, false, 0), 100);
        assert_eq!(score(20, 10, 10, false, 0), 95);
    }

    #[cfg(feature = "rekordbox")]
    #[test]
    fn missing_files_are_counted_with_a_bounded_sample() {
        use super::missing_rekordbox_files;
        use organizer_core::rekordbox::RbEntry;

        let td = tempfile::tempdir().unwrap();
        let existing = td.path().join("exists.wav");
        std::fs::write(&existing, b"audio").unwrap();
        let entry = |path: String| RbEntry {
            id: path.clone(),
            path,
            ext: ".wav".into(),
            title: String::new(),
            created: String::new(),
            cue_ids: Vec::new(),
            playlist_rows: Vec::new(),
            plays: 0,
            rating: 0,
            comment: String::new(),
        };
        let entries = vec![
            entry(existing.to_string_lossy().into_owned()),
            entry(
                td.path()
                    .join("missing-a.wav")
                    .to_string_lossy()
                    .into_owned(),
            ),
            entry(
                td.path()
                    .join("missing-b.wav")
                    .to_string_lossy()
                    .into_owned(),
            ),
        ];

        let (count, samples) = missing_rekordbox_files(&entries, 1);
        assert_eq!(count, 2);
        assert_eq!(samples.len(), 1);
        assert!(samples[0].ends_with("missing-a.wav"));
    }
}
