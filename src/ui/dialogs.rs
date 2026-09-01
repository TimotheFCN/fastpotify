//! Modal dialogs: playlist details, confirmations, shortcuts.

use egui::{Align, CornerRadius, Frame, Layout, Margin, Stroke};

use crate::app::App;
use crate::model::{Action, Dialog};
use crate::theme;

pub fn show(app: &mut App, ctx: &egui::Context) {
    let Some(dialog) = app.dialog.clone() else {
        return;
    };
    let palette = app.palette;
    let frame = Frame::new()
        .fill(palette.overlay)
        .stroke(Stroke::new(1.0, palette.outline))
        .corner_radius(CornerRadius::same(theme::RADIUS + 4))
        .inner_margin(Margin::same(24))
        .shadow(egui::epaint::Shadow {
            offset: [0, 10],
            blur: 40,
            spread: 0,
            color: palette.shadow,
        });
    let response = egui::Modal::new(egui::Id::new("dialog"))
        .frame(frame)
        .backdrop_color(egui::Color32::from_black_alpha(if palette.dark { 150 } else { 80 }))
        .show(ctx, |ui| {
            ui.set_width(420.0);
            match dialog {
                Dialog::CreatePlaylist { .. } => create_playlist(app, ui),
                Dialog::EditFolder { .. } => edit_folder(app, ui),
                Dialog::MoveToFolder { .. } => move_to_folder(app, ui),
                Dialog::ConfirmDeleteFolder {
                    folder,
                    name,
                    playlist_count,
                    can_delete_contents,
                } => delete_folder(
                    app,
                    ui,
                    folder,
                    &name,
                    playlist_count,
                    can_delete_contents,
                ),
                Dialog::EditPlaylist { .. } => edit_playlist(app, ui),
                Dialog::ConfirmDeletePlaylist { id, name, owned } => {
                    theme::text(ui, if owned { "Delete playlist?" } else { "Remove from Your Library?" }, theme::bold(20.0), palette.text);
                    ui.add_space(8.0);
                    let body = if owned {
                        format!("This will delete “{name}” from Your Library. Spotify keeps deleted playlists recoverable for 90 days.")
                    } else {
                        format!("“{name}” will no longer appear in Your Library.")
                    };
                    ui.add(egui::Label::new(egui::RichText::new(body).font(theme::regular(14.0)).color(palette.secondary)).wrap());
                    ui.add_space(20.0);
                    dialog_footer(ui, |ui| {
                        if theme::pill_button(ui, &palette, if owned { "Delete" } else { "Remove" }, true).clicked() {
                            app.actions.push(Action::DeletePlaylist(id.clone()));
                        }
                        if theme::pill_button(ui, &palette, "Cancel", false).clicked() {
                            app.actions.push(Action::CloseDialog);
                        }
                    });
                }
                Dialog::Shortcuts => {
                    theme::text(ui, "Keyboard shortcuts", theme::bold(20.0), palette.text);
                    ui.add_space(12.0);
                    // `theme::text` truncates, which in a grid makes each cell
                    // claim almost no width and turns "Ctrl+Shift+A" into
                    // "Ctrl…". A shortcut is unusable when abbreviated, so
                    // these cells are sized to their content.
                    let cell = |ui: &mut egui::Ui, text: &str, font: egui::FontId, color| {
                        ui.add(
                            egui::Label::new(egui::RichText::new(text).font(font).color(color))
                                .extend()
                                .selectable(false),
                        );
                    };
                    egui::Grid::new("shortcuts")
                        .num_columns(2)
                        .spacing([24.0, 8.0])
                        .show(ui, |ui| {
                            for (keys, description) in super::keys::SHORTCUTS {
                                cell(ui, keys, theme::semibold(13.0), palette.text);
                                cell(ui, description, theme::regular(13.5), palette.secondary);
                                ui.end_row();
                            }
                        });
                    ui.add_space(16.0);
                    dialog_footer(ui, |ui| {
                        if theme::pill_button(ui, &palette, "Done", true).clicked() {
                            app.actions.push(Action::CloseDialog);
                        }
                    });
                }
                Dialog::PremiumNeeded => {
                    theme::text(
                        ui,
                        "This account cannot play music here",
                        theme::bold(20.0),
                        palette.text,
                    );
                    ui.add_space(8.0);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(
                                "Spotify only lets Premium accounts play through another \
                                 app, on this computer or on any other device. With a Free \
                                 account Fastpotify can show your library and search, but \
                                 play, pause, and skip will not work.",
                            )
                            .font(theme::regular(14.0))
                            .color(palette.secondary),
                        )
                        .wrap(),
                    );
                    ui.add_space(20.0);
                    dialog_footer(ui, |ui| {
                        if theme::pill_button(ui, &palette, "OK", true).clicked() {
                            app.actions.push(Action::CloseDialog);
                        }
                    });
                }
            }
        });
    if response.should_close() {
        app.actions.push(Action::CloseDialog);
    }
}

