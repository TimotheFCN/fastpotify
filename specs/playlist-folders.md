# Playlist folders

Status: ready for implementation against main at `ebdc625`. Every protocol claim below was
verified against a real account on 2026-08-31 by `examples/rootlist_probe.rs`.

## Goal

Main already shows Spotify playlist folders in Your Library. Finish the feature so users can
create, rename, move, reorder and delete folders, and move playlists between them. Changes must
appear in other Spotify clients.

Spotify's [public Web API does not expose folders](https://developer.spotify.com/documentation/web-api/concepts/playlists#folders).
The streaming session Fastpotify already owns can reach Spotify's private rootlist endpoint. Use
that session. Do not add another login, local folder storage, desktop-cache imports, polling, or
name conventions.

This is an unsupported private API. If its format changes, Fastpotify must stop writing and fall
back to the existing flat playlist shelf. It must never guess at a hierarchy.

## Starting point on main

Commit `031ef3e` added best-effort, read-only folders: `Engine::rootlist`, `parse_rootlist`,
`Command::Rootlist` and `Event::Rootlist`, collapsible sidebar rows, persisted collapsed ids and a
`folders` demo fixture. Keep the visible work, which is good: fixed-height virtual rows, folder
visuals, indentation, recursive counts, collapse behavior and the pinned block. Replace the
protocol model and lifecycle in place, without a second rootlist representation or a second request
path beside the existing command and event.

The commits through `ebdc625` leave that rootlist path unchanged. They do add two conventions this
work must follow. The interface now shows an action before Spotify confirms it and refuses to let a
stale answer undo it. Saving the queue also creates a playlist through the shared `CreatePlaylist`
and `PlaylistCreated` path, so rootlist reconciliation must cover that entry point.

Two of main's choices cannot support writes and go first. The parser is deliberately forgiving: it
repairs unclosed folders, ignores unmatched end markers, accepts malformed ids and names, and drops
unknown URIs. And `Vec<RootlistEntry>` uses emptiness for not loaded, failed and a valid empty
rootlist alike.

No new dependency is needed: librespot for the GET, Fastpotify's `reqwest` client for the POST,
`protobuf` for the messages. (`protobuf-json-mapping` is a dev dependency for the probe alone.)

## Verified protocol

`examples/rootlist_probe.rs` records these facts and can reproduce them. Its `--write` mode creates
only disposable folders and playlists, keeps a sacrificial span at the head of the list so that an
operation which ignored its items could only destroy probe data, and restores the account before it
exits.

### Reading

`SpClient::get_rootlist(from, length)` reads `GET /playlist/v2/user/{username}/rootlist` and returns
a protobuf `playlist4_external::SelectedListContent`.

Ask for 2,000 items. The test account returned all 114 in one response; asking for 100 set
`contents.truncated`. When a page is truncated, continue from the number of items already
assembled. Reject an inconsistent read and offer Retry, checking that every page carries the same
non-empty revision and declared `length`, that each starts at the requested `contents.pos`, that a
truncated page carried at least one item, and that the assembled count matches the declared length
at the end. These are cheap and they are the only defence against a silently mangled hierarchy.

An account with no playlists has nothing to page through, so a response with declared length zero
and no `contents` is a valid empty rootlist, not a failure.

The response carries no useful `owner_username` and no `capabilities`, even though librespot asks
for both in its decorate list. There is no server-side permission signal to gate writes on.

The items are one flat URI vector. Folders are balanced markers around their children, and may
nest:

```text
spotify:start-group:{id}:{encoded_name}
  ...children...
spotify:end-group:{id}
```

Only playlist and folder markers appeared in the test account. Preserve every other URI in place as
an unknown item.

### Folder ids and names

The server parses a folder id as a hexadecimal `u64`: one to sixteen digits are accepted in either
case, seventeen digits and any non-hex character are rejected with 400. It stores the exact string
it was given, including a leading zero and upper case, so a marker URI round-trips unchanged.
Because another client may have written the same id with different padding or case, compare ids
numerically. Generate a new one as sixteen lowercase hex digits from a random `u64`.

