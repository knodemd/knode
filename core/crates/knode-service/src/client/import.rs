//! [`Client`] method for `import.run`.

use std::path::Path;

use knode_format::Provenance;

use super::{Client, absolute, space_path};
use crate::Result;
use crate::import::{ImportResult, ImportRunParams, ImportSource};
use crate::protocol::methods;

impl Client {
    /// Imports what is at `path` on this machine into the space: a data export, Markdown, a
    /// session, a PDF, a saved web page, Kindle or Readwise highlights, or the notes of Apple
    /// Notes, Bear, Notion or Evernote. `from` names the kind; without it, the service tells it
    /// from the path. With `from` Apple Notes or Bear and no path, the app's own notes on this
    /// Mac are read. People only.
    pub fn import_path(
        &self,
        space: impl AsRef<Path>,
        path: Option<&Path>,
        from: Option<ImportSource>,
        by: &Provenance,
    ) -> Result<ImportResult> {
        let params = ImportRunParams {
            space: space_path(space.as_ref())?,
            path: path.map(absolute).transpose()?,
            url: None,
            from,
            by: by.clone(),
        };
        self.request(methods::IMPORT_RUN, params)
    }

    /// Fetches the web page at `url` (`http` or `https` only) and imports it. People only.
    pub fn import_url(
        &self,
        space: impl AsRef<Path>,
        url: &str,
        by: &Provenance,
    ) -> Result<ImportResult> {
        let params = ImportRunParams {
            space: space_path(space.as_ref())?,
            path: None,
            url: Some(url.to_string()),
            from: Some(ImportSource::Web),
            by: by.clone(),
        };
        self.request(methods::IMPORT_RUN, params)
    }
}
