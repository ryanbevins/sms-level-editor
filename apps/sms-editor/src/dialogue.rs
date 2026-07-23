use super::*;

use sms_formats::{BmgMessageToken, SmsBmgControl, SmsBmgDynamicValue};
use sms_scene::{
    DialogueAuthoringToken, DialogueContent, DialogueDomain, DialogueEditScope, DialogueProvenance,
    DialogueResolutionSeverity, DialogueRouteKind, DialogueVariant, DialogueVariantKey,
};

pub(super) struct DialogueIndexBuildResult {
    pub(super) result: Result<DialogueRouteIndex, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DialogueSharedConfirmation {
    pub(super) object_id: String,
    pub(super) key: DialogueVariantKey,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct DialogueUndoTransaction {
    pub(super) object_id: Option<String>,
    pub(super) before_objects: Vec<SceneObject>,
    pub(super) before_authoring: Option<DialogueAuthoringDocument>,
    pub(super) before_library: ProjectDialogueLibrary,
}

#[derive(Debug, Default)]
struct DialogueMessageEdit {
    changed: bool,
    in_transaction: bool,
    commit_transaction: bool,
}

impl DialogueMessageEdit {
    fn absorb_text_response(&mut self, response: &egui::Response) {
        self.changed |= response.changed();
        self.in_transaction |= response.changed() || response.gained_focus();
        self.commit_transaction |= response.lost_focus();
    }

    fn absorb_drag_response(&mut self, response: &egui::Response) {
        self.changed |= response.changed();
        self.in_transaction |=
            response.drag_started() || response.dragged() || response.drag_stopped();
        self.commit_transaction |= response.drag_stopped() || response.lost_focus();
    }

    fn discrete_change(&mut self) {
        self.changed = true;
    }
}

impl SmsEditorApp {
    pub(super) fn cancel_dialogue_consumer_index_rebuild(&mut self) {
        if let Some(cancel) = self.dialogue_consumer_cancel.take() {
            cancel.store(true, Ordering::Release);
        }
        self.dialogue_consumer_receiver = None;
    }

    pub(super) fn schedule_dialogue_index_rebuild(&mut self) {
        self.dialogue_shared_confirmation = None;
        self.cancel_dialogue_consumer_index_rebuild();
        let Some(document) = self.document.clone() else {
            self.dialogue_route_index = None;
            self.dialogue_consumer_index = None;
            self.dialogue_index_receiver = None;
            self.dialogue_index_error = None;
            self.dialogue_consumer_error = None;
            return;
        };
        // Never leave a derived index usable while a newer document snapshot
        // is being analyzed. Builds and inspector edits must wait for the
        // matching asynchronous result instead of consuming stale anchors.
        self.dialogue_route_index = None;
        self.dialogue_consumer_index = None;
        self.dialogue_consumer_error = None;
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = document
                .build_dialogue_route_index()
                .map_err(|error| error.to_string());
            let _ = sender.send(DialogueIndexBuildResult { result });
        });
        self.dialogue_index_receiver = Some(receiver);
        self.dialogue_index_error = None;
    }

    pub(super) fn poll_dialogue_index(&mut self, ctx: &egui::Context) {
        let result = self
            .dialogue_index_receiver
            .as_ref()
            .map(Receiver::try_recv);
        match result {
            Some(Ok(result)) => {
                self.dialogue_index_receiver = None;
                match result.result {
                    Ok(index) => {
                        let route_count = index.all_variants().count();
                        let issue_count = index.issues.len();
                        self.dialogue_route_index = Some(index);
                        self.dialogue_index_error = None;
                        self.log.push(format!(
                            "Dialogue index resolved {route_count} route(s) with {issue_count} issue(s)."
                        ));
                    }
                    Err(error) => {
                        self.dialogue_route_index = None;
                        self.dialogue_index_error = Some(error.clone());
                        self.log
                            .push(format!("Dialogue index could not be built: {error}"));
                    }
                }
            }
            Some(Err(TryRecvError::Empty)) => {
                ctx.request_repaint_after(Duration::from_millis(33));
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.dialogue_index_receiver = None;
                self.dialogue_route_index = None;
                self.dialogue_index_error =
                    Some("Dialogue index worker ended unexpectedly".to_string());
            }
            None => {}
        }

        let consumer_result = self
            .dialogue_consumer_receiver
            .as_ref()
            .map(Receiver::try_recv);
        match consumer_result {
            Some(Ok(Ok(index))) => {
                self.dialogue_consumer_receiver = None;
                self.dialogue_consumer_cancel = None;
                self.log.push(format!(
                    "Dialogue shared-impact index covers {} message(s).",
                    index.message_count()
                ));
                self.dialogue_consumer_index = Some(index);
                self.dialogue_consumer_error = None;
            }
            Some(Ok(Err(error))) => {
                self.dialogue_consumer_receiver = None;
                self.dialogue_consumer_cancel = None;
                self.dialogue_consumer_index = None;
                self.dialogue_shared_confirmation = None;
                self.dialogue_consumer_error = Some(error.clone());
                self.log.push(format!(
                    "Dialogue shared-impact index could not be built: {error}"
                ));
            }
            Some(Err(TryRecvError::Empty)) => {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.dialogue_consumer_receiver = None;
                self.dialogue_consumer_cancel = None;
                self.dialogue_consumer_index = None;
                self.dialogue_shared_confirmation = None;
                self.dialogue_consumer_error =
                    Some("Dialogue consumer-index worker ended unexpectedly".to_string());
            }
            None => {}
        }
    }

