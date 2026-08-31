# Playlist folders

Status: ready for implementation on main's read-only folder support

## Goal

Main already shows Spotify playlist folders in Your Library. Finish the feature so users can
create, rename, move, reorder and delete folders, and move playlists between them. Changes must
appear in other Spotify clients.

Spotify's [public Web API does not expose folders](https://developer.spotify.com/documentation/web-api/concepts/playlists#folders).
The streaming session already owned by Fastpotify can reach Spotify's private rootlist endpoint.
Use that session. Do not add another login, local folder storage, desktop-cache imports, polling,
or name conventions.

This is an unsupported private API. If its format changes, Fastpotify must stop writing and fall
back to the existing flat playlist shelf. It must never guess at a hierarchy or retry a mutation
whose outcome is unknown.

## Starting point on main

Commit `031ef3e` added best-effort, read-only folders. `Engine::rootlist` fetches pages through
librespot, `player::parse_rootlist` turns some URIs into `RootlistEntry` values, the backend carries
them through `Command::Rootlist` and `Event::Rootlist`, and the sidebar builds collapsible rows.
Main also persists collapsed folder ids and has a `folders` demo fixture.

Keep the visible work that is already good: the fixed-height virtual rows, folder visuals,
indentation, recursive counts, collapse behavior and pinned block. Replace the protocol model and
lifecycle in place. Do not add a second rootlist representation beside `player::RootlistEntry`, or
a second request path beside the existing backend command and event.

The current parser is intentionally forgiving. It repairs unclosed folders, ignores unmatched end
markers, accepts malformed ids and names, and drops unknown URIs. The current `Vec<RootlistEntry>`
also uses emptiness for not loaded, failed and a valid empty rootlist. Those choices cannot support
safe writes. The implementation described below replaces them before enabling a write control.

Main already depends on `protobuf`, `rand` and `percent-encoding`. This branch adds `http` only for
the probe. Move `http` to normal dependencies when the production one-shot POST uses it; do not add
another crate for form encoding or protobuf handling.

## Verified protocol

`examples/rootlist_probe.rs` records the facts in this section. It was rerun against a real
account on 2026-08-31. Its write mode creates only disposable empty folders, guards every index,
and restores the original URI vector before it exits.

