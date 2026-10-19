//! Turning capture off per project.

use std::path::{Path, PathBuf};

use crate::claude_code::project_dir_name;
use crate::discover::SessionFile;
use crate::model::{Agent, Conversation};

/// The projects whose conversations are not captured. Excluding a folder excludes everything
/// inside it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectFilter {
    excluded: Vec<PathBuf>,
}

impl ProjectFilter {
    /// A filter that captures every project.
    pub fn new() -> Self {
        Self::default()
    }

    /// A filter that leaves out these projects.
    pub fn excluding<I, P>(projects: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        Self {
            excluded: projects.into_iter().map(Into::into).collect(),
        }
    }

    /// Leaves out one more project.
    pub fn exclude(&mut self, project: impl Into<PathBuf>) {
        self.excluded.push(project.into());
    }

    /// The projects left out.
    pub fn excluded(&self) -> &[PathBuf] {
        &self.excluded
    }

    /// Whether conversations held in `project` are captured.
    pub fn allows_project(&self, project: &Path) -> bool {
        !self
            .excluded
            .iter()
            .any(|excluded| project.starts_with(excluded))
    }

    /// Whether this conversation is captured. Conversations with no known project are.
    pub fn allows(&self, conversation: &Conversation) -> bool {
        conversation
            .project
            .as_deref()
            .is_none_or(|project| self.allows_project(project))
    }

    /// A cheap check before reading a file: `false` when the file surely belongs to an excluded
    /// project. Only Claude Code names its folders after the project, and only exact matches are
    /// known from the name, so `true` still needs [`ProjectFilter::allows`] once the file is read.
    pub fn may_allow_file(&self, file: &SessionFile) -> bool {
        if file.agent != Agent::ClaudeCode {
            return true;
        }
        let Some(dir) = file.path.parent().and_then(Path::file_name) else {
            return true;
        };
        let dir = dir.to_string_lossy();
        !self
            .excluded
            .iter()
            .any(|excluded| project_dir_name(excluded) == dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excludes_a_project_and_everything_inside_it() {
        let filter = ProjectFilter::excluding(["/Users/example/private"]);
        assert!(!filter.allows_project(Path::new("/Users/example/private")));
        assert!(!filter.allows_project(Path::new("/Users/example/private/app")));
        assert!(filter.allows_project(Path::new("/Users/example/private-notes")));
        assert!(filter.allows_project(Path::new("/Users/example/project")));
    }

    #[test]
    fn allows_conversations_without_a_project() {
        let filter = ProjectFilter::excluding(["/Users/example/private"]);
        assert!(filter.allows(&Conversation::new(Agent::Codex, "id")));
    }

    #[test]
    fn skips_claude_code_folders_of_excluded_projects() {
        let filter = ProjectFilter::excluding(["/Users/example/private"]);
        let file = |dir: &str| SessionFile {
            agent: Agent::ClaudeCode,
            path: PathBuf::from(format!("/root/{dir}/s.jsonl")),
            session_id: "s".into(),
            len: 0,
            modified: None,
        };
        assert!(!filter.may_allow_file(&file("-Users-example-private")));
        assert!(filter.may_allow_file(&file("-Users-example-project")));
    }
}
