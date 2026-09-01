---
title: Everyday Use
description: Library ordering, playlist folders, and local play history.
nav_order: 3
---

## Library order

By default, the sidebar sorts playlists by when you last played them. Drag a
playlist to switch to a custom order. New playlists appear below the pinned
group. Choose **Sort by recently played** from a playlist's context menu to
restore the default order.

## Playlist folders

The Playlists shelf follows Spotify's own folder hierarchy and order. The
**+** button makes a playlist or a folder, a row's menu moves it to another
folder, and dragging drops it before a sibling, into a folder, or back at the
root. Deleting a folder keeps its playlists in Your Library. Every change
reaches Spotify's other clients. Folders start collapsed, and the ones you
open are open again next time.

Folders need playback and the Web API signed in to the same account. After a
full sign-out, use **Enable** beside the sidebar's folder note to start
playback again. Until playback is up, the last hierarchy shows but cannot be
changed. Filtering the shelf lists playlists flat, because hidden siblings
would make a drop ambiguous.

Without a hierarchy the shelf falls back to the flat list described above. A
pinned playlist stays a shortcut at the top: dragging the shortcut moves the
pin, not the playlist.

## Recent

The queue panel's second tab combines Spotify's history with tracks played
through Fastpotify, which Spotify does not record.

A song is added after about 30 seconds, or halfway through a shorter song.
Paused time and seeking do not count.

The local list is stored in `history.json` and is never uploaded. Settings →
Storage shows its location and has a **Clear history** button.
