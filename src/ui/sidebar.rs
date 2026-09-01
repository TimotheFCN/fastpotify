//! The left panel: navigation and Your Library.

use egui::{Align, CornerRadius, Frame, Layout, Margin, Rect, Sense, Vec2, pos2, vec2};

use crate::api::models::pick_image;
use crate::app::App;
use crate::model::{Action, Dialog, DragEntry, DragTrack, Loadable, Page};
use crate::theme::{self, Icon, Palette};

const DEFAULT_ROW_HEIGHT: f32 = 60.0;
const COMPACT_ROW_HEIGHT: f32 = 32.0;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Filter {
    #[default]
    Playlists,
    Albums,
    Artists,
    Podcasts,
}

struct Entry {
    image: Option<String>,
    name: String,
    subtitle: String,
    page: Page,
    uri: String,
    round: bool,
    owned: bool,
    playlist_index: Option<usize>,
    kind: EntryKind,
    /// How deep inside folders the row sits, for the indent.
    depth: u8,
}

enum EntryKind {
    LikedSongs,
    Folder {
        id: crate::rootlist::FolderId,
        collapsed: bool,
    },
    Playlist {
        in_rootlist: bool,
    },
    Other,
}

impl Entry {
    fn is_liked(&self) -> bool {
        matches!(self.kind, EntryKind::LikedSongs)
    }

    fn folder(&self) -> Option<(crate::rootlist::FolderId, bool)> {
        match self.kind {
            EntryKind::Folder { id, collapsed } => Some((id, collapsed)),
            _ => None,
        }
    }

    fn node(&self) -> Option<crate::rootlist::Node> {
        match self.kind {
            EntryKind::Folder { id, .. } => Some(crate::rootlist::Node::Folder(id)),
            EntryKind::Playlist { in_rootlist: true } => {
                Some(crate::rootlist::Node::Playlist(self.uri.clone()))
            }
            _ => None,
        }
    }
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    // The traffic lights float over the top-left of the sidebar now, so the
    // first nav row has to start below them.
    let top = 12 + theme::titlebar_inset(ui.ctx()) as i8;
    let panel = egui::Panel::left("sidebar")
        .resizable(true)
        .default_size(app.settings.sidebar_width)
        .size_range(210.0..=440.0)
        .show_separator_line(false)
        .frame(Frame::new().fill(palette.panel).inner_margin(Margin {
            left: 12,
            right: 8,
            top,
            bottom: 8,
        }));
    let response = panel.show(ui, |ui| {
        art_panel(app, ui);
        contents(app, ui);
    });
    let width = response.response.rect.width();
    if (width - app.settings.sidebar_width).abs() > 1.0 {
        app.settings.sidebar_width = width;
        app.actions.push(Action::SettingsChanged);
    }
}

/// Expanded album art at the bottom of the sidebar (#92).
fn art_panel(app: &mut App, ui: &mut egui::Ui) {
    if !app.settings.art_expanded {
        return;
    }
    let Some(now) = app.now_playing() else {
        return;
    };
    let Some(url) = now.art_url.clone().or_else(|| now.art_small.clone()) else {
        return;
    };
    let palette = app.palette;
    let side = ui
        .available_width()
        .min(ui.available_height() * 0.45)
        .max(80.0);
    egui::Panel::bottom("sidebar-art")
        .exact_size(side)
        .resizable(false)
        .show_separator_line(false)
        .frame(Frame::new())
        .show(ui, |ui| {
            let rect = Rect::from_min_size(
                ui.max_rect().left_top(),
                Vec2::splat(side.min(ui.available_width())),
            );
            super::widgets::paint_cover(ui, &palette, Some(&url), rect, 8.0, Icon::Music);
            let art = ui
                .interact(rect, egui::Id::new("sidebar-art"), Sense::click())
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            let chevron_rect = Rect::from_center_size(
                pos2(rect.right() - 16.0, rect.top() + 16.0),
                Vec2::splat(20.0),
            );
            let over_chevron = ui.rect_contains_pointer(chevron_rect);
            if art.hovered() || over_chevron {
                let chevron = ui
                    .interact(
                        chevron_rect,
                        egui::Id::new("sidebar-art-collapse"),
                        Sense::click(),
                    )
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                ui.painter().circle_filled(
                    chevron_rect.center(),
                    10.0,
                    palette.panel.gamma_multiply(0.9),
                );
                Icon::ChevronDown.image(palette.text, 14.0).paint_at(
                    ui,
                    Rect::from_center_size(chevron_rect.center(), Vec2::splat(14.0)),
                );
                if chevron.clicked() {
                    app.settings.art_expanded = false;
                    app.actions.push(Action::SettingsChanged);
                }
            }
            if art.clicked() && !over_chevron {
                if let Some(id) = &now.album_id {
                    app.actions.push(Action::Open(Page::Album(id.clone())));
                } else if let Some(id) = &now.show_id {
                    app.actions.push(Action::Open(Page::Show(id.clone())));
                }
            }
        });
}

/// Playlist rows in account order, including collapsible folders (#95).
fn folder_rows(app: &App, user_id: &str, entries: &mut Vec<Entry>) {
    let Some(playlists) = app.library.playlists.get() else {
        return;
    };
    let by_uri: std::collections::HashMap<&str, (usize, &crate::api::models::Playlist)> = playlists
        .iter()
        .enumerate()
        .map(|(index, playlist)| (playlist.uri.as_str(), (index, playlist)))
        .collect();
    let Some(snapshot) = app.folders.shown_snapshot() else {
        return;
    };
    let mut listed = std::collections::HashMap::<&str, u32>::new();
    for uri in snapshot.playlist_uris() {
        *listed.entry(uri).or_default() += 1;
    }
    for row in snapshot.project_rows(&app.expanded_folders, &app.settings.pinned_contexts) {
        match row {
            crate::rootlist::Row::Folder {
                id,
                name,
                depth,
                playlist_count,
                collapsed,
            } => entries.push(Entry {
                image: None,
                name: if name.is_empty() {
                    "Folder".into()
                } else {
                    name
                },
                subtitle: match playlist_count {
                    1 => "Folder • 1 playlist".to_string(),
                    count => format!("Folder • {count} playlists"),
                },
                page: Page::Home,
                uri: String::new(),
                round: false,
                owned: false,
                playlist_index: None,
                kind: EntryKind::Folder { id, collapsed },
                depth,
            }),
            crate::rootlist::Row::Playlist { uri, depth, .. } => {
                let Some((index, playlist)) = by_uri.get(uri.as_str()) else {
                    continue;
                };
                let in_rootlist = listed.get(uri.as_str()) == Some(&1);
                entries.push(playlist_entry(
                    playlist,
                    *index,
                    user_id,
                    depth,
                    in_rootlist,
                ));
            }
            crate::rootlist::Row::Unknown { .. } => {}
        }
    }
    // Playlists the rootlist has not met yet, the newly followed, wait at
    // the end rather than vanish.
    for (index, playlist) in playlists.iter().enumerate() {
        if !listed.contains_key(playlist.uri.as_str()) {
            entries.push(playlist_entry(playlist, index, user_id, 0, false));
        }
    }
}

