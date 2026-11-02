//! A local rehearsal of the real Markdown importer, without touching the source or a user space.
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use knode_format::{Provenance, Space};
use serde::{Deserialize, Serialize};

use crate::{ImportReport, Result, io_error};

/// One note in the temporary imported space. IDs are deliberately not exposed as final IDs.
#[derive(Debug, Serialize, Deserialize)]
pub struct PreviewNote {
    /// Original relative path when the importer preserved one.
    pub path: Option<String>,
    /// Imported title, including frontmatter title overrides.
    pub title: String,
    /// Wikilinks that the real importer could not resolve.
    pub unresolved: Vec<String>,
}
/// A migration warning, not necessarily an import error.
#[derive(Debug, Serialize, Deserialize)]
pub struct PreviewWarning {
    /// Relative source path, or an empty string for the whole vault.
    pub path: String,
    /// Why the user should review this item.
    pub reason: String,
}
/// Serializable report from a temporary import; it is not a cutover or a fidelity certificate.
#[derive(Debug, Serialize, Deserialize)]
pub struct PreviewReport {
    /// Always true: no persistent destination has been modified.
    pub simulation_only: bool,
    /// The real importer result, including skips, unused attachments and failed typed links.
    pub import: ImportReport,
    /// Notes present after the rehearsal; sorted by path and title.
    pub notes: Vec<PreviewNote>,
    /// Repeated normalized titles and their source paths; path-qualified links need review.
    pub duplicate_titles: BTreeMap<String, Vec<String>>,
    /// Ignored paths, plugin-specific content and compatibility caveats.
    pub warnings: Vec<PreviewWarning>,
}

/// Rehearse [`crate::import`] in a private temporary space, then remove it.
/// No service, agent, embeddings model or external API is involved. Source files are only read.
/// The rehearsal can use disk proportional to the imported vault and is not a snapshot of
/// concurrently edited source files. Temporary storage is ordinary local storage, not secure erasure.
pub fn preview(dir: impl AsRef<Path>) -> Result<PreviewReport> {
    let dir = dir.as_ref();
    let mut warnings = vec![PreviewWarning { path: String::new(), reason: "Rehearsal in a new temporary space, not validation against an existing destination. Source files must stay unchanged during preview/import. Obsidian plugins, settings and ongoing sync are not migrated; export may add Knode metadata. Review all skipped files and unused attachments before cutover.".into() }];
    inventory(dir, "", &mut warnings)?;
    // Hidden prefix also prevents the importer traversing its own temporary data when the
    // source happens to contain the system temporary directory.
    let temp = tempfile::Builder::new()
        .prefix(".knode-preview-")
        .tempdir()
        .map_err(io_error(std::env::temp_dir()))?;
    let mut space = Space::create(temp.path().join("Preview.knode"), "Migration preview")?;
    let import = crate::import(&mut space, dir, &Provenance::system())?;
    let count = usize::try_from(space.node_count()?).unwrap_or(usize::MAX);
    let mut notes = Vec::new();
    let mut titles: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for node in space.recent(count)? {
        let path = space.property(&node.id, crate::property_keys::PATH)?;
        titles
            .entry(node.title.trim().to_lowercase())
            .or_default()
            .push(path.clone().unwrap_or_else(|| node.title.clone()));
        notes.push(PreviewNote {
            path,
            title: node.title,
            unresolved: space.unresolved_links(&node.id)?,
        });
    }
    titles.retain(|_, paths| paths.len() > 1);
    for paths in titles.values_mut() {
        paths.sort();
    }
    notes.sort_by(|a, b| (&a.path, &a.title).cmp(&(&b.path, &b.title)));
    warnings.sort_by(|a, b| (&a.path, &a.reason).cmp(&(&b.path, &b.reason)));
    drop(space);
    temp.close().map_err(io_error(std::env::temp_dir()))?;
    Ok(PreviewReport {
        simulation_only: true,
        import,
        notes,
        duplicate_titles: titles,
        warnings,
    })
}

