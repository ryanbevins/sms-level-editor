use super::*;
use crate::camera::CameraProjection;

const TABLES_PATH: &[u8] = b"map/tables.bin";
const AUDIO_POINT_COLOR: egui::Color32 = egui::Color32::from_rgb(73, 205, 255);
const AUDIO_INNER_COLOR: egui::Color32 = egui::Color32::from_rgb(108, 245, 171);
const AUDIO_CHANGE_COLOR: egui::Color32 = egui::Color32::from_rgb(196, 111, 255);
const AUDIO_EFFECT_COLOR: egui::Color32 = egui::Color32::from_rgb(255, 157, 73);

#[derive(Debug, Clone, PartialEq)]
pub(super) struct AudioHelper {
    pub(super) id: String,
    pub(super) label: String,
    pub(super) kind: AudioHelperKind,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum AudioHelperKind {
    Point {
        object_id: String,
        actor_name: String,
        original_sound_id: u32,
        sound_id: u32,
        category: Option<AudioAttenuation>,
        position: [f32; 3],
    },
    Rail {
        object_id: String,
        graph_id: Option<String>,
        graph_name: String,
        original_sound_id: u32,
        sound_id: u32,
        category: Option<AudioAttenuation>,
        points: Vec<[f32; 3]>,
    },
    Cube {
        record_path: Vec<usize>,
        manager_kind: sms_schema::CubeManagerKind,
        manager_factory: String,
        table_name: String,
        cube: AudioCube,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AudioAttenuation {
    pub(super) category: u8,
    pub(super) full_volume_distance: u32,
    pub(super) max_distance: u32,
    pub(super) fixed_curve_range: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct AudioCube {
    pub(super) center: [f32; 3],
    pub(super) rotation_degrees: [f32; 3],
    pub(super) dimensions: [f32; 3],
    pub(super) flags: u32,
    pub(super) data_no: i32,
}

impl SmsEditorApp {
    pub(super) fn audio_helpers(&self) -> Vec<AudioHelper> {
        let Some(document) = self.document.as_ref() else {
            return Vec::new();
        };
        let Some(registry) = document.registry.as_ref().or(self.registry.as_ref()) else {
            return Vec::new();
        };
        let attenuation_for = |sound_id: u32| {
            let category = ((sound_id >> 12) & 0x0f) as u8;
            registry
                .sound_attenuation
                .categories
                .iter()
                .find(|definition| definition.category == category)
                .map(|definition| AudioAttenuation {
                    category,
                    full_volume_distance: registry.sound_attenuation.full_volume_distance,
                    max_distance: definition.max_distance,
                    fixed_curve_range: definition.fixed_curve_range,
                })
        };

        let mut helpers = Vec::new();
        for object in &document.objects {
            if let Some(resource_name) = object.raw_param("actor_tail_string") {
                if let Some((sound_id, actor_name)) = registry
                    .map_static_models
                    .iter()
                    .find(|definition| definition.actor_name == resource_name)
                    .and_then(|definition| {
                        definition
                            .sound_id
                            .map(|sound_id| (sound_id, definition.actor_name.as_str()))
                    })
                {
                    let effective_sound_id = self.effective_helper_sound(
                        ProjectSoundAssignmentKind::MapStatic,
                        actor_name,
                        sound_id,
                    );
                    helpers.push(AudioHelper {
                        id: format!("audio:point:{}", object.id),
                        label: format!(
                            "{actor_name} - {}",
                            self.sound_display_name(effective_sound_id)
                        ),
                        kind: AudioHelperKind::Point {
                            object_id: object.id.clone(),
                            actor_name: actor_name.to_string(),
                            original_sound_id: sound_id,
                            sound_id: effective_sound_id,
                            category: attenuation_for(effective_sound_id),
                            position: object.transform.translation,
                        },
                    });
                }
            }

            let Some(graph_name) = object.raw_param("graph_name") else {
                continue;
            };
            let Some(definition) = registry
                .graph_sound_emitters
                .iter()
                .find(|definition| definition.graph_name == graph_name)
            else {
                continue;
            };
            let graph = document
                .route_authoring
                .as_ref()
                .and_then(|routes| routes.graph_by_name(graph_name));
            let effective_sound_id = self.effective_helper_sound(
                ProjectSoundAssignmentKind::Graph,
                graph_name,
                definition.sound_id,
            );
            helpers.push(AudioHelper {
                id: format!("audio:rail:{}", object.id),
                label: format!(
                    "{graph_name} - {}",
                    self.sound_display_name(effective_sound_id)
                ),
                kind: AudioHelperKind::Rail {
                    object_id: object.id.clone(),
                    graph_id: graph.map(|graph| graph.id.clone()),
                    graph_name: graph_name.to_string(),
                    original_sound_id: definition.sound_id,
                    sound_id: effective_sound_id,
                    category: attenuation_for(effective_sound_id),
                    points: graph
                        .map(|graph| {
                            graph
                                .controls
                                .iter()
                                .map(|control| control.node.position.map(f32::from))
                                .collect()
                        })
                        .unwrap_or_default(),
                },
            });
        }

        helpers.extend(self.audio_cube_helpers_cache.iter().cloned());
        helpers
    }

    pub(super) fn rebuild_audio_cube_helpers_cache(&mut self) {
        self.audio_cube_helpers_cache.clear();
        let sound_change_assignment = self
            .current_stage_music()
            .and_then(|music| music.secondary_bgm_id)
            .or_else(|| {
                let stage_id = self.document.as_ref()?.stage_id.as_str();
                self.retail_stage_audio
                    .iter()
                    .find(|profile| profile.stage_id.eq_ignore_ascii_case(stage_id))?
                    .secondary_bgm_id
            })
            .map(|bgm_id| self.music_display_name(bgm_id));
        let Some(document) = self.document.as_ref() else {
            return;
        };
        let Some(registry) = document.registry.as_ref().or(self.registry.as_ref()) else {
            return;
        };
        let audio_tables = registry.cube_managers.iter().filter(|manager| {
            matches!(
                manager.kind,
                sms_schema::CubeManagerKind::SoundChange | sms_schema::CubeManagerKind::SoundEffect
            )
        });
        let Ok(Some(StageResourceDocument::Placement(tables))) =
            document.effective_resource_clone(TABLES_PATH)
        else {
            return;
        };
        for manager in audio_tables {
            let mut table_paths = Vec::new();
            collect_record_paths(&tables.root, &mut Vec::new(), &mut table_paths, &|record| {
                semantic_type(&record.type_name) == "CubeGeneralInfoTable"
                    && record.name == manager.table_name
            });
            for table_path in table_paths {
                let Some(table) = record_at(&tables.root, &table_path) else {
                    continue;
                };
                let mut cube_paths = Vec::new();
                collect_record_paths(table, &mut table_path.clone(), &mut cube_paths, &|record| {
                    semantic_type(&record.type_name) == "CubeGeneralInfo"
                });
                for path in cube_paths {
                    let Some(cube) =
                        record_at(&tables.root, &path).and_then(audio_cube_from_record)
                    else {
                        continue;
                    };
                    let index = self.audio_cube_helpers_cache.len() + 1;
                    self.audio_cube_helpers_cache.push(AudioHelper {
                        id: format!(
                            "audio:cube:{}:{}",
                            manager.runtime_global,
                            path.iter()
                                .map(usize::to_string)
                                .collect::<Vec<_>>()
                                .join(".")
                        ),
                        label: match manager.kind {
                            sms_schema::CubeManagerKind::SoundChange => format!(
                                "{} {} - {}",
                                manager.factory_name,
                                index,
                                sound_change_assignment
                                    .as_deref()
                                    .unwrap_or("No inside track")
                            ),
                            _ => format!(
                                "{} {} - data {}",
                                manager.factory_name, index, cube.data_no
                            ),
                        },
                        kind: AudioHelperKind::Cube {
                            record_path: path,
                            manager_kind: manager.kind,
                            manager_factory: manager.factory_name.clone(),
                            table_name: manager.table_name.clone(),
                            cube,
                        },
                    });
                }
            }
        }
    }

    pub(super) fn audio_helpers_hierarchy(&mut self, ui: &mut egui::Ui) {
        let helpers = self.audio_helpers();
        if helpers.is_empty() {
            return;
        }
        egui::CollapsingHeader::new(format!("Audio Helpers ({})", helpers.len()))
            .default_open(true)
            .show(ui, |ui| {
                for helper in helpers {
                    let selected = self.selected_audio_helper_id.as_deref() == Some(&helper.id);
                    if ui.selectable_label(selected, &helper.label).clicked() {
                        self.select_audio_helper(&helper);
                    }
                }
            });
    }

    fn select_audio_helper(&mut self, helper: &AudioHelper) {
        if self.selected_audio_helper_id.as_deref() != Some(helper.id.as_str()) {
            self.finish_audio_cube_edit();
        }
        self.content_browser.inspector_active = false;
        self.selected_audio_helper_id = Some(helper.id.clone());
        self.selected_model_instance_id = None;
        self.selected_model_asset = None;
        self.selected_model_document = None;
        self.saved_model_document = None;
        match &helper.kind {
            AudioHelperKind::Point { object_id, .. } => {
                self.selected_object_id = Some(object_id.clone());
                self.route_mode = false;
            }
            AudioHelperKind::Rail {
                object_id,
                graph_id,
                ..
            } => {
                self.selected_object_id = Some(object_id.clone());
                if let Some(graph_id) = graph_id {
                    self.route_mode = true;
                    self.active_route_graph = Some(graph_id.clone());
                }
            }
            AudioHelperKind::Cube { .. } => {
                self.selected_object_id = None;
                self.route_mode = false;
            }
        }
    }

    pub(super) fn clear_audio_helper_selection(&mut self) {
        if self.selected_audio_helper_id.is_none() {
            return;
        }
        self.finish_audio_cube_edit();
        self.selected_audio_helper_id = None;
        // Rail helpers enter Routes mode as part of being selected. Once the
        // helper is left, that helper-owned mode must not keep masking the
        // newly selected actor's inspector.
        self.route_mode = false;
    }

    pub(super) fn selected_audio_helper(&self) -> Option<AudioHelper> {
        let selected = self.selected_audio_helper_id.as_deref()?;
        self.audio_helpers()
            .into_iter()
            .find(|helper| helper.id == selected)
    }

    pub(super) fn audio_helper_inspector(&mut self, ui: &mut egui::Ui) -> bool {
        let Some(helper) = self.selected_audio_helper() else {
            return false;
        };
        let AudioHelperKind::Cube {
            record_path,
            manager_kind,
            manager_factory,
            table_name,
            mut cube,
        } = helper.kind
        else {
            return false;
        };
        ui.heading("Audio Volume");
        ui.label(format!("Manager: {manager_factory}"));
        ui.small(format!("Decomp table: {table_name}"));
        ui.label(match manager_kind {
            sms_schema::CubeManagerKind::SoundChange => {
                "Stage-managed music crossfade / switch region"
            }
            sms_schema::CubeManagerKind::SoundEffect => "Stage sound-effect region",
            sms_schema::CubeManagerKind::Other => "Runtime cube region",
        });
        if manager_kind == sms_schema::CubeManagerKind::SoundChange {
            self.sound_change_music_panel(ui);
        } else if manager_kind == sms_schema::CubeManagerKind::SoundEffect {
            ui.colored_label(
                egui::Color32::from_rgb(255, 180, 90),
                "No sound is assigned: the retail runtime creates this table but the decomp has no consumer that maps its data number to an SE.",
            );
        }
        ui.separator();

        let mut changed = false;
        let mut started = false;
        let mut stopped = false;
        for (label, values, speed) in [
            ("Bottom center", &mut cube.center, 1.0),
            ("Rotation", &mut cube.rotation_degrees, 0.5),
            ("Dimensions", &mut cube.dimensions, 1.0),
        ] {
            ui.label(label);
            ui.horizontal(|ui| {
                for (axis, value) in ["X", "Y", "Z"].into_iter().zip(values.iter_mut()) {
                    let response = ui.add(
                        egui::DragValue::new(value)
                            .speed(speed)
                            .prefix(format!("{axis} ")),
                    );
                    changed |= response.changed();
                    started |= response.drag_started();
                    stopped |= response.drag_stopped() || response.lost_focus();
                }
            });
        }
        for dimension in &mut cube.dimensions {
            *dimension = dimension.max(1.0);
        }
        ui.horizontal(|ui| {
            ui.label("Flags");
            let response = ui.add(egui::DragValue::new(&mut cube.flags).speed(1.0));
            changed |= response.changed();
            started |= response.drag_started();
            stopped |= response.drag_stopped() || response.lost_focus();
        });
        ui.horizontal(|ui| {
            ui.label("Data number");
            let response = ui.add(egui::DragValue::new(&mut cube.data_no).speed(1.0));
            changed |= response.changed();
            started |= response.drag_started();
            stopped |= response.drag_stopped() || response.lost_focus();
        });
        ui.small(
            "The cube is bottom-anchored: X/Z are full width and depth; Y grows upward. These values rebuild the real CubeGeneralInfo record.",
        );

        if started && self.audio_cube_edit_before.is_none() {
            self.audio_cube_edit_before = self
                .document
                .as_ref()
                .map(|document| document.archive_edits.clone());
        }
        if changed {
            if self.audio_cube_edit_before.is_none() {
                self.audio_cube_edit_before = self
                    .document
                    .as_ref()
                    .map(|document| document.archive_edits.clone());
            }
            if let Err(error) = self.update_audio_cube(&record_path, cube) {
                self.log
                    .push(format!("Could not edit audio volume: {error}"));
            }
        }
        if stopped {
            self.finish_audio_cube_edit();
        }
        true
    }

    fn update_audio_cube(&mut self, path: &[usize], cube: AudioCube) -> Result<(), String> {
        let document = self
            .document
            .as_mut()
            .ok_or_else(|| "no stage is open".to_string())?;
        let Some(StageResourceDocument::Placement(mut tables)) = document
            .effective_resource_clone(TABLES_PATH)
            .map_err(|error| error.to_string())?
        else {
            return Err("effective map/tables.bin is not typed placement data".to_string());
        };
        let record = record_at_mut(&mut tables.root, path)
            .ok_or_else(|| "the selected cube record no longer exists".to_string())?;
        write_audio_cube(record, cube)?;
        document.archive_edits.upsert_resource(
            TABLES_PATH.to_vec(),
            StageResourceDocument::Placement(tables),
        );
        self.document_dirty = stage_document_differs_from_saved(
            document,
            &self.saved_objects,
            &self.saved_lighting,
            &self.saved_archive_edits,
        );
        self.rebuild_audio_cube_helpers_cache();
        self.clear_viewport_preview_cache();
        Ok(())
    }

    pub(super) fn finish_audio_cube_edit(&mut self) {
        let Some(before) = self.audio_cube_edit_before.take() else {
            return;
        };
        let Some(document) = self.document.as_ref() else {
            return;
        };
        let record = ObjectUndoRecord::between(
            &document.objects,
            &document.objects,
            &before,
            &document.archive_edits,
        );
        if record.is_empty() {
            return;
        }
        self.push_undo_record(record);
        self.flush_document_change();
        self.log.push("Updated audio volume.".to_string());
    }

    pub(super) fn object_audio_helper_panel(&mut self, ui: &mut egui::Ui, object_id: &str) {
        let Some(helper) = self
            .audio_helpers()
            .into_iter()
            .find(|helper| match &helper.kind {
                AudioHelperKind::Point { object_id: id, .. }
                | AudioHelperKind::Rail { object_id: id, .. } => id == object_id,
                AudioHelperKind::Cube { .. } => false,
            })
        else {
            return;
        };
        ui.separator();
        ui.heading("Audio Attenuation");
        match helper.kind {
            AudioHelperKind::Point {
                actor_name,
                original_sound_id,
                sound_id,
                category,
                ..
            } => {
                self.helper_sound_assignment_panel(
                    ui,
                    ProjectSoundAssignmentKind::MapStatic,
                    &actor_name,
                    original_sound_id,
                    sound_id,
                );
                attenuation_summary(ui, category);
                ui.small(
                    "Move this object to move the runtime sound origin. The sound choice belongs to this compiled actor type, so changing it updates every instance in the packaged game.",
                );
            }
            AudioHelperKind::Rail {
                graph_name,
                original_sound_id,
                sound_id,
                category,
                ..
            } => {
                self.helper_sound_assignment_panel(
                    ui,
                    ProjectSoundAssignmentKind::Graph,
                    &graph_name,
                    original_sound_id,
                    sound_id,
                );
                ui.label(format!("Graph: {graph_name}"));
                attenuation_summary(ui, category);
                ui.small(
                    "Edit the route controls to reshape the runtime emitter path. The sound choice belongs to this graph-name binding and applies to every matching rail emitter.",
                );
            }
            AudioHelperKind::Cube { .. } => {}
        }
    }

    pub(super) fn can_assign_sound_to_selected_helper(&self) -> bool {
        let Some(selected_id) = self.selected_audio_helper_id.as_deref() else {
            return false;
        };
        self.audio_helpers().into_iter().any(|helper| {
            helper.id == selected_id
                && matches!(
                    helper.kind,
                    AudioHelperKind::Point { .. } | AudioHelperKind::Rail { .. }
                )
        })
    }

    pub(super) fn assign_sound_to_selected_helper(&mut self, sound_id: u32) -> bool {
        let Some(selected_id) = self.selected_audio_helper_id.clone() else {
            self.log
                .push("Select a compatible audio helper before assigning a sound.".to_string());
            return false;
        };
        let Some(helper) = self
            .audio_helpers()
            .into_iter()
            .find(|helper| helper.id == selected_id)
        else {
            self.log
                .push("The selected audio helper is no longer available.".to_string());
            return false;
        };
        match helper.kind {
            AudioHelperKind::Point {
                actor_name,
                original_sound_id,
                ..
            } => {
                self.set_helper_sound_assignment(
                    ProjectSoundAssignmentKind::MapStatic,
                    &actor_name,
                    original_sound_id,
                    sound_id,
                );
                true
            }
            AudioHelperKind::Rail {
                graph_name,
                original_sound_id,
                ..
            } => {
                self.set_helper_sound_assignment(
                    ProjectSoundAssignmentKind::Graph,
                    &graph_name,
                    original_sound_id,
                    sound_id,
                );
                true
            }
            AudioHelperKind::Cube { .. } => {
                self.log.push(
                    "Sound cubes do not expose a compatible retail sound assignment.".to_string(),
                );
                false
            }
        }
    }

    fn effective_helper_sound(
        &self,
        kind: ProjectSoundAssignmentKind,
        source_name: &str,
        original_sound_id: u32,
    ) -> u32 {
        let key = helper_sound_assignment_key(&kind, source_name);
        self.current_project
            .as_ref()
            .and_then(|project| project.descriptor.sound_assignments.get(&key))
            .filter(|assignment| {
                assignment.kind == kind
                    && assignment.source_name == source_name
                    && assignment.original_sound_id == original_sound_id
            })
            .map_or(original_sound_id, |assignment| assignment.sound_id)
    }

    fn sound_display_name(&self, sound_id: u32) -> String {
        self.retail_sounds
            .iter()
            .find(|entry| entry.sound_id == sound_id)
            .map(|entry| format!("{} (0x{sound_id:04X})", entry.label))
            .unwrap_or_else(|| format!("SE 0x{sound_id:04X}"))
    }

    fn helper_sound_assignment_panel(
        &mut self,
        ui: &mut egui::Ui,
        kind: ProjectSoundAssignmentKind,
        source_name: &str,
        original_sound_id: u32,
        sound_id: u32,
    ) {
        ui.label("Assigned sound");
        let mut selected = sound_id;
        let selected_text = self.sound_display_name(sound_id);
        egui::ComboBox::from_id_salt(("helper-sound", source_name))
            .selected_text(selected_text)
            .width(ui.available_width().clamp(200.0, 360.0))
            .show_ui(ui, |ui| {
                for entry in &self.retail_sounds {
                    ui.selectable_value(
                        &mut selected,
                        entry.sound_id,
                        format!("{} (0x{:04X})", entry.label, entry.sound_id),
                    )
                    .on_hover_text(&entry.symbol);
                }
            });
        if self.retail_sounds.is_empty() {
            ui.colored_label(
                egui::Color32::from_rgb(255, 180, 90),
                "The retail sound-name catalog is unavailable.",
            );
        }
        if selected != sound_id {
            self.set_helper_sound_assignment(kind, source_name, original_sound_id, selected);
        }
        self.se_preview_notice(ui);
    }

    fn set_helper_sound_assignment(
        &mut self,
        kind: ProjectSoundAssignmentKind,
        source_name: &str,
        original_sound_id: u32,
        sound_id: u32,
    ) {
        let Some(project) = self.current_project.as_mut() else {
            self.log
                .push("Sound helper assignments require a saved .sms project.".to_string());
            return;
        };
        let key = helper_sound_assignment_key(&kind, source_name);
        let previous = project.descriptor.sound_assignments.clone();
        if sound_id == original_sound_id {
            project.descriptor.sound_assignments.remove(&key);
        } else {
            project.descriptor.sound_assignments.insert(
                key,
                ProjectSoundAssignment {
                    kind,
                    source_name: source_name.to_string(),
                    original_sound_id,
                    sound_id,
                },
            );
        }
        if let Err(error) = project.save() {
            project.descriptor.sound_assignments = previous;
            self.log
                .push(format!("Could not save sound helper assignment: {error}"));
            return;
        }
        self.log.push(format!(
            "Set sound helper '{source_name}' to {}. Build Game applies it to the packaged main.dol.",
            self.sound_display_name(sound_id)
        ));
    }

    fn sound_change_music_panel(&mut self, ui: &mut egui::Ui) {
        let Some(stage_id) = self
            .document
            .as_ref()
            .map(|document| document.stage_id.clone())
        else {
            return;
        };
        let defaults = self
            .retail_stage_audio
            .iter()
            .find(|profile| profile.stage_id.eq_ignore_ascii_case(&stage_id))
            .cloned();
        let current = self.current_stage_music();
        let effective_primary = current
            .map(|music| (music.bgm_id, music.wave_scene_id))
            .or_else(|| {
                defaults
                    .as_ref()
                    .and_then(|profile| profile.primary_bgm_id.zip(profile.wave_scene_id))
            });
        let effective_secondary = current
            .and_then(|music| music.secondary_bgm_id)
            .or_else(|| {
                defaults
                    .as_ref()
                    .and_then(|profile| profile.secondary_bgm_id)
            });

        ui.separator();
        ui.heading("Assigned Music");
        if let Some((bgm_id, _)) = effective_primary {
            ui.label(format!(
                "Outside volume: {}",
                self.music_display_name(bgm_id)
            ));
        } else {
            ui.label("Outside volume: no track");
        }

        let mut secondary_override = current.and_then(|music| music.secondary_bgm_id);
        let secondary_text = effective_secondary
            .map(|bgm_id| self.music_display_name(bgm_id))
            .unwrap_or_else(|| "No inside track".to_string());
        ui.label("Inside volume");
        egui::ComboBox::from_id_salt(("sound-change-track", stage_id.as_str()))
            .selected_text(secondary_text)
            .width(ui.available_width().clamp(200.0, 360.0))
            .show_ui(ui, |ui| {
                let default_label = defaults
                    .as_ref()
                    .and_then(|profile| profile.secondary_bgm_id)
                    .map(|bgm_id| format!("Game default — {}", self.music_display_name(bgm_id)))
                    .unwrap_or_else(|| "Game default — no inside track".to_string());
                ui.selectable_value(&mut secondary_override, None, default_label);
                for entry in &self.retail_music {
                    ui.selectable_value(&mut secondary_override, Some(entry.bgm_id), &entry.label)
                        .on_hover_text(format!(
                            "BGM 0x{:08X}; wave scene 0x{:X}",
                            entry.bgm_id, entry.wave_scene_id
                        ));
                }
            });
        let preview_secondary = secondary_override.or_else(|| {
            defaults
                .as_ref()
                .and_then(|profile| profile.secondary_bgm_id)
        });
        if let Some(bgm_id) = preview_secondary {
            self.bgm_preview_transport(ui, bgm_id);
        }

        let missing_secondary_wave_scene = current.is_some_and(|music| {
            music.secondary_bgm_id.is_some() && music.secondary_wave_scene_id.is_none()
        });
        if secondary_override != current.and_then(|music| music.secondary_bgm_id)
            || missing_secondary_wave_scene
        {
            let secondary_wave_scene_id = secondary_override.and_then(|bgm_id| {
                self.retail_music
                    .iter()
                    .find(|entry| entry.bgm_id == bgm_id)
                    .map(|entry| entry.wave_scene_id)
            });
            let updated = if let Some(mut music) = current {
                music.secondary_bgm_id = secondary_override;
                music.secondary_wave_scene_id = secondary_wave_scene_id;
                Some(music)
            } else if let (Some(secondary_bgm_id), Some((bgm_id, wave_scene_id))) =
                (secondary_override, effective_primary)
            {
                Some(ProjectStageMusic {
                    bgm_id,
                    wave_scene_id,
                    secondary_bgm_id: Some(secondary_bgm_id),
                    secondary_wave_scene_id,
                })
            } else {
                None
            };
            self.set_current_stage_music(updated);
        }
        if effective_primary.is_none() {
            ui.colored_label(
                egui::Color32::from_rgb(255, 180, 90),
                "Choose the stage's outside music first so the stage assignment can be saved.",
            );
        } else {
            ui.small(
                "All decomp-mapped Sunshine tracks are available. Preview resolves each track's own retail wave scene from the selected base game.",
            );
        }
    }

    fn music_display_name(&self, bgm_id: u32) -> String {
        self.retail_music
            .iter()
            .find(|entry| entry.bgm_id == bgm_id)
            .map(|entry| format!("{} (0x{bgm_id:08X})", entry.label))
            .unwrap_or_else(|| format!("BGM 0x{bgm_id:08X}"))
    }

    pub(super) fn paint_audio_helpers(&self, painter: &egui::Painter, rect: egui::Rect) {
        if !self.show_audio_helpers {
            return;
        }
        let projection = self.camera_projection(rect);
        let selected_id = self.selected_audio_helper_id.as_deref();
        for helper in self.audio_helpers() {
            let selected = selected_id == Some(helper.id.as_str());
            match helper.kind {
                AudioHelperKind::Point {
                    position, category, ..
                } => paint_point_helper(painter, &projection, position, category, selected),
                AudioHelperKind::Rail {
                    points, category, ..
                } => paint_rail_helper(painter, &projection, &points, category, selected),
                AudioHelperKind::Cube {
                    manager_kind, cube, ..
                } => paint_cube_helper(painter, &projection, cube, manager_kind, selected),
            }
        }
    }
}

fn helper_sound_assignment_key(kind: &ProjectSoundAssignmentKind, source_name: &str) -> String {
    let prefix = match kind {
        ProjectSoundAssignmentKind::MapStatic => "map_static",
        ProjectSoundAssignmentKind::Graph => "graph",
    };
    format!("{prefix}:{source_name}")
}

fn attenuation_summary(ui: &mut egui::Ui, category: Option<AudioAttenuation>) {
    if let Some(category) = category {
        ui.label(format!("Shared SE category {}", category.category));
        ui.label(format!(
            "Full volume: {} · category envelope: {}",
            category.full_volume_distance, category.max_distance
        ));
        ui.small(format!(
            "Curve 7 uses the category's {}-unit fixed falloff. The active curve itself comes from the retail sound-info record.",
            category.fixed_curve_range
        ));
    } else {
        ui.colored_label(
            egui::Color32::from_rgb(255, 180, 90),
            "No decomp-derived category profile is loaded.",
        );
    }
}

fn collect_record_paths(
    record: &sms_formats::JDramaRecord,
    path: &mut Vec<usize>,
    out: &mut Vec<Vec<usize>>,
    predicate: &impl Fn(&sms_formats::JDramaRecord) -> bool,
) {
    if predicate(record) {
        out.push(path.clone());
    }
    if let sms_formats::JDramaRecordPayload::Group { children, .. } = &record.payload {
        for (index, child) in children.iter().enumerate() {
            path.push(index);
            collect_record_paths(child, path, out, predicate);
            path.pop();
        }
    }
}

fn record_at<'a>(
    root: &'a sms_formats::JDramaRecord,
    path: &[usize],
) -> Option<&'a sms_formats::JDramaRecord> {
    let mut record = root;
    for index in path {
        let sms_formats::JDramaRecordPayload::Group { children, .. } = &record.payload else {
            return None;
        };
        record = children.get(*index)?;
    }
    Some(record)
}

fn record_at_mut<'a>(
    root: &'a mut sms_formats::JDramaRecord,
    path: &[usize],
) -> Option<&'a mut sms_formats::JDramaRecord> {
    let mut record = root;
    for index in path {
        let sms_formats::JDramaRecordPayload::Group { children, .. } = &mut record.payload else {
            return None;
        };
        record = children.get_mut(*index)?;
    }
    Some(record)
}