fn playlist_entry(
    playlist: &crate::api::models::Playlist,
    index: usize,
    user_id: &str,
    depth: u8,
    in_rootlist: bool,
) -> Entry {
    Entry {
        image: pick_image(&playlist.images, 64).map(str::to_string),
        name: playlist.name.clone(),
        subtitle: format!("Playlist • {}", playlist.owner_name()),
        page: Page::Playlist(playlist.id.clone()),
        uri: playlist.uri.clone(),
        round: false,
        owned: playlist.owned_by(user_id),
        playlist_index: Some(index),
        kind: EntryKind::Playlist { in_rootlist },
        depth,
    }
}

fn nav_row(
    ui: &mut egui::Ui,
    palette: &Palette,
    icon: Icon,
    label: &str,
    active: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let color = if active || response.hovered() {
            palette.text
        } else {
            palette.secondary
        };
        let icon_rect =
            Rect::from_center_size(pos2(rect.left() + 22.0, rect.center().y), Vec2::splat(22.0));
        icon.image(color, 22.0).paint_at(ui, icon_rect);
        ui.painter().text(
            pos2(rect.left() + 46.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            theme::bold(15.0),
            color,
        );
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn contents(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let page = app.page().clone();
    ui.add_space(4.0);
    if nav_row(ui, &palette, Icon::House, "Home", page == Page::Home).clicked() {
        app.actions.push(Action::Open(Page::Home));
    }
    if nav_row(ui, &palette, Icon::Search, "Search", page == Page::Search).clicked() {
        app.actions.push(Action::FocusSearch);
    }
    ui.add_space(10.0);
    ui.painter().hline(
        ui.max_rect().x_range().shrink(4.0),
        ui.cursor().top(),
        egui::Stroke::new(1.0, palette.outline),
    );
    ui.add_space(10.0);

    let filter_id = egui::Id::new("sidebar-filter");
    let mut filter = ui
        .data(|data| data.get_temp::<Filter>(filter_id))
        .unwrap_or_default();
    let show_search_id = egui::Id::new("sidebar-show-search");
    let mut show_search = ui
        .data(|data| data.get_temp::<bool>(show_search_id))
        .unwrap_or(false);

    ui.horizontal(|ui| {
        ui.add_space(6.0);
        theme::icon(ui, Icon::Library, 22.0, palette.secondary);
        ui.add_space(2.0);
        theme::text(ui, "Your Library", theme::bold(15.0), palette.text);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if theme::icon_button(
                ui,
                Icon::PanelLeft,
                16.0,
                palette.secondary,
                palette.text,
                super::keys::platform_shortcut("Hide sidebar (Ctrl+B)", "Hide sidebar (Cmd+B)"),
            )
            .clicked()
            {
                app.actions.push(Action::ToggleSidebar);
            }
            let create = theme::icon_button(
                ui,
                Icon::Plus,
                16.0,
                palette.secondary,
                palette.text,
                "Create",
            );
            if app.folders_writable() {
                egui::Popup::menu(&create)
                    .width(220.0)
                    .frame(super::widgets::menu_frame(&palette))
                    .align(egui::RectAlign::BOTTOM_END)
                    .show(|ui| {
                        if super::widgets::menu_item(
                            ui,
                            &palette,
                            Some(Icon::ListMusic),
                            "New playlist",
                        ) {
                            app.actions.push(Action::ShowDialog(Dialog::create_playlist(
                                Vec::new(),
                                None,
                            )));
                        }
                        if super::widgets::menu_item(ui, &palette, Some(Icon::Folder), "New folder")
                        {
                            app.actions
                                .push(Action::ShowDialog(Dialog::create_folder(None)));
                        }
                    });
            } else if create.clicked() {
                app.actions.push(Action::ShowDialog(Dialog::create_playlist(
                    Vec::new(),
                    None,
                )));
            }
            if theme::icon_button(
                ui,
                Icon::Search,
                16.0,
                palette.secondary,
                palette.text,
                "Search Your Library",
            )
            .clicked()
            {
                show_search = !show_search;
                if show_search {
                    ui.memory_mut(|memory| memory.request_focus(egui::Id::new("sidebar-search")));
                } else {
                    app.library.filter.clear();
                }
            }
        });
    });
    ui.add_space(6.0);

    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        for (value, label) in [
            (Filter::Playlists, "Playlists"),
            (Filter::Albums, "Albums"),
            (Filter::Artists, "Artists"),
            (Filter::Podcasts, "Podcasts"),
        ] {
            if theme::soft_button(ui, &palette, None, label, filter == value).clicked() {
                filter = value;
            }
        }
    });
    ui.data_mut(|data| {
        data.insert_temp(filter_id, filter);
        data.insert_temp(show_search_id, show_search);
    });
    if show_search {
        ui.add_space(4.0);
        super::widgets::search_field(
            ui,
            &palette,
            egui::Id::new("sidebar-search"),
            &mut app.library.filter,
            "Search in Your Library",
            ui.available_width() - 4.0,
        );
    }
    ui.add_space(6.0);

    // Make sure the selected shelf is loading.
    match filter {
        Filter::Playlists => {}
        Filter::Albums => {
            if !app.library.albums.loaded_once && !app.library.albums.loading {
                app.actions.push(Action::LoadMore(Page::Albums));
            }
        }
        Filter::Artists => {
            if !app.library.artists.loaded_once && !app.library.artists.loading {
                app.actions.push(Action::LoadMore(Page::Artists));
            }
        }
        Filter::Podcasts => {
            if !app.library.shows.loaded_once && !app.library.shows.loading {
                app.actions.push(Action::LoadMore(Page::Podcasts));
            }
        }
    }

    let needle = app.library.filter.trim().to_lowercase();
    let user_id = app.user_id().unwrap_or("").to_string();
    let mut entries: Vec<Entry> = Vec::new();
    let mut loading = false;
    let mut error: Option<String> = None;
    let mut more_page: Option<Page> = None;
    let folder_mode = filter == Filter::Playlists && needle.is_empty() && app.folder_mode();
    match filter {
        Filter::Playlists => {
            if needle.is_empty() || "liked songs".contains(&needle) {
                entries.push(Entry {
                    image: None,
                    name: "Liked Songs".into(),
                    subtitle: match app.library.liked.total {
                        Some(total) => format!("Playlist • {total} songs"),
                        None => "Playlist".into(),
                    },
                    page: Page::LikedSongs,
                    uri: String::new(),
                    round: false,
                    owned: false,
                    playlist_index: None,
                    kind: EntryKind::LikedSongs,
                    depth: 0,
                });
            }
            if folder_mode {
                folder_rows(app, &user_id, &mut entries);
            }
            match &app.library.playlists {
                Loadable::Loaded(_) if folder_mode => {}
                Loadable::Loaded(playlists) => {
                    // Recently played first, the way Spotify orders its own
                    // sidebar; the rest keep the library's order.
                    let rank = |uri: &str| {
                        app.recent_contexts
                            .iter()
                            .position(|held| held == uri)
                            .unwrap_or(usize::MAX)
                    };
                    let mut ordered: Vec<_> = playlists.iter().enumerate().collect();
                    ordered.sort_by_key(|(index, playlist)| (rank(&playlist.uri), *index));
                    for (index, playlist) in ordered {
                        if !needle.is_empty() && !playlist.name.to_lowercase().contains(&needle) {
                            continue;
                        }
                        let owned = playlist.owned_by(&user_id);
                        entries.push(Entry {
                            image: pick_image(&playlist.images, 64).map(str::to_string),
                            name: playlist.name.clone(),
                            subtitle: format!("Playlist • {}", playlist.owner_name()),
                            page: Page::Playlist(playlist.id.clone()),
                            uri: playlist.uri.clone(),
                            round: false,
                            owned,
                            playlist_index: Some(index),
                            kind: EntryKind::Playlist { in_rootlist: false },
                            depth: 0,
                        });
                    }
                }
                Loadable::Loading | Loadable::NotLoaded => loading = true,
                Loadable::Failed(message) => error = Some(message.clone()),
            }
        }
        Filter::Albums => {
            for saved in &app.library.albums.items {
                let album = &saved.album;
                if !needle.is_empty()
                    && !album.name.to_lowercase().contains(&needle)
                    && !album
                        .artists
                        .iter()
                        .any(|a| a.name.to_lowercase().contains(&needle))
                {
                    continue;
                }
                entries.push(Entry {
                    image: pick_image(&album.images, 64).map(str::to_string),
                    name: album.name.clone(),
                    subtitle: format!(
                        "{} • {}",
                        album.kind_label(),
                        crate::api::models::join_names(
                            album.artists.iter().map(|a| a.name.as_str())
                        )
                    ),
                    page: Page::Album(album.id.clone()),
                    uri: album.uri.clone(),
                    round: false,
                    owned: false,
                    playlist_index: None,
                    kind: EntryKind::Other,
                    depth: 0,
                });
            }
            loading = app.library.albums.loading && app.library.albums.items.is_empty();
            error = app.library.albums.error.clone();
            if app.library.albums.can_load_more() {
                more_page = Some(Page::Albums);
            }
        }
        Filter::Artists => {
            for artist in &app.library.artists.items {
                if !needle.is_empty() && !artist.name.to_lowercase().contains(&needle) {
                    continue;
                }
                entries.push(Entry {
                    image: pick_image(&artist.images, 64).map(str::to_string),
                    name: artist.name.clone(),
                    subtitle: "Artist".into(),
                    page: Page::Artist(artist.id.clone()),
                    uri: artist.uri.clone(),
                    round: true,
                    owned: false,
                    playlist_index: None,
                    kind: EntryKind::Other,
                    depth: 0,
                });
            }
            loading = app.library.artists.loading && app.library.artists.items.is_empty();
            error = app.library.artists.error.clone();
            if app.library.artists.can_load_more() {
                more_page = Some(Page::Artists);
            }
        }
        Filter::Podcasts => {
            for saved in &app.library.shows.items {
                let show = &saved.show;
                if !needle.is_empty() && !show.name.to_lowercase().contains(&needle) {
                    continue;
                }
                entries.push(Entry {
                    image: pick_image(&show.images, 64).map(str::to_string),
                    name: show.name.clone(),
                    subtitle: format!("Podcast • {}", show.publisher),
                    page: Page::Show(show.id.clone()),
                    uri: show.uri.clone(),
                    round: false,
                    owned: false,
                    playlist_index: None,
                    kind: EntryKind::Other,
                    depth: 0,
                });
            }
            loading = app.library.shows.loading && app.library.shows.items.is_empty();
            error = app.library.shows.error.clone();
            if app.library.shows.can_load_more() {
                more_page = Some(Page::Podcasts);
            }
        }
    }

    // Keep Liked Songs first, then pinned entries. A custom playlist order
    // applies to the remaining rows; newly added playlists precede that order.
    let pin_rank = |uri: &str| {
        app.settings
            .pinned_contexts
            .iter()
            .position(|held| held == uri)
            .unwrap_or(usize::MAX)
    };
    let custom_order =
        filter == Filter::Playlists && !folder_mode && !app.settings.sidebar_order.is_empty();
    let saved_rank = |uri: &str| {
        app.settings
            .sidebar_order
            .iter()
            .position(|held| held == uri)
    };
    // Pins are pins, whatever orders the rest: Liked Songs, then the
    // pinned block, then everyone else by the listener's own order or,
    // failing one, by recency.
    if !folder_mode {
        entries.sort_by_key(|entry| {
            if entry.is_liked() {
                (0, 0)
            } else {
                match pin_rank(&entry.uri) {
                    usize::MAX if custom_order => match saved_rank(&entry.uri) {
                        Some(rank) => (3, rank),
                        None => (2, entry.playlist_index.unwrap_or(0)),
                    },
                    usize::MAX => (2, 0),
                    rank => (1, rank),
                }
            }
        });
    }
    // The zones a dragged row can land in: everything sits below Liked
    // Songs, and the pinned entries form one block right after it.
    let liked_rows = entries.iter().take_while(|entry| entry.is_liked()).count();
    let pinned_rows = entries
        .iter()
        .filter(|entry| !entry.is_liked() && pin_rank(&entry.uri) != usize::MAX)
        .count();
    let playing_context = app.playing_context_uri();
    let context_playing = app.believed_playing();
    let current_page = app.page().clone();
    let folder_access_pending = filter == Filter::Playlists && app.folders_starting();
    let folders_loading_row = folder_access_pending && app.folders.shown_snapshot().is_none();

    egui::ScrollArea::vertical()
        .id_salt("sidebar-list")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if loading || folders_loading_row {
                super::widgets::loading_row(ui, &palette);
            }
            if filter == Filter::Playlists
                && app.folders.confirmed_snapshot().is_none()
                && !app.folders.is_loading()
                && !folder_access_pending
            {
                // A quiet indication, not an alert: playlists still work, only
                // the folder arrangement is waiting on something.
                let needs_playback = matches!(
                    app.local_playback,
                    crate::backend::LocalPlayback::Unavailable
                        | crate::backend::LocalPlayback::Failed(_)
                );
                let other_account = matches!(
                    app.local_playback,
                    crate::backend::LocalPlayback::Ready { .. }
                ) && app.local.connected;
                let (text, remedy) = if needs_playback {
                    (
                        "Playlist folders appear once playback is enabled.",
                        Some(("Enable", Action::EnablePlayback)),
                    )
                } else if other_account {
                    (
                        "Playback runs on another account, so playlist folders stay hidden.",
                        None,
                    )
                } else {
                    (
                        "Spotify didn't list your playlist folders.",
                        Some(("Retry", Action::RefreshFolders)),
                    )
                };
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(8.0);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(text)
                                .font(theme::regular(12.5))
                                .color(palette.dim),
                        )
                        .wrap(),
                    );
                    if let Some((label, action)) = remedy
                        && theme::soft_button(ui, &palette, None, label, false).clicked()
                    {
                        app.actions.push(action);
                    }
                });
                ui.add_space(4.0);
            }
            if let Some(error) = &error {
                super::widgets::error_row(ui, app, error, None);
            }
            if entries.is_empty() && !loading && error.is_none() {
                ui.add_space(12.0);
                theme::subtle(
                    ui,
                    &palette,
                    if needle.is_empty() {
                        "Nothing here yet."
                    } else {
                        "No matches."
                    },
                );
            }
            let compact = app.settings.sidebar_compact;
            let row_height = if compact {
                COMPACT_ROW_HEIGHT
            } else {
                DEFAULT_ROW_HEIGHT
            };
            // Calculate drop positions from fixed row height because rows shift
            // before drawing.
            let list_top = ui.cursor().top();
            let pointer = ui
                .ctx()
                .pointer_latest_pos()
                .filter(|pos| ui.clip_rect().contains(*pos));
            // Tracks may drop on Liked Songs or owned playlists.
            let dragging_song = egui::DragAndDrop::has_payload_of_type::<DragTrack>(ui.ctx());
            let drop_target = dragging_song
                .then_some(pointer)
                .flatten()
                .map(|pos| ((pos.y - list_top) / row_height).floor())
                .filter(|row| *row >= 0.0 && *row < entries.len() as f32)
                .map(|row| row as usize)
                .filter(|row| entries[*row].is_liked() || entries[*row].owned);
            let dragged_entry = egui::DragAndDrop::payload::<DragEntry>(ui.ctx());
            let filtered_rootlist = filter == Filter::Playlists
                && !needle.is_empty()
                && app.folders.confirmed_snapshot().is_some();
            let reordering = dragged_entry.as_ref().is_some_and(|drag| {
                !filtered_rootlist || app.settings.pinned_contexts.contains(&drag.uri)
            });
            let row_drop_target = reordering.then_some(pointer).flatten().map(|pos| {
                split_row_drop_target(
                    pos.y,
                    list_top,
                    row_height,
                    entries.len(),
                    liked_rows,
                    |index| {
                        folder_mode
                            && entries[index].folder().is_some_and(|(folder, _)| {
                                dragged_entry
                                    .as_ref()
                                    .is_some_and(|drag| valid_folder_drop(app, drag, folder))
                            })
                    },
                )
            });
            let (reorder_slot, folder_drop_index) = match row_drop_target {
                Some(RowDropTarget::Between(slot)) => (Some(slot), None),
                Some(RowDropTarget::Inside(index)) => (None, Some(index)),
                None => (None, None),
            };
            super::widgets::virtual_rows(ui, entries.len(), row_height, |ui, index| {
                let entry = &entries[index];
                let droppable = entry.is_liked() || entry.owned;
                let drop_hover = drop_target == Some(index);
                let active = entry.folder().is_none() && entry.page == current_page;
                let playing = context_playing
                    && !entry.uri.is_empty()
                    && playing_context.as_deref() == Some(entry.uri.as_str());
                let pinned =
                    !entry.uri.is_empty() && app.settings.pinned_contexts.contains(&entry.uri);
                let (rect, response) = ui.allocate_exact_size(
                    vec2(ui.available_width(), row_height),
                    Sense::click_and_drag(),
                );
                let folder_drop_hover = folder_drop_index == Some(index);
                // Past the drag threshold the row itself is in hand, to be
                // pinned into place; clicks and the context menu keep their
                // meaning. Liked Songs stays where it is.
                let pinned_entry = app.settings.pinned_contexts.contains(&entry.uri);
                if !entry.is_liked()
                    && (!entry.uri.is_empty() || entry.folder().is_some())
                    && (!filtered_rootlist || pinned_entry)
                    && (!folder_mode
                        || pinned_entry
                        || app.folders_writable() && entry.node().is_some())
                    && response.drag_started_by(egui::PointerButton::Primary)
                {
                    egui::DragAndDrop::set_payload(
                        ui.ctx(),
                        DragEntry {
                            uri: entry.uri.clone(),
                            folder: entry.folder().map(|(id, _)| id),
                            title: entry.name.clone(),
                            image: entry.image.clone(),
                        },
                    );
                }
                // Animate rows around the current track or entry drop target.
                let shift = ui.ctx().animate_value_with_time(
                    ui.id().with(("drop-shift", index)),
                    if let Some(slot) = reorder_slot {
                        if index < slot { -4.0 } else { 4.0 }
                    } else {
                        match drop_target {
                            Some(target) if index < target => -4.0,
                            Some(target) if index > target => 4.0,
                            _ => 0.0,
                        }
                    },
                    0.12,
                );
                let rect = rect.translate(vec2(0.0, shift));
                if ui.is_rect_visible(rect) {
                    if active {
                        ui.painter()
                            .rect_filled(rect, CornerRadius::same(6), palette.surface);
                    } else if response.hovered() {
                        ui.painter().rect_filled(
                            rect,
                            CornerRadius::same(6),
                            palette.surface_hover.gamma_multiply(0.6),
                        );
                    }
                    if drop_hover || folder_drop_hover {
                        ui.painter().rect_filled(
                            rect,
                            CornerRadius::same(6),
                            palette.accent.gamma_multiply(0.18),
                        );
                        ui.painter().rect_stroke(
                            rect,
                            CornerRadius::same(6),
                            egui::Stroke::new(1.5, palette.accent),
                            egui::StrokeKind::Inside,
                        );
                    }
                    let name_color = if playing {
                        palette.accent
                    } else {
                        palette.text
                    };
                    let reserved = if folder_drop_hover { 204.0 } else { 112.0 };
                    let indent =
                        (f32::from(entry.depth) * 14.0).min((rect.width() - reserved).max(0.0));
                    if let Some((_, collapsed)) = entry.folder() {
                        let chevron = if collapsed {
                            Icon::ChevronRight
                        } else {
                            Icon::ChevronDown
                        };
                        let left = rect.left() + 8.0 + indent;
                        chevron.image(palette.secondary, 16.0).paint_at(
                            ui,
                            Rect::from_center_size(
                                pos2(left + 8.0, rect.center().y),
                                Vec2::splat(16.0),
                            ),
                        );
                        Icon::Folder.image(palette.secondary, 20.0).paint_at(
                            ui,
                            Rect::from_center_size(
                                pos2(left + 30.0, rect.center().y),
                                Vec2::splat(20.0),
                            ),
                        );
                        let text_left = left + 46.0;
                        let text_right = rect.right() - if folder_drop_hover { 100.0 } else { 8.0 };
                        let painter = ui.painter().with_clip_rect(Rect::from_min_max(
                            pos2(text_left, rect.top()),
                            pos2(text_right, rect.bottom()),
                        ));
                        crate::bidi::paint_line(
                            &painter,
                            text_left,
                            text_right,
                            rect.center().y - if compact { 0.0 } else { 9.0 },
                            &entry.name,
                            theme::medium(if compact { 13.5 } else { 14.0 }),
                            name_color,
                        );
                        if !compact {
                            crate::bidi::paint_line(
                                &painter,
                                text_left,
                                text_right,
                                rect.center().y + 10.0,
                                &entry.subtitle,
                                theme::regular(12.5),
                                palette.secondary,
                            );
                        }
                        if folder_drop_hover {
                            let label = ui.painter().layout_no_wrap(
                                "Move inside".into(),
                                theme::semibold(11.5),
                                palette.on_accent,
                            );
                            let chip = Rect::from_center_size(
                                pos2(rect.right() - 47.0, rect.center().y),
                                vec2(84.0, 24.0),
                            );
                            ui.painter().rect_filled(chip, 12.0, palette.accent);
                            ui.painter().galley(
                                chip.center() - label.size() / 2.0,
                                label,
                                palette.on_accent,
                            );
                        }
                    } else if compact {
                        let text_left = rect.left() + 8.0 + indent;
                        let text_right = rect.right() - if playing || pinned { 28.0 } else { 8.0 };
                        let painter = ui.painter().with_clip_rect(Rect::from_min_max(
                            pos2(text_left, rect.top()),
                            pos2(text_right, rect.bottom()),
                        ));
                        crate::bidi::paint_line(
                            &painter,
                            text_left,
                            text_right,
                            rect.center().y,
                            &entry.name,
                            theme::medium(13.5),
                            name_color,
                        );
                    } else {
                        let cover_rect = Rect::from_center_size(
                            pos2(rect.left() + 8.0 + indent + 22.0, rect.center().y),
                            Vec2::splat(44.0),
                        );
                        if entry.is_liked() {
                            liked_cover(ui, cover_rect, 6.0);
                        } else {
                            super::widgets::paint_cover(
                                ui,
                                &palette,
                                entry.image.as_deref(),
                                cover_rect,
                                if entry.round { 22.0 } else { 6.0 },
                                if entry.round { Icon::User } else { Icon::Music },
                            );
                        }
                        let text_left = cover_rect.right() + 12.0;
                        let text_right = rect.right() - if playing || pinned { 28.0 } else { 8.0 };
                        let painter = ui.painter().with_clip_rect(Rect::from_min_max(
                            pos2(text_left, rect.top()),
                            pos2(text_right, rect.bottom()),
                        ));
                        crate::bidi::paint_line(
                            &painter,
                            text_left,
                            text_right,
                            rect.center().y - 9.0,
                            &entry.name,
                            theme::medium(14.0),
                            name_color,
                        );
                        crate::bidi::paint_line(
                            &painter,
                            text_left,
                            text_right,
                            rect.center().y + 10.0,
                            &entry.subtitle,
                            theme::regular(12.5),
                            palette.secondary,
                        );
                        // Hovering the art offers to play right from here.
                        let can_play = !entry.uri.is_empty() || entry.is_liked();
                        let play_response = can_play.then(|| {
                            ui.interact(
                                cover_rect,
                                ui.id().with(("sidebar-play", index)),
                                Sense::click(),
                            )
                        });
                        let play_hover = play_response.as_ref().is_some_and(|play| play.hovered());
                        if play_hover || (response.hovered() && can_play) {
                            ui.painter().rect_filled(
                                cover_rect,
                                CornerRadius::same(if entry.round { 22 } else { 6 }),
                                egui::Color32::from_black_alpha(120),
                            );
                            Icon::PlayFilled
                                .image(
                                    if play_hover {
                                        palette.accent
                                    } else {
                                        egui::Color32::WHITE
                                    },
                                    18.0,
                                )
                                .paint_at(
                                    ui,
                                    Rect::from_center_size(
                                        cover_rect.center()
                                            + theme::play_glyph_offset(Icon::PlayFilled, 18.0),
                                        Vec2::splat(18.0),
                                    ),
                                );
                            if let Some(play) = &play_response {
                                play.clone().on_hover_cursor(egui::CursorIcon::PointingHand);
                            }
                        }
                        if play_response.is_some_and(|play| play.clicked()) {
                            let uri = if entry.is_liked() {
                                app.user
                                    .as_ref()
                                    .map(|user| format!("spotify:user:{}:collection", user.id))
                            } else {
                                Some(entry.uri.clone())
                            };
                            if let Some(uri) = uri {
                                app.actions.push(Action::PlayContext {
                                    uri,
                                    offset_uri: None,
                                    offset_index: None,
                                });
                            }
                        }
                    }
                    if playing {
                        let icon_rect = Rect::from_center_size(
                            pos2(rect.right() - 16.0, rect.center().y),
                            Vec2::splat(16.0),
                        );
                        Icon::Volume2
                            .image(palette.accent, 16.0)
                            .paint_at(ui, icon_rect);
                    } else if pinned {
                        let icon_rect = Rect::from_center_size(
                            pos2(rect.right() - 16.0, rect.center().y),
                            Vec2::splat(13.0),
                        );
                        Icon::Pin
                            .image(palette.secondary, 13.0)
                            .paint_at(ui, icon_rect);
                    }
                    // Rows that cannot take the song step back a little.
                    if dragging_song && !droppable {
                        ui.painter().rect_filled(
                            rect,
                            CornerRadius::same(6),
                            palette.panel.gamma_multiply(0.5),
                        );
                    }
                }
                if dragging_song
                    && droppable
                    && let Some(track) = response.dnd_release_payload::<DragTrack>()
                {
                    if entry.is_liked() {
                        // Dropping on Liked Songs saves; a song already
                        // saved is left alone.
                        if app.is_saved(&track.uri) != Some(true) {
                            app.actions.push(Action::ToggleSaved(track.uri.clone()));
                        }
                    } else if let Page::Playlist(id) = &entry.page {
                        app.actions.push(Action::AddToPlaylist {
                            playlist_id: id.clone(),
                            playlist_name: entry.name.clone(),
                            uris: vec![track.uri.clone()],
                        });
                    }
                }
                if folder_drop_hover
                    && let Some(drag) = response.dnd_release_payload::<DragEntry>()
                    && let Some((parent, _)) = entry.folder()
                    && let Some(node) = drag_node(&drag)
                {
                    app.actions
                        .push(Action::ChangeFolders(crate::rootlist::Intent::Move {
                            node,
                            parent: Some(parent),
                            before: None,
                        }));
                }
                if response.clicked() {
                    if let Some((folder_id, _)) = entry.folder() {
                        app.actions.push(Action::ToggleFolder(folder_id));
                    } else {
                        app.actions.push(Action::Open(entry.page.clone()));
                    }
                }
                if let Some((folder, _)) = entry.folder()
                    && app.folders_writable()
                {
                    egui::Popup::context_menu(&response)
                        .width(220.0)
                        .frame(super::widgets::menu_frame(&palette))
                        .show(|ui| {
                            let current = app
                                .folders
                                .confirmed_snapshot()
                                .and_then(|snapshot| {
                                    snapshot
                                        .parent_of(&crate::rootlist::Node::Folder(folder))
                                        .ok()
                                })
                                .flatten();
                            if super::widgets::menu_item(
                                ui,
                                &palette,
                                Some(Icon::ListMusic),
                                "Create playlist here",
                            ) {
                                app.actions.push(Action::ShowDialog(Dialog::create_playlist(
                                    Vec::new(),
                                    Some(folder),
                                )));
                            }
                            if super::widgets::menu_item(
                                ui,
                                &palette,
                                Some(Icon::Folder),
                                "Create folder here",
                            ) {
                                app.actions
                                    .push(Action::ShowDialog(Dialog::create_folder(Some(folder))));
                            }
                            if super::widgets::menu_item(ui, &palette, Some(Icon::Pencil), "Rename")
                            {
                                app.actions.push(Action::ShowDialog(Dialog::EditFolder {
                                    folder: Some(folder),
                                    parent: current,
                                    name: entry.name.clone(),
                                }));
                            }
                            if super::widgets::menu_item(
                                ui,
                                &palette,
                                Some(Icon::ArrowRight),
                                "Move to folder",
                            ) {
                                app.actions.push(Action::ShowDialog(Dialog::MoveToFolder {
                                    node: crate::rootlist::Node::Folder(folder),
                                    current,
                                    query: String::new(),
                                }));
                            }
                            if super::widgets::menu_item(ui, &palette, Some(Icon::Trash), "Delete")
                            {
                                app.actions
                                    .push(Action::ShowDialog(Dialog::ConfirmDeleteFolder {
                                        folder,
                                        name: entry.name.clone(),
                                    }));
                            }
                        });
                } else if !entry.uri.is_empty() {
                    let owned_playlist = entry
                        .owned
                        .then_some(entry.playlist_index)
                        .flatten()
                        .and_then(|index| {
                            app.library
                                .playlists
                                .get()
                                .and_then(|list| list.get(index))
                                .cloned()
                        });
                    egui::Popup::context_menu(&response)
                        .frame(super::widgets::menu_frame(&palette))
                        .show(|ui| {
                            super::widgets::context_menu_items(
                                ui,
                                app,
                                &entry.uri,
                                &entry.name,
                                owned_playlist.as_ref(),
                            );
                            let move_parent = app
                                .folders_writable()
                                .then(|| app.folders.confirmed_snapshot())
                                .flatten()
                                .filter(|snapshot| snapshot.contains_playlist(&entry.uri))
                                .map(|snapshot| {
                                    snapshot
                                        .parent_of(&crate::rootlist::Node::Playlist(
                                            entry.uri.clone(),
                                        ))
                                        .ok()
                                        .flatten()
                                });
                            if let Some(current) = move_parent
                                && super::widgets::menu_item(
                                    ui,
                                    &palette,
                                    Some(Icon::ArrowRight),
                                    "Move to folder",
                                )
                            {
                                app.actions.push(Action::ShowDialog(Dialog::MoveToFolder {
                                    node: crate::rootlist::Node::Playlist(entry.uri.clone()),
                                    current,
                                    query: String::new(),
                                }));
                            }
                            let pinned = app.settings.pinned_contexts.contains(&entry.uri);
                            if super::widgets::menu_item(
                                ui,
                                &palette,
                                Some(if pinned { Icon::PinOff } else { Icon::Pin }),
                                if pinned { "Unpin" } else { "Pin to top" },
                            ) {
                                if pinned {
                                    app.settings
                                        .pinned_contexts
                                        .retain(|held| held != &entry.uri);
                                } else {
                                    app.settings.pinned_contexts.push(entry.uri.clone());
                                    app.settings.sidebar_order.retain(|held| held != &entry.uri);
                                }
                                app.mark_settings_dirty();
                            }
                            if custom_order
                                && super::widgets::menu_item(
                                    ui,
                                    &palette,
                                    Some(Icon::Clock),
                                    "Sort by recently played",
                                )
                            {
                                // Clear the custom order without confirmation.
                                app.settings.sidebar_order.clear();
                                app.mark_settings_dirty();
                            }
                        });
                } else if entry.is_liked() {
                    egui::Popup::context_menu(&response)
                        .frame(super::widgets::menu_frame(&palette))
                        .show(|ui| {
                            if super::widgets::menu_item(ui, &palette, Some(Icon::Play), "Play")
                                && let Some(user) = &app.user
                            {
                                app.actions.push(Action::PlayContext {
                                    uri: format!("spotify:user:{}:collection", user.id),
                                    offset_uri: None,
                                    offset_index: None,
                                });
                            }
                        });
                }
                response.on_hover_cursor(egui::CursorIcon::PointingHand);
            });
            if let Some(slot) = reorder_slot {
                let y = list_top + slot as f32 * row_height;
                let depth = if folder_mode && slot >= liked_rows + pinned_rows {
                    dragged_entry
                        .as_ref()
                        .and_then(|drag| drag_node(drag))
                        .and_then(|node| {
                            entries
                                .iter()
                                .skip(slot)
                                .filter(|entry| !app.settings.pinned_contexts.contains(&entry.uri))
                                .find(|entry| entry.node().as_ref() != Some(&node))
                                .map(|entry| entry.depth)
                        })
                        .unwrap_or(0)
                } else {
                    0
                };
                ui.painter().hline(
                    egui::Rangef::new(
                        ui.max_rect().left() + 6.0 + f32::from(depth) * 14.0,
                        ui.max_rect().right() - 6.0,
                    ),
                    y,
                    egui::Stroke::new(2.0, palette.accent),
                );
                if ui.input(|input| input.pointer.any_released())
                    && let Some(drag) = egui::DragAndDrop::take_payload::<DragEntry>(ui.ctx())
                {
                    if folder_mode {
                        drop_rootlist_row(app, &entries, liked_rows, pinned_rows, slot, &drag);
                    } else if filter == Filter::Playlists {
                        drop_playlist_row(app, &entries, liked_rows, pinned_rows, slot, &drag.uri);
                    } else {
                        drop_row(app, &entries, liked_rows, pinned_rows, slot, &drag.uri);
                    }
                }
            }
            if let Some(page) = more_page {
                super::widgets::load_more_when_near_end(ui, app, page, true);
            }
        });
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RowDropTarget {
    Between(usize),
    Inside(usize),
}

