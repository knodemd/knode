//! File names from titles and titles from file names.

use std::path::{Component, Path};

/// The longest file name stem written, in characters, before ` 2` and `.md`.
const MAX_STEM: usize = 120;

/// The file name stem for a title: the title with characters that file systems or sync
/// services reject (`/ \ : * ? " < > |` and control characters) replaced by `-`, a leading `.`
/// (a hidden file) replaced by `-`, and at most 120 characters.
pub(crate) fn safe_stem(title: &str) -> String {
    let mut stem: String = title
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if c.is_control() => '-',
            c => c,
        })
        .take(MAX_STEM)
        .collect();
    stem = stem.trim().to_string();
    if stem.starts_with('.') {
        stem.replace_range(..1, "-");
    }
    if stem.ends_with('.') {
        stem.pop();
        stem.push('-');
    }
    if stem.is_empty() {
        stem.push_str("Untitled");
    }
    stem
}

/// The title of a note read from a file named `stem`: the stem with `[`, `]` and `|` (which
/// `[[links]]` use) replaced, and runs of whitespace as one space.
pub(crate) fn title_from_stem(stem: &str) -> String {
    let title = stem
        .replace('[', "(")
        .replace(']', ")")
        .replace('|', "-")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if title.is_empty() {
        "Untitled".to_string()
    } else {
        title
    }
}

/// Whether a file named `stem` can hold the node titled `title`: its safe stem, its safe stem
/// with a number (`Plan 2`), or a stem that reads back as the title.
pub(crate) fn stem_fits(stem: &str, title: &str) -> bool {
    let safe = safe_stem(title);
    if stem == safe || title_from_stem(stem) == title {
        return true;
    }
    stem.strip_prefix(safe.as_str())
        .and_then(|rest| rest.strip_prefix(' '))
        .is_some_and(|n| n.parse::<u32>().is_ok_and(|n| n >= 2) && !n.starts_with('0'))
}

/// The stem of a relative path such as `Projects/Plan.md`.
pub(crate) fn stem_of(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.strip_suffix(".md").unwrap_or(name)
}

/// The folder of a relative path, `""` at the root.
pub(crate) fn folder_of(path: &str) -> &str {
    path.rsplit_once('/')
        .map(|(folder, _)| folder)
        .unwrap_or("")
}

/// Joins a folder and a file name.
pub(crate) fn join(folder: &str, name: &str) -> String {
    if folder.is_empty() {
        name.to_string()
    } else {
        format!("{folder}/{name}")
    }
}

/// Whether `path` is a relative path of a `.md` file that stays inside the folder: only plain
/// names, separated by `/`, none hidden.
pub(crate) fn is_safe_relative(path: &str) -> bool {
    !path.is_empty()
        && path.ends_with(".md")
        && !path.contains('\\')
        && path
            .split('/')
            .all(|part| !part.is_empty() && !part.starts_with('.'))
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

/// A path compared the way case-insensitive file systems compare it.
pub(crate) fn fold(path: &str) -> String {
    path.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_stems() {
        assert_eq!(safe_stem("Plan"), "Plan");
        assert_eq!(safe_stem("A/B: c?"), "A-B- c-");
        assert_eq!(safe_stem(".hidden"), "-hidden");
        assert_eq!(safe_stem("ends."), "ends-");
        assert_eq!(safe_stem("Café de ideas"), "Café de ideas");
        assert_eq!(safe_stem(&"x".repeat(300)).chars().count(), MAX_STEM);
    }

    #[test]
    fn stems_fit_titles() {
        assert!(stem_fits("Plan", "Plan"));
        assert!(stem_fits("Plan 2", "Plan"));
        assert!(!stem_fits("Plan 1", "Plan"));
        assert!(!stem_fits("Plan 02", "Plan"));
        assert!(!stem_fits("Planning", "Plan"));
        assert!(stem_fits("A-B", "A/B"));
        assert!(stem_fits("Note (1)", "Note (1)"));
        assert!(stem_fits("Note [1]", "Note (1)"));
    }

    #[test]
    fn titles_from_stems() {
        assert_eq!(title_from_stem("  Two   spaces "), "Two spaces");
        assert_eq!(title_from_stem("a [b] | c"), "a (b) - c");
    }

    #[test]
    fn relative_paths() {
        assert!(is_safe_relative("Plan.md"));
        assert!(is_safe_relative("Projects/Plan.md"));
        assert!(!is_safe_relative("../Plan.md"));
        assert!(!is_safe_relative("/Plan.md"));
        assert!(!is_safe_relative(".knode/Plan.md"));
        assert!(!is_safe_relative("Plan.txt"));
        assert_eq!(stem_of("a/b/Plan.md"), "Plan");
        assert_eq!(folder_of("a/b/Plan.md"), "a/b");
        assert_eq!(folder_of("Plan.md"), "");
    }
}