Names use form encoding inside the marker URI. A raw `+` is a space, percent escapes hold UTF-8,
and a literal plus is `%2B`. A colon inside a name is fine because it is escaped. A thousand
character name was accepted and stored intact, so do not invent a local length limit. Trim a new
name and reject an empty result. Names need not be unique.

### Writing

POST a protobuf `ListChanges` to `/playlist/v2/user/{username}/rootlist/changes`, containing one
`Delta` with the planned operations.

**Every operation names its items and its destination by URI. Fastpotify sends no indexes and no
`base_revision`.** That is not a simplification of the protocol, it is what the protocol prefers:
identity-addressed operations were accepted without a revision, while an index-addressed operation
with no revision was rejected with 400. Where an index and an item disagreed, the item won, both
for `UPDATE_ITEM_URIS` and for a `REM` with `items_as_key` set.

| Intent | Operation |
| --- | --- |
| Create a folder at the end of the root | `ADD` with both markers, `add_last` |
| Create a folder inside a folder | `ADD` with both markers, `add_before_item` its end marker |
| Rename a folder | `UPDATE_ITEM_URIS` with `item` set to the exact start marker and the new marker as `new_uri` |
| Move a playlist or folder into a folder | `MOV` with the node's items, `add_before_item` the destination's end marker |
| Move a node before a sibling | `MOV` with the node's items, `add_before_item` the sibling's first item |
| Move a node to the root | `MOV` with the node's items, `add_last` or `add_first` |
| Delete a folder, keep its contents | `REM` with `items_as_key`, carrying only the two markers |
| Delete a folder and its contents | `REM` with `items_as_key`, carrying the whole span |

The rules behind that table, all observed:

- A `MOV` or `ADD` needs exactly one destination: `add_before_item`, `add_after_item`, `add_first`
  or `add_last`. With no destination at all the request is rejected with 400, not defaulted to the
  front of the list.
- A `REM` must set `items_as_key`. Without it, and without an index, it is rejected with 400.
- A folder moves as its whole marker span, contents included, in one operation.
- Removing both markers of a folder that has contents works as one operation, and the children stay
  where they were, one level further out. There is no ordering rule to respect.
- Removing an item that is already gone is accepted and changes nothing. Anchoring to an item that
  is not in the list is rejected with 400. A stale snapshot therefore produces a loud failure or a
  no-op, never a change to the wrong folder.
- Spotify rejects a change that would leave folder markers unbalanced.
- Removing a playlist from the rootlist removes it from the account's library. "Delete folder and
  contents" really does delete those playlists, so say so in the dialog.

The reply is a `SelectedListContent` carrying `resulting_revisions` and an empty `sync_result`. It
never carries the new contents, so a readback is still the only way to learn the resulting tree.

Do not send the POST through `SpClient::request_with_protobuf`. It retries a failed request up to
ten times, including on 500 and 503, so a lost or failed reply after Spotify applied an `ADD` could
create a folder twice. Send it with Fastpotify's `reqwest` client instead, which retries nothing:
take the access point from `SpClient::base_url`, the bearer token from `Session::login5`, the
client token from `SpClient::client_token`, and keep the `product`, `country` and `salt` query
parameters librespot adds. This also makes the status code and the response body visible, which
librespot's HTTP client hides. Send each mutation exactly once. (Three trials showed Spotify
folding a repeated identical body into one change, but nothing documents that, and sending once
costs nothing here.)

## Model and planner

Replace `player::RootlistEntry`, `player::parse_rootlist`, and the sidebar's structural scans with
one rootlist module that owns marker parsing, folder ids, row projection and change planning. The
player may keep a thin request method, but no other module should parse marker strings. Migrate
every caller, then delete the permissive parser and the old entry type in the same change.