fn split_row_drop_target(
    pointer_y: f32,
    list_top: f32,
    row_height: f32,
    row_count: usize,
    first_slot: usize,
    can_drop_inside: impl FnOnce(usize) -> bool,
) -> RowDropTarget {
    let position = (pointer_y - list_top) / row_height;
    let row = position.floor();
    if row >= 0.0 && row < row_count as f32 {
        let index = row as usize;
        let edge = (10.0 / row_height).min(0.25);
        let within_row = position - row;
        if within_row >= edge && within_row <= 1.0 - edge && can_drop_inside(index) {
            return RowDropTarget::Inside(index);
        }
    }
    RowDropTarget::Between(
        (position.round().max(0.0).min(row_count as f32) as usize).clamp(first_slot, row_count),
    )
}

fn drag_node(drag: &DragEntry) -> Option<crate::rootlist::Node> {
    drag.folder.map(crate::rootlist::Node::Folder).or_else(|| {
        (!drag.uri.is_empty()).then(|| crate::rootlist::Node::Playlist(drag.uri.clone()))
    })
}

fn valid_folder_drop(app: &App, drag: &DragEntry, parent: crate::rootlist::FolderId) -> bool {
    if !app.folders_writable() || app.settings.pinned_contexts.contains(&drag.uri) {
        return false;
    }
    let Some(snapshot) = app.folders.confirmed_snapshot() else {
        return false;
    };
    let Some(node) = drag_node(drag) else {
        return false;
    };
    snapshot.is_valid_destination(&node, parent)
}

