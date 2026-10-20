//! [`Client`] method for `import.run`.

use std::path::Path;

use knode_format::Provenance;

use super::{Client, absolute, space_path};
use crate::Result;
use crate::import::{ImportResult, ImportRunParams, ImportSource, Secret};
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
            token: None,
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
            token: None,
        };
        self.request(methods::IMPORT_RUN, params)
    }

    /// Imports the highlights of Readwise through its API, only what changed since the last
    /// sync of the space. `token` is sent to the service for this call only; without it, the
    /// service reads its own (`READWISE_TOKEN` or the credential store). People only.
    pub fn import_readwise_api(
        &self,
        space: impl AsRef<Path>,
        token: Option<&str>,
        by: &Provenance,
    ) -> Result<ImportResult> {
        let params = ImportRunParams {
            space: space_path(space.as_ref())?,
            path: None,
            url: None,
            from: Some(ImportSource::ReadwiseApi),
            by: by.clone(),
            token: token.map(|t| Secret(t.to_string())),
        };
        self.request(methods::IMPORT_RUN, params)
    }
}
