//! `sources.*`: the importers and connectors of the spaces. Blocking: called on a blocking
//! thread, like [`super::call`]. Only a person calls them.

use std::path::PathBuf;

use knode_format::{Provenance, Space};
use serde_json::Value;

use super::{State, lock_ignoring_poison, params, to_value};
use crate::protocol::{RpcError, codes, methods};
use crate::sources::runtime::Wake;
use crate::sources::{
    self, Container, SourceError, SourceParams, SourceSecrets, SpaceSources, state_key,
};
use crate::spaces::SpaceEventKind;

pub(super) fn call(state: &State, method: &str, raw: Value) -> Result<Value, RpcError> {
    let p: SourceParams = params(method, raw)?;
    if method == methods::SOURCES_STATUS {
        return to_value(status(state, &p)?);
    }
    if matches!(
        method,
        methods::SOURCES_ACCOUNT | methods::SOURCES_SIGN_IN | methods::SOURCES_SIGN_OUT
    ) {
        return account(state, method, p);
    }
    let path = p
        .space
        .clone()
        .ok_or_else(|| RpcError::invalid_params(format!("{method} needs space")))?;
    let space = state.space(&path)?;
    let mut space = lock_ignoring_poison(&space);
    let registry = state.registry();
    if method == methods::SOURCES_LIST {
        return to_value(registry.list(&space)?);
    }
    let id = source(&p, method)?;
    match method {
        methods::SOURCES_CONTAINERS => to_value(registry.containers(&space, &id).map_err(rpc)?),
        methods::SOURCES_RUN => {
            let status = registry.get(&space, &id)?;
            if status.setting.as_ref().is_none_or(|s| !s.on) {
                return Err(RpcError::from(knode_format::Error::Invalid(format!(
                    "{id} is not connected and on in this space"
                ))));
            }
            state.wake_sources(Wake::Run(state_key(&space.manifest().id, &id)));
            to_value(status)
        }
        methods::SOURCES_CONNECT
        | methods::SOURCES_PAUSE
        | methods::SOURCES_RESUME
        | methods::SOURCES_EXCLUDE
        | methods::SOURCES_INCLUDE
        | methods::SOURCES_DISCONNECT => {
            let by = by(&p, method)?;
            change(state, &mut space, method, &id, &p, &by)?;
            let key = format!("{}{id}", sources::SETTING_PREFIX);
            state.notify_space(&space, SpaceEventKind::Settings, |e| e.key = Some(key));
            state.wake_sources(Wake::Reload);
            match method {
                methods::SOURCES_DISCONNECT => {
                    drop(space);
                    forget_unused_account(state, &id);
                    to_value(true)
                }
                _ => to_value(registry.get(&space, &id)?),
            }
        }
        _ => Err(RpcError::new(
            codes::METHOD_NOT_FOUND,
            format!("no method named {method:?}"),
        )),
    }
}

fn change(
    state: &State,
    space: &mut Space,
    method: &str,
    id: &str,
    p: &SourceParams,
    by: &Provenance,
) -> Result<(), RpcError> {
    match method {
        methods::SOURCES_CONNECT => {
            let known: Vec<String> = state
                .registry()
                .connectors()
                .iter()
                .map(|c| c.info().id)
                .collect();
            sources::connect(space, &known, id, p.history, by)?;
        }
        methods::SOURCES_PAUSE => {
            sources::set_on(space, id, false, by)?;
        }
        methods::SOURCES_RESUME => {
            sources::set_on(space, id, true, by)?;
        }
        methods::SOURCES_EXCLUDE => {
            let container =
                Container::new(container_id(p, method)?, p.name.clone().unwrap_or_default());
            sources::exclude(space, id, container, by)?;
        }
        methods::SOURCES_INCLUDE => {
            sources::include(space, id, &container_id(p, method)?, by)?;
        }
        _ => {
            if sources::disconnect(space, id, by)? {
                let space_id = space.manifest().id.clone();
                state.registry().forget(&space_id, id).map_err(|e| {
                    RpcError::new(
                        codes::STORAGE,
                        format!("the state of {id} could not be forgotten: {e}"),
                    )
                })?;
            }
        }
    }
    Ok(())
}

