//! Photos added to a space, with what the device reads in them, stored as nodes.
//!
//! A photo is one node of kind [`PHOTO_KIND`]: a line saying when and where it was taken, an
//! `![[name]]` embed of the original file (attached unchanged) and, once read, the text found in
//! it and what it shows between two HTML comment lines ([`set_reading`]), so a search for "the
//! whiteboard photo" finds it. Its properties ([`keys`]) hold when and where it was taken, the
//! camera, its size, the hash of its file (the same file added twice is one photo) and how far
//! its reading is. A photo added for a project is `part-of` it.
//!
//! Reading happens on the device, in the app (Vision on the Mac); this module only stores what it
//! finds and keeps track of what is still to read ([`pending`]) and what could not be read
//! ([`failed`]). Nothing is sent anywhere. Where the space turns location off (the space setting
//! [`LOCATION_SETTING`]), no coordinates or place are stored, and turning it off removes those
//! the photos have ([`forget_locations`]). The attached original is never changed, so its own
//! EXIF data, location included, stays in the file.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;

use chrono::{DateTime, FixedOffset, Local, TimeZone};
use knode_format::{
    Error, NewNode, NodeId, NodeUpdate, Properties, Provenance, Result, Space, kinds, link_kinds,
    setting_keys,
};
use serde::{Deserialize, Serialize};

use crate::media;
use crate::recordings::{insert_after, marked_range, one_line, string_enum};

/// The kind of a photo's node.
pub const PHOTO_KIND: &str = "photo";

/// The properties of a photo's node. Every value is a string.
pub mod keys {
    /// When it was taken, in Unix milliseconds.
    pub const TAKEN_AT: &str = "photo.taken_at";
    /// Where it was taken: latitude in degrees, north positive.
    pub const LATITUDE: &str = "photo.latitude";
    /// Where it was taken: longitude in degrees, east positive.
    pub const LONGITUDE: &str = "photo.longitude";
    /// The name of the place it was taken, when the device could tell it without the network.
    pub const PLACE: &str = "photo.place";
    /// The camera, such as `Apple iPhone 16 Pro`.
    pub const CAMERA: &str = "photo.camera";
    /// Its width, in pixels.
    pub const WIDTH: &str = "photo.width";
    /// Its height, in pixels.
    pub const HEIGHT: &str = "photo.height";
    /// The hash of its file, `sha256:<hex>`: the same file added again is this photo.
    pub const KEY: &str = "photo.key";
    /// The name of the attached original file.
    pub const FILE: &str = "photo.file";
    /// Its reading: `pending`, `done` or `failed`.
    pub const READING: &str = "photo.reading";
    /// Why its reading failed.
    pub const ERROR: &str = "photo.error";
}

/// The space setting that says whether photos keep where they were taken: `on` (the default) or
/// `off`.
pub const LOCATION_SETTING: &str = setting_keys::PHOTOS_LOCATION;

/// Whether the space keeps where photos were taken: its `photos.location` setting is not `off`.
pub fn location_enabled(space: &Space) -> Result<bool> {
    Ok(space.setting(LOCATION_SETTING)?.as_deref() != Some("off"))
}

string_enum! {
    /// How far the reading of a photo is.
    ReadingState {
        /// Still to be read.
        Pending => "pending",
        /// Read.
        Done => "done",
        /// Reading it failed; [`Photo::error`] says why.
        Failed => "failed",
    }
}

/// What the file of a photo says about it. Every part is optional.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PhotoMetadata {
    /// When it was taken, in Unix milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taken_at: Option<i64>,
    /// Latitude in degrees, north positive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latitude: Option<f64>,
    /// Longitude in degrees, east positive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub longitude: Option<f64>,
    /// The name of the place, only when the device could tell it without the network.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<String>,
    /// The camera.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<String>,
    /// Width in pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    /// Height in pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
}

/// A photo to store with [`create`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewPhoto {
    /// Its title; the file's name without its extension when empty.
    #[serde(default)]
    pub title: String,
    /// The file. It is copied into the space unchanged.
    pub path: PathBuf,
    /// The project it belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<NodeId>,
    /// What its file says about it, as read by the app.
    #[serde(default)]
    pub metadata: PhotoMetadata,
}