    pub(super) fn schedule_dialogue_consumer_index_rebuild(&mut self) {
        self.dialogue_shared_confirmation = None;
        self.cancel_dialogue_consumer_index_rebuild();
        let Some(document) = self.document.clone() else {
            self.dialogue_consumer_index = None;
            self.dialogue_consumer_error = None;
            return;
        };
        // The Edit-all impact list is authoritative only for the exact
        // document snapshot that produced it.
        self.dialogue_consumer_index = None;
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = document
                .build_game_dialogue_consumer_index_with_cancel(&worker_cancel)
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
        self.dialogue_consumer_receiver = Some(receiver);
        self.dialogue_consumer_cancel = Some(cancel);
        self.dialogue_consumer_error = None;
    }

    pub(super) fn begin_dialogue_undo_transaction(&mut self) {
        if self.dialogue_undo_transaction.is_some() {
            return;
        }
        let Some(document) = self.document.as_ref() else {
            return;
        };
        self.dialogue_undo_transaction = Some(DialogueUndoTransaction {
            object_id: self.selected_object_id.clone(),
            before_objects: document.objects.clone(),
            before_authoring: document.dialogue_authoring.clone(),
            before_library: document.dialogue_library.clone(),
        });
    }

    pub(super) fn finish_dialogue_transaction_if_selection_changed(&mut self) {
        let should_commit = self
            .dialogue_undo_transaction
            .as_ref()
            .is_some_and(|transaction| transaction.object_id != self.selected_object_id);
        if should_commit {
            self.commit_dialogue_undo_transaction("Updated dialogue text");
        }
    }

    pub(super) fn commit_dialogue_undo_transaction(&mut self, label: &str) {
        let Some(transaction) = self.dialogue_undo_transaction.take() else {
            return;
        };
        self.persist_dialogue_allocations_if_valid();
        let Some((after_objects, after_authoring, after_library)) =
            self.document.as_ref().map(|document| {
                (
                    document.objects.clone(),
                    document.dialogue_authoring.clone(),
                    document.dialogue_library.clone(),
                )
            })
        else {
            return;
        };
        let routing_identity_changed = transaction.before_objects != after_objects;
        let record = ObjectUndoRecord::dialogue_edit(
            &transaction.before_objects,
            &after_objects,
            transaction.before_authoring,
            after_authoring,
            transaction.before_library,
            after_library,
        );
        self.document_dirty = self.document.as_ref().is_some_and(|document| {
            stage_document_differs_from_saved(
                document,
                &self.saved_objects,
                &self.saved_lighting,
                &self.saved_archive_edits,
                &self.saved_dialogue_authoring,
                &self.saved_dialogue_library,
            )
        });
        if record.is_empty() {
            return;
        }
        self.push_undo_record(record);
        self.flush_document_change();
        if routing_identity_changed {
            self.schedule_dialogue_index_rebuild();
            self.schedule_dialogue_consumer_index_rebuild();
        }
        self.log.push(format!("{label}."));
    }

    fn mutate_dialogue(
        &mut self,
        label: &str,
        in_transaction: bool,
        mutate: impl FnOnce(&mut StageDocument) -> Result<(), String>,
    ) {
        if in_transaction {
            self.begin_dialogue_undo_transaction();
        }
        let before = (!in_transaction).then(|| {
            self.document.as_ref().map(|document| {
                (
                    document.objects.clone(),
                    document.dialogue_authoring.clone(),
                    document.dialogue_library.clone(),
                )
            })
        });
        let result = self
            .document
            .as_mut()
            .ok_or_else(|| "No stage is open".to_string())
            .and_then(mutate);
        if let Err(error) = result {
            if in_transaction {
                if let (Some(transaction), Some(document)) = (
                    self.dialogue_undo_transaction.take(),
                    self.document.as_mut(),
                ) {
                    document.objects = transaction.before_objects;
                    document.dialogue_authoring = transaction.before_authoring;
                    document.dialogue_library = transaction.before_library;
                    self.flush_document_change();
                }
            }
            self.log.push(format!("{label} failed: {error}"));
            return;
        }
        if !in_transaction {
            self.persist_dialogue_allocations_if_valid();
        }
        let Some((after_objects, after_authoring, after_library)) =
            self.document.as_ref().map(|document| {
                (
                    document.objects.clone(),
                    document.dialogue_authoring.clone(),
                    document.dialogue_library.clone(),
                )
            })
        else {
            return;
        };
        if let Some(Some((before_objects, before_authoring, before_library))) = before {
            let record = ObjectUndoRecord::dialogue_edit(
                &before_objects,
                &after_objects,
                before_authoring,
                after_authoring,
                before_library,
                after_library,
            );
            if !record.is_empty() {
                self.push_undo_record(record);
                self.log.push(format!("{label}."));
            }
        }
        self.document_dirty = self.document.as_ref().is_some_and(|document| {
            stage_document_differs_from_saved(
                document,
                &self.saved_objects,
                &self.saved_lighting,
                &self.saved_archive_edits,
                &self.saved_dialogue_authoring,
                &self.saved_dialogue_library,
            )
        });
        self.flush_document_change();
    }

    fn persist_dialogue_allocations_if_valid(&mut self) {
        let Some(index) = self.dialogue_route_index.clone() else {
            return;
        };
        let Some(document) = self.document.as_mut() else {
            return;
        };
        if let Err(error) = document.compile_dialogue_authoring(&index) {
            self.log.push(format!(
                "Dialogue allocations deferred until the draft is valid: {error}"
            ));
        }
    }