fn inventory(dir: &Path, prefix: &str, warnings: &mut Vec<PreviewWarning>) -> Result<()> {
    for entry in fs::read_dir(dir).map_err(io_error(dir))? {
        let entry = entry.map_err(io_error(dir))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            warnings.push(PreviewWarning {
                path: prefix.into(),
                reason: "A non-UTF-8 filename is skipped by the Markdown importer.".into(),
            });
            continue;
        };
        let rel = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}/{name}")
        };
        let meta = fs::symlink_metadata(entry.path()).map_err(io_error(entry.path()))?;
        let skipped = name.starts_with('.')
            || meta.file_type().is_symlink()
            || (meta.is_dir() && name.ends_with(".knode"));
        if skipped {
            warnings.push(PreviewWarning {
                path: rel,
                reason:
                    "Hidden path, symbolic link or nested .knode package: not traversed/imported."
                        .into(),
            });
        } else if meta.is_dir() {
            inventory(&entry.path(), &rel, warnings)?;
        } else if meta.is_file() {
            if name.ends_with(".canvas") {
                warnings.push(PreviewWarning { path:rel.clone(), reason:"Obsidian canvas is not converted into a Knode canvas by Markdown import; review separately.".into() });
            }
            if name.ends_with(".md") {
                match fs::read_to_string(entry.path()) {
                    Ok(text) if text.contains("```dataview") || text.contains("```tasks") || text.contains("<%") => warnings.push(PreviewWarning { path:rel, reason:"Possible plugin query/template: source text is preserved, but the plugin's execution is not migrated. This check is heuristic and does not enumerate every plugin.".into() }),
                    Err(e) => warnings.push(PreviewWarning { path:rel, reason:format!("Cannot read Markdown as UTF-8: {e}") }),
                    _ => {}
                }
            } else if name.to_lowercase().ends_with(".md") {
                warnings.push(PreviewWarning {
                    path: rel,
                    reason: "Only lowercase .md is recognized by the current folder importer."
                        .into(),
                });
            }
        } else {
            warnings.push(PreviewWarning {
                path: rel,
                reason: "Special file: not imported.".into(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rehearsal_reports_real_resolution_skips_attachments_and_preserves_source() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("vault");
        fs::create_dir_all(source.join("A")).unwrap();
        fs::create_dir_all(source.join("B")).unwrap();
        fs::create_dir_all(source.join("assets")).unwrap();
        fs::create_dir_all(source.join(".obsidian")).unwrap();
        let files: &[(&str, &[u8])] = &[
            (
                "A/README.md",
                b"See [[Missing]] and [[B/README]]. ![[picture.png]]",
            ),
            ("B/README.md", b"```dataview\nLIST\n```"),
            ("A/picture.png", b"synthetic image"),
            ("assets/shared.png", b"unused shared image"),
            ("Broken.md", &[0xff]),
            ("Board.canvas", b"{}"),
            (".obsidian/config.json", b"{}"),
        ];
        for (path, bytes) in files {
            fs::write(source.join(path), bytes).unwrap();
        }
        let report = preview(&source).unwrap();
        assert!(report.simulation_only);
        assert_eq!(report.notes.len(), 2);
        assert_eq!(report.duplicate_titles["readme"].len(), 2);
        assert!(
            report
                .notes
                .iter()
                .any(|n| n.unresolved.contains(&"Missing".into()))
        );
        assert_eq!(report.import.attached, vec!["A/picture.png"]);
        assert!(
            report
                .import
                .attachments
                .contains(&"assets/shared.png".into())
        );
        assert!(report.import.skipped.iter().any(|s| s.path == "Broken.md"));
        assert!(report.warnings.iter().any(|w| w.path == ".obsidian"));
        assert!(report.warnings.iter().any(|w| w.path == "Board.canvas"));
        for (path, bytes) in files {
            assert_eq!(fs::read(source.join(path)).unwrap(), *bytes);
        }
        assert!(!source.join(".knode").exists());
    }
    #[cfg(unix)]
    #[test]
    fn never_follows_symlinks_into_external_data() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("vault");
        fs::create_dir(&source).unwrap();
        fs::write(dir.path().join("external.md"), "outside").unwrap();
        std::os::unix::fs::symlink(dir.path().join("external.md"), source.join("Alias.md"))
            .unwrap();
        let report = preview(&source).unwrap();
        assert!(report.notes.is_empty());
        assert!(report.warnings.iter().any(|w| w.path == "Alias.md"));
    }
}