fn semantic_type(type_name: &str) -> &str {
    type_name.rsplit("::").next().unwrap_or(type_name)
}

fn audio_cube_from_record(record: &sms_formats::JDramaRecord) -> Option<AudioCube> {
    let sms_formats::JDramaRecordPayload::Fields { fields } = &record.payload else {
        return None;
    };
    let vec3 = |name: &str| {
        fields
            .iter()
            .find_map(|field| match (&*field.name, &field.value) {
                (field_name, sms_formats::JDramaFieldValue::Vec3F32(value))
                    if field_name == name =>
                {
                    Some(*value)
                }
                _ => None,
            })
    };
    let center = vec3("center")?;
    let rotation_degrees = vec3("rotation_degrees")?;
    let dimensions = vec3("dimensions_scale")?.map(|value| value * 100.0);
    let flags = fields
        .iter()
        .find_map(|field| match (&*field.name, &field.value) {
            ("flags", sms_formats::JDramaFieldValue::U32(value)) => Some(*value),
            _ => None,
        })?;
    let data_no = fields
        .iter()
        .find_map(|field| match (&*field.name, &field.value) {
            ("data_no", sms_formats::JDramaFieldValue::I32(value)) => Some(*value),
            _ => None,
        })?;
    Some(AudioCube {
        center,
        rotation_degrees,
        dimensions,
        flags,
        data_no,
    })
}