Keep the flat item vector as the source of truth. It is the wire format, and every derived fact
(depth, parent, folder span, descendants, recursive playlist counts) is a small linear scan. A
permanent tree or index cache would add invalidation work and buy nothing.

Parse external data into typed items at this boundary, keeping each item's original URI. A folder
id is a `FolderId(u64)` parsed from the marker, kept alongside the exact wire URI that a write must
echo back. A recognized but malformed marker, mismatched nesting, or one id used by two folders
makes the snapshot unsafe and fails the folder read: collapse state, row identity and menu actions
are all keyed by folder id long before the planner sees an intent, so an ambiguous id cannot be
carried any further. An unknown URI becomes an unknown item and stays in place.

Keep folder state together, following the repository's `Loadable<T>` plus adjacent work-state
pattern. Store the Web API account id with the confirmed snapshot. Something like:

```rust
struct FolderState {
    confirmed: Loadable<OwnedSnapshot>,
    work: FolderWork,
    last_error: Option<String>,
}

struct OwnedSnapshot {
    account_id: String,
    snapshot: Snapshot,
}

enum FolderWork {
    Idle,
    Refreshing,
    Mutating(PendingChange),
    Unknown(PendingChange),
}

struct PendingChange {
    projected: Snapshot,
    postcondition: Postcondition,
}
```

Keep these fields private and change them through `FolderState` methods. `FolderWork` may differ
from `Idle` only while `confirmed` is loaded. A first-load failure belongs in `Loadable::Failed`; a
background failure leaves the loaded snapshot in place and sets `last_error`. `shown_snapshot()`
returns a pending projection when one exists and the confirmed snapshot otherwise. Planning always
uses `confirmed_snapshot()`.

Writability is derived from a loaded snapshot, a matching account, a live engine and
`FolderWork::Idle`. Do not store another writable or available boolean. A disconnected engine
leaves a loaded snapshot visible but read-only. An account change resets the whole state before the
new account's metadata can render.

A normal successful load or refresh replaces `confirmed`, returns `work` to `Idle` and clears
`last_error`. A failed first load sets `confirmed` to `Loadable::Failed`. A failed background
refresh keeps `confirmed`, returns `work` to `Idle` and records `last_error`. An account reset sets
`confirmed` to `Loadable::NotLoaded`, sets `work` to `Idle` and clears the error.

UI intents refer to folder ids and playlist URIs, never indexes. The required intents are: create a
folder under a folder or at the root, rename a folder, move a playlist or whole folder under a
parent and optionally before one of that parent's direct children, and delete a folder while either
keeping or removing its contents. A separate `PlaceCreatedPlaylist` intent carries the URI returned
by a successful `PlaylistCreated` response and its destination folder.

Planning resolves each reference against the confirmed snapshot and produces the operations, a
projected snapshot and the intent's postcondition together. The projection comes from the same
plan as the wire operations, so display and transport cannot interpret an intent differently. A
reference that does not resolve to exactly one item is refused, as is an invalid parent, a `before`
node that is not a direct child of the destination, and a folder moved into itself or a descendant.
An intent the confirmed snapshot already satisfies produces no mutation.

`PlaceCreatedPlaylist` is the only source-resolution exception. Its URI comes from the successful
Web API response for the same account, and Spotify has already added that playlist at the root. If
the confirmed snapshot does not contain it yet, plan a `MOV` naming that URI and project it directly
under the destination. If the snapshot contains it once, plan an ordinary move. Refuse a duplicate
or malformed occurrence. The destination must still resolve against the confirmed snapshot, and
the postcondition requires exactly one occurrence under that destination.

Do not offer "delete folder and contents" when the span holds an unknown item or a playlist the
user cannot see, because that operation removes playlists from the library and Fastpotify would be
deleting something it never showed. Keeping the contents stays available.