Fastpotify resolves librespot 0.8 through its `fastpotify-0.8` fork. The commit pinned when this
spec was updated, `a9122af6`, changes only playback normalisation reporting. Its
[`get_rootlist`](https://github.com/crmne/librespot/blob/a9122af6ed4a8a9b8e65f782672934d729cc2970/core/src/spclient.rs)
read helper and
[`playlist4_external`](https://github.com/crmne/librespot/blob/a9122af6ed4a8a9b8e65f782672934d729cc2970/protocol/proto/playlist4_external.proto)
messages match upstream 0.8. It has no rootlist write helper or per-request one-shot retry option.
Keep the one-shot POST in Fastpotify rather than growing the fork solely for this private endpoint.

### Reading

`SpClient::get_rootlist(from, length)` reads
`GET /playlist/v2/user/{username}/rootlist` and returns a protobuf
`playlist4_external::SelectedListContent`.

Ask for 2,000 items first. The test account returned all 112 items in one response. If Spotify
sets `contents.truncated`, continue from the number of items already assembled. Each page must:

- carry the same non-empty opaque revision and declared `length`;
- start at the requested `contents.pos`;
- contain at least one item when `truncated` is true.

When the last page arrives, the assembled count must equal the declared non-negative length.
Reject an inconsistent read and offer Retry. These checks prevent overlap, gaps and endless
pagination.

The returned items are one flat URI vector. Folders are balanced markers around their children,
and may nest:

```text
spotify:start-group:{id}:{encoded_name}
  ...children...
spotify:end-group:{id}
```

Folder ids are hexadecimal. The server accepted 15 and 16 digits in either case, but rejected 17
digits and non-hex characters. Accept one through 16 digits on read. Preserve the marker's exact
wire URI. Generate new ids as 16 lowercase hex digits from a random `u64`, retrying locally if the
id already exists in the snapshot.

Names use form encoding. A raw `+` is a space, and percent escapes hold UTF-8. A literal plus is
`%2B`. Decode strictly enough to reject a malformed marker, and use the original start-marker URI
when a write needs to identify that item.

Only playlist and folder markers appeared in the test account. Preserve every other URI in place
as an unknown item.

### Writing

POST a protobuf `ListChanges` to
`/playlist/v2/user/{username}/rootlist/changes`. It contains the snapshot's `base_revision` and one
`Delta` with the planned operations.

Do not use `SpClient::request_with_protobuf` for this POST. librespot's default request strategy
retries network failures up to ten times. A lost response could therefore create a folder twice.
Issue the POST once through the session's HTTP primitives, with the same authorization,
client-token, product, country and salt fields that `SpClient` supplies. Do not change
`SpClient`'s global retry strategy around the call because other session requests may be running.
An explicit 429 may follow its `Retry-After`; an uncertain network failure must not resend the
body.

The protocol uses indexes, with several sharp edges:

- `REM` and `MOV` use `from_index` and `length`. They ignore their `items`, so omit those items.
- Every index and `base_revision` must come from the same snapshot. Spotify rebases an old issued
  revision, but a fresh index paired with an old revision is rebased a second time and can hit the
  wrong item.
- `UPDATE_ITEM_URIS` may locate an item by index or item. Send both, and make both identify the
  exact original marker. A contradictory pair is rejected.
- Operations in one delta run in order. If one operation shifts the vector, later indexes see the
  shifted vector. Remove a folder's end marker before its start marker.
- `MOV.to_index` is measured before the span is lifted. The span lands before the item at that
  index, and `to_index == len` appends.
- Spotify rejects a change that leaves folder markers unbalanced.

The server rebases a change based on an older revision it issued. It rejected a fabricated
revision with HTTP 509. The POST response contains a new revision but no rootlist contents.

## Model and planner

Replace `player::RootlistEntry`, `player::parse_rootlist`, and the sidebar's structural scans with
one rootlist module that owns marker parsing, folder ids, row projection and change planning. The
player may keep a thin request method, but no other module should parse marker strings or calculate
protocol indexes. Migrate every caller, then delete the permissive parser and old entry type in the
same change.

Keep the flat item vector as the source of truth. It is both the wire format and the coordinate
system for writes. A permanent tree or index cache adds invalidation work without improving the
small linear scans this feature needs. Derive depth, parent, folder span, descendants and recursive
playlist counts in a scan when needed.

Parse external data into typed items at this boundary. Preserve the original URI for every item.
A recognized but malformed marker, mismatched nesting, or an id used by more than one folder makes
the snapshot unsafe to edit and fails the folder read. An unknown URI becomes an unknown item and
stays in its original position. Duplicate playlist URIs may still render, but a planner must reject
a reference that is not unique.

Store a confirmed snapshot with the Web API account id that owns it. Track loading, refresh,
mutation and failure separately so a valid empty rootlist is not confused with one that never
loaded. The UI must be able to answer whether it has a confirmed snapshot, whether that snapshot
belongs to the current account, whether work is in flight and whether writes are allowed without
testing vector emptiness or combining unrelated booleans.

UI intents refer to folder ids and playlist URIs, never indexes. The required intents are:

- create a folder under a folder or at the root;
- rename a folder;
- move a playlist or whole folder under a parent, optionally before one of that parent's direct
  children;
- delete a folder while either keeping or removing its contents.

Validate a new or renamed folder name once when creating the intent. Trim it and reject an empty
result. Folder names need not be unique.

The planner rejects missing or ambiguous nodes, an invalid parent, a `before` node that is not a
direct child of the destination, and a folder moved into itself or a descendant. It should return
no mutation for an intent the snapshot already satisfies.

Planning produces the base revision, protobuf operations and the intent's postcondition as one
value. Keeping them together makes it hard to mix an index with another revision. The operations
are:

| Intent | Operation |
| --- | --- |
| Create folder | one `ADD` carrying both markers at the parent's end |
| Rename folder | one `UPDATE_ITEM_URIS` on the exact start marker |
| Move playlist | one `MOV` with length 1 |
| Move folder | one `MOV` over the complete marker span |
| Delete folder, keep contents | two `REM`s, end marker first |
| Delete folder and contents | one `REM` over the complete marker span |

Do not offer "delete folder and contents" when the span contains an unknown item. Fastpotify does
not know what removing that URI would do. Keeping the contents remains safe.

The rootlist module projects folders, playlists and unknown items in one scan. It should know only
row identity, node ids, depth, recursive counts and collapse state. The sidebar's final row type
uses explicit variants for Liked Songs, folders, playlists and unknown items. Do not identify
folders through an empty URI and `Page::Home`. The sidebar joins playlist names, images and
ownership from `Library.playlists`. Missing metadata does not make the protocol item invalid.

## Transport and synchronization

Folder access requires a live streaming engine whose `Session::username()` matches the current
Web API account id. Check the expected account before each rootlist request and tag every event
with it. The app discards events for an account that is no longer signed in.

Extend the existing `Command::Rootlist` and `Event::Rootlist` path into the serialized rootlist
lane. The backend worker owns whether a read or write is in flight and whether one later refresh is
pending. Do not leave each command as an independent `tokio::spawn`, add another top-level sync
service, or mutate this state from the UI. Coalesce refresh requests that arrive while the lane is
busy, and disable write controls until the current operation finishes.

A normal change uses the last confirmed snapshot:

1. Plan the change from that snapshot.
2. Send its POST body once.
3. Read and publish one complete rootlist.
4. Check the published rootlist against the planned postcondition.

There is no safety benefit in a GET before step 1. Spotify rebases issued revisions, and the
planner already keeps the indexes with their exact revision. Removing that GET makes the common
change one POST and one GET.

Always do the readback after a POST attempt, including a timeout or transport error. If the final
tree satisfies the postcondition, report success. If it does not, publish the tree and report the
write error. If the readback also fails, keep the last confirmed tree and say that the outcome is
unknown. Never retry the mutation automatically. Put a finite timeout around the operation so the
serialized lane always clears.

Do not update the rootlist optimistically. The drag preview may animate, but the confirmed tree
stays in place until Spotify answers.

Load or refresh the rootlist:

- when a matching streaming engine becomes ready;
- after a user asks to retry;
- once when the window returns to focus;
- after a Web API action changes playlist membership.

The readback is already the refresh after a private rootlist mutation. Do not enqueue another one.
Do not poll. A focus refresh requested while busy becomes one later refresh, not a queue of GETs.

Remove the current request from the end of generic `MyPlaylists` pagination. Playlist metadata
reloads after a rename or item edit do not change rootlist membership and should not cause a GET.
The successful create, follow and unfollow response handlers request the membership refresh they
need. Rootlist loading and metadata pagination may then run independently.

When the engine disconnects, keep a previously confirmed tree on screen but make it read-only. If
no confirmed tree exists, or the first read fails, show the existing flat playlist shelf with a
clear "Folders unavailable" error and Retry. Keep `Settings.sidebar_order` intact for that fallback.

An engine disconnect is different from an account change. When the Web API account changes or
signs out, clear the in-memory snapshot before new metadata can render against it. Persist the
account id beside `SessionState.collapsed_folders`; if the saved account is absent or does not
match, start with every folder expanded. Keep the session file backward compatible.

## Playlist metadata and cross-API flows

The rootlist owns membership, hierarchy and order in folder mode. `Library.playlists` remains the
metadata source. Index it by URI while building sidebar rows. An unknown item or playlist with no
metadata gets a disabled row rather than disappearing or failing the whole shelf.

Do not blank the metadata list during a background refresh. Keep the playlist returned by the
create endpoint in the local metadata list while the full list reloads. Pure folder moves and
renames do not need a Web API metadata refresh.

Creating a playlist in a destination folder crosses two APIs. Create it through the Web API first,
then read the rootlist because the previous snapshot cannot contain the new URI. Plan the move from
that read, POST it once, and read back. If placement fails, keep the playlist and say that Spotify
created it at the root. Never create it again or roll it back.

Create-at-root, follow and unfollow need one rootlist refresh after the Web API succeeds. The
destination flow and private deletion already end with a rootlist readback. Reconcile saved and
metadata state from those confirmed results without replacing a loaded shelf with a spinner. A
later focus refresh handles Spotify propagation delays. Do not poll for convergence.

## Sidebar behavior

Folders affect only the Playlists shelf. Albums, Artists and Podcasts remain flat. Liked Songs is
the first row and cannot move.

When a confirmed rootlist is available, Spotify's order replaces `sidebar_order` and recent-play
sorting without erasing either setting. This applies to a valid rootlist with no folders too. It
also deliberately replaces main's current behavior where a non-empty `sidebar_order` hides every
folder. The saved order returns only when folder mode falls back to the flat shelf.

Pinned playlists remain in Fastpotify's pinned block and appear only once. Project a pinned
shortcut at depth zero without changing the playlist's rootlist parent or its folder's recursive
count. Dragging a pinned shortcut may reorder or remove a pin, as it does today, but it never writes
the rootlist.

Build one fixed-height visible row vector and use the existing virtual row renderer. Do not recurse
inside egui. Folder rows show a chevron, folder icon, name and recursive playlist count where the
row mode has room. Use the existing bidirectional text helper and cap indentation to the sidebar
width. Clicking a folder emits an action that toggles its saved collapse state after drawing. Make
`App::session_dirty` private again; the sidebar should not mutate it or the collapse vector while
drawing.

Keep main's flat playlist results while filtering. Do not synthesize ancestor folder rows or alter
saved collapse state. A playlist subtitle may show its folder path when names collide. Disable
rootlist drag and gap reordering while filtering because hidden siblings make the destination
ambiguous. Pin dragging may keep its local behavior, and the playlist menu's destination picker
remains available.

Reuse the existing drag visuals and fixed-row arithmetic. In confirmed folder mode, an unpinned
playlist drag emits a rootlist move instead of writing `Settings.sidebar_order`; a folder may be
dragged by its id. A drop on a folder appends to it. A drop in an unfiltered gap goes before the
following visible sibling. A root-edge drop moves to the root. In flat fallback mode, the existing
local playlist ordering remains unchanged. Reject self and descendant drops before emitting an
action. Unknown items and missing-metadata rows are not draggable and cannot anchor a gap target.

Use the existing action, dialog, menu and toast patterns:

- Library `+` offers playlist and folder creation when folders are writable.
- A folder menu offers create playlist here, create folder here, rename, move and delete.
- A playlist menu adds "Move to folder".
- The move dialog lists the root and valid folders in tree order, with the current parent selected.
- Folder deletion offers "keep playlists" and a separate destructive "delete playlists" choice.

Do not add Move Up or Move Down in the first release. Dragging and the destination picker cover the
current sidebar interaction model.

## Verification

Keep tests focused on invariants rather than mirroring every implementation detail:

- parser tests cover pagination consistency, strict marker parsing, nesting, names, unknown items
  and the 15-digit id already seen in the account; include cases the old parser repaired or dropped
  and prove that they now fail the read;
- planner tests cover each operation, moves in both directions, invalid destinations, ambiguous
  references, marker-preserving deletes and no-op intents;
- coordinator tests prove single-send POST behavior, account isolation, serialized operations,
  coalesced refreshes, account changes and both outcomes of an ambiguous POST;
- projection tests cover a valid flat rootlist in Spotify's order, a valid empty rootlist, pinned
  shortcuts at depth zero, missing metadata and the local-order fallback;
- expand main's `folders` demo fixture to include a small nested rootlist, fallback and read-only
  states, flat filtering, collapse, representative drag targets and both row modes.

Run `cargo run --example rootlist_probe` read-only before implementation. Use `--write` only with
the disposable account procedure documented by the probe. For live acceptance, create a folder,
nested folder and disposable playlist, exercise each mutation from Fastpotify, confirm it in an
official client, make one concurrent official-client edit, then remove every disposable item and
verify the original URI vector is unchanged.

Run the full checks in `CONTRIBUTING.md` before committing. Update the connection guide for the
private rootlist GET and POST. Update the user guide and README sections that currently describe
playlist dragging as local sidebar order, and document the flat fallback and read-only states.

Out of scope: local-only folders, folder playback, bulk moves, offline queues, optimistic writes,
push subscriptions and alternate Spotify data sources.