    pub(super) fn update_dialogue_content(
        &mut self,
        object_id: &str,
        key: DialogueVariantKey,
        scope: DialogueEditScope,
        content: DialogueContent,
        in_transaction: bool,
    ) {
        let adopts_generated_identity = key.original_message.is_none();
        let previous_runtime_name = adopts_generated_identity
            .then(|| {
                self.document.as_ref().and_then(|document| {
                    document
                        .objects
                        .iter()
                        .find(|object| object.id == object_id)
                        .and_then(|object| object.raw_param("name"))
                        .map(str::to_string)
                })
            })
            .flatten();
        let Some((route_kind, condition_path)) = self
            .dialogue_route_index
            .as_ref()
            .and_then(|index| index.find_variant(object_id, &key))
            .map(|variant| (variant.route_kind, variant.condition_path.clone()))
        else {
            self.log.push(format!(
                "Dialogue edit failed: route for object '{object_id}' is stale or unresolved."
            ));
            return;
        };
        self.mutate_dialogue("Updated dialogue", in_transaction, move |document| {
            document
                .set_dialogue_override(object_id, key, scope, route_kind, condition_path, content)
                .map_err(|error| error.to_string())
        });
        let generated_identity_changed = adopts_generated_identity
            && previous_runtime_name
                != self.document.as_ref().and_then(|document| {
                    document
                        .objects
                        .iter()
                        .find(|object| object.id == object_id)
                        .and_then(|object| object.raw_param("name"))
                        .map(str::to_string)
                });
        if generated_identity_changed && !in_transaction {
            self.schedule_dialogue_index_rebuild();
            self.schedule_dialogue_consumer_index_rebuild();
        }
    }

    pub(super) fn remove_dialogue_instance_override(
        &mut self,
        object_id: &str,
        key: DialogueVariantKey,
    ) {
        let removes_generated_route = key.original_message.is_none();
        self.mutate_dialogue("Restored original dialogue", false, move |document| {
            document.remove_dialogue_override(object_id, &key, DialogueEditScope::Instance);
            Ok(())
        });
        if removes_generated_route {
            self.schedule_dialogue_index_rebuild();
            self.schedule_dialogue_consumer_index_rebuild();
        }
    }

    pub(super) fn apply_dialogue_shared_content(
        &mut self,
        object_id: &str,
        key: DialogueVariantKey,
        content: DialogueContent,
    ) {
        let mut page_line_counts = content
            .authored_tokens
            .as_ref()
            .into_iter()
            .flatten()
            .filter_map(|token| match token {
                DialogueAuthoringToken::PageBreak { line_count } => Some(*line_count),
                _ => None,
            })
            .collect::<Vec<_>>();
        page_line_counts.sort_unstable();
        page_line_counts.dedup();
        if !page_line_counts.is_empty() && key.original_message.is_some() {
            let Some(consumers) = self.dialogue_consumer_index.as_ref() else {
                self.log.push(
                    "Shared dialogue edit deferred until the base-wide consumer index is ready."
                        .to_string(),
                );
                return;
            };
            let Some(variant) = self
                .dialogue_route_index
                .as_ref()
                .and_then(|index| index.find_variant(object_id, &key))
            else {
                self.log.push(format!(
                    "Shared dialogue edit failed: route for object '{object_id}' is stale or unresolved."
                ));
                return;
            };
            for consumer in consumers.shared_edit_consumers_for_variant(variant) {
                if page_line_counts
                    .iter()
                    .any(|line_count| *line_count != consumer.page_line_count)
                {
                    self.log.push(format!(
                        "Shared dialogue edit blocked: {:?}/{:?} uses a {}-line presentation, which does not match the authored page break.",
                        consumer.stage_id, consumer.object_id, consumer.page_line_count
                    ));
                    return;
                }
            }
        }
        let Some((route_kind, condition_path)) = self
            .dialogue_route_index
            .as_ref()
            .and_then(|index| index.find_variant(object_id, &key))
            .map(|variant| (variant.route_kind, variant.condition_path.clone()))
        else {
            self.log.push(format!(
                "Shared dialogue edit failed: route for object '{object_id}' is stale or unresolved."
            ));
            return;
        };
        self.mutate_dialogue("Updated shared dialogue", false, move |document| {
            document.remove_dialogue_override(object_id, &key, DialogueEditScope::Instance);
            document
                .set_dialogue_override(
                    object_id,
                    key,
                    DialogueEditScope::Shared,
                    route_kind,
                    condition_path,
                    content,
                )
                .map_err(|error| error.to_string())
        });
    }

    pub(super) fn initialize_empty_dialogue(&mut self, object_id: &str) {
        self.mutate_dialogue("Created empty normal dialogue", false, move |document| {
            document
                .initialize_dialogue_for_new_object(object_id)
                .map(|_| ())
                .map_err(|error| error.to_string())
        });
        self.schedule_dialogue_index_rebuild();
        self.schedule_dialogue_consumer_index_rebuild();
    }

    pub(super) fn dialogue_inspector_panel(&mut self, ui: &mut egui::Ui, object: &SceneObject) {
        let variants = self
            .dialogue_route_index
            .as_ref()
            .map(|index| index.variants_for_object(&object.id).to_vec())
            .unwrap_or_default();
        let talk_capable = dialogue_inspector_is_available(self.registry.as_ref(), object);
        if !talk_capable {
            return;
        }

        ui.separator();
        egui::CollapsingHeader::new("Dialogue")
            .id_salt(("dialogue-inspector", object.id.as_str()))
            .default_open(true)
            .show(ui, |ui| {
                ui.small(
                    "Dialogue is derived from BMG/SPC routing and saved as an authoring delta; it is not part of this actor's raw parameters.",
                );
                self.dialogue_resolution_issues(ui);

                if variants.is_empty() {
                    self.dialogue_empty_state(ui, object);
                    return;
                }

                for (variant_index, variant) in variants.iter().enumerate() {
                    ui.push_id(
                        (
                            "dialogue-variant",
                            object.id.as_str(),
                            variant.key.source.normalized_fingerprint,
                            variant.key.source.callsite_occurrence,
                            variant_index,
                        ),
                        |ui| self.dialogue_variant_card(ui, object, variant),
                    );
                    ui.add_space(5.0);
                }
            });
    }

