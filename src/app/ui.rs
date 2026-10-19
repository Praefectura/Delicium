use std::path::PathBuf;

use glam::Vec3;

use crate::i18n::{self, Text};
use crate::library::LibraryModel;
use crate::model::ModelKind;
use crate::preferences::Resolved;

use super::chat::{Message, Role, session::ChatSession};

pub(super) struct InstanceSummary {
    pub id: u64,
    pub name: String,
    pub kind: ModelKind,
    pub position: Vec3,
    pub neighbor: Option<u64>,
}

pub(super) enum UiCommand {
    SpawnExample(ModelKind),
    /// Add a character from the library by id.
    AddLibrary(String),
    Remove(u64),
    Load {
        source: PathBuf,
        replace: Option<u64>,
    },
    Reload(u64),
    Move(u64, Vec3),
    /// Enable or disable per-character hit regions (default on).
    PixelHitTest(bool),
    /// Enable or disable autonomous wandering.
    Wander(bool),
    /// Send one user message to a character.
    ChatSend {
        instance: u64,
        text: String,
    },
    /// Forget one character's conversation.
    ChatClear(u64),
    /// Clear the last chat error.
    ChatDismiss,
    Quit,
}

pub(super) struct ControlUi {
    pub selected: Option<u64>,
    pub file_path: String,
    /// Whether the scene window intercepts input only over character regions
    /// (`true`, the default) or over the whole scene window.
    pub pixel_hittest: bool,
    /// Whether characters walk around on their own.
    pub wander: bool,
    /// The text being composed for the selected character.
    pub chat_input: String,
    /// The interface language; every label is looked up through it.
    pub language: Resolved,
    pub last_error: Option<String>,
    pub status: String,
    pub adapter: String,
    pub frame_ms: f32,
    pub tick_count: u64,
    pub interactions: usize,
    pub pending: usize,
}

impl Default for ControlUi {
    fn default() -> Self {
        Self {
            selected: None,
            file_path: String::new(),
            pixel_hittest: true,
            wander: true,
            chat_input: String::new(),
            language: Resolved::En,
            last_error: None,
            status: "Loading bundled models...".into(),
            adapter: String::new(),
            frame_ms: 0.0,
            tick_count: 0,
            interactions: 0,
            pending: 0,
        }
    }
}