fn write_audio_cube(record: &mut sms_formats::JDramaRecord, cube: AudioCube) -> Result<(), String> {
    let sms_formats::JDramaRecordPayload::Fields { fields } = &mut record.payload else {
        return Err("selected cube does not have typed fields".to_string());
    };
    for field in fields {
        match field.name.as_str() {
            "center" => field.value = sms_formats::JDramaFieldValue::Vec3F32(cube.center),
            "rotation_degrees" => {
                field.value = sms_formats::JDramaFieldValue::Vec3F32(cube.rotation_degrees)
            }
            "dimensions_scale" => {
                field.value = sms_formats::JDramaFieldValue::Vec3F32(
                    cube.dimensions.map(|value| value / 100.0),
                )
            }
            "flags" => field.value = sms_formats::JDramaFieldValue::U32(cube.flags),
            "data_no" => field.value = sms_formats::JDramaFieldValue::I32(cube.data_no),
            _ => {}
        }
    }
    Ok(())
}

fn paint_point_helper(
    painter: &egui::Painter,
    projection: &CameraProjection,
    position: [f32; 3],
    category: Option<AudioAttenuation>,
    selected: bool,
) {
    let Some(category) = category else {
        return;
    };
    for (radius, color, width) in [
        (
            category.max_distance as f32,
            AUDIO_POINT_COLOR,
            if selected { 2.4 } else { 1.2 },
        ),
        (
            category.full_volume_distance as f32,
            AUDIO_INNER_COLOR,
            if selected { 2.0 } else { 1.0 },
        ),
    ] {
        for axis in 0..3 {
            paint_world_ring(painter, projection, position, radius, axis, color, width);
        }
    }
}