    fn dialogue_resolution_issues(&self, ui: &mut egui::Ui) {
        let Some(index) = self.dialogue_route_index.as_ref() else {
            return;
        };
        if index.issues.is_empty() {
            return;
        }
        let error_count = index
            .issues
            .iter()
            .filter(|issue| issue.severity == DialogueResolutionSeverity::Error)
            .count();
        let warning_count = index.issues.len() - error_count;
        egui::CollapsingHeader::new(format!(
            "Resolver issues - {error_count} errors, {warning_count} warnings"
        ))
        .default_open(error_count > 0)
        .show(ui, |ui| {
            for issue in &index.issues {
                let color = match issue.severity {
                    DialogueResolutionSeverity::Error => egui::Color32::from_rgb(255, 116, 104),
                    DialogueResolutionSeverity::Warning => egui::Color32::from_rgb(255, 180, 90),
                };
                ui.colored_label(color, format!("{}: {}", issue.code, issue.message));
                if let Some(path) = issue.script_path.as_deref() {
                    ui.monospace(format!("  {}", String::from_utf8_lossy(path)));
                }
            }
        });
    }

    fn dialogue_empty_state(&mut self, ui: &mut egui::Ui, object: &SceneObject) {
        if self.dialogue_index_receiver.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak("Resolving script routes and message references...");
            });
            return;
        }
        if let Some(error) = self.dialogue_index_error.as_deref() {
            ui.colored_label(
                egui::Color32::from_rgb(255, 116, 104),
                format!("Dialogue routes could not be resolved: {error}"),
            );
            return;
        }

        ui.colored_label(
            egui::Color32::from_rgb(255, 180, 90),
            "This talk-capable actor has no resolved placed-instance route.",
        );
        if ui
            .small_button("Create empty normal-talk variant")
            .on_hover_text(
                "Creates a stable generated route owned by this placed actor. Empty text is valid but will warn at build time.",
            )
            .clicked()
        {
            self.initialize_empty_dialogue(&object.id);
        }
    }

    fn dialogue_variant_card(
        &mut self,
        ui: &mut egui::Ui,
        object: &SceneObject,
        variant: &DialogueVariant,
    ) {
        let Some(mut content) = self.document.as_ref().and_then(|document| {
            self.dialogue_route_index.as_ref().and_then(|index| {
                document.effective_dialogue_content(index, &object.id, &variant.key)
            })
        }) else {
            ui.colored_label(
                egui::Color32::from_rgb(255, 116, 104),
                "This dialogue route became stale. Reopen the stage to rebuild it.",
            );
            return;
        };

        let has_instance_override = self
            .document
            .as_ref()
            .and_then(|document| document.dialogue_authoring.as_ref())
            .and_then(|authoring| authoring.objects.get(&object.id))
            .is_some_and(|authoring| {
                authoring.overrides.iter().any(|candidate| {
                    candidate.key == variant.key && candidate.scope == DialogueEditScope::Instance
                })
            });
        let is_authored = has_instance_override
            || self
                .document
                .as_ref()
                .and_then(|document| document.dialogue_authoring.as_ref())
                .and_then(|authoring| authoring.objects.get(&object.id))
                .is_some_and(|authoring| {
                    authoring
                        .overrides
                        .iter()
                        .any(|candidate| candidate.key == variant.key)
                })
            || variant.message.as_ref().is_some_and(|message| {
                self.document.as_ref().is_some_and(|document| {
                    document
                        .dialogue_library
                        .common_overrides
                        .iter()
                        .any(|candidate| &candidate.message == message)
                })
            });
        let effective_consumers = variant.message.as_ref().and_then(|_| {
            self.dialogue_consumer_index
                .as_ref()
                .map(|index| index.consumers_for_variant(variant).to_vec())
        });
        let shared_edit_consumers = variant.message.as_ref().and_then(|_| {
            self.dialogue_consumer_index
                .as_ref()
                .map(|index| index.shared_edit_consumers_for_variant(variant))
        });

        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                dialogue_badge(ui, dialogue_route_kind_label(variant.route_kind));
                if let Some(message) = variant.message.as_ref() {
                    dialogue_badge(ui, dialogue_domain_label(message.domain));
                }
                match effective_consumers.as_ref() {
                    Some(consumers) if consumers.len() > 1 => {
                        dialogue_badge(ui, &format!("Shared by {}", consumers.len()));
                    }
                    None if variant.shared_consumers.len() > 1 => {
                        dialogue_badge(
                            ui,
                            &format!("{} local users", variant.shared_consumers.len()),
                        );
                    }
                    _ => {}
                }
                if has_instance_override {
                    dialogue_badge(ui, "Instance override");
                }
            });

            ui.label(egui::RichText::new(&variant.condition_path).strong())
                .on_hover_text("Routing conditions are derived from the retail script and are read-only.");
            ui.small(format!(
                "Source: {}",
                dialogue_provenance_label(&variant.provenance)
            ));
            if let Some(message) = variant.message.as_ref() {
                ui.small(format!(
                    "Message: {} - full ID 0x{:08X} - entry {}",
                    String::from_utf8_lossy(&message.raw_resource_path),
                    message.full_message_id,
                    message.entry_index
                ));
            } else {
                ui.small("Message: allocated when this generated route is compiled");
            }

            egui::CollapsingHeader::new("Routing and presentation (read-only)")
                .default_open(false)
                .show(ui, |ui| {
                    ui.monospace(format!(
                        "script: {}",
                        String::from_utf8_lossy(&variant.key.source.raw_resource_path)
                    ));
                    ui.monospace(format!(
                        "function: {} - callsite {} - fingerprint {:016X}",
                        variant.key.source.function_symbol,
                        variant.key.source.callsite_occurrence,
                        variant.key.source.normalized_fingerprint
                    ));
                    ui.monospace(format!(
                        "presentation flags: {}",
                        optional_hex_u32(variant.presentation_flags)
                    ));
                    ui.monospace(format!(
                        "talk flags: {}",
                        optional_hex_u32(variant.talk_flags)
                    ));
                    ui.monospace(format!(
                        "INF1 opaque attributes: {}",
                        dialogue_hex_bytes(&content.attributes)
                    ));
                });

            ui.add_space(4.0);
            ui.label(egui::RichText::new("Message").strong());
            ui.small("Newlines create page/line continuation. Known controls remain inline; unknown controls stay locked as raw bytes.");
            let mut authored_tokens = dialogue_tokens_for_editor(&content);
            let page_line_count = self
                .document
                .as_ref()
                .map(|document| {
                    document.dialogue_page_line_count(&object.id, variant.route_kind)
                })
                .unwrap_or(3);
            let mut edit =
                dialogue_message_editor(ui, &mut authored_tokens, page_line_count);
            if edit.changed {
                content.authored_tokens = Some(authored_tokens);
            }

            ui.horizontal(|ui| {
                ui.label("Voice");
                let mut selected_voice = content.voice_index;
                let previous_voice = selected_voice;
                let selected_label = selected_voice
                    .and_then(|index| {
                        self.retail_dialogue_voices
                            .iter()
                            .find(|entry| entry.index == index)
                            .map(|entry| entry.label.clone())
                    })
                    .or_else(|| selected_voice.map(|index| format!("Voice {index}")))
                    .unwrap_or_else(|| "Unspecified".to_string());
                egui::ComboBox::from_id_salt("dialogue-voice")
                    .selected_text(selected_label)
                    .width((ui.available_width() - 8.0).max(120.0))
                    .show_ui(ui, |ui| {
                        if selected_voice.is_none() {
                            ui.selectable_value(&mut selected_voice, None, "Unspecified");
                        }
                        for voice in &self.retail_dialogue_voices {
                            ui.selectable_value(
                                &mut selected_voice,
                                Some(voice.index),
                                &voice.label,
                            )
                            .on_hover_text(format!(
                                "TTalk2D2 voice {} - sound 0x{:08X}",
                                voice.index, voice.sound_id
                            ));
                        }
                    });
                if selected_voice != previous_voice {
                    content.voice_index = selected_voice;
                    if let (Some(attribute), Some(voice)) =
                        (content.attributes.get_mut(4), selected_voice)
                    {
                        *attribute = voice;
                    }
                    edit.discrete_change();
                }
            });
            if self.retail_dialogue_voices.is_empty() {
                ui.colored_label(
                    egui::Color32::from_rgb(255, 180, 90),
                    "The decomp-derived 135-entry voice catalog is unavailable; the original index remains preserved.",
                );
            }

            if dialogue_message_is_empty(&content) && is_authored {
                ui.colored_label(
                    egui::Color32::from_rgb(255, 180, 90),
                    "Empty authored dialogue is valid, but export will report a warning.",
                );
            }
            let mut reset_instance = false;
            let mut request_shared_confirmation = false;
            ui.horizontal_wrapped(|ui| {
                if has_instance_override
                    && ui
                        .small_button("Reset instance")
                        .on_hover_text("Removes only this actor's instance override.")
                        .clicked()
                {
                    reset_instance = true;
                }
                if let Some(consumers) = shared_edit_consumers.as_ref() {
                    if consumers.len() > 1
                        && ui
                            .small_button(format!("Edit all {} users...", consumers.len()))
                            .on_hover_text(
                                "Mutates the shared original deliberately. The next step lists every affected placed or global consumer.",
                            )
                            .clicked()
                    {
                        request_shared_confirmation = true;
                    }
                } else if self.dialogue_consumer_receiver.is_some() {
                    ui.spinner();
                    ui.weak("Building complete game-wide impact list...");
                } else if let Some(error) = self.dialogue_consumer_error.as_deref() {
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 180, 90),
                        "Shared editing unavailable",
                    )
                    .on_hover_text(format!(
                        "The game-wide consumer index is incomplete: {error}"
                    ));
                }
            });

            if request_shared_confirmation {
                self.dialogue_shared_confirmation = Some(DialogueSharedConfirmation {
                    object_id: object.id.clone(),
                    key: variant.key.clone(),
                });
            }

            let confirming_shared = self
                .dialogue_shared_confirmation
                .as_ref()
                .is_some_and(|confirmation| {
                    confirmation.object_id == object.id && confirmation.key == variant.key
                });
            let mut confirm_shared = false;
            let mut cancel_shared = false;
            if confirming_shared && shared_edit_consumers.is_none() {
                // An impact confirmation is valid only while its complete,
                // authoritative base-wide consumer snapshot is still present.
                self.dialogue_shared_confirmation = None;
            }
            if let Some(consumers) = confirming_shared
                .then_some(shared_edit_consumers.as_deref())
                .flatten()
            {
                ui.separator();
                ui.colored_label(
                    egui::Color32::from_rgb(255, 180, 90),
                    format!(
                        "Confirm shared edit: this changes all {} resolved consumers.",
                        consumers.len()
                    ),
                );
                egui::ScrollArea::vertical()
                    .id_salt("dialogue-shared-impact")
                    .max_height(130.0)
                    .show(ui, |ui| {
                        for consumer in consumers {
                            ui.monospace(format!(
                                "{} / {}",
                                consumer.stage_id, consumer.object_id
                            ));
                        }
                    });
                ui.horizontal(|ui| {
                    if ui
                        .button(format!("Edit all {} users", consumers.len()))
                        .clicked()
                    {
                        confirm_shared = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel_shared = true;
                    }
                });
            }

            if edit.changed {
                self.update_dialogue_content(
                    &object.id,
                    variant.key.clone(),
                    DialogueEditScope::Instance,
                    content.clone(),
                    edit.in_transaction || edit.commit_transaction,
                );
            }
            if edit.commit_transaction {
                self.commit_dialogue_undo_transaction("Updated dialogue text");
            }
            if reset_instance {
                self.remove_dialogue_instance_override(&object.id, variant.key.clone());
            }
            if confirm_shared {
                self.commit_dialogue_undo_transaction("Updated dialogue text");
                self.apply_dialogue_shared_content(
                    &object.id,
                    variant.key.clone(),
                    content,
                );
                self.dialogue_shared_confirmation = None;
            } else if cancel_shared {
                self.dialogue_shared_confirmation = None;
            }
        });
    }
}