The rootlist module projects folders, playlists and unknown items in one scan, knowing only row
identity, node ids, depth, recursive counts and collapse state. The sidebar's row type uses
explicit variants for Liked Songs, folders and playlists. Do not identify a folder through an empty
URI and `Page::Home`. The sidebar joins names, images and ownership from `Library.playlists`.

## Transport and synchronization

Folder access requires a live streaming engine whose `Session::username()` matches the current Web
API account id. They were the same string on the test account, and there is no other signal to
check, so treat a mismatch as "folders unavailable" rather than guessing. Tag every rootlist event
with the account it was fetched for and discard events for an account that is no longer signed in,
the way `Event::PlaylistCache` already does.

Extend the existing `Command::Rootlist` and `Event::Rootlist` path. The backend worker owns whether
a read or write is in flight and whether one later refresh is pending. Do not leave each command as
an independent `tokio::spawn`, add another top-level sync service, or mutate this state from the
UI. Coalesce refresh requests that arrive while the lane is busy, and disable write controls until
the current operation finishes. Because operations are addressed by item, a mutation planned from a
slightly stale snapshot fails loudly or does nothing rather than hitting the wrong folder, so this
serialization is for a calm UI, not for safety.

Keep the backend lane separate from `FolderState`. The lane is authoritative for network
scheduling and coalescing. `FolderState.work` records what the interface should show after it emits
a command and while it handles the resulting events; it never decides whether the backend may
start another request.

For each change, plan it from the confirmed snapshot and install the projected snapshot in
`FolderWork::Mutating`. Send one POST, then read complete rootlists until the postcondition appears
or a fixed stale-read budget expires. There is no GET before the POST. Never plan another mutation
from the projected snapshot, and keep write controls disabled until the work returns to `Idle`.

Classify the send by what actually happened rather than by status family, because HTTP 408 and 5xx
are as uncertain as a dropped connection while a token or request-building failure means nothing
was sent at all:

- nothing sent: clear the projection, return to `Idle`, show the confirmed snapshot, record the
  error and report the failure.
- rejected: 400 for a change Spotify will not apply, such as an anchor item that is no longer
  there, and 509 for a revision it never issued. A 429 is also a refusal, applied to nothing.
  Clear the projection, return to `Idle`, show the confirmed snapshot, record the error and report
  the failure.
- accepted, or uncertain: read back, then let the postcondition decide.

Always read back after a POST that may have reached Spotify, including after a timeout. If a tree
satisfies the postcondition, publish it as confirmed, clear the projection, return to `Idle`, clear
the error and report success. If a tree does not satisfy it, treat that answer as stale at first:
keep the projection on screen and ask again after a short delay. Use a fixed retry count within the
operation timeout, following the queue's existing stale-answer rule rather than introducing a
shared synchronization abstraction.

If Spotify keeps returning a tree that contradicts the postcondition, publish the last tree, clear
the projection, return to `Idle` and record the failed change. If no readback succeeds before the
timeout, keep the projection visible in `FolderWork::Unknown`, make it read-only and record that the
outcome is unknown. A later successful refresh continues the same bounded postcondition check. It
either confirms the change or lets Spotify's tree win with an error. Never retry a mutation
automatically.

Put a finite timeout around token acquisition, the POST and its immediate readbacks so the backend
lane always clears. An unknown projection may remain visible after the lane clears, but it does not
make the UI writable and it does not block a later reconciliation read.

Load or refresh the rootlist when a matching engine becomes ready, when the user retries, once when
the window regains focus, and after a Web API action changes playlist membership. The readback is
already the refresh after a private mutation, so do not enqueue another one, and do not poll. A
focus refresh requested while busy becomes one later refresh, not a queue of GETs.

Remove the current request from the end of generic `MyPlaylists` pagination. Playlist metadata
reloads after a rename or item edit do not change rootlist membership. Rootlist loading and
metadata pagination then run independently.

