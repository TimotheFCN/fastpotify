//! Spotify's playlist rootlist: strict parsing, row projection, and writes.

use std::collections::{HashMap, HashSet};
use std::fmt;

use anyhow::{Context, Result, anyhow, bail};
use librespot_protocol::playlist4_external::{
    Add, Delta, Item as WireItem, ListChanges, Mov, Op, Rem, SelectedListContent, UpdateItemUris,
    UriReplacement, op::Kind,
};
use protobuf::Message as _;

use crate::model::Loadable;

const START_PREFIX: &str = "spotify:start-group:";
const END_PREFIX: &str = "spotify:end-group:";
const PLAYLIST_PREFIX: &str = "spotify:playlist:";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FolderId(u64);

impl FolderId {
    fn parse(raw: &str) -> Result<Self> {
        if raw.is_empty() || raw.len() > 16 || !raw.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("invalid folder id {raw:?}");
        }
        Ok(Self(u64::from_str_radix(raw, 16)?))
    }

    fn random() -> Self {
        Self(rand::random())
    }

    fn wire(self) -> String {
        format!("{:016x}", self.0)
    }
}

impl fmt::Display for FolderId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:016x}", self.0)
    }
}

impl std::str::FromStr for FolderId {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        Self::parse(value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RootlistItem {
    Playlist {
        uri: String,
    },
    FolderStart {
        id: FolderId,
        name: String,
        uri: String,
    },
    FolderEnd {
        id: FolderId,
        uri: String,
    },
    Unknown {
        uri: String,
    },
}

impl RootlistItem {
    fn uri(&self) -> &str {
        match self {
            Self::Playlist { uri }
            | Self::FolderStart { uri, .. }
            | Self::FolderEnd { uri, .. }
            | Self::Unknown { uri } => uri,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    items: Vec<RootlistItem>,
}

impl Snapshot {
    pub fn parse(uris: &[String]) -> Result<Self> {
        let mut items = Vec::with_capacity(uris.len());
        let mut stack = Vec::new();
        let mut folders = HashSet::new();

        for uri in uris {
            if let Some(marker) = uri.strip_prefix(START_PREFIX) {
                let (raw_id, encoded_name) = marker
                    .split_once(':')
                    .with_context(|| format!("malformed folder start marker {uri:?}"))?;
                let id = FolderId::parse(raw_id)
                    .with_context(|| format!("malformed folder start marker {uri:?}"))?;
                if !folders.insert(id) {
                    bail!("folder id {raw_id:?} occurs more than once");
                }
                let name = decode_name(encoded_name)
                    .with_context(|| format!("malformed folder name in {uri:?}"))?;
                stack.push(id);
                items.push(RootlistItem::FolderStart {
                    id,
                    name,
                    uri: uri.clone(),
                });
            } else if let Some(raw_id) = uri.strip_prefix(END_PREFIX) {
                if raw_id.contains(':') {
                    bail!("malformed folder end marker {uri:?}");
                }
                let id = FolderId::parse(raw_id)
                    .with_context(|| format!("malformed folder end marker {uri:?}"))?;
                let Some(open) = stack.pop() else {
                    bail!("folder end marker {uri:?} has no start marker");
                };
                if open != id {
                    bail!("folder end marker {uri:?} does not match its start marker");
                }
                items.push(RootlistItem::FolderEnd {
                    id,
                    uri: uri.clone(),
                });
            } else if uri.starts_with(PLAYLIST_PREFIX) {
                items.push(RootlistItem::Playlist { uri: uri.clone() });
            } else {
                items.push(RootlistItem::Unknown { uri: uri.clone() });
            }
        }

        if !stack.is_empty() {
            bail!("rootlist has unclosed folder markers");
        }
        Ok(Self { items })
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub(crate) fn uris(&self) -> impl Iterator<Item = &str> {
        self.items.iter().map(RootlistItem::uri)
    }

    fn has_folder(&self, id: FolderId) -> bool {
        self.folder_start(id).is_some()
    }

    pub fn folders(&self) -> Vec<Folder> {
        let mut folders = Vec::new();
        let mut stack = Vec::new();
        for item in &self.items {
            match item {
                RootlistItem::FolderStart { id, name, .. } => {
                    folders.push(Folder {
                        id: *id,
                        name: name.clone(),
                        depth: stack.len() as u8,
                        parent: stack.last().copied(),
                    });
                    stack.push(*id);
                }
                RootlistItem::FolderEnd { .. } => {
                    stack.pop();
                }
                _ => {}
            }
        }
        folders
    }

    pub fn parent_of(&self, node: &Node) -> Result<Option<FolderId>> {
        let span = self.resolve_node(node)?;
        Ok(self.parent_at(span.start))
    }

    /// True when the rootlist lists this playlist exactly once; a duplicated
    /// playlist cannot be moved unambiguously.
    pub fn contains_playlist(&self, uri: &str) -> bool {
        self.playlist_uris()
            .filter(|held| *held == uri)
            .take(2)
            .count()
            == 1
    }

    pub fn playlist_uris(&self) -> impl Iterator<Item = &str> {
        self.items.iter().filter_map(|item| match item {
            RootlistItem::Playlist { uri } => Some(uri.as_str()),
            _ => None,
        })
    }

    pub fn valid_folder_destinations(&self, node: &Node) -> Result<Vec<Folder>> {
        let source = self.resolve_node(node)?;
        let mut folders = Vec::new();
        let mut stack = Vec::new();
        for (index, item) in self.items.iter().enumerate() {
            match item {
                RootlistItem::FolderStart { id, name, .. } => {
                    if index < source.start || index > source.end {
                        folders.push(Folder {
                            id: *id,
                            name: name.clone(),
                            depth: stack.len() as u8,
                            parent: stack.last().copied(),
                        });
                    }
                    stack.push(*id);
                }
                RootlistItem::FolderEnd { .. } => {
                    stack.pop();
                }
                _ => {}
            }
        }
        Ok(folders)
    }

    /// Whether `parent` may receive `node`: both exist and the destination is
    /// not the node itself or one of its descendants.
    pub fn is_valid_destination(&self, node: &Node, parent: FolderId) -> bool {
        let Ok(source) = self.resolve_node(node) else {
            return false;
        };
        self.folder_start(parent)
            .is_some_and(|start| start < source.start || start > source.end)
    }

    pub fn folder_contents(&self, id: FolderId) -> Result<FolderContents> {
        let span = self.folder_span(id)?;
        let mut playlist_uris = Vec::new();
        let mut has_unknown = false;
        for item in &self.items[span.start + 1..span.end] {
            match item {
                RootlistItem::Playlist { uri } => playlist_uris.push(uri.clone()),
                RootlistItem::Unknown { .. } => has_unknown = true,
                _ => {}
            }
        }
        Ok(FolderContents {
            playlist_uris,
            has_unknown,
        })
    }

    pub fn project_rows(&self, expanded: &HashSet<FolderId>, pinned: &[String]) -> Vec<Row> {
        let pinned_set: HashSet<&str> = pinned.iter().map(String::as_str).collect();
        let mut counts: HashMap<FolderId, usize> = HashMap::new();
        let mut stack = Vec::new();
        for item in &self.items {
            match item {
                RootlistItem::FolderStart { id, .. } => stack.push(*id),
                RootlistItem::FolderEnd { .. } => {
                    stack.pop();
                }
                RootlistItem::Playlist { .. } => {
                    for id in &stack {
                        *counts.entry(*id).or_default() += 1;
                    }
                }
                RootlistItem::Unknown { .. } => {}
            }
        }

        // Pinned rows keep the user's pin order; the set is only for lookups.
        let mut rows = pinned
            .iter()
            .map(|uri| Row::Playlist {
                uri: uri.clone(),
                depth: 0,
            })
            .collect::<Vec<_>>();
        let mut depth = 0u8;
        let mut hidden_at = None;
        for item in &self.items {
            match item {
                RootlistItem::FolderStart { id, name, .. } => {
                    if hidden_at.is_none() {
                        let is_collapsed = !expanded.contains(id);
                        rows.push(Row::Folder {
                            id: *id,
                            name: name.clone(),
                            depth,
                            playlist_count: counts.get(id).copied().unwrap_or_default(),
                            collapsed: is_collapsed,
                        });
                        if is_collapsed {
                            hidden_at = Some(depth);
                        }
                    }
                    depth = depth.saturating_add(1);
                }
                RootlistItem::FolderEnd { .. } => {
                    depth = depth.saturating_sub(1);
                    if hidden_at == Some(depth) {
                        hidden_at = None;
                    }
                }
                RootlistItem::Playlist { uri }
                    if hidden_at.is_none() && !pinned_set.contains(uri.as_str()) =>
                {
                    rows.push(Row::Playlist {
                        uri: uri.clone(),
                        depth,
                    });
                }
                RootlistItem::Unknown { uri } if hidden_at.is_none() => rows.push(Row::Unknown {
                    uri: uri.clone(),
                    depth,
                }),
                _ => {}
            }
        }
        rows
    }

    fn plan(&self, intent: Intent) -> Result<Option<Plan>> {
        match intent {
            Intent::CreateFolder { parent, name, id } => self.plan_create(parent, &name, id),
            Intent::RenameFolder { folder, name } => self.plan_rename(folder, &name),
            Intent::Move {
                node,
                parent,
                before,
            } => self.plan_move(node, parent, before),
            Intent::DeleteFolder { folder, contents } => self.plan_delete(folder, contents),
            Intent::PlaceCreatedPlaylist { uri, parent } => {
                self.plan_created_playlist(&uri, parent)
            }
        }
    }

    fn plan_create(
        &self,
        parent: Option<FolderId>,
        name: &str,
        id: FolderId,
    ) -> Result<Option<Plan>> {
        let name = checked_name(name)?;
        if self.has_folder(id) {
            bail!("folder id {id} already exists");
        }
        let raw_id = id.wire();
        let start = format!("{START_PREFIX}{raw_id}:{}", encode_name(name));
        let end = format!("{END_PREFIX}{raw_id}");
        let (at, destination) = self.destination(parent, None)?;
        let mut projected = self.clone();
        projected.items.splice(
            at..at,
            [
                RootlistItem::FolderStart {
                    id,
                    name: name.to_string(),
                    uri: start.clone(),
                },
                RootlistItem::FolderEnd {
                    id,
                    uri: end.clone(),
                },
            ],
        );

        let mut add = Add::new();
        add.items = wire_items([start.as_str(), end.as_str()]);
        set_add_destination(&mut add, destination);
        Ok(Some(Plan::new(
            projected,
            wrap_add(add),
            Postcondition::Folder {
                id,
                name: name.to_string(),
                parent,
            },
        )?))
    }

    fn plan_rename(&self, folder: FolderId, name: &str) -> Result<Option<Plan>> {
        let name = checked_name(name)?;
        let start = self.folder_start(folder).context("folder does not exist")?;
        let RootlistItem::FolderStart {
            name: old_name,
            uri: old_uri,
            ..
        } = &self.items[start]
        else {
            unreachable!();
        };
        if old_name == name {
            return Ok(None);
        }
        let raw_id = old_uri[START_PREFIX.len()..]
            .split_once(':')
            .map(|(id, _)| id)
            .context("folder start marker lost its id")?;
        let new_uri = format!("{START_PREFIX}{raw_id}:{}", encode_name(name));
        let mut projected = self.clone();
        projected.items[start] = RootlistItem::FolderStart {
            id: folder,
            name: name.to_string(),
            uri: new_uri.clone(),
        };

        let mut replacement = UriReplacement::new();
        replacement.item = Some(wire_item(old_uri)).into();
        replacement.set_new_uri(new_uri);
        let mut update = UpdateItemUris::new();
        update.uri_replacements.push(replacement);
        let mut operation = operation(Kind::UPDATE_ITEM_URIS);
        operation.update_item_uris = Some(update).into();
        Ok(Some(Plan::new(
            projected,
            operation,
            Postcondition::Renamed {
                id: folder,
                name: name.to_string(),
            },
        )?))
    }

    fn plan_move(
        &self,
        node: Node,
        parent: Option<FolderId>,
        before: Option<Node>,
    ) -> Result<Option<Plan>> {
        let source = self.resolve_node(&node)?;
        if before.as_ref() == Some(&node) {
            bail!("a node cannot move before itself");
        }
        if let Some(parent) = parent {
            let parent_span = self.folder_span(parent)?;
            if parent_span.start >= source.start && parent_span.start <= source.end {
                bail!("a folder cannot move into itself or a descendant");
            }
        }
        let (_, destination) = self.destination(parent, before.as_ref())?;
        let moving = self.items[source.start..=source.end].to_vec();
        let mut mov = Mov::new();
        mov.items = moving.iter().map(|item| wire_item(item.uri())).collect();
        let mut projected = self.clone();
        projected.items.drain(source.start..=source.end);
        let insert_at = destination.index_in(&projected)?;
        projected.items.splice(insert_at..insert_at, moving);
        if projected == *self {
            return Ok(None);
        }

        set_move_destination(&mut mov, &destination);
        Ok(Some(Plan::new(
            projected,
            wrap_move(mov),
            Postcondition::Position {
                node,
                parent,
                before,
            },
        )?))
    }

    fn plan_delete(&self, folder: FolderId, contents: bool) -> Result<Option<Plan>> {
        let span = self.folder_span(folder)?;
        let removed = if contents {
            self.items[span.start..=span.end].to_vec()
        } else {
            vec![self.items[span.start].clone(), self.items[span.end].clone()]
        };
        let mut projected = self.clone();
        if contents {
            projected.items.drain(span.start..=span.end);
        } else {
            projected.items.remove(span.end);
            projected.items.remove(span.start);
        }

        let mut rem = Rem::new();
        rem.set_items_as_key(true);
        rem.items = removed.iter().map(|item| wire_item(item.uri())).collect();
        Ok(Some(Plan::new(
            projected,
            wrap_remove(rem),
            Postcondition::FolderAbsent(folder),
        )?))
    }

    fn plan_created_playlist(&self, uri: &str, parent: FolderId) -> Result<Option<Plan>> {
        if !uri.starts_with(PLAYLIST_PREFIX) {
            bail!("created playlist has an invalid URI");
        }
        let destination = Destination::Before(self.folder_end_uri(parent)?.to_string());
        let existing = self
            .unique_playlist_index(uri)
            .map_err(|_| anyhow!("created playlist occurs more than once"))?;
        if let Some(index) = existing
            && self.parent_at(index) == Some(parent)
            && self.is_last_child(index, Some(parent))
        {
            return Ok(None);
        }

        let mut projected = self.clone();
        if let Some(index) = existing {
            projected.items.remove(index);
        }
        let at = destination.index_in(&projected)?;
        projected
            .items
            .insert(at, RootlistItem::Playlist { uri: uri.into() });

        let mut mov = Mov::new();
        mov.items = vec![wire_item(uri)];
        set_move_destination(&mut mov, &destination);
        Ok(Some(Plan::new(
            projected,
            wrap_move(mov),
            Postcondition::CreatedPlaylist {
                uri: uri.to_string(),
                parent,
            },
        )?))
    }

    fn destination(
        &self,
        parent: Option<FolderId>,
        before: Option<&Node>,
    ) -> Result<(usize, Destination)> {
        if let Some(before) = before {
            let span = self.resolve_node(before)?;
            if self.parent_at(span.start) != parent {
                bail!("the destination node is not a direct child of the parent");
            }
            let uri = self.items[span.start].uri().to_string();
            return Ok((span.start, Destination::Before(uri)));
        }
        match parent {
            Some(folder) => {
                let end = self.folder_span(folder)?.end;
                let uri = self.items[end].uri().to_string();
                Ok((end, Destination::Before(uri)))
            }
            None => Ok((self.items.len(), Destination::Last)),
        }
    }

    /// Index of the playlist when it occurs exactly once, `Ok(None)` when it
    /// is absent, and an error when it is duplicated.
    fn unique_playlist_index(&self, uri: &str) -> Result<Option<usize>> {
        let mut indices = self.items.iter().enumerate().filter_map(|(index, item)| {
            matches!(item, RootlistItem::Playlist { uri: held } if held == uri).then_some(index)
        });
        let first = indices.next();
        if indices.next().is_some() {
            bail!("playlist occurs more than once");
        }
        Ok(first)
    }

    fn resolve_node(&self, node: &Node) -> Result<Span> {
        match node {
            Node::Folder(id) => self.folder_span(*id),
            Node::Playlist(uri) => {
                let Some(index) = self.unique_playlist_index(uri)? else {
                    bail!("playlist must occur exactly once");
                };
                Ok(Span {
                    start: index,
                    end: index,
                })
            }
        }
    }

    fn folder_span(&self, id: FolderId) -> Result<Span> {
        let start = self.folder_start(id).context("folder does not exist")?;
        let end = self.items[start + 1..]
            .iter()
            .position(
                |item| matches!(item, RootlistItem::FolderEnd { id: held, .. } if *held == id),
            )
            .map(|offset| start + 1 + offset)
            .context("folder has no end marker")?;
        Ok(Span { start, end })
    }

    fn folder_start(&self, id: FolderId) -> Option<usize> {
        self.items.iter().position(
            |item| matches!(item, RootlistItem::FolderStart { id: held, .. } if *held == id),
        )
    }

    fn folder_end_uri(&self, id: FolderId) -> Result<&str> {
        let span = self.folder_span(id)?;
        Ok(self.items[span.end].uri())
    }

    fn parent_at(&self, index: usize) -> Option<FolderId> {
        let mut stack = Vec::new();
        for item in &self.items[..index] {
            match item {
                RootlistItem::FolderStart { id, .. } => stack.push(*id),
                RootlistItem::FolderEnd { .. } => {
                    stack.pop();
                }
                _ => {}
            }
        }
        stack.last().copied()
    }

    fn is_last_child(&self, index: usize, parent: Option<FolderId>) -> bool {
        let end = match parent {
            Some(parent) => match self.folder_span(parent) {
                Ok(span) => span.end,
                // A vanished parent cannot have a last child.
                Err(_) => return false,
            },
            None => self.items.len(),
        };
        let span = match &self.items[index] {
            RootlistItem::FolderStart { id, .. } => self.folder_span(*id).ok(),
            _ => Some(Span {
                start: index,
                end: index,
            }),
        };
        span.is_some_and(|span| span.end + 1 == end)
    }

    fn position_satisfied(
        &self,
        node: &Node,
        parent: Option<FolderId>,
        before: Option<&Node>,
    ) -> bool {
        let Ok(span) = self.resolve_node(node) else {
            return false;
        };
        if self.parent_at(span.start) != parent {
            return false;
        }
        match before {
            Some(before) => self.resolve_node(before).is_ok_and(|next| {
                span.end + 1 == next.start && self.parent_at(next.start) == parent
            }),
            None => self.is_last_child(span.start, parent),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Folder {
    pub id: FolderId,
    pub name: String,
    pub depth: u8,
    pub parent: Option<FolderId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderContents {
    pub playlist_uris: Vec<String>,
    pub has_unknown: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    Folder {
        id: FolderId,
        name: String,
        depth: u8,
        playlist_count: usize,
        collapsed: bool,
    },
    Playlist {
        uri: String,
        depth: u8,
    },
    Unknown {
        uri: String,
        depth: u8,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Node {
    Playlist(String),
    Folder(FolderId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Intent {
    CreateFolder {
        parent: Option<FolderId>,
        name: String,
        id: FolderId,
    },
    RenameFolder {
        folder: FolderId,
        name: String,
    },
    Move {
        node: Node,
        parent: Option<FolderId>,
        before: Option<Node>,
    },
    DeleteFolder {
        folder: FolderId,
        contents: bool,
    },
    PlaceCreatedPlaylist {
        uri: String,
        parent: FolderId,
    },
}

impl Intent {
    pub fn create_folder(parent: Option<FolderId>, name: String) -> Self {
        Self::CreateFolder {
            parent,
            name,
            id: FolderId::random(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Plan {
    projected: Snapshot,
    mutation: Mutation,
}

impl Plan {
    fn new(projected: Snapshot, operation: Op, postcondition: Postcondition) -> Result<Self> {
        let mut delta = Delta::new();
        delta.ops.push(operation);
        let mut changes = ListChanges::new();
        changes.deltas.push(delta);
        changes.set_want_resulting_revisions(true);
        changes.set_want_sync_result(true);
        Ok(Self {
            projected,
            mutation: Mutation {
                body: changes.write_to_bytes()?,
                postcondition,
            },
        })
    }

    #[cfg(test)]
    fn projected(&self) -> &Snapshot {
        &self.projected
    }

    #[cfg(test)]
    fn mutation(&self) -> &Mutation {
        &self.mutation
    }
}

#[derive(Clone, Debug)]
pub struct Mutation {
    body: Vec<u8>,
    postcondition: Postcondition,
}

#[derive(Clone, Debug, Default)]
pub struct FolderState {
    confirmed: Loadable<OwnedSnapshot>,
    cached: Option<Snapshot>,
    work: FolderWork,
    last_error: Option<String>,
}

#[derive(Clone, Debug)]
struct OwnedSnapshot {
    account_id: String,
    snapshot: Snapshot,
}

#[derive(Clone, Debug, Default)]
enum FolderWork {
    #[default]
    Idle,
    Refreshing,
    Mutating(PendingChange),
}

#[derive(Clone, Debug)]
struct PendingChange {
    projected: Snapshot,
    mutation: Mutation,
}

#[derive(Clone, Debug)]
pub enum MutationOutcome {
    Confirmed(Snapshot),
    NotSent(String),
    Rejected(String),
    Unknown {
        last_snapshot: Option<Snapshot>,
        error: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendOutcome {
    Accepted,
    NotSent(String),
    Rejected(String),
    Uncertain(String),
}

impl FolderState {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn begin_refresh(&mut self) -> bool {
        if !matches!(self.work, FolderWork::Idle) {
            return false;
        }
        if self.confirmed.get().is_some() || self.cached.is_some() {
            self.work = FolderWork::Refreshing;
        } else {
            self.confirmed = Loadable::Loading;
        }
        true
    }

    pub fn finish_refresh(&mut self, account_id: String, result: Result<Snapshot, String>) {
        match result {
            Ok(snapshot) => {
                self.confirmed = Loadable::Loaded(OwnedSnapshot {
                    account_id,
                    snapshot,
                });
                self.cached = None;
                self.last_error = None;
            }
            Err(error) => {
                if self.confirmed.get().is_none() {
                    self.confirmed = Loadable::Failed(error.clone());
                }
                // Spotify could not be reached, so stop showing a hierarchy
                // the user cannot interact with.
                self.cached = None;
                self.last_error = Some(error);
            }
        }
        self.work = FolderWork::Idle;
    }

    pub fn plan(&self, intent: Intent) -> Result<Option<Plan>> {
        if !matches!(self.work, FolderWork::Idle) {
            bail!("playlist folders are busy");
        }
        self.confirmed_snapshot()
            .context("playlist folders are not loaded")?
            .plan(intent)
    }

    pub fn begin_mutation(&mut self, plan: Plan) -> Result<Mutation> {
        if !matches!(self.work, FolderWork::Idle) || self.confirmed.get().is_none() {
            bail!("playlist folders are not ready for a change");
        }
        let mutation = plan.mutation.clone();
        self.work = FolderWork::Mutating(PendingChange {
            projected: plan.projected,
            mutation: plan.mutation,
        });
        self.last_error = None;
        Ok(mutation)
    }

    pub fn finish_mutation(&mut self, outcome: MutationOutcome) {
        let account_id = self.confirmed.get().map(|owned| owned.account_id.clone());
        match outcome {
            MutationOutcome::Confirmed(snapshot) => {
                if let Some(account_id) = account_id {
                    self.confirmed = Loadable::Loaded(OwnedSnapshot {
                        account_id,
                        snapshot,
                    });
                }
                self.last_error = None;
            }
            MutationOutcome::NotSent(error) | MutationOutcome::Rejected(error) => {
                self.last_error = Some(error);
            }
            MutationOutcome::Unknown {
                last_snapshot,
                error,
            } => {
                if let (Some(account_id), Some(snapshot)) = (account_id, last_snapshot) {
                    self.confirmed = Loadable::Loaded(OwnedSnapshot {
                        account_id,
                        snapshot,
                    });
                }
                self.last_error = Some(error);
            }
        }
        self.work = FolderWork::Idle;
    }

    pub fn confirmed_snapshot(&self) -> Option<&Snapshot> {
        self.confirmed.get().map(|owned| &owned.snapshot)
    }

    pub fn shown_snapshot(&self) -> Option<&Snapshot> {
        match &self.work {
            FolderWork::Mutating(pending) => Some(&pending.projected),
            _ => self.confirmed_snapshot().or(self.cached.as_ref()),
        }
    }

    /// The cache only bridges the wait for the first live rootlist; once
    /// Spotify has answered (either way) it is no longer trusted.
    pub fn install_cache(&mut self, snapshot: Snapshot) -> bool {
        if matches!(self.confirmed, Loadable::NotLoaded | Loadable::Loading) {
            self.cached = Some(snapshot);
            true
        } else {
            false
        }
    }

    pub fn drop_cache(&mut self) -> bool {
        self.cached.take().is_some()
    }

    fn pending_mutation(&self) -> Option<&Mutation> {
        match &self.work {
            FolderWork::Mutating(pending) => Some(&pending.mutation),
            _ => None,
        }
    }

    pub fn is_placing_created_playlist(&self) -> bool {
        self.pending_mutation()
            .is_some_and(Mutation::places_created_playlist)
    }

    fn account_id(&self) -> Option<&str> {
        self.confirmed.get().map(|owned| owned.account_id.as_str())
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref().or(match &self.confirmed {
            Loadable::Failed(error) => Some(error.as_str()),
            _ => None,
        })
    }

    pub fn is_loading(&self) -> bool {
        self.confirmed.is_loading() || matches!(self.work, FolderWork::Refreshing)
    }

    pub fn needs_initial_load(&self) -> bool {
        matches!(self.confirmed, Loadable::NotLoaded) && matches!(self.work, FolderWork::Idle)
    }

    pub fn writable(&self, account_id: Option<&str>, engine_username: Option<&str>) -> bool {
        self.confirmed.get().is_some()
            && matches!(self.work, FolderWork::Idle)
            && self.account_id() == account_id
            && engine_username == account_id
    }

    pub fn retain_known_expanded(&self, expanded: &mut HashSet<FolderId>) {
        let Some(snapshot) = self.confirmed_snapshot() else {
            return;
        };
        expanded.retain(|id| snapshot.has_folder(*id));
    }

    #[cfg(any(test, feature = "demo"))]
    pub fn set_demo(&mut self, account_id: &str, snapshot: Snapshot) {
        self.finish_refresh(account_id.to_string(), Ok(snapshot));
    }

    #[cfg(any(test, feature = "demo"))]
    pub fn set_demo_pending(&mut self, plan: Plan) {
        let _ = self.begin_mutation(plan);
    }
}

impl Mutation {
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    pub fn is_confirmed_by(&self, snapshot: &Snapshot) -> bool {
        self.postcondition.satisfied_by(snapshot)
    }

    fn places_created_playlist(&self) -> bool {
        matches!(self.postcondition, Postcondition::CreatedPlaylist { .. })
    }
}

#[derive(Clone, Debug)]
enum Postcondition {
    Folder {
        id: FolderId,
        name: String,
        parent: Option<FolderId>,
    },
    Renamed {
        id: FolderId,
        name: String,
    },
    Position {
        node: Node,
        parent: Option<FolderId>,
        before: Option<Node>,
    },
    FolderAbsent(FolderId),
    CreatedPlaylist {
        uri: String,
        parent: FolderId,
    },
}

impl Postcondition {
    fn satisfied_by(&self, snapshot: &Snapshot) -> bool {
        match self {
            Self::Folder { id, name, parent } => snapshot.folder_start(*id).is_some_and(|index| {
                matches!(&snapshot.items[index], RootlistItem::FolderStart { name: held, .. } if held == name)
                    && snapshot.parent_at(index) == *parent
            }),
            Self::Renamed { id, name } => snapshot.folder_start(*id).is_some_and(|index| {
                matches!(&snapshot.items[index], RootlistItem::FolderStart { name: held, .. } if held == name)
            }),
            Self::Position {
                node,
                parent,
                before,
            } => snapshot.position_satisfied(node, *parent, before.as_ref()),
            Self::FolderAbsent(id) => !snapshot.has_folder(*id),
            Self::CreatedPlaylist { uri, parent } => snapshot
                .unique_playlist_index(uri)
                .ok()
                .flatten()
                .is_some_and(|index| snapshot.parent_at(index) == Some(*parent)),
        }
    }
}

#[derive(Clone, Debug)]
enum Destination {
    Before(String),
    Last,
}

impl Destination {
    fn index_in(&self, snapshot: &Snapshot) -> Result<usize> {
        match self {
            Self::Before(uri) => snapshot
                .items
                .iter()
                .position(|item| item.uri() == uri)
                .with_context(|| format!("destination item {uri:?} no longer exists")),
            Self::Last => Ok(snapshot.items.len()),
        }
    }
}

#[derive(Clone, Copy)]
struct Span {
    start: usize,
    end: usize,
}

#[derive(Debug, Default)]
pub struct SnapshotBuilder {
    revision: Option<Vec<u8>>,
    declared: Option<usize>,
    uris: Vec<String>,
    finished: bool,
}

impl SnapshotBuilder {
    pub fn next_offset(&self) -> usize {
        self.uris.len()
    }

    pub fn push(&mut self, page: SelectedListContent) -> Result<bool> {
        if self.finished {
            bail!("rootlist received a page after the final page");
        }
        if page.length() < 0 {
            bail!("Spotify declared a negative rootlist length");
        }
        if page.revision().is_empty() {
            bail!("Spotify returned an empty rootlist revision");
        }
        let declared = page.length() as usize;
        match (&self.revision, self.declared) {
            (None, None) => {
                self.revision = Some(page.revision().to_vec());
                self.declared = Some(declared);
            }
            (Some(revision), Some(expected)) => {
                if page.revision() != revision.as_slice() {
                    bail!("rootlist revision changed while reading");
                }
                if declared != expected {
                    bail!("rootlist length changed while reading");
                }
            }
            _ => unreachable!(),
        }

        let Some(contents) = page.contents.as_ref() else {
            if declared == 0 && self.uris.is_empty() {
                self.finished = true;
                return Ok(false);
            }
            bail!("rootlist page carried no contents");
        };
        if contents.pos() < 0 || contents.pos() as usize != self.uris.len() {
            bail!(
                "rootlist page started at {}, expected {}",
                contents.pos(),
                self.uris.len()
            );
        }
        if contents.truncated() && contents.items.is_empty() {
            bail!("truncated rootlist page made no progress");
        }
        self.uris
            .extend(contents.items.iter().map(|item| item.uri().to_string()));
        self.finished = !contents.truncated();
        Ok(contents.truncated())
    }

    pub fn finish(self) -> Result<Snapshot> {
        if !self.finished {
            bail!("rootlist ended before its final page");
        }
        let declared = self.declared.context("rootlist returned no pages")?;
        if self.uris.len() != declared {
            bail!(
                "assembled {} rootlist items, Spotify declared {declared}",
                self.uris.len()
            );
        }
        Snapshot::parse(&self.uris)
    }
}

fn checked_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty() {
        bail!("folder name cannot be empty");
    }
    Ok(name)
}

fn decode_name(encoded: &str) -> Result<String> {
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                decoded.push(b' ');
                index += 1;
            }
            b'%' => {
                if index + 2 >= bytes.len() {
                    bail!("incomplete percent escape");
                }
                let digits = std::str::from_utf8(&bytes[index + 1..index + 3])?;
                decoded.push(u8::from_str_radix(digits, 16).context("invalid percent escape")?);
                index += 3;
            }
            // Other clients may leave bytes like `!` or `(` unescaped. A name
            // never round-trips through Fastpotify's own encoder unless the
            // user renames the folder, so accept any literal byte; only a
            // malformed percent escape is a structural failure.
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    Ok(String::from_utf8(decoded)?)
}

fn encode_name(name: &str) -> String {
    let mut encoded = String::new();
    for byte in name.as_bytes() {
        match byte {
            b' ' => encoded.push('+'),
            byte if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') => {
                encoded.push(char::from(*byte));
            }
            byte => {
                use std::fmt::Write as _;
                let _ = write!(encoded, "%{byte:02X}");
            }
        }
    }
    encoded
}

fn wire_item(uri: &str) -> WireItem {
    let mut item = WireItem::new();
    item.set_uri(uri.to_string());
    item
}

fn wire_items<'a>(uris: impl IntoIterator<Item = &'a str>) -> Vec<WireItem> {
    uris.into_iter().map(wire_item).collect()
}

fn operation(kind: Kind) -> Op {
    let mut operation = Op::new();
    operation.set_kind(kind);
    operation
}

fn wrap_add(add: Add) -> Op {
    let mut operation = operation(Kind::ADD);
    operation.add = Some(add).into();
    operation
}

fn wrap_move(mov: Mov) -> Op {
    let mut operation = operation(Kind::MOV);
    operation.mov = Some(mov).into();
    operation
}

fn wrap_remove(rem: Rem) -> Op {
    let mut operation = operation(Kind::REM);
    operation.rem = Some(rem).into();
    operation
}

fn set_add_destination(add: &mut Add, destination: Destination) {
    match destination {
        Destination::Before(uri) => add.add_before_item = Some(wire_item(&uri)).into(),
        Destination::Last => add.set_add_last(true),
    }
}

fn set_move_destination(mov: &mut Mov, destination: &Destination) {
    match destination {
        Destination::Before(uri) => mov.add_before_item = Some(wire_item(uri)).into(),
        Destination::Last => mov.set_add_last(true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use librespot_protocol::playlist4_external::{Item, ListItems};

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    fn snapshot(values: &[&str]) -> Snapshot {
        Snapshot::parse(&strings(values)).unwrap()
    }

    fn id(value: &str) -> FolderId {
        FolderId::parse(value).unwrap()
    }

    fn changes(plan: &Plan) -> ListChanges {
        ListChanges::parse_from_bytes(plan.mutation().body()).unwrap()
    }

    fn page(
        revision: &[u8],
        length: i32,
        pos: i32,
        truncated: bool,
        uris: &[&str],
    ) -> SelectedListContent {
        let mut page = SelectedListContent::new();
        page.set_revision(revision.to_vec());
        page.set_length(length);
        let mut contents = ListItems::new();
        contents.set_pos(pos);
        contents.set_truncated(truncated);
        for uri in uris {
            let mut item = Item::new();
            item.set_uri((*uri).to_string());
            contents.items.push(item);
        }
        page.contents = Some(contents).into();
        page
    }

    #[test]
    fn strict_parser_preserves_unknown_items_and_numeric_ids() {
        let parsed = snapshot(&[
            "spotify:start-group:000A:Night+%2B+Day%3A+%E2%98%95",
            "spotify:unknown:item",
            "spotify:end-group:a",
        ]);
        assert_eq!(
            parsed.uris().collect::<Vec<_>>(),
            [
                "spotify:start-group:000A:Night+%2B+Day%3A+%E2%98%95",
                "spotify:unknown:item",
                "spotify:end-group:a",
            ]
        );
        assert_eq!(parsed.folders()[0].id, id("a"));
        assert_eq!(parsed.folders()[0].name, "Night + Day: ☕");
        assert!(parsed.folder_contents(id("A")).unwrap().has_unknown);
    }

    #[test]
    fn malformed_or_ambiguous_markers_fail_instead_of_being_repaired() {
        for uris in [
            strings(&["spotify:start-group:1:open"]),
            strings(&["spotify:end-group:1"]),
            strings(&["spotify:start-group:nope:name", "spotify:end-group:nope"]),
            strings(&["spotify:start-group:1:bad%", "spotify:end-group:1"]),
            strings(&[
                "spotify:start-group:01:first",
                "spotify:end-group:1",
                "spotify:start-group:1:second",
                "spotify:end-group:01",
            ]),
            strings(&[
                "spotify:start-group:1:outer",
                "spotify:start-group:2:inner",
                "spotify:end-group:1",
                "spotify:end-group:2",
            ]),
        ] {
            assert!(Snapshot::parse(&uris).is_err(), "accepted {uris:?}");
        }
    }

    #[test]
    fn pagination_requires_one_consistent_complete_snapshot() {
        let mut builder = SnapshotBuilder::default();
        assert!(
            builder
                .push(page(&[1], 2, 0, true, &["spotify:playlist:a"]))
                .unwrap()
        );
        assert!(
            !builder
                .push(page(&[1], 2, 1, false, &["spotify:playlist:b"]))
                .unwrap()
        );
        assert_eq!(
            builder.finish().unwrap().uris().collect::<Vec<_>>(),
            ["spotify:playlist:a", "spotify:playlist:b",]
        );

        let mut empty = SnapshotBuilder::default();
        let mut empty_page = SelectedListContent::new();
        empty_page.set_revision(vec![1]);
        empty_page.set_length(0);
        assert!(!empty.push(empty_page).unwrap());
        assert!(empty.finish().unwrap().is_empty());

        for second in [
            page(&[2], 2, 1, false, &["spotify:playlist:b"]),
            page(&[1], 3, 1, false, &["spotify:playlist:b"]),
            page(&[1], 2, 0, false, &["spotify:playlist:b"]),
        ] {
            let mut invalid = SnapshotBuilder::default();
            invalid
                .push(page(&[1], 2, 0, true, &["spotify:playlist:a"]))
                .unwrap();
            assert!(invalid.push(second).is_err());
        }
        let mut stalled = SnapshotBuilder::default();
        assert!(stalled.push(page(&[1], 1, 0, true, &[])).is_err());
        let mut no_revision = SnapshotBuilder::default();
        assert!(
            no_revision
                .push(page(&[], 1, 0, false, &["spotify:playlist:a"]))
                .is_err()
        );
        let mut no_contents = SnapshotBuilder::default();
        let mut missing = SelectedListContent::new();
        missing.set_revision(vec![1]);
        missing.set_length(1);
        assert!(no_contents.push(missing).is_err());
    }

    #[test]
    fn pinned_rows_keep_pin_order() {
        let tree = snapshot(&[
            "spotify:playlist:a",
            "spotify:playlist:b",
            "spotify:playlist:c",
        ]);
        let rows = tree.project_rows(
            &HashSet::new(),
            &["spotify:playlist:c".into(), "spotify:playlist:a".into()],
        );
        assert_eq!(
            rows,
            vec![
                Row::Playlist {
                    uri: "spotify:playlist:c".into(),
                    depth: 0,
                },
                Row::Playlist {
                    uri: "spotify:playlist:a".into(),
                    depth: 0,
                },
                Row::Playlist {
                    uri: "spotify:playlist:b".into(),
                    depth: 0,
                },
            ]
        );
    }

    #[test]
    fn rows_keep_spotify_order_counts_collapse_and_pinned_shortcuts() {
        let tree = snapshot(&[
            "spotify:start-group:1:Outer",
            "spotify:playlist:a",
            "spotify:start-group:2:Inner",
            "spotify:playlist:b",
            "spotify:end-group:2",
            "spotify:end-group:1",
            "spotify:playlist:c",
        ]);
        let rows = tree.project_rows(&HashSet::from([id("1")]), &["spotify:playlist:b".into()]);
        assert_eq!(
            rows,
            vec![
                Row::Playlist {
                    uri: "spotify:playlist:b".into(),
                    depth: 0,
                },
                Row::Folder {
                    id: id("1"),
                    name: "Outer".into(),
                    depth: 0,
                    playlist_count: 2,
                    collapsed: false
                },
                Row::Playlist {
                    uri: "spotify:playlist:a".into(),
                    depth: 1,
                },
                Row::Folder {
                    id: id("2"),
                    name: "Inner".into(),
                    depth: 1,
                    playlist_count: 1,
                    collapsed: true
                },
                Row::Playlist {
                    uri: "spotify:playlist:c".into(),
                    depth: 0,
                },
            ]
        );
        assert_eq!(
            tree.playlist_uris().collect::<Vec<_>>(),
            [
                "spotify:playlist:a",
                "spotify:playlist:b",
                "spotify:playlist:c"
            ]
        );
    }

    #[test]
    fn collapsing_a_folder_does_not_change_playlist_membership() {
        let tree = snapshot(&[
            "spotify:start-group:1:Hidden",
            "spotify:playlist:a",
            "spotify:end-group:1",
        ]);
        let rows = tree.project_rows(&HashSet::new(), &[]);
        assert!(
            !rows
                .iter()
                .any(|row| matches!(row, Row::Playlist { uri, .. } if uri == "spotify:playlist:a"))
        );
        assert_eq!(
            tree.playlist_uris().collect::<Vec<_>>(),
            ["spotify:playlist:a"]
        );
    }

    #[test]
    fn every_write_uses_items_and_an_explicit_destination() {
        let tree = snapshot(&[
            "spotify:playlist:a",
            "spotify:start-group:1:One",
            "spotify:playlist:b",
            "spotify:end-group:1",
            "spotify:start-group:2:Two",
            "spotify:end-group:2",
        ]);
        let create = tree
            .plan(Intent::CreateFolder {
                parent: Some(id("1")),
                name: "New + Name".into(),
                id: id("3"),
            })
            .unwrap()
            .unwrap();
        let create_at_root = tree
            .plan(Intent::CreateFolder {
                parent: None,
                name: "Root".into(),
                id: id("4"),
            })
            .unwrap()
            .unwrap();
        let rename = tree
            .plan(Intent::RenameFolder {
                folder: id("1"),
                name: "Renamed".into(),
            })
            .unwrap()
            .unwrap();
        let move_playlist = tree
            .plan(Intent::Move {
                node: Node::Playlist("spotify:playlist:a".into()),
                parent: Some(id("1")),
                before: Some(Node::Playlist("spotify:playlist:b".into())),
            })
            .unwrap()
            .unwrap();
        let move_folder = tree
            .plan(Intent::Move {
                node: Node::Folder(id("2")),
                parent: Some(id("1")),
                before: None,
            })
            .unwrap()
            .unwrap();
        let move_to_root = tree
            .plan(Intent::Move {
                node: Node::Playlist("spotify:playlist:b".into()),
                parent: None,
                before: None,
            })
            .unwrap()
            .unwrap();
        let keep = tree
            .plan(Intent::DeleteFolder {
                folder: id("1"),
                contents: false,
            })
            .unwrap()
            .unwrap();
        let remove = tree
            .plan(Intent::DeleteFolder {
                folder: id("1"),
                contents: true,
            })
            .unwrap()
            .unwrap();
        let placed = tree
            .plan(Intent::PlaceCreatedPlaylist {
                uri: "spotify:playlist:new".into(),
                parent: id("1"),
            })
            .unwrap()
            .unwrap();

        for plan in [
            &create,
            &create_at_root,
            &rename,
            &move_playlist,
            &move_folder,
            &move_to_root,
            &keep,
            &remove,
            &placed,
        ] {
            let changes = changes(plan);
            assert!(changes.base_revision().is_empty());
            assert_eq!(changes.deltas.len(), 1);
            assert_eq!(changes.deltas[0].ops.len(), 1);
            assert!(plan.mutation().is_confirmed_by(plan.projected()));
        }
        let create_changes = changes(&create);
        let add = create_changes.deltas[0].ops[0].add.as_ref().unwrap();
        assert!(!add.has_from_index());
        assert!(add.add_before_item.is_some());
        assert_eq!(add.items.len(), 2);
        assert!(
            changes(&create_at_root).deltas[0].ops[0]
                .add
                .as_ref()
                .unwrap()
                .add_last()
        );
        let rename_changes = changes(&rename);
        let replacement = &rename_changes.deltas[0].ops[0]
            .update_item_uris
            .as_ref()
            .unwrap()
            .uri_replacements[0];
        assert!(!replacement.has_index());
        assert!(replacement.item.is_some());
        let move_changes = changes(&move_folder);
        let mov = move_changes.deltas[0].ops[0].mov.as_ref().unwrap();
        assert!(!mov.has_from_index());
        assert!(!mov.has_to_index());
        assert_eq!(mov.items.len(), 2);
        assert!(
            changes(&move_to_root).deltas[0].ops[0]
                .mov
                .as_ref()
                .unwrap()
                .add_last()
        );
        let keep_changes = changes(&keep);
        let rem = keep_changes.deltas[0].ops[0].rem.as_ref().unwrap();
        assert!(rem.items_as_key());
        assert!(!rem.has_from_index());
        assert_eq!(rem.items.len(), 2);
        let remove_changes = changes(&remove);
        assert_eq!(
            remove_changes.deltas[0].ops[0]
                .rem
                .as_ref()
                .unwrap()
                .items
                .len(),
            3
        );
    }

    #[test]
    fn planner_refuses_ambiguous_or_impossible_moves_and_skips_satisfied_intents() {
        let tree = snapshot(&[
            "spotify:start-group:1:One",
            "spotify:start-group:2:Two",
            "spotify:playlist:a",
            "spotify:end-group:2",
            "spotify:end-group:1",
            "spotify:playlist:b",
        ]);
        assert!(
            tree.plan(Intent::Move {
                node: Node::Folder(id("1")),
                parent: Some(id("2")),
                before: None,
            })
            .is_err()
        );
        assert!(
            tree.plan(Intent::Move {
                node: Node::Playlist("spotify:playlist:b".into()),
                parent: Some(id("1")),
                before: Some(Node::Playlist("spotify:playlist:a".into())),
            })
            .is_err()
        );
        assert!(
            tree.plan(Intent::Move {
                node: Node::Playlist("spotify:playlist:a".into()),
                parent: Some(id("2")),
                before: None,
            })
            .unwrap()
            .is_none()
        );
        assert!(
            tree.plan(Intent::RenameFolder {
                folder: id("1"),
                name: " One ".into()
            })
            .unwrap()
            .is_none()
        );
        assert!(
            tree.plan(Intent::PlaceCreatedPlaylist {
                uri: "spotify:playlist:a".into(),
                parent: id("2")
            })
            .unwrap()
            .is_none()
        );
        assert!(
            tree.plan(Intent::CreateFolder {
                parent: Some(id("9")),
                name: "Missing".into(),
                id: id("3"),
            })
            .is_err()
        );
        let duplicate = snapshot(&["spotify:playlist:a", "spotify:playlist:a"]);
        assert!(
            duplicate
                .plan(Intent::Move {
                    node: Node::Playlist("spotify:playlist:a".into()),
                    parent: None,
                    before: None,
                })
                .is_err()
        );
    }

    #[test]
    fn folder_state_keeps_confirmed_data_during_refresh_and_projection_during_mutation() {
        let original = snapshot(&["spotify:playlist:a"]);
        let mut state = FolderState::default();
        assert!(state.begin_refresh());
        assert!(state.is_loading());
        state.finish_refresh("account".into(), Ok(original.clone()));
        assert_eq!(state.confirmed_snapshot(), Some(&original));
        assert!(state.writable(Some("account"), Some("account")));
        assert!(!state.writable(Some("account"), None));
        assert!(!state.writable(Some("other"), Some("account")));

        assert!(state.begin_refresh());
        assert_eq!(state.shown_snapshot(), Some(&original));
        state.finish_refresh("account".into(), Err("offline".into()));
        assert_eq!(state.confirmed_snapshot(), Some(&original));
        assert_eq!(state.last_error(), Some("offline"));

        let plan = state
            .plan(Intent::CreateFolder {
                parent: None,
                name: "New".into(),
                id: id("1"),
            })
            .unwrap()
            .unwrap();
        let projected = plan.projected().clone();
        let mutation = state.begin_mutation(plan).unwrap();
        assert_eq!(state.shown_snapshot(), Some(&projected));
        assert_eq!(state.confirmed_snapshot(), Some(&original));
        assert!(
            state
                .plan(Intent::RenameFolder {
                    folder: id("1"),
                    name: "No".into()
                })
                .is_err()
        );
        assert!(mutation.is_confirmed_by(&projected));
        state.finish_mutation(MutationOutcome::NotSent("token failed".into()));
        assert_eq!(state.shown_snapshot(), Some(&original));
        assert_eq!(state.last_error(), Some("token failed"));

        let mut failed = FolderState::default();
        assert!(failed.begin_refresh());
        failed.finish_refresh("account".into(), Err("bad tree".into()));
        assert!(failed.confirmed_snapshot().is_none());
        assert_eq!(failed.last_error(), Some("bad tree"));
    }

    #[test]
    fn cached_rootlist_is_read_only_and_yields_to_live_data() {
        let cached = snapshot(&["spotify:start-group:1:Cached", "spotify:end-group:1"]);
        let live = snapshot(&["spotify:start-group:2:Live", "spotify:end-group:2"]);
        let mut state = FolderState::default();

        assert!(state.install_cache(cached.clone()));
        assert_eq!(state.shown_snapshot(), Some(&cached));
        assert!(state.confirmed_snapshot().is_none());
        assert!(!state.writable(Some("account"), Some("account")));
        assert!(state.needs_initial_load());

        assert!(state.begin_refresh());
        assert!(state.is_loading());
        assert_eq!(state.shown_snapshot(), Some(&cached));
        state.finish_refresh("account".into(), Ok(live.clone()));

        assert_eq!(state.shown_snapshot(), Some(&live));
        assert_eq!(state.confirmed_snapshot(), Some(&live));
        assert!(state.writable(Some("account"), Some("account")));
        assert!(!state.install_cache(cached));
        assert_eq!(state.shown_snapshot(), Some(&live));

        // A failed refresh drops the cache and refuses a late-arriving one:
        // an unreachable hierarchy must not linger read-only.
        let mut failed = FolderState::default();
        assert!(failed.install_cache(live.clone()));
        assert!(failed.begin_refresh());
        failed.finish_refresh("account".into(), Err("offline".into()));
        assert!(failed.shown_snapshot().is_none());
        assert!(!failed.install_cache(live));
        assert!(failed.shown_snapshot().is_none());
        assert!(!failed.writable(Some("account"), Some("account")));
    }

    #[test]
    fn folder_state_resets_accounts_and_publishes_spotifys_bounded_answer() {
        let original = snapshot(&["spotify:start-group:1:One", "spotify:end-group:1"]);
        let spotify = snapshot(&["spotify:playlist:other"]);
        let mut state = FolderState::default();
        state.set_demo("first", original);
        let plan = state
            .plan(Intent::RenameFolder {
                folder: id("1"),
                name: "Changed".into(),
            })
            .unwrap()
            .unwrap();
        state.begin_mutation(plan).unwrap();
        state.finish_mutation(MutationOutcome::Unknown {
            last_snapshot: Some(spotify.clone()),
            error: "Spotify did not confirm the change".into(),
        });
        assert_eq!(state.confirmed_snapshot(), Some(&spotify));
        assert_eq!(state.account_id(), Some("first"));
        state.reset();
        assert!(state.confirmed_snapshot().is_none());
        assert!(state.last_error().is_none());
    }
}