/// A stored photo, as its node and properties say.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Photo {
    /// Its node.
    pub id: NodeId,
    /// Its title.
    pub title: String,
    /// The name of the attached original file.
    pub file: String,
    /// The hash of its file, `sha256:<hex>`.
    pub key: Option<String>,
    /// What is known about it: when, where, the camera and its size.
    pub metadata: PhotoMetadata,
    /// How far its reading is.
    pub reading: ReadingState,
    /// Why its reading failed.
    pub error: Option<String>,
    /// The project it is part of.
    pub project: Option<NodeId>,
}

/// What the device read in a photo.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PhotoReading {
    /// What read it, such as `Apple Vision`.
    pub engine: String,
    /// The text found in it, line by line.
    #[serde(default)]
    pub text: String,
    /// What it shows, most likely first, such as `whiteboard`.
    #[serde(default)]
    pub labels: Vec<String>,
    /// A short description, where an on-device model wrote one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// What its file says about it, read at the same time; added to what the photo already has.
    #[serde(default)]
    pub metadata: PhotoMetadata,
}

/// The line before the reading in a photo's body. An HTML comment: Markdown hides it.
pub const READING_START: &str = "<!-- knode:photo-reading -->";
/// The line after the reading in a photo's body.
pub const READING_END: &str = "<!-- /knode:photo-reading -->";

/// Stores a new photo: its node, with the original file attached unchanged, its properties and a
/// `part-of` link to its project. Its reading is pending. The file is checked before anything is
/// written; where the space turns location off, no coordinates or place are kept.
///
/// The same file (by hash) the space already holds as a photo returns that photo, with its
/// project link added. The hash is written last, and a node left half made by a failed step is
/// deleted, so a hash the space has always names a whole photo.
pub fn create(space: &mut Space, new: &NewPhoto, by: &Provenance) -> Result<Photo> {
    media::check_file(&new.path)?;
    let key = media::file_key(&new.path)?;
    if let Some(id) = find_by_key(space, &key)? {
        if let Some(project) = &new.project
            && space.get_node(project)?.is_some()
        {
            space.link(&id, project, link_kinds::PART_OF, by)?;
        }
        return get(space, &id);
    }
    create_separate(space, new, &key, by)
}

/// Stores a new photo like [`create`], even when the space holds the same file as a photo
/// already: for a source that must not take over a node someone else made, such as the Photos
/// library. The file is stored once either way (blobs are kept by hash). `key` is the file's
/// [`media::file_key`].
pub(crate) fn create_separate(
    space: &mut Space,
    new: &NewPhoto,
    key: &str,
    by: &Provenance,
) -> Result<Photo> {
    media::check_file(&new.path)?;
    if let Some(project) = &new.project
        && space.get_node(project)?.is_none()
    {
        return Err(Error::NotFound(*project));
    }
    let metadata = allowed(space, &new.metadata)?;
    let name = media::file_name(&new.path);
    let title = match one_line(&new.title) {
        t if t.is_empty() => media::stem(&new.path),
        t => t,
    };
    let mut props = metadata_properties(&metadata);
    props.insert(keys::FILE.into(), name.clone());
    props.insert(keys::READING.into(), ReadingState::Pending.to_string());
    let header = header(&metadata, &Local);
    let node = space.create_node_with_properties(
        NewNode::note(title, body(&header, &name)).with_kind(PHOTO_KIND),
        &props,
        by,
    )?;
    let id = node.id;
    let filled = (|| -> Result<()> {
        let attachment = space.attach_file(&id, &new.path, by)?;
        if attachment.name != name {
            space.update_node(
                &id,
                NodeUpdate {
                    body: Some(body(&header, &attachment.name)),
                    ..Default::default()
                },
                by,
            )?;
            space.set_property(&id, keys::FILE, &attachment.name, by)?;
        }
        if let Some(project) = &new.project {
            space.link(&id, project, link_kinds::PART_OF, by)?;
        }
        space.set_property(&id, keys::KEY, key, by)?;
        Ok(())
    })();
    if let Err(e) = filled {
        // Best effort: the error says what went wrong either way.
        let _ = space.delete_node(&id, by);
        return Err(e);
    }
    get(space, &id)
}

/// The photo of the space whose file has this hash, if any.
pub fn find_by_key(space: &Space, key: &str) -> Result<Option<NodeId>> {
    Ok(space
        .find_by_property(keys::KEY, key)?
        .into_iter()
        .find(|s| s.kind == PHOTO_KIND)
        .map(|s| s.id))
}