fn paint_rail_helper(
    painter: &egui::Painter,
    projection: &CameraProjection,
    points: &[[f32; 3]],
    category: Option<AudioAttenuation>,
    selected: bool,
) {
    let color = if selected {
        egui::Color32::WHITE
    } else {
        AUDIO_POINT_COLOR
    };
    for segment in points.windows(2) {
        if let Some([a, b]) = projection.project_world_segment_to_screen(segment[0], segment[1]) {
            painter.line_segment(
                [a, b],
                egui::Stroke::new(if selected { 3.0 } else { 1.5 }, color),
            );
        }
    }
    if let Some(category) = category {
        for point in points {
            paint_world_ring(
                painter,
                projection,
                *point,
                category.max_distance as f32,
                1,
                AUDIO_POINT_COLOR.gamma_multiply(0.32),
                0.8,
            );
        }
    }
}

fn paint_world_ring(
    painter: &egui::Painter,
    projection: &CameraProjection,
    center: [f32; 3],
    radius: f32,
    axis: usize,
    color: egui::Color32,
    width: f32,
) {
    if radius <= 0.0 || !radius.is_finite() {
        return;
    }
    let points = (0..=64)
        .map(|step| {
            let angle = step as f32 / 64.0 * std::f32::consts::TAU;
            let (sin, cos) = angle.sin_cos();
            let mut offset = [cos * radius, sin * radius, 0.0];
            if axis == 1 {
                offset = [cos * radius, 0.0, sin * radius];
            } else if axis == 2 {
                offset = [0.0, cos * radius, sin * radius];
            }
            [
                center[0] + offset[0],
                center[1] + offset[1],
                center[2] + offset[2],
            ]
        })
        .collect::<Vec<_>>();
    for segment in points.windows(2) {
        if let Some(screen) = projection.project_world_segment_to_screen(segment[0], segment[1]) {
            painter.line_segment(screen, egui::Stroke::new(width, color));
        }
    }
}