fn drop_rootlist_row(
    app: &mut App,
    entries: &[Entry],
    liked_rows: usize,
    pinned_rows: usize,
    slot: usize,
    drag: &DragEntry,
) {
    let section_end = liked_rows + pinned_rows;
    let pinned = app.settings.pinned_contexts.contains(&drag.uri);
    if drag.folder.is_none() && (pinned || slot < section_end) {
        drop_playlist_row(app, entries, liked_rows, pinned_rows, slot, &drag.uri);
        return;
    }
    let Some(node) = drag_node(drag) else {
        return;
    };
    let before = entries
        .iter()
        .skip(slot)
        .filter(|entry| !app.settings.pinned_contexts.contains(&entry.uri))
        .filter_map(Entry::node)
        .find(|candidate| candidate != &node);
    let Some(snapshot) = app.folders.confirmed_snapshot() else {
        return;
    };
    let parent = match &before {
        Some(before) => match snapshot.parent_of(before) {
            Ok(parent) => parent,
            Err(_) => return,
        },
        None => None,
    };
    if let Some(parent) = parent
        && !snapshot.is_valid_destination(&node, parent)
    {
        return;
    }
    app.actions
        .push(Action::ChangeFolders(crate::rootlist::Intent::Move {
            node,
            parent,
            before,
        }));
}