fn dialogue_inspector_is_available(
    registry: Option<&sms_schema::ObjectRegistry>,
    object: &SceneObject,
) -> bool {
    registry.is_some_and(|registry| registry.is_dialogue_instance_eligible(&object.factory_name))
}

fn dialogue_message_editor(
    ui: &mut egui::Ui,
    tokens: &mut Vec<DialogueAuthoringToken>,
    page_line_count: u8,
) -> DialogueMessageEdit {
    let mut edit = DialogueMessageEdit::default();
    let mut remove_token = None;
    for (token_index, token) in tokens.iter_mut().enumerate() {
        ui.push_id(("dialogue-token", token_index), |ui| {
            egui::Frame::group(ui.style()).show(ui, |ui| match token {
                DialogueAuthoringToken::Text(text) => {
                    ui.horizontal(|ui| {
                        ui.strong("Text");
                        if ui.small_button("Remove").clicked() {
                            remove_token = Some(token_index);
                        }
                    });
                    let response = ui.add(
                        egui::TextEdit::multiline(text)
                            .desired_rows(3)
                            .desired_width(f32::INFINITY),
                    );
                    edit.absorb_text_response(&response);
                }
                DialogueAuthoringToken::Control(control) => {
                    let mut replacement = None;
                    match control.clone() {
                        SmsBmgControl::CharacterDelay(mut delay) => {
                            ui.horizontal(|ui| {
                                ui.strong("Character delay");
                                let response = ui.add(
                                    egui::DragValue::new(&mut delay)
                                        .range(0..=u8::MAX)
                                        .suffix(" frames"),
                                );
                                edit.absorb_drag_response(&response);
                                if response.changed() {
                                    replacement = Some(SmsBmgControl::CharacterDelay(delay));
                                }
                                if ui.small_button("Remove").clicked() {
                                    remove_token = Some(token_index);
                                }
                            });
                        }
                        SmsBmgControl::AutomaticContinuation => {
                            ui.horizontal(|ui| {
                                ui.strong("Automatic continuation");
                                ui.weak("advances without player input");
                                if ui.small_button("Remove").clicked() {
                                    remove_token = Some(token_index);
                                }
                            });
                        }
                        SmsBmgControl::Choice {
                            mut slot,
                            mut text,
                        } => {
                            let mut choice_changed = false;
                            ui.horizontal(|ui| {
                                ui.strong("Choice text");
                                ui.label("slot");
                                let response =
                                    ui.add(egui::DragValue::new(&mut slot).range(0..=1));
                                choice_changed |= response.changed();
                                edit.absorb_drag_response(&response);
                                if ui.small_button("Remove").clicked() {
                                    remove_token = Some(token_index);
                                }
                            });
                            let response = ui.add(
                                egui::TextEdit::singleline(&mut text)
                                    .desired_width(f32::INFINITY),
                            );
                            choice_changed |= response.changed();
                            edit.absorb_text_response(&response);
                            if choice_changed {
                                replacement = Some(SmsBmgControl::Choice { slot, text });
                            }
                        }
                        SmsBmgControl::DynamicValue(mut value) => {
                            ui.horizontal(|ui| {
                                ui.strong("Dynamic value");
                                let before = value;
                                egui::ComboBox::from_id_salt("dynamic-value")
                                    .selected_text(dialogue_dynamic_value_label(value))
                                    .show_ui(ui, |ui| {
                                        for candidate in [
                                            SmsBmgDynamicValue::TimerFlag20003,
                                            SmsBmgDynamicValue::TimerFlag20002,
                                            SmsBmgDynamicValue::RoundedFlag20004,
                                            SmsBmgDynamicValue::BlueCoinTradeRemainder,
                                            SmsBmgDynamicValue::TimerFlag20014,
                                        ] {
                                            ui.selectable_value(
                                                &mut value,
                                                candidate,
                                                dialogue_dynamic_value_label(candidate),
                                            );
                                        }
                                    });
                                if value != before {
                                    edit.discrete_change();
                                    replacement = Some(SmsBmgControl::DynamicValue(value));
                                }
                                if ui.small_button("Remove").clicked() {
                                    remove_token = Some(token_index);
                                }
                            });
                        }
                        SmsBmgControl::FruitBasketRemaining(mut basket) => {
                            ui.horizontal(|ui| {
                                ui.strong("Fruit-basket remainder");
                                let response =
                                    ui.add(egui::DragValue::new(&mut basket).range(0..=3));
                                edit.absorb_drag_response(&response);
                                if response.changed() {
                                    replacement =
                                        Some(SmsBmgControl::FruitBasketRemaining(basket));
                                }
                                if ui.small_button("Remove").clicked() {
                                    remove_token = Some(token_index);
                                }
                            });
                        }
                        SmsBmgControl::Color(mut color) => {
                            ui.horizontal(|ui| {
                                ui.strong("Text color");
                                let before = color;
                                egui::ComboBox::from_id_salt("text-color")
                                    .selected_text(format!("Color {color}"))
                                    .show_ui(ui, |ui| {
                                        for candidate in 0..6 {
                                            ui.selectable_value(
                                                &mut color,
                                                candidate,
                                                format!("Color {candidate}"),
                                            );
                                        }
                                    });
                                if color != before {
                                    edit.discrete_change();
                                    replacement = Some(SmsBmgControl::Color(color));
                                }
                                if ui.small_button("Remove").clicked() {
                                    remove_token = Some(token_index);
                                }
                            });
                        }
                        SmsBmgControl::Unknown(payload) => {
                            ui.strong("Unknown control (locked)");
                            ui.monospace(dialogue_hex_bytes(&payload));
                            ui.weak("Opaque retail bytes are preserved exactly and cannot be edited here.");
                        }
                    }
                    if let Some(replacement) = replacement {
                        *control = replacement;
                    }
                }
                DialogueAuthoringToken::PageBreak { line_count } => {
                    ui.horizontal(|ui| {
                        ui.strong("Page break");
                        ui.weak(format!(
                            "fills the remaining {line_count}-line page and continues"
                        ));
                        if ui.small_button("Remove").clicked() {
                            remove_token = Some(token_index);
                        }
                    });
                }
            });
            ui.add_space(3.0);
        });
    }

    if let Some(token_index) = remove_token {
        tokens.remove(token_index);
        edit.discrete_change();
    }

    ui.horizontal_wrapped(|ui| {
        if ui.small_button("+ Text").clicked() {
            tokens.push(DialogueAuthoringToken::Text(String::new()));
            edit.discrete_change();
        }
        if ui
            .small_button("+ Page break")
            .on_hover_text(format!(
                "Continues at Sunshine's next {page_line_count}-line dialogue page."
            ))
            .clicked()
        {
            tokens.push(DialogueAuthoringToken::PageBreak {
                line_count: page_line_count,
            });
            edit.discrete_change();
        }
        ui.menu_button("+ Control", |ui| {
            let controls = [
                ("Character delay", SmsBmgControl::CharacterDelay(8)),
                (
                    "Automatic continuation",
                    SmsBmgControl::AutomaticContinuation,
                ),
                (
                    "Choice 1 text",
                    SmsBmgControl::Choice {
                        slot: 0,
                        text: String::new(),
                    },
                ),
                (
                    "Choice 2 text",
                    SmsBmgControl::Choice {
                        slot: 1,
                        text: String::new(),
                    },
                ),
                (
                    "Dynamic value",
                    SmsBmgControl::DynamicValue(SmsBmgDynamicValue::TimerFlag20003),
                ),
                (
                    "Fruit-basket remainder",
                    SmsBmgControl::FruitBasketRemaining(0),
                ),
                ("Text color", SmsBmgControl::Color(0)),
            ];
            for (label, control) in controls {
                if ui.button(label).clicked() {
                    tokens.push(DialogueAuthoringToken::Control(control));
                    edit.discrete_change();
                    ui.close();
                }
            }
        });
    });
    edit
}