fn dialog_footer(ui: &mut egui::Ui, content: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), content);
    });
}

fn edit_folder(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let Some(Dialog::EditFolder {
        folder,
        parent,
        name,
    }) = &mut app.dialog
    else {
        return;
    };
    theme::text(
        ui,
        if folder.is_some() {
            "Rename folder"
        } else {
            "New folder"
        },
        theme::bold(20.0),
        palette.text,
    );
    ui.add_space(12.0);
    theme::text(ui, "Name", theme::medium(13.0), palette.secondary);
    let field = text_field(ui, &palette, "folder-name", name, "Folder name", true);
    ui.add_space(20.0);
    let submit = field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
    let name = name.trim().to_string();
    let folder = *folder;
    let parent = *parent;
    dialog_footer(ui, |ui| {
        let label = if folder.is_some() { "Rename" } else { "Create" };
        let clicked = ui
            .scope(|ui| {
                if name.is_empty() {
                    ui.set_opacity(0.5);
                }
                theme::pill_button(ui, &palette, label, true).clicked()
            })
            .inner;
        if (clicked || submit) && !name.is_empty() {
            let intent = match folder {
                Some(folder) => crate::rootlist::Intent::RenameFolder {
                    folder,
                    name: name.clone(),
                },
                None => crate::rootlist::Intent::create_folder(parent, name.clone()),
            };
            app.actions.push(Action::ChangeFolders(intent));
            app.actions.push(Action::CloseDialog);
        }
        if theme::pill_button(ui, &palette, "Cancel", false).clicked() {
            app.actions.push(Action::CloseDialog);
        }
    });
}

fn move_to_folder(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let Some(Dialog::MoveToFolder {
        node,
        current,
        query,
    }) = &mut app.dialog
    else {
        return;
    };
    let node = node.clone();
    let current = *current;
    theme::text(ui, "Move to folder", theme::bold(20.0), palette.text);
    ui.add_space(12.0);
    let folders = app
        .folders
        .confirmed_snapshot()
        .and_then(|snapshot| snapshot.valid_folder_destinations(&node).ok())
        .unwrap_or_default();
    if folders.len() > 6 || !query.is_empty() {
        super::widgets::search_field(
            ui,
            &palette,
            egui::Id::new("folder-destination-search"),
            query,
            "Find a folder",
            ui.available_width(),
        );
        ui.add_space(10.0);
    }
    let needle = query.trim().to_lowercase();
    let visible = visible_folder_destinations(&folders, &needle);
    let mut choice = None;
    egui::ScrollArea::vertical()
        .max_height(320.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            if folder_destination_row(
                ui,
                &palette,
                crate::theme::Icon::House,
                "Root",
                0,
                current.is_none(),
            )
            .clicked()
            {
                choice = Some(None);
            }
            for folder in folders.iter().filter(|folder| visible.contains(&folder.id)) {
                let name = if folder.name.is_empty() {
                    "Folder"
                } else {
                    folder.name.as_str()
                };
                if folder_destination_row(
                    ui,
                    &palette,
                    crate::theme::Icon::Folder,
                    name,
                    folder.depth.saturating_add(1),
                    current == Some(folder.id),
                )
                .clicked()
                {
                    choice = Some(Some(folder.id));
                }
            }
            if visible.is_empty() {
                ui.add_space(12.0);
                theme::subtle(ui, &palette, "No folders found.");
                ui.add_space(12.0);
            }
        });
    if let Some(parent) = choice {
        app.actions
            .push(Action::ChangeFolders(crate::rootlist::Intent::Move {
                node,
                parent,
                before: None,
            }));
        app.actions.push(Action::CloseDialog);
    }
    ui.add_space(16.0);
    dialog_footer(ui, |ui| {
        if theme::pill_button(ui, &palette, "Cancel", false).clicked() {
            app.actions.push(Action::CloseDialog);
        }
    });
}

