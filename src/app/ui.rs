use std::path::PathBuf;

use glam::Vec3;

use crate::model::ModelKind;

pub(super) struct InstanceSummary {
    pub id: u64,
    pub name: String,
    pub kind: ModelKind,
    pub position: Vec3,
    pub neighbor: Option<u64>,
}

pub(super) enum UiCommand {
    SpawnExample(ModelKind),
    Remove(u64),
    Load {
        source: PathBuf,
        replace: Option<u64>,
    },
    Reload(u64),
    Move(u64, Vec3),
    ClickThrough(bool),
    Quit,
}

pub(super) struct ControlUi {
    pub selected: Option<u64>,
    pub file_path: String,
    pub click_through: bool,
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
            click_through: true,
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
    pub fn draw(
        &mut self,
        root_ui: &mut egui::Ui,
        instances: &[InstanceSummary],
    ) -> Vec<UiCommand> {
        let mut commands = Vec::new();
        egui::CentralPanel::default().show(root_ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("controls")
                .show(ui, |ui| {
                    ui.heading("Delicium");
                    ui.label("Native 2D / 3D character prototype");
                    ui.separator();
                    if ui
                        .checkbox(&mut self.click_through, "Scene click-through")
                        .changed()
                    {
                        commands.push(UiCommand::ClickThrough(self.click_through));
                    }
                    ui.small("This control window never becomes click-through.");
                    ui.horizontal(|ui| {
                        if ui.button("Add 2D").clicked() {
                            commands.push(UiCommand::SpawnExample(ModelKind::Sprite2d));
                        }
                        if ui.button("Add 3D").clicked() {
                            commands.push(UiCommand::SpawnExample(ModelKind::Skeleton3d));
                        }
                    });
                    ui.separator();
                    ui.label(format!("Characters: {} / 64", instances.len()));
                    egui::ScrollArea::vertical()
                        .id_salt("characters")
                        .max_height(140.0)
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
                            ui.label("Position");
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
                            Some(neighbor) => format!("Responding to character {neighbor}"),
                            None => "No nearby interaction".into(),
                        });
                        ui.horizontal(|ui| {
                            if ui.button("Reload selected").clicked() {
                                commands.push(UiCommand::Reload(instance.id));
                            }
                            if ui.button("Remove selected").clicked() {
                                commands.push(UiCommand::Remove(instance.id));
                            }
                        });
                    }
                    ui.separator();
                    ui.label("Local .dmodel.json path");
                    ui.text_edit_singleline(&mut self.file_path);
                    ui.horizontal(|ui| {
                        let path_present = !self.file_path.trim().is_empty();
                        if ui
                            .add_enabled(path_present, egui::Button::new("Load as new"))
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
                                egui::Button::new("Replace selected"),
                            )
                            .clicked()
                        {
                            commands.push(UiCommand::Load {
                                source: PathBuf::from(self.file_path.trim()),
                                replace: self.selected,
                            });
                        }
                    });
                    ui.small("Dropping a model file on either window adds a character.");
                    ui.separator();
                    ui.label(&self.status);
                    ui.small(format!(
                        "Frame {:.2} ms | tick {} | interactions {} | pending {}",
                        self.frame_ms, self.tick_count, self.interactions, self.pending
                    ));
                    ui.small(&self.adapter);
                    if let Some(error) = &self.last_error {
                        ui.colored_label(egui::Color32::from_rgb(190, 45, 45), error);
                        if ui.button("Dismiss error").clicked() {
                            self.last_error = None;
                        }
                    }
                    ui.separator();
                    if ui.button("Quit").clicked() {
                        commands.push(UiCommand::Quit);
                    }
                });
        });
        commands
    }
}