/// A photo, read from its node. Fails with [`Error::NotFound`] when there is no such node and
/// [`Error::Invalid`] when the node is not a photo.
pub fn get(space: &Space, id: &NodeId) -> Result<Photo> {
    let node = space.get_node(id)?.ok_or(Error::NotFound(*id))?;
    if node.kind != PHOTO_KIND {
        return Err(Error::Invalid(format!("node {id} is not a photo")));
    }
    let props = space.properties(id)?;
    let text = |key: &str| props.get(key).filter(|v| !v.is_empty()).cloned();
    let mut project = None;
    for link in space.links_from(id)? {
        if link.kind == link_kinds::PART_OF
            && space
                .get_node(&link.to)?
                .is_some_and(|n| n.kind == kinds::PROJECT)
        {
            project = Some(link.to);
            break;
        }
    }
    Ok(Photo {
        id: *id,
        title: node.title,
        file: text(keys::FILE).unwrap_or_default(),
        key: text(keys::KEY),
        metadata: PhotoMetadata {
            taken_at: text(keys::TAKEN_AT).and_then(|v| v.parse().ok()),
            latitude: text(keys::LATITUDE).and_then(|v| v.parse().ok()),
            longitude: text(keys::LONGITUDE).and_then(|v| v.parse().ok()),
            place: text(keys::PLACE),
            camera: text(keys::CAMERA),
            width: text(keys::WIDTH).and_then(|v| v.parse().ok()),
            height: text(keys::HEIGHT).and_then(|v| v.parse().ok()),
        },
        reading: text(keys::READING)
            .and_then(|s| s.parse().ok())
            .unwrap_or(ReadingState::Pending),
        error: text(keys::ERROR),
        project,
    })
}

/// The photos still to read, oldest first (in the order they were added). A photo that cannot
/// be read is left out, and said on standard error, so it does not hide the others.
pub fn pending(space: &Space) -> Result<Vec<Photo>> {
    find(space, ReadingState::Pending)
}

/// The photos whose reading failed, newest first. A photo that cannot be read is left out.
pub fn failed(space: &Space) -> Result<Vec<Photo>> {
    let mut list = find(space, ReadingState::Failed)?;
    list.reverse();
    Ok(list)
}

fn find(space: &Space, state: ReadingState) -> Result<Vec<Photo>> {
    let mut found = BTreeMap::new();
    let mut order = Vec::new();
    for summary in space.find_by_property(keys::READING, state.as_str())? {
        if summary.kind != PHOTO_KIND || found.contains_key(&summary.id) {
            continue;
        }
        match get(space, &summary.id) {
            Ok(photo) if photo.reading == state => {
                order.push(summary.id);
                found.insert(summary.id, photo);
            }
            Ok(_) => {}
            Err(e) => eprintln!("Knode: skipping photo {}: {e}", summary.id),
        }
    }
    Ok(order
        .into_iter()
        .filter_map(|id| found.remove(&id))
        .collect())
}

/// Writes what the device read in a photo into its body, between the lines [`READING_START`] and
/// [`READING_END`]: replacing what is between them when the body has them once, in order, and
/// else as a new block after the embed of its file. Nothing outside the markers changes, except
/// the first line, said again when it is still the one Knode wrote and the metadata adds to it.
/// The metadata read is added to the photo's properties. Where the space turns location off, no
/// location is added, and any the photo still has is removed. Its reading is then done.
pub fn set_reading(
    space: &mut Space,
    id: &NodeId,
    reading: &PhotoReading,
    by: &Provenance,
) -> Result<Photo> {
    let photo = get(space, id)?;
    let current = space.get_node(id)?.ok_or(Error::NotFound(*id))?;
    let added = allowed(space, &reading.metadata)?;
    let mut merged = merge(&photo.metadata, &added);
    let located = location_enabled(space)?;
    if !located {
        merged = without_location(&merged);
    }
    let mut body = current.body.clone();
    let old_header = header(&photo.metadata, &Local);
    let new_header = header(&merged, &Local);
    if old_header != new_header && body.lines().next() == Some(old_header.as_str()) {
        body = format!("{new_header}{}", &body[old_header.len()..]);
    }
    let block = format!(
        "{READING_START}\n{}\n{READING_END}",
        render_reading(reading)
    );
    body = match marked_range(&body, READING_START, READING_END) {
        Some((start, end)) => format!("{}{block}{}", &body[..start], &body[end..]),
        None => insert_after(&body, &[format!("![[{}]]", photo.file)], &block),
    };
    if body != current.body {
        space.update_node(
            id,
            NodeUpdate {
                body: Some(body),
                ..Default::default()
            },
            by,
        )?;
    }
    let mut props = metadata_properties(&added);
    props.insert(keys::READING.into(), ReadingState::Done.to_string());
    space.set_properties(id, &props, by)?;
    if !located {
        remove_location_properties(space, id)?;
    }
    space.remove_property(id, keys::ERROR)?;
    get(space, id)
}