/// A dropped playlist row lands in one of two worlds. Inside the pinned
/// block it pins, or reorders the pins, exactly where it fell. Below the
/// block it orders the rest: the first such drop snapshots the order on
/// screen so nothing jumps, and rows then sit where they are put. A
/// pinned row dropped below the block is unpinned.
fn drop_playlist_row(
    app: &mut App,
    entries: &[Entry],
    liked_rows: usize,
    pinned_rows: usize,
    slot: usize,
    uri: &str,
) {
    let section_end = liked_rows + pinned_rows;
    let was_pinned = app.settings.pinned_contexts.iter().any(|held| held == uri);
    if pinned_rows > 0 && slot < section_end {
        // Into the pinned block: the pinned entry the drop lands in front
        // of anchors the new pin position.
        let anchor = entries[liked_rows..section_end]
            .iter()
            .skip(slot.saturating_sub(liked_rows))
            .map(|entry| entry.uri.as_str())
            .find(|held| *held != uri)
            .map(str::to_string);
        let mut pinned = app.settings.pinned_contexts.clone();
        pinned.retain(|held| held != uri);
        let at = anchor
            .and_then(|anchor| pinned.iter().position(|held| *held == anchor))
            .unwrap_or(pinned.len());
        pinned.insert(at, uri.to_string());
        if pinned != app.settings.pinned_contexts {
            app.settings.pinned_contexts = pinned;
            app.settings.sidebar_order.retain(|held| held != uri);
            app.mark_settings_dirty();
        }
        return;
    }
    // Below the pinned block, use custom playlist order and unpin moved rows.
    if was_pinned {
        app.settings.pinned_contexts.retain(|held| held != uri);
        app.mark_settings_dirty();
        if app.settings.sidebar_order.is_empty() {
            // Keep automatic recency order when no custom order exists.
            return;
        }
    }
    let mut order = full_playlist_order(app);
    let anchor = entries
        .iter()
        .skip(slot)
        .filter(|entry| !entry.is_liked())
        .map(|entry| entry.uri.as_str())
        .find(|held| *held != uri)
        .map(str::to_string);
    order.retain(|held| held != uri);
    let at = anchor
        .and_then(|anchor| order.iter().position(|held| *held == anchor))
        .unwrap_or(order.len());
    order.insert(at, uri.to_string());
    if order != app.settings.sidebar_order {
        app.settings.sidebar_order = order;
        app.mark_settings_dirty();
    }
}