When the engine disconnects, keep the shown tree on screen but make it read-only. This may be the
confirmed tree or an unknown projection. If no confirmed tree exists, or the first read fails, show
the existing flat playlist shelf with a clear "Folders unavailable" error and Retry, leaving
`Settings.sidebar_order` intact for that fallback.

An engine disconnect is different from an account change. When the Web API account changes or signs
out, clear the confirmed snapshot, any pending projection and the in-memory collapse set before new
metadata can render against them; `reset_data` is the place. The persisted collapse list needs no
account tag, because folder ids are random 64-bit values and an id from another account simply
matches nothing.

## Playlist metadata and cross-API flows

The rootlist owns membership, hierarchy and order in folder mode. `Library.playlists` remains the
metadata source; index it by URI while building rows. Because the two now load independently, enter
folder mode only once both a confirmed snapshot and usable playlist metadata are present, or the
sidebar briefly shows folders full of nameless rows. Once folder mode is active, render
`shown_snapshot()` so a pending change appears immediately.

Where the two disagree:

- a playlist with metadata that the rootlist does not list yet appears temporarily at the root, and
  cannot be dragged until Spotify lists it;
- a rootlist item with no metadata, and any unknown item, stays in the protocol vector but does not
  render, because Fastpotify has no name or action for it;
- the next focus or membership refresh reconciles both.

Spotify was prompt about this in testing: a Web API create put the new playlist at rootlist index 0
and a Web API unfollow removed its rootlist entry, both visible on the very next read. So these
rules are a safety net, not the normal path.

Do not blank the metadata list during a background refresh. Keep the current `PlaylistCreated`
behavior that inserts the returned playlist into the local metadata list immediately. Pure folder
moves and renames need no Web API refresh.

Creating a playlist in a destination folder crosses two APIs: create it through the Web API, then
move it with one POST that names the new URI and the destination's end marker, then read back. No
GET is needed in between, because `PlaceCreatedPlaylist` names the URI returned by the create call
rather than an index. If placement fails, keep the playlist and say Spotify created it at the root.
Never create it again.

Extend the shared playlist-creation context with an optional destination `FolderId`, separate from
the existing `add_uris`. Every successful `PlaylistCreated` response must lead to exactly one
rootlist reconciliation:

- a root destination requests one ordinary rootlist refresh when a matching engine is live;
- without a matching engine, the next engine-ready load performs that reconciliation;
- a folder destination starts the placement mutation, whose readback is the reconciliation;
- Save Queue as a playlist and New playlist from a track use the root destination and preserve
  their existing `add_uris` behavior.

Do not make each creation entry point manage this separately. The shared `PlaylistCreated` handler
has the returned playlist URI and the creation context, so it owns the choice. Preserve Save Queue's
track insertion and page-opening behavior while adding the refresh.

After a successful follow or unfollow, refresh at once when a matching engine is live. Otherwise,
reconcile on the next engine-ready load. Reconcile saved and metadata state from confirmed results
without replacing a loaded shelf with a spinner.

## Sidebar behavior

Folders affect only the Playlists shelf. Albums, Artists and Podcasts stay flat. Liked Songs is the
first row and cannot move.

When a confirmed rootlist is available, its confirmed or pending projected order replaces
`sidebar_order` and recent-play sorting without erasing either setting, including for a valid
rootlist with no folders. This deliberately replaces main's behavior, where a non-empty
`sidebar_order` hides every folder. The saved order returns only in the flat fallback. Say so in the
user guide, because dragging a playlist changes meaning for anyone who has reordered their sidebar
before.

Pinned playlists stay in Fastpotify's pinned block and appear once. Project a pinned shortcut at
depth zero without changing the playlist's rootlist parent or its folder's recursive count.
Dragging a pinned shortcut reorders or removes a pin as it does today, and never writes the
rootlist.