/// Marks the reading of a photo as failed, with why.
pub fn set_failed(space: &mut Space, id: &NodeId, error: &str, by: &Provenance) -> Result<Photo> {
    get(space, id)?;
    let mut props =
        Properties::from([(keys::READING.to_string(), ReadingState::Failed.to_string())]);
    let error = error.trim();
    if error.is_empty() {
        space.remove_property(id, keys::ERROR)?;
    } else {
        props.insert(keys::ERROR.into(), error.to_string());
    }
    space.set_properties(id, &props, by)?;
    get(space, id)
}

/// Puts the reading of a photo back to pending, so the app reads it again.
pub fn retry(space: &mut Space, id: &NodeId, by: &Provenance) -> Result<Photo> {
    get(space, id)?;
    space.set_property(id, keys::READING, ReadingState::Pending.as_str(), by)?;
    space.remove_property(id, keys::ERROR)?;
    get(space, id)
}

/// Removes where they were taken from every photo of the space, as `by`: their coordinates and
/// place, and the place from the first line of their body when it is still the one Knode wrote.
/// Called when the space turns location off. The attached originals are not changed: their own
/// EXIF data, location included, stays in the files. Returns how many photos changed.
pub fn forget_locations(space: &mut Space, by: &Provenance) -> Result<usize> {
    let mut ids = BTreeSet::new();
    for state in [
        ReadingState::Pending,
        ReadingState::Done,
        ReadingState::Failed,
    ] {
        for summary in space.find_by_property(keys::READING, state.as_str())? {
            if summary.kind == PHOTO_KIND {
                ids.insert(summary.id);
            }
        }
    }
    let mut changed = 0;
    for id in ids {
        let photo = get(space, &id)?;
        let node = space.get_node(&id)?.ok_or(Error::NotFound(id))?;
        let old_header = header(&photo.metadata, &Local);
        let new_header = header(&without_location(&photo.metadata), &Local);
        let mut touched = false;
        if old_header != new_header && node.body.lines().next() == Some(old_header.as_str()) {
            space.update_node(
                &id,
                NodeUpdate {
                    body: Some(format!("{new_header}{}", &node.body[old_header.len()..])),
                    ..Default::default()
                },
                by,
            )?;
            touched = true;
        }
        touched |= remove_location_properties(space, &id)?;
        if touched {
            changed += 1;
        }
    }
    Ok(changed)
}

fn without_location(metadata: &PhotoMetadata) -> PhotoMetadata {
    PhotoMetadata {
        latitude: None,
        longitude: None,
        place: None,
        ..metadata.clone()
    }
}

/// Removes the coordinates and place of a photo. Returns whether it had any.
fn remove_location_properties(space: &mut Space, id: &NodeId) -> Result<bool> {
    let mut removed = false;
    for key in [keys::LATITUDE, keys::LONGITUDE, keys::PLACE] {
        removed |= space.remove_property(id, key)?;
    }
    Ok(removed)
}

/// `metadata` without the location where the space turns it off, checked. Coordinates out of
/// range (a bad GPS reading) are dropped, both of them, rather than refusing the photo.
fn allowed(space: &Space, metadata: &PhotoMetadata) -> Result<PhotoMetadata> {
    let mut metadata = metadata.clone();
    if !location_enabled(space)? {
        metadata = without_location(&metadata);
    }
    let bad_latitude = metadata
        .latitude
        .is_some_and(|v| !(-90.0..=90.0).contains(&v));
    let bad_longitude = metadata
        .longitude
        .is_some_and(|v| !(-180.0..=180.0).contains(&v));
    if bad_latitude || bad_longitude {
        metadata.latitude = None;
        metadata.longitude = None;
    }
    metadata.place = metadata
        .place
        .as_deref()
        .map(one_line)
        .filter(|p| !p.is_empty());
    metadata.camera = metadata
        .camera
        .as_deref()
        .map(one_line)
        .filter(|c| !c.is_empty());
    Ok(metadata)
}

/// What `old` has, with what `new` has on top.
fn merge(old: &PhotoMetadata, new: &PhotoMetadata) -> PhotoMetadata {
    PhotoMetadata {
        taken_at: new.taken_at.or(old.taken_at),
        latitude: new.latitude.or(old.latitude),
        longitude: new.longitude.or(old.longitude),
        place: new.place.clone().or_else(|| old.place.clone()),
        camera: new.camera.clone().or_else(|| old.camera.clone()),
        width: new.width.or(old.width),
        height: new.height.or(old.height),
    }
}