/// Every loaded playlist in the order the shelf presents them when no
/// filter narrows the view: the saved order once one exists, with the
/// playlists it has not met yet first, otherwise the pinned block and
/// then recency. The saved order is rewritten from this, so it covers
/// the whole library rather than the rows that happened to be visible.
fn full_playlist_order(app: &App) -> Vec<String> {
    let Some(playlists) = app.library.playlists.get() else {
        return Vec::new();
    };
    let mut ordered: Vec<_> = playlists.iter().enumerate().collect();
    if app.settings.sidebar_order.is_empty() {
        let recent = |uri: &str| {
            app.recent_contexts
                .iter()
                .position(|held| held == uri)
                .unwrap_or(usize::MAX)
        };
        let pinned = |uri: &str| {
            app.settings
                .pinned_contexts
                .iter()
                .position(|held| held == uri)
        };
        ordered.sort_by_key(|(index, playlist)| match pinned(&playlist.uri) {
            Some(rank) => (0, rank, 0),
            None => (1, recent(&playlist.uri), *index),
        });
    } else {
        let saved = |uri: &str| {
            app.settings
                .sidebar_order
                .iter()
                .position(|held| held == uri)
        };
        ordered.sort_by_key(|(index, playlist)| match saved(&playlist.uri) {
            Some(rank) => (1, rank, 0),
            None => (0, *index, 0),
        });
    }
    ordered
        .into_iter()
        .map(|(_, playlist)| playlist.uri.clone())
        // Pins live in their own list; the saved order holds the rest.
        .filter(|uri| !app.settings.pinned_contexts.contains(uri))
        .collect()
}