fn visible_folder_destinations(
    folders: &[crate::rootlist::Folder],
    needle: &str,
) -> std::collections::HashSet<crate::rootlist::FolderId> {
    if needle.is_empty() {
        return folders.iter().map(|folder| folder.id).collect();
    }
    let parents = folders
        .iter()
        .map(|folder| (folder.id, folder.parent))
        .collect::<std::collections::HashMap<_, _>>();
    let mut visible = std::collections::HashSet::new();
    for folder in folders
        .iter()
        .filter(|folder| folder.name.to_lowercase().contains(needle))
    {
        let mut id = Some(folder.id);
        while let Some(folder) = id {
            if !visible.insert(folder) {
                break;
            }
            id = parents.get(&folder).copied().flatten();
        }
    }
    visible
}

fn folder_destination_row(
    ui: &mut egui::Ui,
    palette: &theme::Palette,
    icon: crate::theme::Icon,
    name: &str,
    depth: u8,
    current: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 38.0),
        if current {
            egui::Sense::hover()
        } else {
            egui::Sense::click()
        },
    );
    if ui.is_rect_visible(rect) {
        let fill = if current {
            palette.accent.gamma_multiply(0.12)
        } else if response.hovered() {
            palette.surface_hover
        } else {
            egui::Color32::TRANSPARENT
        };
        ui.painter().rect_filled(rect, CornerRadius::same(6), fill);
        if current {
            ui.painter().rect_stroke(
                rect,
                CornerRadius::same(6),
                Stroke::new(1.0, palette.accent.gamma_multiply(0.7)),
                egui::StrokeKind::Inside,
            );
        }
        let depth = f32::from(depth.min(12));
        let guide_left = rect.left() + 16.0;
        for level in 0..depth as usize {
            let x = guide_left + level as f32 * 18.0;
            ui.painter()
                .vline(x, rect.y_range(), Stroke::new(1.0, palette.outline));
        }
        let icon_center = egui::pos2(guide_left + depth * 18.0, rect.center().y);
        if depth > 0.0 {
            ui.painter().hline(
                icon_center.x - 18.0..=icon_center.x - 9.0,
                icon_center.y,
                Stroke::new(1.0, palette.outline),
            );
        }
        icon.image(
            if current {
                palette.accent
            } else {
                palette.secondary
            },
            17.0,
        )
        .paint_at(
            ui,
            egui::Rect::from_center_size(icon_center, egui::Vec2::splat(17.0)),
        );
        let text_left = icon_center.x + 14.0;
        let text_right = rect.right() - if current { 36.0 } else { 12.0 };
        let painter = ui.painter().with_clip_rect(egui::Rect::from_min_max(
            egui::pos2(text_left, rect.top()),
            egui::pos2(text_right, rect.bottom()),
        ));
        crate::bidi::paint_line(
            &painter,
            text_left,
            text_right,
            rect.center().y,
            name,
            theme::medium(13.5),
            palette.text,
        );
        if current {
            crate::theme::Icon::Check
                .image(palette.accent, 16.0)
                .paint_at(
                    ui,
                    egui::Rect::from_center_size(
                        egui::pos2(rect.right() - 16.0, rect.center().y),
                        egui::Vec2::splat(16.0),
                    ),
                );
        }
    }
    if current {
        response
    } else {
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    }
}

