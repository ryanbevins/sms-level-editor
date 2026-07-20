use super::*;

const ACTIVE_ROUTE_NODE_RADIUS: f32 = 10.0;
const SELECTED_ROUTE_NODE_RADIUS: f32 = 12.0;
const INACTIVE_ROUTE_NODE_RADIUS: f32 = 5.5;
const ROUTE_NODE_HALO_WIDTH: f32 = 3.5;
const ROUTE_NODE_HIT_RADIUS: f32 = 16.0;
const GENERATED_ROUTE_NODE_RADIUS: f32 = 3.75;

impl SmsEditorApp {
    pub(super) fn routes_hierarchy_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Routes");
            ui.checkbox(&mut self.show_all_routes, "Show all")
                .on_hover_text("Render inactive graphs and their linked actors as context");
        });
        ui.small("One graph is active for editing; all other graphs remain subdued context.");
        ui.separator();

        let rows = self
            .document
            .as_ref()
            .and_then(|document| {
                document
                    .route_authoring
                    .as_ref()
                    .map(|routes| (document, routes))
            })
            .map(|(document, routes)| {
                routes
                    .graphs
                    .iter()
                    .map(|graph| {
                        let consumers = document.route_reference_count(&graph.name);
                        let route_type = if graph.name.starts_with("S_") {
                            "Auto spline"
                        } else {
                            "Waypoint"
                        };
                        let status = if graph.controls.is_empty() {
                            "error"
                        } else {
                            "ok"
                        };
                        (
                            graph.id.clone(),
                            graph.name.clone(),
                            graph.controls.len(),
                            consumers,
                            route_type,
                            status,
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (id, name, nodes, consumers, route_type, status) in rows {
                let active = self.active_route_graph.as_deref() == Some(id.as_str());
                let label = format!(
                    "{}  |  {} nodes  |  {} users  |  {}  |  {}",
                    name, nodes, consumers, route_type, status
                );
                if ui.selectable_label(active, label).clicked() {
                    self.active_route_graph = Some(id);
                    self.selected_route_controls.clear();
                    self.selected_route_link = None;
                }
            }
        });
    }

    pub(super) fn route_inspector_panel(&mut self, ui: &mut egui::Ui) -> bool {
        if !self.route_mode {
            return false;
        }
        let Some(graph_id) = self.active_route_graph.clone() else {
            ui.heading("Route Inspector");
            ui.small("Select a route in the Routes hierarchy.");
            return true;
        };
        let graph = self
            .document
            .as_ref()
            .and_then(|document| document.route_authoring.as_ref())
            .and_then(|routes| routes.graph(&graph_id))
            .cloned();
        let Some(graph) = graph else {
            return true;
        };
        ui.heading(&graph.name);
        let mut route_name = graph.name.clone();
        if ui.text_edit_singleline(&mut route_name).changed() && !route_name.is_empty() {
            let graph_id = graph_id.clone();
            self.apply_route_edit("Renamed route and typed consumers", move |document| {
                document
                    .rename_route_graph(&graph_id, &route_name)
                    .map(|_| ())
            });
        }
        let consumer_count = self
            .document
            .as_ref()
            .map_or(0, |document| document.route_reference_count(&graph.name));
        if ui
            .add_enabled(consumer_count == 0, egui::Button::new("Delete Route"))
            .on_hover_text(if consumer_count == 0 {
                "Delete this unreferenced graph"
            } else {
                "Unassign all consumers before deleting this graph"
            })
            .clicked()
        {
            let graph_id = graph_id.clone();
            self.apply_route_edit("Deleted route", move |document| {
                document.ensure_route_authoring()?.remove_graph(&graph_id);
                Ok(())
            });
            self.active_route_graph = None;
            self.selected_route_controls.clear();
            self.selected_route_link = None;
            return true;
        }
        ui.small(format!(
            "{} control points, {} links; bake tolerance {} units",
            graph.controls.len(),
            graph.links.len(),
            self.document
                .as_ref()
                .and_then(|document| document.route_authoring.as_ref())
                .map_or(25.0, |routes| routes.bake_tolerance)
        ));
        ui.separator();

        if self.selected_route_controls.len() == 2
            && ui.button("Connect Selected (Bidirectional)").clicked()
        {
            let endpoints = self
                .selected_route_controls
                .iter()
                .cloned()
                .collect::<Vec<_>>();
            let graph_id = graph_id.clone();
            self.apply_route_edit("Connected route controls", move |document| {
                document
                    .ensure_route_authoring()?
                    .graph_mut(&graph_id)
                    .ok_or_else(|| SceneError::StageExport("active route disappeared".to_string()))?
                    .connect_bidirectional(&endpoints[0], &endpoints[1])
                    .map_err(|error| SceneError::StageExport(error.to_string()))?;
                Ok(())
            });
        }
        if let Some(control_id) = self.selected_route_controls.iter().next().cloned() {
            if let Some(control) = graph.control(&control_id) {
                ui.strong("Control Point");
                let mut position = control.node.position;
                let mut changed = false;
                ui.horizontal(|ui| {
                    for (axis, label) in ["X", "Y", "Z"].into_iter().enumerate() {
                        changed |= ui
                            .add(
                                egui::DragValue::new(&mut position[axis])
                                    .prefix(format!("{label} ")),
                            )
                            .changed();
                    }
                });
                if changed {
                    let graph_id = graph_id.clone();
                    let control_id = control_id.clone();
                    self.apply_route_edit("Moved route control point", move |document| {
                        document
                            .ensure_route_authoring()?
                            .graph_mut(&graph_id)
                            .ok_or_else(|| {
                                SceneError::StageExport("active route disappeared".to_string())
                            })?
                            .set_control_position(&control_id, position);
                        Ok(())
                    });
                }
                if ui.button("Delete Control Point").clicked() {
                    let graph_id = graph_id.clone();
                    self.apply_route_edit("Deleted route control point", move |document| {
                        document
                            .ensure_route_authoring()?
                            .graph_mut(&graph_id)
                            .ok_or_else(|| {
                                SceneError::StageExport("active route disappeared".to_string())
                            })?
                            .remove_control(&control_id);
                        Ok(())
                    });
                    self.selected_route_controls.clear();
                }
            }
        }

        if let Some(link_id) = self.selected_route_link.clone() {
            if let Some(link) = graph.links.iter().find(|link| link.id == link_id) {
                ui.separator();
                ui.strong("Connection");
                ui.small(format!("{} -> {}", link.from, link.to));
                let forward = link.forward.is_some();
                let reverse = link.reverse.is_some();
                ui.horizontal(|ui| {
                    if ui.button("Reverse").clicked() {
                        let graph_id = graph_id.clone();
                        let link_id = link_id.clone();
                        self.apply_route_edit("Reversed route connection", move |document| {
                            document
                                .ensure_route_authoring()?
                                .graph_mut(&graph_id)
                                .ok_or_else(|| {
                                    SceneError::StageExport("active route disappeared".to_string())
                                })?
                                .reverse_link(&link_id);
                            Ok(())
                        });
                    }
                    if ui
                        .button(if forward && reverse {
                            "Make One-Way"
                        } else {
                            "Make Bidirectional"
                        })
                        .clicked()
                    {
                        let graph_id = graph_id.clone();
                        let link_id = link_id.clone();
                        self.apply_route_edit("Changed route direction", move |document| {
                            document
                                .ensure_route_authoring()?
                                .graph_mut(&graph_id)
                                .ok_or_else(|| {
                                    SceneError::StageExport("active route disappeared".to_string())
                                })?
                                .set_link_direction(&link_id, true, !(forward && reverse));
                            Ok(())
                        });
                    }
                });
                if let Some(mut handles) = link.bezier {
                    ui.label("Bezier Handles (world space)");
                    let mut changed = false;
                    for (label, handle) in [("Start", &mut handles.from), ("End", &mut handles.to)]
                    {
                        ui.horizontal(|ui| {
                            ui.small(label);
                            for (axis, axis_label) in ["X", "Y", "Z"].into_iter().enumerate() {
                                changed |= ui
                                    .add(
                                        egui::DragValue::new(&mut handle[axis])
                                            .prefix(format!("{axis_label} "))
                                            .speed(5.0),
                                    )
                                    .changed();
                            }
                        });
                    }
                    if changed {
                        let graph_id = graph_id.clone();
                        let link_id = link_id.clone();
                        self.apply_route_edit("Moved Bezier handles", move |document| {
                            document
                                .ensure_route_authoring()?
                                .graph_mut(&graph_id)
                                .ok_or_else(|| {
                                    SceneError::StageExport("active route disappeared".to_string())
                                })?
                                .set_link_bezier(&link_id, Some(handles));
                            Ok(())
                        });
                    }
                }
                ui.horizontal(|ui| {
                    if link.bezier.is_none() {
                        if ui
                            .button("Curve...")
                            .on_hover_text(
                                "Bezier samples become real Sunshine nodes and may be stop points",
                            )
                            .clicked()
                        {
                            self.route_curve_confirmation =
                                Some((graph_id.clone(), link_id.clone()));
                        }
                    } else if ui.button("Reset Straight").clicked() {
                        let graph_id = graph_id.clone();
                        let link_id = link_id.clone();
                        self.apply_route_edit(
                            "Reset route connection to straight",
                            move |document| {
                                document
                                    .ensure_route_authoring()?
                                    .graph_mut(&graph_id)
                                    .ok_or_else(|| {
                                        SceneError::StageExport(
                                            "active route disappeared".to_string(),
                                        )
                                    })?
                                    .set_link_bezier(&link_id, None);
                                Ok(())
                            },
                        );
                    }
                    if ui.button("Split").clicked() {
                        let graph_id = graph_id.clone();
                        let link_id = link_id.clone();
                        self.apply_route_edit("Split route connection", move |document| {
                            document
                                .ensure_route_authoring()?
                                .graph_mut(&graph_id)
                                .ok_or_else(|| {
                                    SceneError::StageExport("active route disappeared".to_string())
                                })?
                                .split_link(&link_id)
                                .map_err(|error| SceneError::StageExport(error.to_string()))?;
                            Ok(())
                        });
                    }
                    if ui.button("Disconnect").clicked() {
                        let graph_id = graph_id.clone();
                        let link_id = link_id.clone();
                        self.apply_route_edit("Disconnected route controls", move |document| {
                            document
                                .ensure_route_authoring()?
                                .graph_mut(&graph_id)
                                .ok_or_else(|| {
                                    SceneError::StageExport("active route disappeared".to_string())
                                })?
                                .remove_link(&link_id);
                            Ok(())
                        });
                        self.selected_route_link = None;
                    }
                });
            }
        }

        if self.route_curve_confirmation.as_ref()
            == Some(&(
                graph_id.clone(),
                self.selected_route_link.clone().unwrap_or_default(),
            ))
        {
            ui.separator();
            ui.colored_label(
                egui::Color32::from_rgb(240, 190, 90),
                "Baking this curve creates visible runtime nodes. Piantas and other consumers may pause at them; movement presets will not be changed.",
            );
            ui.horizontal(|ui| {
                if ui.button("Bake Curve Handles").clicked() {
                    if let Some((graph_id, link_id)) = self.route_curve_confirmation.take() {
                        self.apply_route_edit("Added Bezier route handles", move |document| {
                            document
                                .ensure_route_authoring()?
                                .graph_mut(&graph_id)
                                .ok_or_else(|| {
                                    SceneError::StageExport("active route disappeared".to_string())
                                })?
                                .reset_link_to_curve(&link_id);
                            Ok(())
                        });
                    }
                }
                if ui.button("Cancel").clicked() {
                    self.route_curve_confirmation = None;
                }
            });
        }
        true
    }

    pub(super) fn graph_reference_control(&mut self, ui: &mut egui::Ui, object: &SceneObject) {
        if object.raw_param("graph_name").is_none() {
            return;
        }
        ui.separator();
        ui.heading("Route Assignment");
        let current = object.raw_param("graph_name").unwrap_or("(null)");
        ui.label(format!("Current: {current}"));
        let suggestions = self
            .document
            .as_ref()
            .map(|document| document.route_assignment_suggestions(&object.id))
            .unwrap_or_default();
        for suggestion in suggestions.iter().take(5) {
            let label = format!(
                "{}  |  {:.0}u  |  {} users{}",
                suggestion.graph_name,
                suggestion.nearest_distance,
                suggestion.consumer_count,
                if suggestion.same_factory_uses > 0 {
                    "  | same class"
                } else {
                    ""
                }
            );
            if ui.selectable_label(suggestion.current, label).clicked() && !suggestion.current {
                self.pending_route_assignment =
                    Some((object.id.clone(), suggestion.graph_name.clone()));
            }
        }
        if let Some((object_id, graph_name)) = self.pending_route_assignment.clone() {
            if object_id == object.id {
                ui.horizontal(|ui| {
                    ui.label(format!("Assign {graph_name}?"));
                    if ui.button("Confirm").clicked() {
                        let target = graph_name.clone();
                        let object_id = object_id.clone();
                        self.apply_route_edit("Assigned actor route", move |document| {
                            document.assign_object_route(&object_id, Some(&target))
                        });
                        self.pending_route_assignment = None;
                    }
                    if ui.button("Cancel").clicked() {
                        self.pending_route_assignment = None;
                    }
                });
            }
        }
        ui.horizontal(|ui| {
            if ui.button("Edit Route").clicked() {
                if let Some(graph) = self
                    .document
                    .as_ref()
                    .and_then(|document| document.route_authoring.as_ref())
                    .and_then(|routes| routes.graph_by_name(current))
                {
                    self.active_route_graph = Some(graph.id.clone());
                    self.route_mode = true;
                }
            }
            if ui.button("Pick in Viewport").clicked() {
                if let Some(graph) = self
                    .document
                    .as_ref()
                    .and_then(|document| document.route_authoring.as_ref())
                    .and_then(|routes| routes.graph_by_name(current))
                {
                    self.active_route_graph = Some(graph.id.clone());
                    self.route_mode = true;
                    self.selected_route_controls.clear();
                    self.selected_route_link = None;
                }
            }
            if ui.button("Unassign").clicked() {
                let object_id = object.id.clone();
                self.apply_route_edit("Unassigned actor route", move |document| {
                    document.assign_object_route(&object_id, None)
                });
            }
            if current != "(null)" && ui.button("Duplicate and Reassign").clicked() {
                let object_id = object.id.clone();
                let mut created = None;
                self.apply_route_edit("Duplicated and reassigned actor route", |document| {
                    created = Some(document.duplicate_route_and_reassign(&object_id)?);
                    Ok(())
                });
                if let Some(graph_id) = created {
                    self.active_route_graph = Some(graph_id);
                    self.route_mode = true;
                }
            }
            if ui.button("Create Route from Actor").clicked() {
                let object_id = object.id.clone();
                let mut created = None;
                self.apply_route_edit("Created and assigned actor route", |document| {
                    created = Some(document.create_route_from_actor(&object_id)?);
                    Ok(())
                });
                if let Some(graph_id) = created {
                    self.active_route_graph = Some(graph_id);
                    self.route_mode = true;
                }
            }
        });
    }

    pub(super) fn selected_route_control_position(&self) -> Option<[f32; 3]> {
        if !self.route_mode || self.selected_route_controls.len() != 1 {
            return None;
        }
        let graph_id = self.active_route_graph.as_deref()?;
        let control_id = self.selected_route_controls.iter().next()?;
        self.document
            .as_ref()?
            .route_authoring
            .as_ref()?
            .graph(graph_id)?
            .control(control_id)
            .map(|control| control.node.position.map(f32::from))
    }

    pub(super) fn begin_route_undo_transaction(&mut self) {
        if self.route_undo_transaction.is_some() {
            return;
        }
        if let Some(document) = &self.document {
            self.route_undo_transaction = Some(RouteUndoTransaction {
                before_objects: document.objects.clone(),
                before_archive_edits: document.archive_edits.clone(),
                before_route: document.route_authoring.clone(),
            });
        }
    }

    pub(super) fn update_selected_route_control_position(&mut self, position: [f32; 3]) {
        let Some(graph_id) = self.active_route_graph.clone() else {
            return;
        };
        let Some(control_id) = self.selected_route_controls.iter().next().cloned() else {
            return;
        };
        let position = position.map(|value| {
            value
                .round()
                .clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16
        });
        let Some(document) = self.document.as_mut() else {
            return;
        };
        let changed = document
            .route_authoring
            .as_mut()
            .and_then(|routes| routes.graph_mut(&graph_id))
            .is_some_and(|graph| graph.set_control_position(&control_id, position));
        if changed {
            if let Err(error) = document.compile_route_authoring() {
                self.log
                    .push(format!("Could not preview route move: {error}"));
            }
            self.document_dirty = true;
        }
    }

    pub(super) fn commit_route_undo_transaction(&mut self, label: &str) {
        let Some(transaction) = self.route_undo_transaction.take() else {
            return;
        };
        let Some(document) = self.document.as_ref() else {
            return;
        };
        let record = ObjectUndoRecord::route_edit(
            &transaction.before_objects,
            &document.objects,
            &transaction.before_archive_edits,
            &document.archive_edits,
            transaction.before_route,
            document.route_authoring.clone(),
        );
        if record.is_empty() {
            return;
        }
        self.push_undo_record(record);
        self.flush_document_change();
        self.issues = self
            .document
            .as_ref()
            .map_or_else(Vec::new, StageDocument::validate);
        self.log.push(format!("{label}."));
    }

    fn apply_route_edit(
        &mut self,
        label: &str,
        edit: impl FnOnce(&mut StageDocument) -> sms_scene::Result<()>,
    ) -> bool {
        let Some(document) = self.document.as_mut() else {
            return false;
        };
        let before_objects = document.objects.clone();
        let before_archive_edits = document.archive_edits.clone();
        let before_route = document.route_authoring.clone();
        if let Err(error) = edit(document).and_then(|_| document.compile_route_authoring()) {
            document.objects = before_objects;
            document.archive_edits = before_archive_edits;
            document.route_authoring = before_route;
            self.log.push(format!("{label} failed: {error}"));
            return false;
        }
        let record = ObjectUndoRecord::route_edit(
            &before_objects,
            &document.objects,
            &before_archive_edits,
            &document.archive_edits,
            before_route,
            document.route_authoring.clone(),
        );
        self.push_undo_record(record);
        self.document_dirty = true;
        self.flush_document_change();
        self.issues = self
            .document
            .as_ref()
            .map_or_else(Vec::new, StageDocument::validate);
        self.log.push(format!("{label}."));
        true
    }

    pub(super) fn paint_routes(&self, painter: &egui::Painter, rect: egui::Rect) {
        if !self.route_mode {
            return;
        }
        let Some(routes) = self
            .document
            .as_ref()
            .and_then(|document| document.route_authoring.as_ref())
        else {
            return;
        };
        let projection = self.camera_projection(rect);
        for graph in &routes.graphs {
            let active = self.active_route_graph.as_deref() == Some(graph.id.as_str());
            if !active && !self.show_all_routes {
                continue;
            }
            let color = if active {
                egui::Color32::from_rgb(80, 220, 255)
            } else {
                egui::Color32::from_rgba_unmultiplied(120, 155, 165, 70)
            };
            for link in &graph.links {
                let Some(from) = graph.control(&link.from) else {
                    continue;
                };
                let Some(to) = graph.control(&link.to) else {
                    continue;
                };
                let p0 = from.node.position.map(f32::from);
                let p3 = to.node.position.map(f32::from);
                let world_points = if let Some(handles) = link.bezier {
                    (0..=24)
                        .map(|step| {
                            cubic_bezier(p0, handles.from, handles.to, p3, step as f32 / 24.0)
                        })
                        .collect::<Vec<_>>()
                } else {
                    vec![p0, p3]
                };
                let points = world_points
                    .iter()
                    .filter_map(|point| {
                        projection
                            .project_world_to_screen(*point)
                            .map(|(screen, _)| screen)
                    })
                    .collect::<Vec<_>>();
                if points.len() >= 2 {
                    painter.add(egui::Shape::line(
                        points.clone(),
                        egui::Stroke::new(if active { 3.0 } else { 1.3 }, color),
                    ));
                    if active && (link.forward.is_some() || link.reverse.is_some()) {
                        let mid = points[points.len() / 2];
                        let prev = points[points.len() / 2 - 1];
                        painter.arrow(prev, mid - prev, egui::Stroke::new(2.0, color));
                    }
                    if active && link.bezier.is_some() {
                        for point in points.iter().skip(1).step_by(4) {
                            painter.circle_filled(
                                *point,
                                GENERATED_ROUTE_NODE_RADIUS + 2.0,
                                egui::Color32::from_rgba_unmultiplied(15, 18, 20, 220),
                            );
                            painter.circle_filled(
                                *point,
                                GENERATED_ROUTE_NODE_RADIUS,
                                egui::Color32::from_rgb(255, 174, 82),
                            );
                        }
                    }
                }
                if active && self.selected_route_link.as_deref() == Some(link.id.as_str()) {
                    if let Some(handles) = link.bezier {
                        for (control, handle) in [(p0, handles.from), (p3, handles.to)] {
                            if let (Some((a, _)), Some((b, _))) = (
                                projection.project_world_to_screen(control),
                                projection.project_world_to_screen(handle),
                            ) {
                                painter.line_segment(
                                    [a, b],
                                    egui::Stroke::new(1.5, egui::Color32::from_rgb(255, 160, 95)),
                                );
                                painter.rect_filled(
                                    egui::Rect::from_center_size(b, egui::vec2(9.0, 9.0)),
                                    1.0,
                                    egui::Color32::from_rgb(255, 160, 95),
                                );
                            }
                        }
                    }
                }
            }
            for (control_index, control) in graph.controls.iter().enumerate() {
                if let Some((screen, _)) =
                    projection.project_world_to_screen(control.node.position.map(f32::from))
                {
                    let selected = self.selected_route_controls.contains(&control.id);
                    let radius = if selected {
                        SELECTED_ROUTE_NODE_RADIUS
                    } else if active {
                        ACTIVE_ROUTE_NODE_RADIUS
                    } else {
                        INACTIVE_ROUTE_NODE_RADIUS
                    };
                    painter.circle_filled(
                        screen,
                        radius + ROUTE_NODE_HALO_WIDTH,
                        egui::Color32::from_rgba_unmultiplied(
                            8,
                            12,
                            16,
                            if active { 225 } else { 105 },
                        ),
                    );
                    painter.circle_filled(
                        screen,
                        radius,
                        if selected {
                            egui::Color32::YELLOW
                        } else {
                            color
                        },
                    );
                    if active {
                        painter.circle_stroke(
                            screen,
                            radius + 1.0,
                            egui::Stroke::new(
                                if selected { 3.0 } else { 2.0 },
                                if selected {
                                    egui::Color32::from_rgb(255, 145, 35)
                                } else {
                                    egui::Color32::WHITE
                                },
                            ),
                        );
                        painter.circle_filled(screen, 2.25, egui::Color32::from_rgb(12, 30, 38));
                        painter.text(
                            screen + egui::vec2(radius + 5.0, -radius - 2.0),
                            egui::Align2::LEFT_TOP,
                            (control_index + 1).to_string(),
                            egui::FontId::monospace(12.0),
                            egui::Color32::WHITE,
                        );
                    }
                }
                if self.selected_route_controls.contains(&control.id)
                    && active
                    && self.selected_route_controls.len() == 1
                {
                    self.paint_gizmo(painter, rect, control.node.position.map(f32::from));
                }
            }
        }
        if let Some(document) = &self.document {
            let active_name = self
                .active_route_graph
                .as_deref()
                .and_then(|id| routes.graph(id))
                .map(|graph| graph.name.as_str());
            for object in &document.objects {
                let Some(graph_name) = object.raw_param("graph_name") else {
                    continue;
                };
                if graph_name == "(null)"
                    || (!self.show_all_routes && active_name != Some(graph_name))
                {
                    continue;
                }
                if let Some((screen, _)) =
                    projection.project_world_to_screen(object.transform.translation)
                {
                    let active = active_name == Some(graph_name);
                    painter.circle_stroke(
                        screen,
                        10.0,
                        egui::Stroke::new(
                            2.0,
                            if active {
                                egui::Color32::from_rgb(160, 235, 255)
                            } else {
                                egui::Color32::from_rgba_unmultiplied(150, 180, 185, 70)
                            },
                        ),
                    );
                    painter.text(
                        screen + egui::vec2(12.0, 4.0),
                        egui::Align2::LEFT_CENTER,
                        &object.factory_name,
                        egui::FontId::proportional(11.0),
                        egui::Color32::from_rgba_unmultiplied(
                            210,
                            230,
                            235,
                            if active { 190 } else { 70 },
                        ),
                    );
                }
            }
        }
    }

    pub(super) fn handle_route_handle_drag(
        &mut self,
        ui: &egui::Ui,
        rect: egui::Rect,
        response: &egui::Response,
    ) -> bool {
        if !self.route_mode {
            return false;
        }
        let primary_pressed = ui.input(|input| input.pointer.primary_pressed());
        let pointer = ui.input(|input| input.pointer.interact_pos());
        let mut started = false;
        if self.route_handle_drag.is_none() && primary_pressed && response.hovered() {
            let candidate = (|| {
                let graph_id = self.active_route_graph.clone()?;
                let link_id = self.selected_route_link.clone()?;
                let graph = self
                    .document
                    .as_ref()?
                    .route_authoring
                    .as_ref()?
                    .graph(&graph_id)?;
                let link = graph.links.iter().find(|link| link.id == link_id)?;
                let handles = link.bezier?;
                let pointer = pointer?;
                let projection = self.camera_projection(rect);
                let from_screen = projection.project_world_to_screen(handles.from)?.0;
                let to_screen = projection.project_world_to_screen(handles.to)?.0;
                let (from_handle, handle) =
                    if pointer.distance(from_screen) <= pointer.distance(to_screen) {
                        (true, handles.from)
                    } else {
                        (false, handles.to)
                    };
                (pointer.distance(if from_handle { from_screen } else { to_screen }) <= 13.0)
                    .then_some(RouteHandleDrag {
                        graph_id,
                        link_id,
                        from_handle,
                        plane_y: handle[1],
                    })
            })();
            if let Some(drag) = candidate {
                self.begin_route_undo_transaction();
                self.route_handle_drag = Some(drag);
                started = true;
            }
        }

        let was_dragging = self.route_handle_drag.is_some();
        if let (Some(drag), Some(pointer)) = (self.route_handle_drag.clone(), pointer) {
            if ui.input(|input| input.pointer.primary_down()) {
                let point = self.screen_to_world_plane_y(rect, pointer, drag.plane_y);
                if let Some(document) = self.document.as_mut() {
                    if let Some(graph) = document
                        .route_authoring
                        .as_mut()
                        .and_then(|routes| routes.graph_mut(&drag.graph_id))
                    {
                        if let Some(link) = graph.links.iter().find(|link| link.id == drag.link_id)
                        {
                            if let Some(mut handles) = link.bezier {
                                if drag.from_handle {
                                    handles.from = point;
                                } else {
                                    handles.to = point;
                                }
                                graph.set_link_bezier(&drag.link_id, Some(handles));
                            }
                        }
                    }
                    let _ = document.compile_route_authoring();
                    self.document_dirty = true;
                }
            }
        }
        if ui.input(|input| input.pointer.primary_released())
            && self.route_handle_drag.take().is_some()
        {
            self.commit_route_undo_transaction("Moved Bezier handle");
        }
        started || was_dragging || self.route_handle_drag.is_some()
    }

    pub(super) fn handle_route_viewport_click(
        &mut self,
        rect: egui::Rect,
        response: &egui::Response,
        modifiers: egui::Modifiers,
    ) -> bool {
        if !self.route_mode || !response.clicked() {
            return false;
        }
        let Some(pos) = response.interact_pointer_pos() else {
            return false;
        };
        if modifiers.ctrl {
            if let (Some(graph_id), Some(endpoint)) = (
                self.active_route_graph.clone(),
                self.selected_route_controls.iter().next().cloned(),
            ) {
                let world = self.screen_to_world_floor(rect, pos);
                let point = world.map(|value| {
                    value
                        .round()
                        .clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16
                });
                let mut new_id = None;
                self.apply_route_edit("Extended route", |document| {
                    let graph = document
                        .ensure_route_authoring()?
                        .graph_mut(&graph_id)
                        .ok_or_else(|| {
                            SceneError::StageExport("active route disappeared".to_string())
                        })?;
                    let id = graph.add_control(point);
                    graph
                        .connect_bidirectional(&endpoint, &id)
                        .map_err(|error| SceneError::StageExport(error.to_string()))?;
                    new_id = Some(id);
                    Ok(())
                });
                if let Some(id) = new_id {
                    self.selected_route_controls.clear();
                    self.selected_route_controls.insert(id);
                }
                return true;
            }
        }
        let hit = self.route_hit_at_screen_position(rect, pos);
        match hit {
            Some(RouteViewportHit::Control(id)) => {
                if !modifiers.shift {
                    self.selected_route_controls.clear();
                }
                if !self.selected_route_controls.insert(id.clone()) && modifiers.shift {
                    self.selected_route_controls.remove(&id);
                }
                self.selected_route_link = None;
                true
            }
            Some(RouteViewportHit::Link(id)) => {
                self.selected_route_link = Some(id.clone());
                if !modifiers.shift {
                    self.selected_route_controls.clear();
                }
                if response.double_clicked() {
                    if let Some(graph_id) = self.active_route_graph.clone() {
                        self.apply_route_edit("Split route connection", move |document| {
                            document
                                .ensure_route_authoring()?
                                .graph_mut(&graph_id)
                                .ok_or_else(|| {
                                    SceneError::StageExport("active route disappeared".to_string())
                                })?
                                .split_link(&id)
                                .map_err(|error| SceneError::StageExport(error.to_string()))?;
                            Ok(())
                        });
                    }
                }
                true
            }
            None => false,
        }
    }

    fn route_hit_at_screen_position(
        &self,
        rect: egui::Rect,
        pos: egui::Pos2,
    ) -> Option<RouteViewportHit> {
        let graph = self
            .document
            .as_ref()?
            .route_authoring
            .as_ref()?
            .graph(self.active_route_graph.as_deref()?)?;
        let projection = self.camera_projection(rect);
        if let Some((_, id)) = graph
            .controls
            .iter()
            .filter_map(|control| {
                let (screen, depth) =
                    projection.project_world_to_screen(control.node.position.map(f32::from))?;
                (screen.distance(pos) <= ROUTE_NODE_HIT_RADIUS)
                    .then_some((depth, control.id.clone()))
            })
            .min_by(|left, right| left.0.total_cmp(&right.0))
        {
            return Some(RouteViewportHit::Control(id));
        }
        graph
            .links
            .iter()
            .filter_map(|link| {
                let from = graph.control(&link.from)?.node.position.map(f32::from);
                let to = graph.control(&link.to)?.node.position.map(f32::from);
                let world_points = if let Some(handles) = link.bezier {
                    (0..=24)
                        .map(|step| {
                            cubic_bezier(from, handles.from, handles.to, to, step as f32 / 24.0)
                        })
                        .collect::<Vec<_>>()
                } else {
                    vec![from, to]
                };
                let screen_points = world_points
                    .into_iter()
                    .map(|point| {
                        projection
                            .project_world_to_screen(point)
                            .map(|value| value.0)
                    })
                    .collect::<Option<Vec<_>>>()?;
                let distance = screen_points
                    .windows(2)
                    .map(|segment| screen_segment_distance(pos, segment[0], segment[1]))
                    .min_by(f32::total_cmp)?;
                (distance <= 9.0).then_some((distance, link.id.clone()))
            })
            .min_by(|left, right| left.0.total_cmp(&right.0))
            .map(|(_, id)| RouteViewportHit::Link(id))
    }
}

#[derive(Debug, Clone)]
enum RouteViewportHit {
    Control(String),
    Link(String),
}

fn cubic_bezier(p0: [f32; 3], p1: [f32; 3], p2: [f32; 3], p3: [f32; 3], t: f32) -> [f32; 3] {
    let u = 1.0 - t;
    std::array::from_fn(|axis| {
        u * u * u * p0[axis]
            + 3.0 * u * u * t * p1[axis]
            + 3.0 * u * t * t * p2[axis]
            + t * t * t * p3[axis]
    })
}

fn screen_segment_distance(point: egui::Pos2, start: egui::Pos2, end: egui::Pos2) -> f32 {
    let line = end - start;
    let length_sq = line.length_sq();
    if length_sq <= f32::EPSILON {
        return point.distance(start);
    }
    let t = ((point - start).dot(line) / length_sq).clamp(0.0, 1.0);
    point.distance(start + line * t)
}

#[cfg(test)]
mod tests {
    use super::{cubic_bezier, screen_segment_distance};

    #[test]
    fn cubic_curve_preserves_endpoints_and_expected_midpoint() {
        let start = [0.0, 0.0, 0.0];
        let first = [0.0, 100.0, 0.0];
        let second = [100.0, 100.0, 0.0];
        let end = [100.0, 0.0, 0.0];
        assert_eq!(cubic_bezier(start, first, second, end, 0.0), start);
        assert_eq!(cubic_bezier(start, first, second, end, 1.0), end);
        assert_eq!(
            cubic_bezier(start, first, second, end, 0.5),
            [50.0, 75.0, 0.0]
        );
    }

    #[test]
    fn screen_segment_hit_distance_clamps_to_the_segment() {
        let start = egui::pos2(10.0, 10.0);
        let end = egui::pos2(20.0, 10.0);
        assert_eq!(
            screen_segment_distance(egui::pos2(15.0, 13.0), start, end),
            3.0
        );
        assert_eq!(
            screen_segment_distance(egui::pos2(5.0, 10.0), start, end),
            5.0
        );
    }
}