impl ControlUi {
    /// The text table for the current language.
    fn text(&self) -> &'static Text {
        i18n::text(self.language)
    }

    pub fn set_language(&mut self, language: Resolved) {
        self.language = language;
    }

    pub fn draw(
        &mut self,
        root_ui: &mut egui::Ui,
        instances: &[InstanceSummary],
        library: &[LibraryModel],
        chat: &ChatSession,
    ) -> Vec<UiCommand> {
        let text = self.text();
        let mut commands = Vec::new();
        egui::CentralPanel::default().show(root_ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("controls")
                .show(ui, |ui| {
                    ui.heading("Delicium");
                    ui.label(text.controls_subtitle);
                    ui.separator();
                    let response = ui
                        .checkbox(&mut self.pixel_hittest, text.hit_regions)
                        .on_hover_text(text.hit_regions_hint);
                    if response.changed() {
                        commands.push(UiCommand::PixelHitTest(self.pixel_hittest));
                    }
                    ui.small(text.hit_regions_detail);
                    let wander = ui
                        .checkbox(&mut self.wander, text.wander)
                        .on_hover_text(text.wander_hint);
                    if wander.changed() {
                        commands.push(UiCommand::Wander(self.wander));
                    }
                    ui.horizontal(|ui| {
                        if ui.button(text.add_2d).clicked() {
                            commands.push(UiCommand::SpawnExample(ModelKind::Sprite2d));
                        }
                        if ui.button(text.add_3d).clicked() {
                            commands.push(UiCommand::SpawnExample(ModelKind::Skeleton3d));
                        }
                    });
                    ui.separator();
                    ui.label(format!("{}: {} / 64", text.characters, instances.len()));
                    egui::ScrollArea::vertical()
                        .id_salt("characters")
                        .max_height(120.0)
                        .show(ui, |ui| {
                            for instance in instances {
                                let label = format!(
                                    "{}: {} ({:?})",
                                    instance.id, instance.name, instance.kind
                                );
                                if ui
                                    .selectable_label(self.selected == Some(instance.id), label)
                                    .clicked()
                                {
                                    self.selected = Some(instance.id);
                                }
                            }
                        });
                    if let Some(instance) = instances
                        .iter()
                        .find(|instance| Some(instance.id) == self.selected)
                    {
                        let mut position = instance.position;
                        let mut changed = false;
                        ui.horizontal(|ui| {
                            ui.label(text.position);
                            changed |= ui
                                .add(
                                    egui::DragValue::new(&mut position.x)
                                        .speed(0.05)
                                        .prefix("x "),
                                )
                                .changed();
                            changed |= ui
                                .add(
                                    egui::DragValue::new(&mut position.y)
                                        .speed(0.05)
                                        .prefix("y "),
                                )
                                .changed();
                        });
                        if changed {
                            commands.push(UiCommand::Move(instance.id, position));
                        }
                        ui.label(match instance.neighbor {
                            Some(neighbor) => format!("{} {neighbor}", text.responding_to),
                            None => text.no_interaction.into(),
                        });
                        ui.horizontal(|ui| {
                            if ui.button(text.reload_selected).clicked() {
                                commands.push(UiCommand::Reload(instance.id));
                            }
                            if ui.button(text.remove_selected).clicked() {
                                commands.push(UiCommand::Remove(instance.id));
                            }
                        });
                    }
                    ui.separator();
                    self.draw_library(ui, text, library, &mut commands);
                    ui.separator();
                    ui.label(text.local_path);
                    ui.text_edit_singleline(&mut self.file_path);
                    ui.horizontal(|ui| {
                        let path_present = !self.file_path.trim().is_empty();
                        if ui
                            .add_enabled(path_present, egui::Button::new(text.load_as_new))
                            .clicked()
                        {
                            commands.push(UiCommand::Load {
                                source: PathBuf::from(self.file_path.trim()),
                                replace: None,
                            });
                        }
                        if ui
                            .add_enabled(
                                path_present && self.selected.is_some(),
                                egui::Button::new(text.replace_selected),
                            )
                            .clicked()
                        {
                            commands.push(UiCommand::Load {
                                source: PathBuf::from(self.file_path.trim()),
                                replace: self.selected,
                            });
                        }
                    });
                    ui.small(text.drop_hint);
                    ui.separator();
                    self.draw_chat(ui, text, instances, chat, &mut commands);
                    ui.separator();
                    ui.label(&self.status);
                    ui.small(
                        text.frame_stats
                            .replace("{frame:.2}", &format!("{:.2}", self.frame_ms))
                            .replace("{tick}", &self.tick_count.to_string())
                            .replace("{interactions}", &self.interactions.to_string())
                            .replace("{pending}", &self.pending.to_string()),
                    );
                    ui.small(&self.adapter);
                    if let Some(error) = &self.last_error {
                        ui.colored_label(egui::Color32::from_rgb(190, 45, 45), error);
                        if ui.button(text.dismiss_error).clicked() {
                            self.last_error = None;
                        }
                    }
                    ui.separator();
                    if ui.button(text.quit).clicked() {
                        commands.push(UiCommand::Quit);
                    }
                });
        });
        commands
    }

    /// The model library: one button per entry, bundled first.
    fn draw_library(
        &mut self,
        ui: &mut egui::Ui,
        text: &'static Text,
        library: &[LibraryModel],
        commands: &mut Vec<UiCommand>,
    ) {
        ui.label(text.library);
        egui::ScrollArea::vertical()
            .id_salt("library")
            .max_height(110.0)
            .show(ui, |ui| {
                for model in library {
                    ui.horizontal(|ui| {
                        let is_bundled = matches!(model.source, crate::library::Source::Bundled);
                        let label = if is_bundled {
                            model.name.clone()
                        } else {
                            format!("{} [{}]", model.name, text.library_user)
                        };
                        ui.label(label);
                        if ui.button(text.add_from_library).clicked() {
                            commands.push(UiCommand::AddLibrary(model.id.clone()));
                        }
                    });
                }
            });
        if library.len() <= crate::library::BUILTIN.len() {
            ui.small(text.library_hint);
        }
    }

    /// The conversation panel for the selected character.
    fn draw_chat(
        &mut self,
        ui: &mut egui::Ui,
        text: &'static Text,
        instances: &[InstanceSummary],
        chat: &ChatSession,
        commands: &mut Vec<UiCommand>,
    ) {
        ui.label(text.chat);
        let Some(target) = instances
            .iter()
            .find(|instance| Some(instance.id) == self.selected)
        else {
            ui.small(text.chat_select_target);
            return;
        };
        ui.small(format!(
            "{}: {} ({})",
            text.chat_target,
            target.name,
            match chat.language() {
                Resolved::ZhHans => "中文",
                Resolved::En => "English",
            }
        ));
        let history: &[Message] = chat.history(target.id);
        egui::ScrollArea::vertical()
            .id_salt("chat_log")
            .max_height(150.0)
            .show(ui, |ui| {
                if history.is_empty() {
                    ui.small(text.chat_empty);
                }
                for message in history {
                    let speaker = match message.role {
                        Role::User => text.chat_you.to_owned(),
                        Role::Assistant => target.name.clone(),
                        Role::System => text.chat_system.to_owned(),
                    };
                    ui.label(
                        egui::RichText::new(format!("{speaker}: {}", message.content)).small(),
                    );
                }
            });
        let busy = chat.inflight() == Some(target.id);
        if busy {
            ui.small(text.chat_waiting);
        } else if chat.remote_ready() {
            ui.small(text.chat_remote);
        } else {
            ui.small(text.chat_disabled);
        }
        if let Some(error) = chat.last_error() {
            ui.colored_label(egui::Color32::from_rgb(190, 45, 45), error);
        }
        let mut submit = false;
        ui.horizontal(|ui| {
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.chat_input)
                    .hint_text(text.chat_placeholder)
                    .desired_width(220.0),
            );
            submit |=
                response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
            let has_text = !self.chat_input.trim().is_empty();
            submit |= ui
                .add_enabled(!busy && has_text, egui::Button::new(text.chat_send))
                .clicked();
        });
        if submit && !self.chat_input.trim().is_empty() {
            commands.push(UiCommand::ChatSend {
                instance: target.id,
                text: std::mem::take(&mut self.chat_input),
            });
        }
        ui.horizontal(|ui| {
            if ui.button(text.chat_clear).clicked() {
                commands.push(UiCommand::ChatClear(target.id));
            }
            if chat.last_error().is_some() && ui.button(text.dismiss_error).clicked() {
                commands.push(UiCommand::ChatDismiss);
            }
        });
    }
}