fn delete_folder(
    app: &mut App,
    ui: &mut egui::Ui,
    folder: crate::rootlist::FolderId,
    name: &str,
    playlist_count: usize,
    can_delete_contents: bool,
) {
    let palette = app.palette;
    theme::text(
        ui,
        format!("Delete \u{201c}{name}\u{201d}?"),
        theme::bold(20.0),
        palette.text,
    );
    ui.add_space(8.0);
    ui.add(
        egui::Label::new(
            egui::RichText::new("Choose what happens to the playlists inside this folder.")
                .font(theme::regular(14.0))
                .color(palette.secondary),
        )
        .wrap(),
    );
    ui.add_space(16.0);
    let keep = ui
        .add_sized(
            [ui.available_width(), 38.0],
            egui::Button::new(
                egui::RichText::new("Delete folder and keep playlists")
                    .font(theme::semibold(13.0))
                    .color(palette.text),
            )
            .fill(palette.surface)
            .stroke(Stroke::new(1.0, palette.outline))
            .corner_radius(CornerRadius::same(8)),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if keep.clicked() {
        app.actions.push(Action::ChangeFolders(
            crate::rootlist::Intent::DeleteFolder {
                folder,
                contents: false,
            },
        ));
        app.actions.push(Action::CloseDialog);
    }
    if can_delete_contents {
        ui.add_space(12.0);
        theme::text(
            ui,
            format!(
                "This also removes {playlist_count} playlist{} from Your Library.",
                if playlist_count == 1 { "" } else { "s" }
            ),
            theme::regular(12.5),
            palette.danger,
        );
        ui.add_space(6.0);
        let delete = ui
            .add_sized(
                [ui.available_width(), 38.0],
                egui::Button::new(
                    egui::RichText::new("Delete folder and contents")
                        .font(theme::semibold(13.0))
                        .color(palette.danger),
                )
                .fill(palette.danger.gamma_multiply(0.08))
                .stroke(Stroke::new(1.0, palette.danger.gamma_multiply(0.7)))
                .corner_radius(CornerRadius::same(8)),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if delete.clicked() {
            app.actions.push(Action::ChangeFolders(
                crate::rootlist::Intent::DeleteFolder {
                    folder,
                    contents: true,
                },
            ));
            app.actions.push(Action::CloseDialog);
        }
    } else {
        ui.add_space(8.0);
        ui.add(
            egui::Label::new(
                egui::RichText::new(
                    "Some of this folder's contents aren't loaded yet, \
                     so only the folder itself can be deleted from here.",
                )
                .font(theme::regular(12.5))
                .color(palette.secondary),
            )
            .wrap(),
        );
    }
    ui.add_space(16.0);
    dialog_footer(ui, |ui| {
        if theme::pill_button(ui, &palette, "Cancel", false).clicked() {
            app.actions.push(Action::CloseDialog);
        }
    });
}

fn text_field(
    ui: &mut egui::Ui,
    palette: &theme::Palette,
    id: &str,
    text: &mut String,
    hint: &str,
    focus: bool,
) -> egui::Response {
    let response = Frame::new()
        .fill(palette.surface)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.add(
                egui::TextEdit::singleline(text)
                    .id(egui::Id::new(id))
                    .hint_text(egui::RichText::new(hint).color(palette.dim))
                    .font(theme::regular(14.0))
                    .frame(egui::Frame::NONE)
                    .desired_width(f32::INFINITY),
            )
        })
        .inner;
    if focus && ui.memory(|memory| memory.focused().is_none()) {
        response.request_focus();
    }
    response
}