/// `sources.account`, `sources.sign_in` and `sources.sign_out`: the account a connector
/// reads belongs to this machine, not to a space. Its session goes only into the keychain.
fn account(state: &State, method: &str, p: SourceParams) -> Result<Value, RpcError> {
    let id = source(&p, method)?;
    let registry = state.registry();
    let connector = registry
        .connector(&id)
        .ok_or_else(|| RpcError::from(sources::unknown(&id)))?
        .clone();
    let secrets = SourceSecrets::new(registry.home(), &id);
    match method {
        methods::SOURCES_ACCOUNT => to_value(connector.sign_in_state(&secrets).map_err(rpc)?),
        methods::SOURCES_SIGN_IN => {
            let step = p
                .step
                .ok_or_else(|| RpcError::invalid_params(format!("{method} needs step")))?;
            let signed = connector.sign_in(step, &secrets).map_err(rpc)?;
            if matches!(signed, sources::SignInState::SignedIn { .. }) {
                // Signed in again: it runs now in every space, without waiting out the
                // backoff of the runs that failed for want of a session.
                state.wake_sources(Wake::Reload);
                for key in registry.states().keys_of(&id) {
                    state.wake_sources(Wake::Run(key));
                }
            }
            to_value(signed)
        }
        _ => to_value(connector.sign_out(&secrets).map_err(rpc)?),
    }
}

/// Signs a connector out of its account once no space this service knows has it connected:
/// its session is forgotten with the last space it brought things into.
fn forget_unused_account(state: &State, id: &str) {
    let registry = state.registry();
    let Some(connector) = registry.connector(id).cloned() else {
        return;
    };
    if !connector
        .info()
        .permissions
        .contains(&sources::Permission::SignIn)
    {
        return;
    }
    let still = known_spaces(state).into_iter().any(|path| {
        let Ok(space) = state.space(&path) else {
            return false;
        };
        let space = lock_ignoring_poison(&space);
        sources::source_settings(&space).is_ok_and(|s| s.contains_key(id))
    });
    if still {
        return;
    }
    let secrets = SourceSecrets::new(registry.home(), id);
    if let Err(e) = connector.sign_out(&secrets) {
        eprintln!("Knode Service: {id} could not sign out: {e}");
    }
}

/// The spaces in `config.toml` and the ones open now.
fn known_spaces(state: &State) -> Vec<PathBuf> {
    let home = state.registry().home().to_path_buf();
    let mut paths: Vec<PathBuf> =
        knode_config::Config::load_from(&home.join(knode_config::CONFIG_FILE))
            .map(|c| c.known_spaces())
            .unwrap_or_default()
            .into_iter()
            .filter(|p| p.is_absolute() && p.exists())
            .collect();
    paths.extend(state.open_paths());
    paths
}

/// The connectors connected to the space asked for, or to every space the service knows.
fn status(state: &State, p: &SourceParams) -> Result<Vec<SpaceSources>, RpcError> {
    let paths: Vec<PathBuf> = match &p.space {
        Some(path) => vec![path.clone()],
        None => known_spaces(state),
    };
    let mut out: Vec<SpaceSources> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for path in paths {
        let space = match state.space(&path) {
            Ok(space) => space,
            Err(e) if p.space.is_some() => return Err(e),
            Err(_) => continue,
        };
        let space = lock_ignoring_poison(&space);
        let id = space.manifest().id.clone();
        if seen.contains(&id) {
            continue;
        }
        seen.push(id);
        let mut list = state.registry().connected(&space)?;
        if let Some(source) = &p.source {
            list.retain(|s| &s.info.id == source);
        }
        if list.is_empty() && p.space.is_none() {
            continue;
        }
        out.push(SpaceSources {
            space: space.path().to_path_buf(),
            name: space.manifest().name.clone(),
            sources: list,
        });
    }
    Ok(out)
}

fn source(p: &SourceParams, method: &str) -> Result<String, RpcError> {
    p.source
        .as_deref()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| RpcError::invalid_params(format!("{method} needs source")))
}

fn container_id(p: &SourceParams, method: &str) -> Result<String, RpcError> {
    p.id.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| RpcError::invalid_params(format!("{method} needs id")))
}

fn by(p: &SourceParams, method: &str) -> Result<Provenance, RpcError> {
    p.by.clone()
        .ok_or_else(|| RpcError::invalid_params(format!("{method} needs by")))
}

/// A connector's error as the service answers it.
fn rpc(error: SourceError) -> RpcError {
    let kind = error.kind();
    let code = match &error {
        SourceError::Permission(..) => codes::NO_ACCESS,
        SourceError::NeedsUpdate(_) | SourceError::Unavailable(_) | SourceError::SignIn(_) => {
            codes::INVALID
        }
        _ => codes::INTERNAL_ERROR,
    };
    match error {
        SourceError::Space(e) => RpcError::from(e),
        other => RpcError::new(code, other.to_string()).with_kind(kind),
    }
}