Build one fixed-height visible row vector and use the existing virtual row renderer, rather than
recursing inside egui. Folder rows show a chevron, folder icon, name and recursive playlist count
where the row mode has room, through the existing bidirectional text helper, with indentation
capped to the sidebar width. Clicking a folder emits an action that toggles its saved collapse
state after drawing. Make `App::session_dirty` private again; the sidebar should not mutate it or
the collapse vector while drawing.

Filtering keeps main's flat results, with no synthesized ancestor rows and no change to saved
collapse state. A playlist subtitle may show its folder path when names collide. Rootlist drag and
gap reordering are disabled while filtering, because hidden siblings make the destination
ambiguous; pin dragging and the destination picker still work.

Reuse the existing drag visuals and fixed-row arithmetic. In confirmed folder mode an unpinned
playlist drag emits a rootlist move instead of writing `Settings.sidebar_order`, and a folder may
be dragged by its id. A drop on a folder appends to it, a drop in an unfiltered gap goes before the
following visible sibling, and a root-edge drop moves to the root. Each of those maps to one
anchored `MOV`. Install the plan's projection as soon as the action is applied, after drawing. In
flat fallback mode the existing local ordering is unchanged. Reject self and descendant drops
before emitting an action.

Use the existing action, dialog, menu and toast patterns:

- Library `+` offers playlist and folder creation when folders are writable.
- A folder menu offers create playlist here, create folder here, rename, move and delete.
- A playlist menu adds "Move to folder".
- The move dialog lists the root and valid folders in tree order, with the current parent selected.
- Folder deletion offers "keep playlists" and a separate destructive choice that names how many
  playlists it will remove from the library.

Leave Move Up and Move Down out of the first release; dragging and the destination picker cover the
current interaction model.

## Verification

Keep tests focused on invariants rather than mirroring every implementation detail:

- parsing: pagination consistency, the empty rootlist, strict markers, nesting, names, unknown
  items, numeric id identity across padding and case, and the 15-digit id this account already has.
  Include what the old parser repaired or dropped, and prove it now fails;
- planning: every row of the operation table, unresolvable and ambiguous references, invalid
  destinations, no-op intents, that projection and wire operations describe the same result, and
  that no operation carries an index or a revision. Cover `PlaceCreatedPlaylist` when its URI is
  absent, already present once and duplicated;
- folder state: first load, retained data after a background failure, derived writability, immediate
  pending projection, confirmed-only planning, account reset and the read-only unknown state;
- the coordinator: single-send behavior, send classification, account isolation, serialization,
  coalesced refreshes, a stale readback that does not undo the projection, eventual confirmation,
  the bounded case where Spotify's tree wins, and an uncertain POST whose readbacks all fail;
- projection: Spotify's order, an empty rootlist, pinned shortcuts at depth zero, hidden
  metadata-less rows, the metadata gate, pending order and the local-order fallback;
- playlist creation: root creation requests one refresh, folder creation uses its placement
  readback instead, and Save Queue keeps its tracks and opens the new playlist while requesting no
  duplicate refresh;
- the `folders` demo fixture: a small nested rootlist, the fallback and read-only states, flat
  filtering, collapse, representative drag targets, a pending mutation and both row modes.

`cargo run --example rootlist_probe` re-reads the live hierarchy; `--write` reruns the protocol
experiments and `--only <section>` reruns one. Both write modes need the Fastpotify app open, so
that the cached Web API token is fresh: the probe must not refresh it itself, because that would
rotate the token the running app holds. For live acceptance, create a folder, a nested folder and a
disposable playlist, exercise each mutation from Fastpotify, confirm it in an official client, make
one concurrent official-client edit, then remove every disposable item and check the URI vector is
unchanged.

Run the full checks in `CONTRIBUTING.md` before committing. Update the connection guide for the
private rootlist GET and POST. Update the user guide and README where they describe playlist
dragging as local sidebar order, and document the flat fallback and read-only states.

Out of scope: local-only folders, folder playback, bulk moves, offline queues, using a projected
snapshot as the base for another write, automatic mutation retries, push subscriptions and
alternate Spotify data sources.