fn dialogue_tokens_for_editor(content: &DialogueContent) -> Vec<DialogueAuthoringToken> {
    content.authored_tokens.clone().unwrap_or_else(|| {
        content
            .message
            .tokens
            .iter()
            .map(|token| match token {
                BmgMessageToken::Text(text) => DialogueAuthoringToken::Text(text.clone()),
                BmgMessageToken::Control(raw) => DialogueAuthoringToken::Control(
                    SmsBmgControl::decode(raw)
                        .unwrap_or_else(|_| SmsBmgControl::Unknown(raw.clone())),
                ),
            })
            .collect()
    })
}

fn dialogue_badge(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .small()
            .color(egui::Color32::from_rgb(205, 220, 235))
            .background_color(egui::Color32::from_rgb(45, 55, 66)),
    );
}

fn dialogue_route_kind_label(kind: DialogueRouteKind) -> &'static str {
    match kind {
        DialogueRouteKind::Normal => "Normal",
        DialogueRouteKind::Choice => "Choice follow-up",
        DialogueRouteKind::Forced => "Forced/cutscene",
        DialogueRouteKind::HappyOverride => "Happy/reward override",
        DialogueRouteKind::Balloon => "Balloon",
        DialogueRouteKind::BoardOrSign => "Board/sign",
        DialogueRouteKind::Shop => "Shop",
        DialogueRouteKind::Generated => "Generated normal",
    }
}