fn paint_cube_helper(
    painter: &egui::Painter,
    projection: &CameraProjection,
    cube: AudioCube,
    kind: sms_schema::CubeManagerKind,
    selected: bool,
) {
    let half_x = cube.dimensions[0] * 0.5;
    let half_z = cube.dimensions[2] * 0.5;
    let local = [
        [-half_x, 0.0, -half_z],
        [half_x, 0.0, -half_z],
        [half_x, 0.0, half_z],
        [-half_x, 0.0, half_z],
        [-half_x, cube.dimensions[1], -half_z],
        [half_x, cube.dimensions[1], -half_z],
        [half_x, cube.dimensions[1], half_z],
        [-half_x, cube.dimensions[1], half_z],
    ];
    let corners =
        local.map(|point| rotate_zxy_translate(point, cube.rotation_degrees, cube.center));
    let color = if selected {
        egui::Color32::WHITE
    } else if kind == sms_schema::CubeManagerKind::SoundChange {
        AUDIO_CHANGE_COLOR
    } else {
        AUDIO_EFFECT_COLOR
    };
    for (a, b) in [
        (0, 1),
        (1, 2),
        (2, 3),
        (3, 0),
        (4, 5),
        (5, 6),
        (6, 7),
        (7, 4),
        (0, 4),
        (1, 5),
        (2, 6),
        (3, 7),
    ] {
        if let Some([from, to]) = projection.project_world_segment_to_screen(corners[a], corners[b])
        {
            painter.line_segment(
                [from, to],
                egui::Stroke::new(if selected { 3.0 } else { 1.7 }, color),
            );
        }
    }
}