fn create_playlist(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let busy = app.playlist_busy;
    let Some(Dialog::CreatePlaylist {
        name,
        public,
        add_uris,
        destination,
    }) = &mut app.dialog
    else {
        return;
    };
    theme::text(ui, "New playlist", theme::bold(20.0), palette.text);
    ui.add_space(12.0);
    theme::text(ui, "Name", theme::medium(13.0), palette.secondary);
    let field = text_field(ui, &palette, "playlist-name", name, "My playlist", true);
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        super::widgets::switch(ui, &palette, public);
        theme::text(ui, "Public playlist", theme::regular(14.0), palette.text);
    });
    if !add_uris.is_empty() {
        ui.add_space(6.0);
        let count = add_uris.len();
        theme::text(
            ui,
            format!(
                "{count} song{} will be added.",
                if count == 1 { "" } else { "s" }
            ),
            theme::regular(13.0),
            palette.secondary,
        );
    }
    ui.add_space(20.0);
    let submit = field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
    let name_value = name.trim().to_string();
    let public_value = *public;
    let uris = add_uris.clone();
    let destination = *destination;
    dialog_footer(ui, |ui| {
        if busy {
            theme::spinner(ui, 18.0, palette.accent);
        } else {
            let clicked = ui
                .scope(|ui| {
                    if name_value.is_empty() {
                        ui.set_opacity(0.5);
                    }
                    theme::pill_button(ui, &palette, "Create", true).clicked()
                })
                .inner;
            if (clicked || submit) && !name_value.is_empty() {
                app.actions.push(Action::CreatePlaylist {
                    name: name_value.clone(),
                    public: public_value,
                    add_uris: uris.clone(),
                    destination,
                });
            }
            if theme::pill_button(ui, &palette, "Cancel", false).clicked() {
                app.actions.push(Action::CloseDialog);
            }
        }
    });
}

fn edit_playlist(app: &mut App, ui: &mut egui::Ui) {
    let palette = app.palette;
    let busy = app.playlist_busy;
    let Some(Dialog::EditPlaylist {
        id,
        name,
        description,
        public,
    }) = &mut app.dialog
    else {
        return;
    };
    theme::text(ui, "Edit details", theme::bold(20.0), palette.text);
    ui.add_space(12.0);
    theme::text(ui, "Name", theme::medium(13.0), palette.secondary);
    text_field(ui, &palette, "edit-name", name, "Playlist name", true);
    ui.add_space(10.0);
    theme::text(ui, "Description", theme::medium(13.0), palette.secondary);
    Frame::new()
        .fill(palette.surface)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(description)
                    .id(egui::Id::new("edit-description"))
                    .hint_text(
                        egui::RichText::new("Add an optional description").color(palette.dim),
                    )
                    .font(theme::regular(14.0))
                    .frame(egui::Frame::NONE)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY),
            );
        });
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        super::widgets::switch(ui, &palette, public);
        theme::text(ui, "Public playlist", theme::regular(14.0), palette.text);
    });
    ui.add_space(20.0);
    let id = id.clone();
    let name_value = name.trim().to_string();
    let description_value = description.trim().to_string();
    let public_value = *public;
    dialog_footer(ui, |ui| {
        if busy {
            theme::spinner(ui, 18.0, palette.accent);
        } else {
            if theme::pill_button(ui, &palette, "Save", true).clicked() && !name_value.is_empty() {
                app.actions.push(Action::UpdatePlaylist {
                    id: id.clone(),
                    name: name_value.clone(),
                    description: description_value.clone(),
                    public: public_value,
                });
            }
            if theme::pill_button(ui, &palette, "Cancel", false).clicked() {
                app.actions.push(Action::CloseDialog);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_search_keeps_matching_paths_visible() {
        let parent = crate::rootlist::Folder {
            id: "1".parse().unwrap(),
            name: "Work".into(),
            depth: 0,
            parent: None,
        };
        let child = crate::rootlist::Folder {
            id: "2".parse().unwrap(),
            name: "Research".into(),
            depth: 1,
            parent: Some(parent.id),
        };
        let other = crate::rootlist::Folder {
            id: "3".parse().unwrap(),
            name: "Weekend".into(),
            depth: 0,
            parent: None,
        };

        let visible =
            visible_folder_destinations(&[parent.clone(), child.clone(), other], "search");

        assert_eq!(visible, [parent.id, child.id].into_iter().collect());
    }
}