fn dialogue_domain_label(domain: DialogueDomain) -> &'static str {
    match domain {
        DialogueDomain::Stage => "Stage BMG",
        DialogueDomain::System => "System BMG",
        DialogueDomain::Balloon => "Balloon BMG",
    }
}

fn dialogue_provenance_label(provenance: &DialogueProvenance) -> String {
    match provenance {
        DialogueProvenance::ScriptBuiltin { symbol } => format!("retail script - {symbol}"),
        DialogueProvenance::Generated => "Graffito generated route".to_string(),
        DialogueProvenance::RuntimeOverride => {
            "runtime override after retail selection".to_string()
        }
    }
}

fn dialogue_dynamic_value_label(value: SmsBmgDynamicValue) -> &'static str {
    match value {
        SmsBmgDynamicValue::TimerFlag20003 => "Timer flag 20003",
        SmsBmgDynamicValue::TimerFlag20002 => "Timer flag 20002",
        SmsBmgDynamicValue::RoundedFlag20004 => "Rounded flag 20004",
        SmsBmgDynamicValue::BlueCoinTradeRemainder => "Blue-coin trade remainder",
        SmsBmgDynamicValue::TimerFlag20014 => "Timer flag 20014",
    }
}

fn optional_hex_u32(value: Option<u32>) -> String {
    value.map_or_else(
        || "not exposed".to_string(),
        |value| format!("0x{value:08X}"),
    )
}