fn metadata_properties(metadata: &PhotoMetadata) -> Properties {
    let mut props = Properties::new();
    let mut put = |key: &str, value: Option<String>| {
        if let Some(value) = value {
            props.insert(key.to_string(), value);
        }
    };
    put(keys::TAKEN_AT, metadata.taken_at.map(|v| v.to_string()));
    put(keys::LATITUDE, metadata.latitude.map(degrees));
    put(keys::LONGITUDE, metadata.longitude.map(degrees));
    put(keys::PLACE, metadata.place.clone());
    put(keys::CAMERA, metadata.camera.clone());
    put(keys::WIDTH, metadata.width.map(|v| v.to_string()));
    put(keys::HEIGHT, metadata.height.map(|v| v.to_string()));
    props
}

/// Degrees with six decimals (about 10 cm), without trailing zeros.
fn degrees(value: f64) -> String {
    let text = format!("{value:.6}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" {
        "0".into()
    } else {
        text.to_string()
    }
}

/// The first line of a photo's body: when and where it was taken, in the time zone given.
fn header<Tz: TimeZone>(metadata: &PhotoMetadata, zone: &Tz) -> String
where
    Tz::Offset: fmt::Display,
{
    let when = metadata
        .taken_at
        .and_then(|ms| zone.timestamp_millis_opt(ms).single())
        .map(|t: DateTime<Tz>| t.fixed_offset())
        .map(|t: DateTime<FixedOffset>| {
            format!("on {} at {}", t.format("%A %-d %B %Y"), t.format("%H:%M"))
        });
    let place = match (&metadata.place, metadata.latitude, metadata.longitude) {
        (Some(place), _, _) => Some(format!("in {place}")),
        (None, Some(lat), Some(lon)) => Some(format!("at {}, {}", degrees(lat), degrees(lon))),
        _ => None,
    };
    let mut line = match (when, place) {
        (Some(when), Some(place)) => format!("Taken {when}, {place}."),
        (Some(when), None) => format!("Taken {when}."),
        (None, Some(place)) => format!("Taken {place}."),
        (None, None) => "A photo.".into(),
    };
    if let Some(camera) = &metadata.camera {
        line.pop();
        line.push_str(&format!(", with {camera}."));
    }
    line
}

fn body(header: &str, name: &str) -> String {
    format!("{header}\n\n![[{name}]]\n")
}

const TEXT_HEADING: &str = "## Text in the photo";
const LABELS_HEADING: &str = "## What it shows";
const DESCRIPTION_HEADING: &str = "## Description";

/// What was read, as Markdown: the text found, what it shows, a description, and who read it.
/// A line of text that would close the block, or start a heading, is kept as plain text.
fn render_reading(reading: &PhotoReading) -> String {
    let mut parts = Vec::new();
    let lines: Vec<String> = reading
        .text
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let trimmed = l.trim_start();
            if trimmed == READING_START
                || trimmed == READING_END
                || trimmed.starts_with('#')
                || trimmed.starts_with('>')
            {
                format!("\\{trimmed}")
            } else {
                trimmed.to_string()
            }
        })
        .collect();
    if !lines.is_empty() {
        // Each line its own paragraph line: a hard break keeps the photo's line breaks.
        parts.push(format!("{TEXT_HEADING}\n\n{}", lines.join("  \n")));
    }
    let mut seen = Vec::new();
    for label in &reading.labels {
        let label = one_line(label);
        if !label.is_empty() && !seen.contains(&label) {
            seen.push(label);
        }
    }
    if !seen.is_empty() {
        parts.push(format!("{LABELS_HEADING}\n\n{}", seen.join(", ")));
    }
    if let Some(description) = reading
        .description
        .as_deref()
        .map(one_line)
        .filter(|d| !d.is_empty())
    {
        parts.push(format!("{DESCRIPTION_HEADING}\n\n{description}"));
    }
    if parts.is_empty() {
        parts.push("Nothing to read in this photo.".into());
    }
    let engine = one_line(&reading.engine);
    let engine = if engine.is_empty() {
        "the device"
    } else {
        &engine
    };
    parts.push(format!("_Read on this Mac by {engine}._"));
    parts.join("\n\n")
}

#[cfg(test)]
mod tests;
