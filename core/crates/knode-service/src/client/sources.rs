//! [`Client`] methods for `sources.*`. People only.

use std::path::Path;

use knode_format::Provenance;

use super::{Client, space_path};
use crate::Result;
use crate::protocol::methods;
use crate::sources::{
    ContainerInfo, History, SignInState, SignInStep, SourceParams, SourceStatus, SpaceSources,
};

impl Client {
    fn sources_call<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        space: Option<&Path>,
        source: Option<&str>,
        fill: impl FnOnce(&mut SourceParams),
    ) -> Result<T> {
        let mut params = SourceParams {
            space: space.map(space_path).transpose()?,
            source: source.map(str::to_string),
            ..SourceParams::default()
        };
        fill(&mut params);
        self.request(method, params)
    }

    /// Every source of the space, connectors then importers, with its status.
    pub fn sources_list(&self, space: impl AsRef<Path>) -> Result<Vec<SourceStatus>> {
        self.sources_call(methods::SOURCES_LIST, Some(space.as_ref()), None, |_| {})
    }

    /// The connectors connected to `space`, or with `None` to every space the service knows.
    pub fn sources_status(&self, space: Option<&Path>) -> Result<Vec<SpaceSources>> {
        self.sources_call(methods::SOURCES_STATUS, space, None, |_| {})
    }

    /// Connects the connector `source` to the space with the history chosen (from today when
    /// `None`), or turns on one already connected.
    pub fn sources_connect(
        &self,
        space: impl AsRef<Path>,
        source: &str,
        history: Option<History>,
        by: &Provenance,
    ) -> Result<SourceStatus> {
        self.sources_call(
            methods::SOURCES_CONNECT,
            Some(space.as_ref()),
            Some(source),
            |p| {
                p.history = history;
                p.by = Some(by.clone());
            },
        )
    }

    /// Connects the connector `source` to the space paused: its history is chosen and its
    /// chats, channels or folders can be listed and excluded, and nothing is read until
    /// [`sources_set_on`](Self::sources_set_on) resumes it. Pauses one already connected.
    pub fn sources_connect_paused(
        &self,
        space: impl AsRef<Path>,
        source: &str,
        history: Option<History>,
        by: &Provenance,
    ) -> Result<SourceStatus> {
        self.sources_call(
            methods::SOURCES_CONNECT,
            Some(space.as_ref()),
            Some(source),
            |p| {
                p.history = history;
                p.on = Some(false);
                p.by = Some(by.clone());
            },
        )
    }

    /// Pauses a connector in the space (`on` false), or resumes it.
    pub fn sources_set_on(
        &self,
        space: impl AsRef<Path>,
        source: &str,
        on: bool,
        by: &Provenance,
    ) -> Result<SourceStatus> {
        let method = if on {
            methods::SOURCES_RESUME
        } else {
            methods::SOURCES_PAUSE
        };
        self.sources_call(method, Some(space.as_ref()), Some(source), |p| {
            p.by = Some(by.clone());
        })
    }

    /// Excludes the chat, channel or folder `id` (named `name`) of a connector.
    pub fn sources_exclude(
        &self,
        space: impl AsRef<Path>,
        source: &str,
        id: &str,
        name: Option<&str>,
        by: &Provenance,
    ) -> Result<SourceStatus> {
        self.sources_call(
            methods::SOURCES_EXCLUDE,
            Some(space.as_ref()),
            Some(source),
            |p| {
                p.id = Some(id.to_string());
                p.name = name.map(str::to_string);
                p.by = Some(by.clone());
            },
        )
    }

    /// Includes again the chat, channel or folder `id` of a connector.
    pub fn sources_include(
        &self,
        space: impl AsRef<Path>,
        source: &str,
        id: &str,
        by: &Provenance,
    ) -> Result<SourceStatus> {
        self.sources_call(
            methods::SOURCES_INCLUDE,
            Some(space.as_ref()),
            Some(source),
            |p| {
                p.id = Some(id.to_string());
                p.by = Some(by.clone());
            },
        )
    }

    /// The chats, channels or folders of a connector, the excluded ones marked.
    pub fn sources_containers(
        &self,
        space: impl AsRef<Path>,
        source: &str,
    ) -> Result<Vec<ContainerInfo>> {
        self.sources_call(
            methods::SOURCES_CONTAINERS,
            Some(space.as_ref()),
            Some(source),
            |_| {},
        )
    }

    /// Runs a connector now, without waiting for it to finish.
    pub fn sources_run(&self, space: impl AsRef<Path>, source: &str) -> Result<SourceStatus> {
        self.sources_call(
            methods::SOURCES_RUN,
            Some(space.as_ref()),
            Some(source),
            |_| {},
        )
    }

    /// Disconnects a connector from the space. What it brought in stays. Returns `true`.
    pub fn sources_disconnect(
        &self,
        space: impl AsRef<Path>,
        source: &str,
        by: &Provenance,
    ) -> Result<bool> {
        self.sources_call(
            methods::SOURCES_DISCONNECT,
            Some(space.as_ref()),
            Some(source),
            |p| p.by = Some(by.clone()),
        )
    }

    /// Whether the connector `source` is signed in to the account it reads, on this machine.
    pub fn sources_account(&self, source: &str) -> Result<SignInState> {
        self.sources_call(methods::SOURCES_ACCOUNT, None, Some(source), |_| {})
    }

    /// Takes one step of signing the connector `source` in, and says what comes next.
    pub fn sources_sign_in(&self, source: &str, step: SignInStep) -> Result<SignInState> {
        self.sources_call(methods::SOURCES_SIGN_IN, None, Some(source), |p| {
            p.step = Some(step);
        })
    }

    /// Signs the connector `source` out and forgets its session. Returns whether it was
    /// signed in.
    pub fn sources_sign_out(&self, source: &str) -> Result<bool> {
        self.sources_call(methods::SOURCES_SIGN_OUT, None, Some(source), |_| {})
    }
}