fn dialogue_hex_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn dialogue_message_is_empty(content: &DialogueContent) -> bool {
    if let Some(tokens) = content.authored_tokens.as_ref() {
        return !tokens.iter().any(|token| match token {
            DialogueAuthoringToken::Text(text) => !text.trim().is_empty(),
            DialogueAuthoringToken::Control(SmsBmgControl::Choice { text, .. }) => {
                !text.trim().is_empty()
            }
            DialogueAuthoringToken::Control(_) | DialogueAuthoringToken::PageBreak { .. } => false,
        });
    }
    !content.message.tokens.iter().any(|token| match token {
        BmgMessageToken::Text(text) => !text.trim().is_empty(),
        BmgMessageToken::Control(raw) => matches!(
            SmsBmgControl::decode(raw),
            Ok(SmsBmgControl::Choice { text, .. }) if !text.trim().is_empty()
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content(tokens: Vec<BmgMessageToken>) -> DialogueContent {
        DialogueContent {
            message: sms_formats::BmgMessage { tokens },
            authored_tokens: None,
            attributes: vec![0; 8],
            voice_index: Some(0),
        }
    }

    #[test]
    fn cancelling_a_consumer_rebuild_signals_the_orphaned_worker() {
        let mut app = SmsEditorApp::default();
        let cancel = Arc::new(AtomicBool::new(false));
        let (_sender, receiver) = mpsc::channel();
        app.dialogue_consumer_cancel = Some(Arc::clone(&cancel));
        app.dialogue_consumer_receiver = Some(receiver);

        app.cancel_dialogue_consumer_index_rebuild();

        assert!(cancel.load(Ordering::Acquire));
        assert!(app.dialogue_consumer_cancel.is_none());
        assert!(app.dialogue_consumer_receiver.is_none());
    }

    #[test]
    fn empty_message_warning_counts_choice_labels_as_visible_text() {
        let empty = content(vec![
            BmgMessageToken::Text(" \n".to_string()),
            BmgMessageToken::from_sms_control(SmsBmgControl::AutomaticContinuation).unwrap(),
        ]);
        assert!(dialogue_message_is_empty(&empty));

        let choice = content(vec![BmgMessageToken::from_sms_control(
            SmsBmgControl::Choice {
                slot: 0,
                text: "Yes".to_string(),
            },
        )
        .unwrap()]);
        assert!(!dialogue_message_is_empty(&choice));
    }

    #[test]
    fn invalid_choice_text_remains_in_authoring_until_export_validation() {
        let mut draft = content(Vec::new());
        draft.authored_tokens = Some(vec![DialogueAuthoringToken::Control(
            SmsBmgControl::Choice {
                slot: 0,
                text: "Not Shift-JIS: \u{1F600}".to_string(),
            },
        )]);

        assert!(!dialogue_message_is_empty(&draft));
        assert!(draft.compiled_message().is_err());
        assert!(matches!(
            draft.authored_tokens.as_deref(),
            Some([DialogueAuthoringToken::Control(SmsBmgControl::Choice { text, .. })])
                if text.ends_with('\u{1F600}')
        ));
    }

    #[test]
    fn dialogue_inspector_excludes_dummy_proxy_but_includes_normal_npc() {
        let registry = sms_schema::ObjectRegistry {
            npc_factory_actor_types: vec![
                sms_schema::NpcFactoryActorTypeDefinition {
                    factory_name: "NPCDummy".to_string(),
                    actor_type: 0x0400_001c,
                    source_file: "dialogue fixture".to_string(),
                },
                sms_schema::NpcFactoryActorTypeDefinition {
                    factory_name: "NPCMonteM".to_string(),
                    actor_type: 0x0400_0001,
                    source_file: "dialogue fixture".to_string(),
                },
            ],
            ..sms_schema::ObjectRegistry::default()
        };

        assert!(!dialogue_inspector_is_available(
            Some(&registry),
            &SceneObject::new("dummy", "NPCDummy")
        ));
        assert!(dialogue_inspector_is_available(
            Some(&registry),
            &SceneObject::new("normal", "NPCMonteM")
        ));
    }

    #[test]
    fn every_route_kind_has_a_compact_inspector_label() {
        for kind in [
            DialogueRouteKind::Normal,
            DialogueRouteKind::Choice,
            DialogueRouteKind::Forced,
            DialogueRouteKind::HappyOverride,
            DialogueRouteKind::Balloon,
            DialogueRouteKind::BoardOrSign,
            DialogueRouteKind::Shop,
            DialogueRouteKind::Generated,
        ] {
            assert!(!dialogue_route_kind_label(kind).is_empty());
        }
    }
}