/// Reorders pinned albums, artists, and podcasts. Dropping below the pinned
/// block unpins the row. Liked Songs never moves.
fn drop_row(
    app: &mut App,
    entries: &[Entry],
    liked_rows: usize,
    pinned_rows: usize,
    slot: usize,
    uri: &str,
) {
    let mut pinned = app.settings.pinned_contexts.clone();
    let section_end = liked_rows + pinned_rows;
    if slot <= section_end {
        // The pinned entry the drop lands in front of anchors the new
        // position, so entries pinned from another shelf keep theirs.
        let anchor = entries[liked_rows..section_end]
            .iter()
            .skip(slot - liked_rows)
            .map(|entry| entry.uri.as_str())
            .find(|held| *held != uri)
            .map(str::to_string);
        pinned.retain(|held| held != uri);
        let at = anchor
            .and_then(|anchor| pinned.iter().position(|held| *held == anchor))
            .unwrap_or(pinned.len());
        pinned.insert(at, uri.to_string());
    } else {
        pinned.retain(|held| held != uri);
    }
    if pinned != app.settings.pinned_contexts {
        app.settings.pinned_contexts = pinned;
        app.mark_settings_dirty();
    }
}

/// The purple-to-blue Liked Songs tile.
pub fn liked_cover(ui: &egui::Ui, rect: Rect, radius: f32) {
    let texture_id = egui::Id::new("liked-cover-gradient");
    let texture = ui
        .data(|data| data.get_temp::<egui::TextureHandle>(texture_id))
        .unwrap_or_else(|| {
            let size = 64;
            let lerp = |a: u8, b: u8, t: f32| (a as f32 + (b as f32 - a as f32) * t) as u8;
            let top_left = [0x45, 0x0a, 0xf5];
            let top_right = [0x6a, 0x3a, 0xe8];
            let bottom_left = [0x8e, 0x9f, 0xe5];
            let bottom_right = [0xc4, 0xef, 0xd9];
            let pixels = (0..size)
                .flat_map(|y| {
                    let y = y as f32 / (size - 1) as f32;
                    (0..size).map(move |x| {
                        let x = x as f32 / (size - 1) as f32;
                        egui::Color32::from_rgb(
                            lerp(
                                lerp(top_left[0], top_right[0], x),
                                lerp(bottom_left[0], bottom_right[0], x),
                                y,
                            ),
                            lerp(
                                lerp(top_left[1], top_right[1], x),
                                lerp(bottom_left[1], bottom_right[1], x),
                                y,
                            ),
                            lerp(
                                lerp(top_left[2], top_right[2], x),
                                lerp(bottom_left[2], bottom_right[2], x),
                                y,
                            ),
                        )
                    })
                })
                .collect();
            let texture = ui.ctx().load_texture(
                "liked-cover-gradient",
                egui::ColorImage::new([size, size], pixels),
                egui::TextureOptions::LINEAR,
            );
            ui.data_mut(|data| data.insert_temp(texture_id, texture.clone()));
            texture
        });
    egui::Image::new(&texture)
        .corner_radius(CornerRadius::same(radius.min(127.0) as u8))
        .paint_at(ui, rect);
    let size = rect.width() * 0.45;
    let icon_rect = Rect::from_center_size(rect.center(), Vec2::splat(size));
    Icon::HeartFilled
        .image(egui::Color32::WHITE, size)
        .paint_at(ui, icon_rect);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_body_and_edges_are_distinct_drop_targets() {
        let top = 100.0;
        let height = 56.0;

        assert_eq!(
            split_row_drop_target(top + height / 2.0, top, height, 3, 0, |_| true),
            RowDropTarget::Inside(0)
        );
        assert_eq!(
            split_row_drop_target(top + height - 2.0, top, height, 3, 0, |_| true),
            RowDropTarget::Between(1)
        );
        assert_eq!(
            split_row_drop_target(top + 2.0, top, height, 3, 0, |_| true),
            RowDropTarget::Between(0)
        );
    }

    #[test]
    fn rows_that_cannot_accept_children_fall_back_to_a_gap() {
        assert_eq!(
            split_row_drop_target(128.0, 100.0, 56.0, 3, 1, |_| false),
            RowDropTarget::Between(1)
        );
    }
}