fn rotate_zxy_translate(
    point: [f32; 3],
    rotation_degrees: [f32; 3],
    translation: [f32; 3],
) -> [f32; 3] {
    let [x, y, z] = rotation_degrees.map(f32::to_radians);
    let (sx, cx) = x.sin_cos();
    let (sy, cy) = y.sin_cos();
    let (sz, cz) = z.sin_cos();
    [
        (cy * cz + sy * sx * sz) * point[0]
            + (-sz * cy + cz * sy * sx) * point[1]
            + sy * cx * point[2]
            + translation[0],
        cx * sz * point[0] + cx * cz * point[1] - sx * point[2] + translation[1],
        (-sy * cz + sz * cy * sx) * point[0]
            + (sy * sz + cz * cy * sx) * point[1]
            + cy * cx * point[2]
            + translation[2],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cube_fields_round_trip_runtime_scale() {
        let mut record = sms_formats::JDramaRecord {
            type_name: "JDrama::CubeGeneralInfo".to_string(),
            name: "audio".to_string(),
            payload: sms_formats::JDramaRecordPayload::Fields {
                fields: vec![
                    sms_formats::JDramaField {
                        name: "center".to_string(),
                        value: sms_formats::JDramaFieldValue::Vec3F32([1.0, 2.0, 3.0]),
                    },
                    sms_formats::JDramaField {
                        name: "rotation_degrees".to_string(),
                        value: sms_formats::JDramaFieldValue::Vec3F32([0.0, 45.0, 0.0]),
                    },
                    sms_formats::JDramaField {
                        name: "dimensions_scale".to_string(),
                        value: sms_formats::JDramaFieldValue::Vec3F32([2.0, 3.0, 4.0]),
                    },
                    sms_formats::JDramaField {
                        name: "flags".to_string(),
                        value: sms_formats::JDramaFieldValue::U32(7),
                    },
                    sms_formats::JDramaField {
                        name: "data_no".to_string(),
                        value: sms_formats::JDramaFieldValue::I32(9),
                    },
                ],
            },
        };
        let mut cube = audio_cube_from_record(&record).unwrap();
        assert_eq!(cube.dimensions, [200.0, 300.0, 400.0]);
        cube.dimensions = [500.0, 600.0, 700.0];
        write_audio_cube(&mut record, cube).unwrap();
        assert_eq!(
            audio_cube_from_record(&record).unwrap().dimensions,
            cube.dimensions
        );
    }

    #[test]
    fn cube_rotation_matches_bottom_anchored_runtime_geometry() {
        let point = rotate_zxy_translate([50.0, 100.0, 0.0], [0.0, 90.0, 0.0], [10.0, 20.0, 30.0]);
        assert!((point[0] - 10.0).abs() < 0.001);
        assert!((point[1] - 120.0).abs() < 0.001);
        assert!((point[2] + 20.0).abs() < 0.001);
    }

    #[test]
    fn clearing_audio_helper_selection_also_leaves_helper_owned_route_mode() {
        let mut app = SmsEditorApp {
            selected_audio_helper_id: Some("rail:fixture".to_string()),
            route_mode: true,
            ..SmsEditorApp::default()
        };
        app.clear_audio_helper_selection();
        assert!(app.selected_audio_helper_id.is_none());
        assert!(!app.route_mode);
    }

    #[test]
    #[ignore = "requires SMS_DECOMP_ROOT and SMS_BASE_ROOT"]
    fn pinna_stage_discovers_decomp_bound_audio_helpers() {
        let decomp = std::env::var_os("SMS_DECOMP_ROOT").expect("SMS_DECOMP_ROOT");
        let base = std::env::var_os("SMS_BASE_ROOT").expect("SMS_BASE_ROOT");
        let registry = sms_schema::SchemaGenerator::new(decomp).generate().unwrap();
        let document = StageDocument::open(base, "pinnaParco1")
            .unwrap()
            .with_registry(registry);
        let mut app = SmsEditorApp {
            document: Some(document),
            ..SmsEditorApp::default()
        };
        app.rebuild_audio_cube_helpers_cache();
        let helpers = app.audio_helpers();
        let (path, mut cube) = helpers
            .iter()
            .find_map(|helper| match &helper.kind {
                AudioHelperKind::Cube {
                    record_path,
                    manager_kind: sms_schema::CubeManagerKind::SoundChange,
                    cube,
                    ..
                } => Some((record_path.clone(), *cube)),
                _ => None,
            })
            .expect("Pinna sound-change helper");
        cube.dimensions[0] += 100.0;
        app.update_audio_cube(&path, cube).unwrap();
        assert!(app.audio_helpers().iter().any(|helper| matches!(
            &helper.kind,
            AudioHelperKind::Cube {
                manager_kind: sms_schema::CubeManagerKind::SoundChange,
                cube: updated,
                ..
            } if (updated.dimensions[0] - cube.dimensions[0]).abs() < 0.001
        )));
    }
}
