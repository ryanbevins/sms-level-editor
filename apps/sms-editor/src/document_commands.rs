use super::*;

impl ObjectUndoRecord {
    pub(super) fn between(
        before: &[SceneObject],
        after: &[SceneObject],
        before_archive_edits: &StageArchiveEdits,
        after_archive_edits: &StageArchiveEdits,
    ) -> Self {
        let before_by_id = before
            .iter()
            .enumerate()
            .map(|(index, object)| (object.id.as_str(), (index, object)))
            .collect::<BTreeMap<_, _>>();
        let after_by_id = after
            .iter()
            .enumerate()
            .map(|(index, object)| (object.id.as_str(), (index, object)))
            .collect::<BTreeMap<_, _>>();
        let mut deltas = Vec::new();
        for (id, (index, object)) in &before_by_id {
            match after_by_id.get(id) {
                None => deltas.push(ObjectDelta::Remove {
                    index: *index,
                    object: (*object).clone(),
                }),
                Some((_, after)) if *object != *after => deltas.push(ObjectDelta::Update {
                    before: Box::new((*object).clone()),
                    after: Box::new((*after).clone()),
                }),
                Some(_) => {}
            }
        }
        for (id, (index, object)) in after_by_id {
            if !before_by_id.contains_key(id) {
                deltas.push(ObjectDelta::Insert {
                    index,
                    object: object.clone(),
                });
            }
        }
        Self {
            deltas,
            resource_deltas: resource_edit_deltas_between(
                before_archive_edits,
                after_archive_edits,
            ),
            route_delta: None,
        }
    }
    pub(super) fn route_edit(
        before_objects: &[SceneObject],
        after_objects: &[SceneObject],
        before_archive_edits: &StageArchiveEdits,
        after_archive_edits: &StageArchiveEdits,
        before_route: Option<RouteAuthoringDocument>,
        after_route: Option<RouteAuthoringDocument>,
    ) -> Self {
        let mut record = Self::between(
            before_objects,
            after_objects,
            before_archive_edits,
            after_archive_edits,
        );
        if before_route != after_route {
            record.route_delta = Some(RouteAuthoringDelta {
                before: before_route,
                after: after_route,
            });
        }
        record
    }

    fn apply_forward(&self, document: &mut StageDocument) {
        self.apply_resource_edits(document, false);
        if let Some(delta) = &self.route_delta {
            document.route_authoring = delta.after.clone();
        }
        let objects = &mut document.objects;
        let mut removals = self
            .deltas
            .iter()
            .filter_map(|delta| match delta {
                ObjectDelta::Remove { index, object } => Some((*index, object)),
                _ => None,
            })
            .collect::<Vec<_>>();
        removals.sort_by_key(|(index, _)| std::cmp::Reverse(*index));
        for (index, object) in removals {
            remove_object_delta(objects, index, &object.id);
        }
        for delta in &self.deltas {
            if let ObjectDelta::Update { before, after } = delta {
                replace_object_delta(objects, &before.id, after.as_ref().clone());
            }
        }
        let mut inserts = self
            .deltas
            .iter()
            .filter_map(|delta| match delta {
                ObjectDelta::Insert { index, object } => Some((*index, object)),
                _ => None,
            })
            .collect::<Vec<_>>();
        inserts.sort_by_key(|(index, _)| *index);
        for (index, object) in inserts {
            objects.insert(index.min(objects.len()), object.clone());
        }
    }

    fn apply_reverse(&self, document: &mut StageDocument) {
        let objects = &mut document.objects;
        let mut inserted = self
            .deltas
            .iter()
            .filter_map(|delta| match delta {
                ObjectDelta::Insert { index, object } => Some((*index, object)),
                _ => None,
            })
            .collect::<Vec<_>>();
        inserted.sort_by_key(|(index, _)| std::cmp::Reverse(*index));
        for (index, object) in inserted {
            remove_object_delta(objects, index, &object.id);
        }
        for delta in &self.deltas {
            if let ObjectDelta::Update { before, after } = delta {
                replace_object_delta(objects, &after.id, before.as_ref().clone());
            }
        }
        let mut removed = self
            .deltas
            .iter()
            .filter_map(|delta| match delta {
                ObjectDelta::Remove { index, object } => Some((*index, object)),
                _ => None,
            })
            .collect::<Vec<_>>();
        removed.sort_by_key(|(index, _)| *index);
        for (index, object) in removed {
            objects.insert(index.min(objects.len()), object.clone());
        }
        if let Some(delta) = &self.route_delta {
            document.route_authoring = delta.before.clone();
        }
        self.apply_resource_edits(document, true);
    }

    fn apply_resource_edits(&self, document: &mut StageDocument, reverse: bool) {
        if self.resource_deltas.is_empty() {
            return;
        }
        document.set_authored_resource_overlay_states(self.resource_deltas.iter().map(|delta| {
            let state = if reverse { &delta.before } else { &delta.after };
            (
                delta.raw_resource_path.clone(),
                state.edit.clone(),
                state.edit_index,
                state.removal_index,
            )
        }));
    }

    pub(super) fn is_empty(&self) -> bool {
        self.deltas.is_empty() && self.resource_deltas.is_empty() && self.route_delta.is_none()
    }
}

fn resource_edit_state(edits: &StageArchiveEdits, raw_resource_path: &[u8]) -> ResourceEditState {
    ResourceEditState {
        edit_index: edits
            .resources
            .iter()
            .position(|edit| edit.raw_resource_path == raw_resource_path),
        edit: edits
            .resources
            .iter()
            .find(|edit| edit.raw_resource_path == raw_resource_path)
            .cloned(),
        removal_index: edits
            .resource_removals
            .iter()
            .position(|removed| removed == raw_resource_path),
    }
}

fn resource_edit_deltas_between(
    before: &StageArchiveEdits,
    after: &StageArchiveEdits,
) -> Vec<ResourceEditDelta> {
    let paths = before
        .resources
        .iter()
        .map(|edit| edit.raw_resource_path.clone())
        .chain(before.resource_removals.iter().cloned())
        .chain(
            after
                .resources
                .iter()
                .map(|edit| edit.raw_resource_path.clone()),
        )
        .chain(after.resource_removals.iter().cloned())
        .collect::<BTreeSet<_>>();
    paths
        .into_iter()
        .filter_map(|raw_resource_path| {
            let before = resource_edit_state(before, &raw_resource_path);
            let after = resource_edit_state(after, &raw_resource_path);
            (before != after).then_some(ResourceEditDelta {
                raw_resource_path,
                before,
                after,
            })
        })
        .collect()
}

fn remove_object_delta(objects: &mut Vec<SceneObject>, expected_index: usize, id: &str) {
    let index = objects
        .get(expected_index)
        .filter(|object| object.id == id)
        .map(|_| expected_index)
        .or_else(|| objects.iter().position(|object| object.id == id));
    if let Some(index) = index {
        objects.remove(index);
    }
}

fn replace_object_delta(objects: &mut [SceneObject], id: &str, replacement: SceneObject) {
    if let Some(object) = objects.iter_mut().find(|object| object.id == id) {
        *object = replacement;
    }
}

fn ensure_authored_mario_placement(
    document: &mut StageDocument,
    translation: [f32; 3],
) -> Result<sms_scene::PlacementAddress, String> {
    let used_addresses = document
        .objects
        .iter()
        .filter_map(|object| {
            object
                .placement
                .as_ref()
                .and_then(|binding| binding.source_address().cloned())
        })
        .collect::<BTreeSet<_>>();
    let archive = document
        .stage_archive
        .as_mut()
        .ok_or_else(|| "the stage has no detached semantic archive".to_string())?;
    if !matches!(archive.origin(), sms_scene::StageOrigin::Blank { .. }) {
        return Err(
            "new typed Mario construction is currently limited to authored stages; existing stages can duplicate their typed Mario record"
                .to_string(),
        );
    }
    let transform = sms_formats::JDramaTransform {
        translation,
        rotation: [0.0; 3],
        scale: [1.0; 3],
    };
    if let Some(placement) = archive.object_placements().into_iter().find(|placement| {
        placement
            .type_name
            .rsplit("::")
            .next()
            .is_some_and(|type_name| type_name == "Mario")
            && !used_addresses.contains(&sms_scene::PlacementAddress {
                raw_resource_path: placement.raw_resource_path.clone(),
                record_path: placement.record_path.clone(),
            })
    }) {
        archive
            .set_object_transform(
                &placement.raw_resource_path,
                &placement.record_path,
                transform,
            )
            .map_err(|error| error.to_string())?;
        return Ok(sms_scene::PlacementAddress {
            raw_resource_path: placement.raw_resource_path,
            record_path: placement.record_path,
        });
    }

    let parent_path = archive
        .find_group_record_path(b"map/scene.bin", "IdxGroup", Some(6))
        .map_err(|error| format!("could not locate the typed player group: {error}"))?
        .ok_or_else(|| {
            "map/scene.bin has no unambiguous IdxGroup with group_index 6 for Mario".to_string()
        })?;
    let record =
        sms_scene::blank_stage_mario_record(transform).map_err(|error| error.to_string())?;
    let record_path = archive
        .insert_placement_record(b"map/scene.bin", &parent_path, record)
        .map_err(|error| error.to_string())?;
    Ok(sms_scene::PlacementAddress {
        raw_resource_path: b"map/scene.bin".to_vec(),
        record_path,
    })
}

pub(super) fn ensure_sky_placement(
    document: &mut StageDocument,
    translation: [f32; 3],
) -> Result<sms_scene::PlacementAddress, String> {
    let used_addresses = document
        .objects
        .iter()
        .filter_map(|object| {
            object
                .placement
                .as_ref()
                .and_then(|binding| binding.source_address().cloned())
        })
        .collect::<BTreeSet<_>>();
    let archive = document
        .stage_archive
        .as_mut()
        .ok_or_else(|| "the stage has no detached semantic archive".to_string())?;
    let transform = sms_formats::JDramaTransform {
        translation,
        rotation: [0.0; 3],
        scale: [1.0; 3],
    };
    if let Some(placement) = archive.object_placements().into_iter().find(|placement| {
        placement
            .type_name
            .rsplit("::")
            .next()
            .is_some_and(|type_name| type_name == "Sky")
            && !used_addresses.contains(&sms_scene::PlacementAddress {
                raw_resource_path: placement.raw_resource_path.clone(),
                record_path: placement.record_path.clone(),
            })
    }) {
        archive
            .set_object_transform(
                &placement.raw_resource_path,
                &placement.record_path,
                transform,
            )
            .map_err(|error| error.to_string())?;
        return Ok(sms_scene::PlacementAddress {
            raw_resource_path: placement.raw_resource_path,
            record_path: placement.record_path,
        });
    }

    let parent_path = archive
        .find_group_record_path(b"map/scene.bin", "IdxGroup", Some(1))
        .map_err(|error| format!("could not locate the typed sky group: {error}"))?
        .ok_or_else(|| {
            "map/scene.bin has no unambiguous IdxGroup with group_index 1 for Sky".to_string()
        })?;
    let record = sms_scene::blank_stage_sky_record(transform).map_err(|error| error.to_string())?;
    let record_path = archive
        .insert_placement_record(b"map/scene.bin", &parent_path, record)
        .map_err(|error| error.to_string())?;
    Ok(sms_scene::PlacementAddress {
        raw_resource_path: b"map/scene.bin".to_vec(),
        record_path,
    })
}

fn authored_runtime_readiness_error(
    document: &StageDocument,
    has_authored_skybox_model: bool,
) -> Option<String> {
    let authored_blank = document
        .stage_archive
        .as_ref()
        .is_some_and(|archive| matches!(archive.origin(), sms_scene::StageOrigin::Blank { .. }));
    if authored_blank
        && !document.objects.iter().any(|object| {
            object
                .factory_name
                .rsplit("::")
                .next()
                .is_some_and(|factory| factory == "Mario")
        })
    {
        return Some(
            "The authored stage has no Mario placement. Drag the Mario template from the Content Browser into the viewport before building or launching."
            .to_string(),
        );
    }
    let has_sky_actor = document.objects.iter().any(|object| {
        object
            .factory_name
            .rsplit("::")
            .next()
            .is_some_and(|factory| factory == "Sky")
    });
    let has_skybox_resource = document
        .stage_archive
        .as_ref()
        .is_some_and(|archive| archive.resource(b"map/map/sky.bmd").is_some())
        || document.archive_edits.resources.iter().any(|edit| {
            edit.raw_resource_path
                .eq_ignore_ascii_case(b"map/map/sky.bmd")
        })
        || document.archive_edits.models.iter().any(|edit| {
            edit.raw_resource_path
                .eq_ignore_ascii_case(b"map/map/sky.bmd")
        });
    if authored_blank && has_sky_actor && !has_skybox_resource && !has_authored_skybox_model {
        return Some(
            "The authored stage has a Sky actor but no skybox model. Drag a .smsmodel into the viewport and set its Stage export role to Stage skybox before building or launching."
                .to_string(),
        );
    }
    None
}

const CATALOG_SCENE_PATH: &[u8] = b"map/scene.bin";
const CATALOG_TABLES_PATH: &[u8] = b"map/tables.bin";
const CATALOG_RAL_PATH: &[u8] = b"map/scene.ral";

struct CatalogResourceWrite {
    raw_resource_path: Vec<u8>,
    document: StageResourceDocument,
    upsert: bool,
}

#[derive(Default)]
struct CatalogResourcePreflight {
    writes: Vec<CatalogResourceWrite>,
    graph_name_rewrites: BTreeMap<String, String>,
    reused_existing_resources: usize,
    upgraded_bootstrap_proxies: usize,
}

fn is_exact_blank_stage_bootstrap_proxy(
    document: &StageDocument,
    raw_resource_path: &[u8],
    effective: &StageResourceDocument,
) -> Result<bool, String> {
    if !document
        .stage_archive
        .as_ref()
        .is_some_and(|archive| matches!(archive.origin(), sms_scene::StageOrigin::Blank { .. }))
    {
        return Ok(false);
    }
    let Some(requirement) = sms_scene::BLANK_STAGE_BOOTSTRAP_REQUIREMENTS
        .iter()
        .find(|requirement| requirement.raw_path == raw_resource_path)
    else {
        return Ok(false);
    };

    let proxy = sms_authoring::built_in_blank_stage_proxy(raw_resource_path);
    let bytes = match requirement.kind {
        sms_scene::BlankStageBootstrapKind::Model => {
            proxy.compile_bmd().map_err(|error| error.to_string())?
        }
        sms_scene::BlankStageBootstrapKind::Collision => proxy
            .collision
            .as_ref()
            .ok_or_else(|| {
                format!(
                    "built-in bootstrap proxy {} has no collision",
                    String::from_utf8_lossy(raw_resource_path)
                )
            })?
            .to_col_bytes()
            .map_err(|error| error.to_string())?,
    };
    let expected = StageResourceDocument::parse_for_path(raw_resource_path, &bytes)
        .map_err(|error| error.to_string())?;
    Ok(effective == &expected)
}

fn catalog_resource_edit_deltas(
    document: &StageDocument,
    writes: Vec<CatalogResourceWrite>,
) -> Vec<ResourceEditDelta> {
    let mut deltas = BTreeMap::<Vec<u8>, ResourceEditDelta>::new();
    let mut next_new_index = document.archive_edits.resources.len();
    for write in writes {
        let raw_resource_path = write.raw_resource_path;
        let delta = deltas.entry(raw_resource_path.clone()).or_insert_with(|| {
            let before = resource_edit_state(&document.archive_edits, &raw_resource_path);
            ResourceEditDelta {
                raw_resource_path: raw_resource_path.clone(),
                before: before.clone(),
                after: before,
            }
        });
        delta.after = ResourceEditState {
            edit: Some(StageResourceEdit {
                raw_resource_path,
                document: write.document,
                mode: if write.upsert {
                    sms_scene::StageResourceEditMode::Upsert
                } else {
                    sms_scene::StageResourceEditMode::Insert
                },
            }),
            edit_index: delta.after.edit_index.or_else(|| {
                let index = next_new_index;
                next_new_index += 1;
                Some(index)
            }),
            removal_index: None,
        };
    }
    deltas
        .into_values()
        .filter(|delta| delta.before != delta.after)
        .collect()
}

#[derive(Default)]
struct CatalogResourceRepair {
    resource_writes: usize,
    runtime_links_added: usize,
    repaired_factories: Vec<String>,
    errors: Vec<String>,
}

fn repair_authored_catalog_resources(
    document: &mut StageDocument,
    templates: &[sms_scene::ObjectAuthoringTemplate],
) -> CatalogResourceRepair {
    let mut repair = CatalogResourceRepair::default();
    for template in templates {
        let mut added_links = false;
        for object in document
            .objects
            .iter_mut()
            .filter(|object| object.factory_name == template.factory_name)
        {
            for reference in &template.runtime_actor_references {
                if let Some(existing) = object.runtime_references.iter_mut().find(|existing| {
                    existing.required_factory_name == reference.required_factory_name
                        && existing.runtime_name == reference.runtime_name
                }) {
                    if existing.required != reference.required {
                        existing.required = reference.required;
                        repair.runtime_links_added += 1;
                        added_links = true;
                    }
                    continue;
                }
                object
                    .runtime_references
                    .push(sms_scene::SceneRuntimeReferenceBinding {
                        required_factory_name: reference.required_factory_name.clone(),
                        runtime_name: reference.runtime_name.clone(),
                        required: reference.required,
                        target_object_id: None,
                    });
                repair.runtime_links_added += 1;
                added_links = true;
            }
        }
        if added_links {
            repair
                .repaired_factories
                .push(template.factory_name.clone());
        }

        let preflight = match preflight_catalog_resources(document, template) {
            Ok(preflight) => preflight,
            Err(error) => {
                repair
                    .errors
                    .push(format!("{}: {error}", template.factory_name));
                continue;
            }
        };
        let resource_deltas = catalog_resource_edit_deltas(document, preflight.writes);
        if resource_deltas.is_empty() {
            continue;
        }
        repair.resource_writes += resource_deltas.len();
        repair
            .repaired_factories
            .push(template.factory_name.clone());
        ObjectUndoRecord {
            deltas: Vec::new(),
            resource_deltas,
            route_delta: None,
        }
        .apply_forward(document);
    }
    repair.repaired_factories.sort();
    repair.repaired_factories.dedup();
    repair
}

fn preflight_catalog_resources(
    document: &StageDocument,
    template: &sms_scene::ObjectAuthoringTemplate,
) -> Result<CatalogResourcePreflight, String> {
    let mut preflight = CatalogResourcePreflight::default();
    let mut seen = BTreeSet::new();
    for resource in &template.resources {
        let normalized = String::from_utf8_lossy(&resource.raw_resource_path)
            .replace('\\', "/")
            .trim_matches('/')
            .to_ascii_lowercase();
        if normalized == "map/scene.ral" {
            continue;
        }
        if !seen.insert(resource.raw_resource_path.clone()) {
            continue;
        }
        match document
            .effective_resource_clone(&resource.raw_resource_path)
            .map_err(|error| {
                format!(
                    "could not inspect effective resource {}: {error}",
                    String::from_utf8_lossy(&resource.raw_resource_path)
                )
            })? {
            None => {
                let source = parse_catalog_resource(resource)?;
                let restores_removed_baseline = document
                    .archive_edits
                    .resource_removals
                    .iter()
                    .any(|removed| removed == &resource.raw_resource_path)
                    && document.stage_archive.as_ref().is_some_and(|archive| {
                        archive.resource(&resource.raw_resource_path).is_some()
                    });
                preflight.writes.push(CatalogResourceWrite {
                    raw_resource_path: resource.raw_resource_path.clone(),
                    document: source,
                    upsert: restores_removed_baseline,
                });
            }
            Some(existing)
                if is_exact_blank_stage_bootstrap_proxy(
                    document,
                    &resource.raw_resource_path,
                    &existing,
                )? =>
            {
                // New authored stages carry tiny deterministic stand-ins for
                // resources which retail managers open unconditionally. Once
                // a catalog object owns that runtime path, upgrade only the
                // exact built-in stand-in to the real retail resource. Exact
                // matching keeps stage-local and user-authored replacements
                // authoritative.
                preflight.writes.push(CatalogResourceWrite {
                    raw_resource_path: resource.raw_resource_path.clone(),
                    document: parse_catalog_resource(resource)?,
                    upsert: true,
                });
                preflight.upgraded_bootstrap_proxies += 1;
            }
            Some(_) => {
                // Catalog entries express runtime path dependencies. An exact
                // target path is already satisfied and remains authoritative;
                // never replace stage-local or user-authored data just because
                // another retail stage carries byte-different contents there.
                preflight.reused_existing_resources += 1;
            }
        }
    }

    if !template.character_records.is_empty() || !template.table_dependencies.is_empty() {
        let mut scene = effective_placement_document(document, CATALOG_SCENE_PATH)?;
        let mut tables = effective_placement_document(document, CATALOG_TABLES_PATH)?;
        let mut scene_changed = false;
        let mut tables_changed = false;

        if !template.character_records.is_empty() {
            let current = tables.take().map(StageResourceDocument::Placement);
            let (updated, changed) = merge_character_table(current, &template.character_records)?;
            tables = Some(updated);
            tables_changed |= changed;
        }
        if !template.table_dependencies.is_empty() {
            let (changed_scene, changed_tables) = merge_runtime_table_dependencies(
                scene.as_mut(),
                tables.as_mut(),
                &template.table_dependencies,
            )?;
            scene_changed |= changed_scene;
            tables_changed |= changed_tables;
        }
        if scene_changed {
            preflight.writes.push(CatalogResourceWrite {
                raw_resource_path: CATALOG_SCENE_PATH.to_vec(),
                document: StageResourceDocument::Placement(
                    scene.expect("a changed scene dependency document exists"),
                ),
                upsert: true,
            });
        }
        if tables_changed {
            preflight.writes.push(CatalogResourceWrite {
                raw_resource_path: CATALOG_TABLES_PATH.to_vec(),
                document: StageResourceDocument::Placement(
                    tables.expect("a changed table dependency document exists"),
                ),
                upsert: true,
            });
        }
    }

    if !template.required_graph_names.is_empty() {
        let source_resource = template
            .resources
            .iter()
            .find(|resource| {
                String::from_utf8_lossy(&resource.raw_resource_path)
                    .replace('\\', "/")
                    .trim_matches('/')
                    .eq_ignore_ascii_case("map/scene.ral")
            })
            .ok_or_else(|| {
                format!(
                    "source stage {} has no map/scene.ral for required graph(s) {}",
                    template.source_stage,
                    template.required_graph_names.join(", ")
                )
            })?;
        let StageResourceDocument::Rail(source_rail) = parse_catalog_resource(source_resource)?
        else {
            return Err("catalog map/scene.ral did not parse as rail data".to_string());
        };
        let (mut target_rail, target_existed) = match document
            .effective_resource_clone(CATALOG_RAL_PATH)
            .map_err(|error| format!("could not inspect effective map/scene.ral: {error}"))?
        {
            Some(StageResourceDocument::Rail(rail)) => (rail, true),
            Some(_) => {
                return Err("effective map/scene.ral is not typed rail data".to_string());
            }
            None => (sms_formats::RalDocument::empty_canonical(), false),
        };
        let outcomes = target_rail
            .merge_named_graphs(&source_rail, &template.required_graph_names)
            .map_err(|error| format!("could not merge required rail graph: {error}"))?;
        for outcome in &outcomes {
            if outcome.source_name != outcome.target_name {
                preflight
                    .graph_name_rewrites
                    .insert(outcome.source_name.clone(), outcome.target_name.clone());
            }
        }
        if !target_existed || outcomes.iter().any(|outcome| outcome.inserted) {
            preflight.writes.push(CatalogResourceWrite {
                raw_resource_path: CATALOG_RAL_PATH.to_vec(),
                document: StageResourceDocument::Rail(target_rail),
                upsert: true,
            });
        }
    }
    Ok(preflight)
}

fn effective_placement_document(
    document: &StageDocument,
    raw_resource_path: &[u8],
) -> Result<Option<sms_formats::JDramaDocument>, String> {
    match document
        .effective_resource_clone(raw_resource_path)
        .map_err(|error| {
            format!(
                "could not inspect effective {}: {error}",
                String::from_utf8_lossy(raw_resource_path)
            )
        })? {
        Some(StageResourceDocument::Placement(document)) => Ok(Some(document)),
        Some(_) => Err(format!(
            "effective {} is not placement data",
            String::from_utf8_lossy(raw_resource_path)
        )),
        None => Ok(None),
    }
}

#[derive(Clone, Copy)]
enum RuntimeTableResource {
    Scene,
    Tables,
}

fn matching_record_paths(
    document: &sms_formats::JDramaDocument,
    mut predicate: impl FnMut(&sms_formats::JDramaRecord) -> bool,
) -> Vec<Vec<usize>> {
    fn visit(
        record: &sms_formats::JDramaRecord,
        path: &mut Vec<usize>,
        predicate: &mut impl FnMut(&sms_formats::JDramaRecord) -> bool,
        out: &mut Vec<Vec<usize>>,
    ) {
        if predicate(record) {
            out.push(path.clone());
        }
        if let sms_formats::JDramaRecordPayload::Group { children, .. } = &record.payload {
            for (index, child) in children.iter().enumerate() {
                path.push(index);
                visit(child, path, predicate, out);
                path.pop();
            }
        }
    }
    let mut paths = Vec::new();
    visit(&document.root, &mut Vec::new(), &mut predicate, &mut paths);
    paths
}

fn merge_runtime_table_dependencies(
    scene: Option<&mut sms_formats::JDramaDocument>,
    tables: Option<&mut sms_formats::JDramaDocument>,
    dependencies: &[sms_scene::ObjectAuthoringTableDependency],
) -> Result<(bool, bool), String> {
    let mut scene = scene;
    let mut tables = tables;
    let mut scene_changed = false;
    let mut tables_changed = false;
    let mut dependencies = dependencies.to_vec();
    dependencies.sort_by(|left, right| {
        left.record
            .name
            .cmp(&right.record.name)
            .then_with(|| left.record.type_name.cmp(&right.record.type_name))
    });

    for dependency in dependencies {
        let mut same_name = Vec::new();
        let mut exact = Vec::new();
        let mut exact_targets = Vec::new();
        let mut type_targets = Vec::new();
        for (resource, document) in [
            (RuntimeTableResource::Scene, scene.as_deref()),
            (RuntimeTableResource::Tables, tables.as_deref()),
        ] {
            let Some(document) = document else {
                continue;
            };
            same_name.extend(
                matching_record_paths(document, |record| record.name == dependency.record.name)
                    .into_iter()
                    .map(|path| (resource, path)),
            );
            exact.extend(
                matching_record_paths(document, |record| {
                    record.name == dependency.record.name
                        && semantic_record_type(&record.type_name)
                            == semantic_record_type(&dependency.record.type_name)
                })
                .into_iter()
                .map(|path| (resource, path)),
            );
            let sms_scene::AuthoredPlacementDependencyTarget::NamedGroup { type_name, name } =
                &dependency.target
            else {
                return Err(format!(
                    "runtime table dependency {:?} has a non-named target",
                    dependency.record.name
                ));
            };
            exact_targets.extend(
                matching_record_paths(document, |record| {
                    record.name == *name
                        && semantic_record_type(&record.type_name)
                            == semantic_record_type(type_name)
                        && matches!(
                            record.payload,
                            sms_formats::JDramaRecordPayload::Group { .. }
                        )
                })
                .into_iter()
                .map(|path| (resource, path)),
            );
            type_targets.extend(
                matching_record_paths(document, |record| {
                    semantic_record_type(&record.type_name) == semantic_record_type(type_name)
                        && matches!(
                            record.payload,
                            sms_formats::JDramaRecordPayload::Group { .. }
                        )
                })
                .into_iter()
                .map(|path| (resource, path)),
            );
        }

        if same_name.len() != exact.len() {
            return Err(format!(
                "required runtime table record {:?} conflicts with a different semantic type",
                dependency.record.name
            ));
        }
        match exact.len() {
            0 => {}
            1 => continue,
            count => {
                return Err(format!(
                    "required runtime table record {:?} is ambiguous across stage tables ({count} matches)",
                    dependency.record.name
                ));
            }
        }
        // Retail stages do not consistently use the same NameRef label for a
        // table class. Prefer the source label when present, then fall back to
        // the unique semantic table type so authored/blank-stage aliases are
        // repaired without guessing between multiple runtime containers.
        let targets = if exact_targets.is_empty() {
            &type_targets
        } else {
            &exact_targets
        };
        let [(resource, target_path)] = targets.as_slice() else {
            return Err(format!(
                "required runtime table record {:?} has {} matching target containers",
                dependency.record.name,
                targets.len()
            ));
        };
        let document = match resource {
            RuntimeTableResource::Scene => {
                scene_changed = true;
                scene.as_deref_mut().expect("located scene target exists")
            }
            RuntimeTableResource::Tables => {
                tables_changed = true;
                tables.as_deref_mut().expect("located tables target exists")
            }
        };
        let target = jdrama_record_mut(&mut document.root, target_path)
            .ok_or_else(|| "runtime table dependency target path became invalid".to_string())?;
        let sms_formats::JDramaRecordPayload::Group { children, .. } = &mut target.payload else {
            return Err("runtime table dependency target is not a group".to_string());
        };
        children.push(dependency.record);
    }
    Ok((scene_changed, tables_changed))
}

fn parse_catalog_resource(
    resource: &sms_scene::ObjectAuthoringResource,
) -> Result<StageResourceDocument, String> {
    let bytes =
        sms_formats::read_stage_asset_bytes(&resource.source_asset_path).map_err(|error| {
            format!(
                "could not read required retail resource {} from {}: {error}",
                String::from_utf8_lossy(&resource.raw_resource_path),
                resource.source_asset_path.display()
            )
        })?;
    StageResourceDocument::parse_for_path(&resource.raw_resource_path, &bytes).map_err(|error| {
        format!(
            "could not parse required retail resource {} from {}: {error}",
            String::from_utf8_lossy(&resource.raw_resource_path),
            resource.source_asset_path.display()
        )
    })
}

fn semantic_record_type(type_name: &str) -> &str {
    type_name.rsplit("::").next().unwrap_or(type_name)
}

fn authored_shine_fields_mut(
    record: &mut sms_formats::JDramaRecord,
) -> Option<&mut Vec<sms_formats::JDramaField>> {
    if semantic_record_type(&record.type_name) != "Shine" {
        return None;
    }
    match &mut record.payload {
        sms_formats::JDramaRecordPayload::Actor { fields, .. }
        | sms_formats::JDramaRecordPayload::Fields { fields }
        | sms_formats::JDramaRecordPayload::Group { fields, .. } => Some(fields),
        sms_formats::JDramaRecordPayload::Empty => None,
    }
}

fn set_authored_shine_string_field(
    record: &mut sms_formats::JDramaRecord,
    field_name: &str,
    value: String,
) -> Result<(), String> {
    if field_name == "name" {
        record.name = value;
        return Ok(());
    }
    let fields = authored_shine_fields_mut(record)
        .ok_or_else(|| "authored Shine has no editable typed fields".to_string())?;
    let field = fields
        .iter_mut()
        .find(|field| field.name == field_name)
        .ok_or_else(|| format!("authored Shine is missing field '{field_name}'"))?;
    let sms_formats::JDramaFieldValue::String(current) = &mut field.value else {
        return Err(format!(
            "authored Shine field '{field_name}' is not a string"
        ));
    };
    *current = value;
    Ok(())
}

fn set_authored_shine_i32_field(
    record: &mut sms_formats::JDramaRecord,
    field_name: &str,
    value: i32,
) -> Result<(), String> {
    let fields = authored_shine_fields_mut(record)
        .ok_or_else(|| "authored Shine has no editable typed fields".to_string())?;
    let field = fields
        .iter_mut()
        .find(|field| field.name == field_name)
        .ok_or_else(|| format!("authored Shine is missing field '{field_name}'"))?;
    let sms_formats::JDramaFieldValue::I32(current) = &mut field.value else {
        return Err(format!("authored Shine field '{field_name}' is not an i32"));
    };
    *current = value;
    Ok(())
}

fn apply_new_authored_shine_defaults(
    prototype: &mut sms_formats::JDramaRecord,
    object_id: &str,
) -> Result<bool, String> {
    if semantic_record_type(&prototype.type_name) != "Shine" {
        return Ok(false);
    }
    set_authored_shine_string_field(
        prototype,
        "name",
        format!("Graffito-Editor Shine {object_id}"),
    )?;
    set_authored_shine_string_field(prototype, "collection_type", "normal".to_string())?;
    set_authored_shine_i32_field(prototype, "in_stage", -1)?;
    Ok(true)
}

fn migrate_legacy_authored_shine_defaults(object: &mut SceneObject) -> Result<bool, String> {
    if object.authoring_defaults_version >= sms_scene::OBJECT_AUTHORING_DEFAULTS_VERSION {
        return Ok(false);
    }
    let Some(sms_scene::PlacementBinding::Authored(authored)) = object.placement.as_ref() else {
        return Ok(false);
    };
    if semantic_record_type(&authored.prototype.type_name) != "Shine" {
        return Ok(false);
    }

    // Older overlays deserialize their parameter values as clean source values. Read the overlay
    // first so this narrow migration never resets a user's Shine ID or other custom parameters to
    // the retail prototype that originally seeded the authored object.
    let unique_name = format!("Graffito-Editor Shine {}", object.id);
    let collection_type = match object.raw_param("collection_type") {
        Some("demo") | None => "normal".to_string(),
        Some(value) => value.to_string(),
    };
    let in_stage = object
        .raw_param("in_stage")
        .and_then(|value| value.parse::<i32>().ok())
        .filter(|value| matches!(value, -1 | 0))
        .unwrap_or(-1);

    let Some(sms_scene::PlacementBinding::Authored(authored)) = &mut object.placement else {
        unreachable!("authored Shine placement was checked above");
    };
    set_authored_shine_string_field(&mut authored.prototype, "name", unique_name.clone())?;
    set_authored_shine_string_field(
        &mut authored.prototype,
        "collection_type",
        collection_type.clone(),
    )?;
    set_authored_shine_i32_field(&mut authored.prototype, "in_stage", in_stage)?;

    object.insert_source_raw_param("name", unique_name);
    object.insert_source_raw_param("collection_type", collection_type);
    object.insert_source_raw_param("in_stage", in_stage.to_string());
    object.authoring_defaults_version = sms_scene::OBJECT_AUTHORING_DEFAULTS_VERSION;
    Ok(true)
}

fn character_record_equal(
    left: &sms_formats::JDramaRecord,
    right: &sms_formats::JDramaRecord,
) -> bool {
    semantic_record_type(&left.type_name) == semantic_record_type(&right.type_name)
        && left.name == right.name
        && left.payload == right.payload
}

fn name_ref_group_paths(document: &sms_formats::JDramaDocument) -> Vec<Vec<usize>> {
    fn visit(record: &sms_formats::JDramaRecord, path: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
        if semantic_record_type(&record.type_name) == "NameRefGrp" {
            out.push(path.clone());
        }
        if let sms_formats::JDramaRecordPayload::Group { children, .. } = &record.payload {
            for (index, child) in children.iter().enumerate() {
                path.push(index);
                visit(child, path, out);
                path.pop();
            }
        }
    }
    let mut out = Vec::new();
    visit(&document.root, &mut Vec::new(), &mut out);
    out
}

fn jdrama_record_mut<'a>(
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

fn merge_character_table(
    current: Option<StageResourceDocument>,
    registrations: &[sms_formats::JDramaRecord],
) -> Result<(sms_formats::JDramaDocument, bool), String> {
    let (mut document, mut changed) = match current {
        Some(StageResourceDocument::Placement(document)) => (document, false),
        Some(_) => return Err("effective map/tables.bin is not placement data".to_string()),
        None => (
            sms_formats::JDramaDocument {
                root: sms_formats::JDramaRecord {
                    type_name: "NameRefGrp".to_string(),
                    name: "SMS authored character registrations".to_string(),
                    payload: sms_formats::JDramaRecordPayload::Group {
                        fields: Vec::new(),
                        children: Vec::new(),
                    },
                },
            },
            true,
        ),
    };
    let mut paths = name_ref_group_paths(&document);
    if paths.is_empty() {
        let sms_formats::JDramaRecordPayload::Group { children, .. } = &mut document.root.payload
        else {
            return Err("map/tables.bin has no NameRefGrp and its root is not a group".to_string());
        };
        children.push(sms_formats::JDramaRecord {
            type_name: "NameRefGrp".to_string(),
            name: "SMS authored character registrations".to_string(),
            payload: sms_formats::JDramaRecordPayload::Group {
                fields: Vec::new(),
                children: Vec::new(),
            },
        });
        paths = name_ref_group_paths(&document);
        changed = true;
    }
    if paths.len() != 1 {
        return Err(format!(
            "map/tables.bin has {} NameRefGrp records; character insertion is ambiguous",
            paths.len()
        ));
    }
    let target = jdrama_record_mut(&mut document.root, &paths[0])
        .ok_or_else(|| "map/tables.bin NameRefGrp path became invalid".to_string())?;
    let sms_formats::JDramaRecordPayload::Group { children, .. } = &mut target.payload else {
        return Err("map/tables.bin NameRefGrp is not a typed group".to_string());
    };
    let mut registrations = registrations.to_vec();
    registrations.sort_by(|a, b| {
        a.name.cmp(&b.name).then_with(|| {
            semantic_record_type(&a.type_name).cmp(semantic_record_type(&b.type_name))
        })
    });
    for registration in registrations {
        let matches = children
            .iter()
            .filter(|existing| existing.name == registration.name)
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [] => {
                children.push(registration);
                changed = true;
            }
            [existing] if character_record_equal(existing, &registration) => {}
            [_] => {
                return Err(format!(
                    "character registration {:?} conflicts with an existing map/tables.bin record",
                    registration.name
                ));
            }
            _ => {
                return Err(format!(
                    "character registration {:?} is ambiguous because map/tables.bin already contains {} exact-name records",
                    registration.name,
                    matches.len()
                ));
            }
        }
    }
    Ok((document, changed))
}

fn set_typed_vec3_field(
    fields: &mut [sms_formats::JDramaField],
    name: &str,
    value: [f32; 3],
) -> Result<(), String> {
    let field = fields
        .iter_mut()
        .find(|field| field.name == name)
        .ok_or_else(|| format!("typed transform field '{name}' is missing"))?;
    let sms_formats::JDramaFieldValue::Vec3F32(current) = &mut field.value else {
        return Err(format!("typed transform field '{name}' is not a vec3"));
    };
    *current = value;
    Ok(())
}

fn reset_catalog_prototype_transform(
    record: &mut sms_formats::JDramaRecord,
    translation: [f32; 3],
) -> Result<(), String> {
    match &mut record.payload {
        sms_formats::JDramaRecordPayload::Actor { transform, .. } => {
            *transform = sms_formats::JDramaTransform {
                translation,
                rotation: [0.0; 3],
                scale: [1.0; 3],
            };
            Ok(())
        }
        sms_formats::JDramaRecordPayload::Fields { fields } => {
            let (translation_name, rotation_name, scale_name) = match record
                .type_name
                .rsplit("::")
                .next()
                .unwrap_or(&record.type_name)
            {
                "AreaCylinder" => ("center", "authoring_vector", "cylinder_parameters"),
                "Generator" => ("position", "rotation", "authoring_vector"),
                type_name => {
                    return Err(format!(
                        "catalog prototype '{type_name}' has no editable typed transform"
                    ))
                }
            };
            set_typed_vec3_field(fields, translation_name, translation)?;
            set_typed_vec3_field(fields, rotation_name, [0.0; 3])?;
            set_typed_vec3_field(fields, scale_name, [1.0; 3])
        }
        _ => Err(format!(
            "catalog prototype '{}' is not transform-editable",
            record.type_name
        )),
    }
}

fn rewrite_catalog_graph_names_in_record(
    record: &mut sms_formats::JDramaRecord,
    rewrites: &BTreeMap<String, String>,
    applied: &mut BTreeSet<String>,
) -> Result<(), String> {
    let fields = match &mut record.payload {
        sms_formats::JDramaRecordPayload::Actor { fields, .. }
        | sms_formats::JDramaRecordPayload::Fields { fields }
        | sms_formats::JDramaRecordPayload::Group { fields, .. } => fields,
        sms_formats::JDramaRecordPayload::Empty => return Ok(()),
    };
    for field in fields.iter_mut().filter(|field| field.name == "graph_name") {
        let sms_formats::JDramaFieldValue::String(current) = &mut field.value else {
            return Err(format!(
                "typed graph_name field on '{}' is not a string",
                record.type_name
            ));
        };
        if let Some(rewritten) = rewrites.get(current) {
            applied.insert(current.clone());
            *current = rewritten.clone();
        }
    }
    if let sms_formats::JDramaRecordPayload::Group { children, .. } = &mut record.payload {
        for child in children {
            rewrite_catalog_graph_names_in_record(child, rewrites, applied)?;
        }
    }
    Ok(())
}

fn rewrite_catalog_graph_names(
    prototype: &mut sms_formats::JDramaRecord,
    dependencies: &mut [sms_scene::AuthoredPlacementDependency],
    rewrites: &BTreeMap<String, String>,
) -> Result<(), String> {
    if rewrites.is_empty() {
        return Ok(());
    }
    let mut applied = BTreeSet::new();
    rewrite_catalog_graph_names_in_record(prototype, rewrites, &mut applied)?;
    for dependency in dependencies {
        rewrite_catalog_graph_names_in_record(&mut dependency.record, rewrites, &mut applied)?;
    }
    if let Some(missing) = rewrites.keys().find(|name| !applied.contains(*name)) {
        return Err(format!(
            "catalog actor/dependency closure has no typed graph_name value {missing:?} to rewrite"
        ));
    }
    Ok(())
}

fn object_from_catalog_template(
    id: String,
    factory_name: String,
    translation: [f32; 3],
    template: &sms_scene::ObjectAuthoringTemplate,
    graph_name_rewrites: &BTreeMap<String, String>,
) -> Result<SceneObject, String> {
    let mut prototype = template.record.clone();
    let mut dependencies = template
        .dependencies
        .iter()
        .map(|dependency| sms_scene::AuthoredPlacementDependency {
            target: Some(dependency.target.clone()),
            target_group_index: dependency.group_index,
            record: dependency.record.clone(),
        })
        .collect::<Vec<_>>();
    rewrite_catalog_graph_names(&mut prototype, &mut dependencies, graph_name_rewrites)?;
    reset_catalog_prototype_transform(&mut prototype, translation)?;
    let uses_versioned_defaults = apply_new_authored_shine_defaults(&mut prototype, &id)?;
    let mut object = SceneObject::new(id, factory_name);
    object.runtime_references = template
        .runtime_actor_references
        .iter()
        .map(|reference| sms_scene::SceneRuntimeReferenceBinding {
            required_factory_name: reference.required_factory_name.clone(),
            runtime_name: reference.runtime_name.clone(),
            required: reference.required,
            target_object_id: None,
        })
        .collect();
    object.transform = Transform {
        translation,
        ..Transform::default()
    };
    sms_scene::seed_scene_object_parameters(&mut object, &prototype)
        .map_err(|error| error.to_string())?;
    if uses_versioned_defaults {
        object.authoring_defaults_version = sms_scene::OBJECT_AUTHORING_DEFAULTS_VERSION;
    }
    object.placement = Some(sms_scene::PlacementBinding::Authored(
        sms_scene::AuthoredPlacement {
            raw_resource_path: b"map/scene.bin".to_vec(),
            target_group_index: template.group_index,
            prototype,
            dependencies,
        },
    ));
    Ok(object)
}

fn is_shine_object(object: &SceneObject) -> bool {
    object.factory_name == "Shine"
        || object.class_name.as_deref() == Some("TShine")
        || matches!(
            &object.placement,
            Some(sms_scene::PlacementBinding::Authored(authored))
                if semantic_record_type(&authored.prototype.type_name) == "Shine"
        )
}

fn effective_shine_flag(shine_id: i32) -> Option<i32> {
    match shine_id {
        -1 => Some(0),
        0..=119 => Some(shine_id),
        _ => None,
    }
}

fn assign_unique_shine_id_for_spawn(
    object: &mut SceneObject,
    document: &StageDocument,
) -> Result<Option<i32>, String> {
    if !is_shine_object(object) {
        return Ok(None);
    }
    let used_flags = document
        .objects
        .iter()
        .filter(|existing| is_shine_object(existing))
        .filter_map(|existing| existing.raw_param("shine_id"))
        .filter_map(|value| value.parse::<i32>().ok())
        .filter_map(effective_shine_flag)
        .collect::<BTreeSet<_>>();
    let current_id = object
        .raw_param("shine_id")
        .and_then(|value| value.parse::<i32>().ok());
    let selected_id = current_id
        .filter(|value| {
            effective_shine_flag(*value).is_some_and(|flag| !used_flags.contains(&flag))
        })
        .or_else(|| (0..=119).find(|flag| !used_flags.contains(flag)))
        .ok_or_else(|| {
            "all 120 independent persistent Shine save-flag slots are already used in this stage"
                .to_string()
        })?;

    if let Some(sms_scene::PlacementBinding::Authored(authored)) = &mut object.placement {
        set_authored_shine_i32_field(&mut authored.prototype, "shine_id", selected_id)?;
    }
    object.insert_source_raw_param("shine_id", selected_id.to_string());
    Ok(Some(selected_id))
}

fn duplicate_object_for_spawn(
    mut source: SceneObject,
    id: String,
    translation: [f32; 3],
    registry: Option<&ObjectRegistry>,
) -> SceneObject {
    for reference in &mut source.runtime_references {
        reference.target_object_id = None;
    }
    if let Some(registry) = registry {
        source.refresh_manager_capacity_dependencies(registry);
    }
    source.id = id;
    source.source = None;
    source.placement = source
        .placement
        .as_ref()
        .map(sms_scene::PlacementBinding::duplicate_for_new_object);
    let shine_name = if let Some(sms_scene::PlacementBinding::Authored(authored)) =
        &mut source.placement
    {
        if semantic_record_type(&authored.prototype.type_name) == "Shine" {
            let unique_name = format!("Graffito-Editor Shine {}", source.id);
            set_authored_shine_string_field(&mut authored.prototype, "name", unique_name.clone())
                .ok()
                .map(|_| unique_name)
        } else {
            None
        }
    } else {
        None
    };
    if let Some(unique_name) = shine_name {
        source.insert_source_raw_param("name", unique_name);
        source.authoring_defaults_version = sms_scene::OBJECT_AUTHORING_DEFAULTS_VERSION;
    }
    source.transform.translation = translation;
    source
}

fn add_catalog_preview_hint(
    object: &mut SceneObject,
    document: &StageDocument,
    template: &sms_scene::ObjectAuthoringTemplate,
) {
    let (Some(source_path), Some(raw_path)) = (
        document.stage_archive_source_path.as_ref(),
        template.preview_resource_path.as_ref(),
    ) else {
        return;
    };
    object
        .asset_hints
        .retain(|hint| hint.role != AssetRole::PreviewModel);
    object.asset_hints.push(AssetRef {
        path: format!(
            "{}!/{}",
            source_path.display(),
            String::from_utf8_lossy(raw_path).replace('\\', "/")
        ),
        role: AssetRole::PreviewModel,
    });
}
impl SmsEditorApp {
    pub(super) fn validate(&mut self) {
        if let Some(document) = &self.document {
            self.issues = validation_issues_for_preview(document, self.model_preview.as_ref());
            self.log.push(format!(
                "Validation produced {} issue(s).",
                self.issues.len()
            ));
            for issue in &self.issues {
                self.log.push(format!(
                    "{:?} [{}] {}",
                    issue.severity, issue.code, issue.message
                ));
            }
        } else {
            self.log.push("No stage open.".to_string());
        }
    }

    pub(super) fn save_project(&mut self) -> bool {
        let had_selected_model_asset = self.selected_model_asset.is_some();
        let mut validation_error_count = 0usize;
        if let Some(document) = &self.document {
            if !document_uses_selected_base(document, self.base_root.trim()) {
                self.log.push(format!(
                    "Project save blocked: the open stage belongs to '{}', but Base Game Root is '{}'. Open a stage from the selected base before saving its project.",
                    document.base_root.display(),
                    self.base_root.trim()
                ));
                return false;
            }
        }
        if let Some(document) = &self.document {
            self.issues = document.validate();
            validation_error_count = self
                .issues
                .iter()
                .filter(|issue| issue.severity == ValidationSeverity::Error)
                .count();
        }
        if !self.save_selected_model_asset() {
            return false;
        }
        if let Err(error) = self.save_model_instances() {
            self.log.push(format!("Project save failed: {error}"));
            return false;
        }

        let project_root = self.project_root.trim().to_string();
        if project_root.is_empty() {
            self.log.push("Project folder is required.".to_string());
            return false;
        }
        let result = match &mut self.document {
            Some(document) => match document.save_project_folder(PathBuf::from(project_root)) {
                Ok(outcome) => {
                    self.saved_objects = document.objects.clone();
                    self.saved_lighting = document.lighting.clone();
                    self.saved_archive_edits = document.archive_edits.clone();
                    self.document_dirty = false;
                    self.log.push(format!(
                        "Saved editor project with {} file(s).",
                        outcome.manifest.changed_files.len()
                    ));
                    if validation_error_count > 0 {
                        self.log.push(format!(
                            "Saved with {validation_error_count} validation error(s); build and launch remain blocked until they are fixed."
                        ));
                    }
                    for warning in outcome.warnings {
                        self.log.push(format!(
                            "Project save warning (recovery path {}): {}",
                            warning.recovery_path.display(),
                            warning.message
                        ));
                    }
                    true
                }
                Err(err) => {
                    self.log.push(format!("Project save failed: {err}"));
                    false
                }
            },
            None => {
                if had_selected_model_asset {
                    self.log
                        .push("Saved model authoring content (no stage is open).".to_string());
                    true
                } else {
                    self.log.push("No stage open.".to_string());
                    false
                }
            }
        };
        if result && self.current_project.is_some() {
            self.persist_project_settings(false)
        } else {
            result
        }
    }

    pub(super) fn build_game(&mut self) {
        self.start_managed_stage_build(None);
    }

    pub(super) fn build_and_launch(&mut self, mode: DolphinLaunchMode) {
        if mode == DolphinLaunchMode::Editor && self.embedded_dolphin.is_some() {
            self.log
                .push("A Play in Editor session is already running.".to_string());
            return;
        }
        self.start_managed_stage_build(Some(mode));
    }

    fn start_managed_stage_build(&mut self, launch_mode: Option<DolphinLaunchMode>) {
        if self.background_receiver.is_some() {
            self.log
                .push("Another background operation is already running.".to_string());
            return;
        }
        if launch_mode.is_some() && self.dolphin_path.trim().is_empty() {
            self.log
                .push("Launching requires a Dolphin executable.".to_string());
            return;
        }
        if self.document.is_none() {
            self.log.push("No stage open.".to_string());
            return;
        }
        let has_authored_skybox_model = self.model_instances.iter().any(|instance| {
            instance.stage_id.eq_ignore_ascii_case(&self.stage_id)
                && instance.placement.export_mode == sms_authoring::ModelInstanceExportMode::Skybox
        });
        if let Some(error) = self.document.as_ref().and_then(|document| {
            authored_runtime_readiness_error(document, has_authored_skybox_model)
        }) {
            self.log.push(format!("Stage build stopped: {error}"));
            return;
        }
        if self.current_project.is_none() {
            self.log.push(
                "Stage build requires a saved .sms project so the managed game build has a safe owned location."
                    .to_string(),
            );
            return;
        }
        if !self.save_project() {
            self.log
                .push("Stage build stopped because the project could not be saved.".to_string());
            return;
        }
        let Some(document) = self.document.clone() else {
            self.log
                .push("No stage open after project save.".to_string());
            return;
        };
        let Some(project) = self.current_project.clone() else {
            self.log
                .push("Project closed while preparing the game build.".to_string());
            return;
        };
        let model_instances = self
            .model_instances
            .iter()
            .filter(|instance| instance.stage_id.eq_ignore_ascii_case(&document.stage_id))
            .cloned()
            .collect::<Vec<_>>();
        let model_instance_count = model_instances.len();
        let Some(content_root) = self.model_content_root() else {
            self.log
                .push("Stage build blocked: project Content root is unavailable.".to_string());
            return;
        };
        let model_assets = match Self::load_model_asset_snapshot(&content_root, &model_instances) {
            Ok(model_assets) => model_assets,
            Err(error) => {
                self.log.push(format!(
                    "Stage build blocked while snapshotting model assets: {error}"
                ));
                return;
            }
        };

        let (sender, receiver) = mpsc::channel();
        let build_cancel = Arc::new(AtomicBool::new(false));
        let task_cancel = Arc::clone(&build_cancel);
        thread::spawn(move || {
            let result = managed_build::check_cancelled(&task_cancel)
                .and_then(|()| {
                    SmsEditorApp::stage_edits_with_model_instances_from_snapshot_cancellable(
                        &model_assets,
                        &model_instances,
                        &document.archive_edits,
                        document.stage_archive.as_ref(),
                        document.registry.as_ref(),
                        &task_cancel,
                    )
                })
                .and_then(|edits| {
                    managed_build::check_cancelled(&task_cancel)?;
                    let mut finalized_document = document.clone();
                    finalized_document.archive_edits = edits.clone();
                    finalized_document
                        .refresh_goop_stale_status()
                        .map_err(|error| error.to_string())?;
                    if let Some(goop) = &finalized_document.goop_authoring {
                        goop.validate().map_err(|error| {
                            format!("final terrain/goop validation failed: {error}")
                        })?;
                    }
                    finalized_document
                        .build_stage_archive_with_edits(&edits)
                        .map_err(|error| error.to_string())
                });
            let result = result.and_then(|archive_bytes| {
                managed_build::check_cancelled(&task_cancel)?;
                managed_build::build_managed_game(&project, &document, &archive_bytes, &task_cancel)
            });
            if let Some(mode) = launch_mode {
                let result = result.and_then(|build| {
                    managed_build::prepare_managed_game_launch(build, &task_cancel)
                });
                let _ = sender.send(BackgroundResult::BuildAndRun { mode, result });
            } else {
                let _ = sender.send(BackgroundResult::Build(result));
            }
        });
        self.background_receiver = Some(receiver);
        self.active_build_cancel = Some(build_cancel);
        self.background_label = Some(match launch_mode {
            Some(DolphinLaunchMode::Editor) => "Preparing Play in Editor".to_string(),
            Some(DolphinLaunchMode::External) => {
                "Preparing and launching current scene".to_string()
            }
            None => "Building managed game".to_string(),
        });
        self.log.push(format!(
            "Building stage from semantic documents and {} placed model instance(s) into the project's managed game directory{}...",
            model_instance_count,
            match launch_mode {
                Some(DolphinLaunchMode::Editor) => {
                    ", then preparing direct scene boot for Play in Editor"
                }
                Some(DolphinLaunchMode::External) => {
                    ", then preparing direct scene boot in Dolphin"
                }
                None => "",
            }
        ));
    }

    pub(super) fn cancel_active_build(&mut self) {
        let Some(cancel) = &self.active_build_cancel else {
            return;
        };
        if !cancel.swap(true, Ordering::AcqRel) {
            self.log.push(if self.background_label.as_deref() == Some("Rebuilding goopmaps") {
                "Cancelling goop rebuild; the current layer will stop at its next checked chunk."
                    .to_string()
            } else {
                "Cancelling managed game build; the current file operation will finish or stop at its next checked chunk."
                    .to_string()
            });
        }
    }

    pub(super) fn launch_managed_dolphin(
        &mut self,
        outcome: &managed_build::ManagedGameLaunchOutcome,
        mode: DolphinLaunchMode,
        frame: Option<&eframe::Frame>,
    ) {
        if self.dolphin_path.trim().is_empty() {
            self.log.push(
                "Managed game build completed, but Dolphin executable is not configured."
                    .to_string(),
            );
            return;
        }
        if !managed_dolphin_exec_is_directory_main(
            &outcome.run.run_root,
            &outcome.direct_boot.launch_dol,
        ) {
            self.log.push(format!(
                "Refusing managed Dolphin launch because its executable must be the managed directory mount point '{}': got '{}'.",
                outcome.run.run_root.join("sys").join("main.dol").display(),
                outcome.direct_boot.launch_dol.display()
            ));
            return;
        }

        let editor_host = if mode == DolphinLaunchMode::Editor {
            let Some(frame) = frame else {
                self.log
                    .push("Play in Editor could not access the native editor window.".to_string());
                return;
            };
            match play_in_editor::EditorHostWindow::from_frame(frame) {
                Ok(host) => Some(host),
                Err(error) => {
                    self.log
                        .push(format!("Play in Editor is unavailable: {error}"));
                    return;
                }
            }
        } else {
            None
        };

        let mut command = Command::new(&self.dolphin_path);
        let configured_user_dir =
            Self::configure_dolphin_user_directory(&mut command, &self.dolphin_user_dir);
        if mode == DolphinLaunchMode::Editor {
            Self::configure_play_in_editor_input(&mut command);
        }
        command
            .current_dir(&outcome.run.run_root)
            .arg("-b")
            .arg("-e")
            .arg(&outcome.direct_boot.launch_dol);

        let profile = configured_user_dir
            .as_ref()
            .map(|path| format!("configured Dolphin user directory '{}'", path.display()))
            .unwrap_or_else(|| "Dolphin's normal user profile".to_string());
        match (mode, command.spawn()) {
            (DolphinLaunchMode::External, Ok(_)) => self.log.push(format!(
                "Launched Dolphin directly into '{}' (runtime area {}, scenario {}) with managed game '{}' using {}.",
                outcome.direct_boot.target.archive_name,
                outcome.direct_boot.target.area_index,
                outcome.direct_boot.target.scenario_index,
                outcome.direct_boot.launch_dol.display(),
                profile,
            )),
            (DolphinLaunchMode::Editor, Ok(child)) => {
                self.embedded_dolphin = Some(play_in_editor::EmbeddedDolphinSession::new(
                    child,
                    editor_host.expect("editor host was validated before spawning Dolphin"),
                ));
                self.log.push(format!(
                    "Started Play in Editor for '{}' (runtime area {}, scenario {}) using {}; waiting for Dolphin's render window.",
                    outcome.direct_boot.target.archive_name,
                    outcome.direct_boot.target.area_index,
                    outcome.direct_boot.target.scenario_index,
                    profile,
                ));
            }
            (_, Err(error)) => self
                .log
                .push(format!("Failed to launch managed Dolphin build: {error}")),
        }
    }

    pub(super) fn poll_embedded_dolphin(&mut self, ctx: &egui::Context) {
        let event = self
            .embedded_dolphin
            .as_mut()
            .map(play_in_editor::EmbeddedDolphinSession::poll);
        match event {
            Some(Ok(Some(play_in_editor::EmbeddedDolphinEvent::Attached))) => {
                self.log
                    .push("Dolphin is now running inside the editor viewport.".to_string());
            }
            Some(Ok(Some(play_in_editor::EmbeddedDolphinEvent::Exited))) => {
                self.embedded_dolphin = None;
                self.log.push("Play in Editor session ended.".to_string());
            }
            Some(Err(error)) => {
                self.embedded_dolphin = None;
                self.log.push(format!(
                    "Play in Editor could not embed Dolphin; the managed Dolphin process was stopped: {error}"
                ));
            }
            Some(Ok(None)) => {
                ctx.request_repaint_after(Duration::from_millis(50));
            }
            None => {}
        }
    }

    pub(super) fn embedded_dolphin_viewport(&mut self, ui: &mut egui::Ui, _frame: &eframe::Frame) {
        let available = ui.available_size();
        let size = egui::vec2(available.x.max(240.0), available.y.max(240.0));
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        ui.painter()
            .rect_filled(rect, 0.0, egui::Color32::from_rgb(8, 10, 12));

        let attached = self
            .embedded_dolphin
            .as_ref()
            .is_some_and(play_in_editor::EmbeddedDolphinSession::is_attached);
        if !attached {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Starting Dolphin...",
                egui::FontId::proportional(18.0),
                egui::Color32::from_gray(190),
            );
        }

        let position_result = self
            .embedded_dolphin
            .as_mut()
            .map(|session| session.set_viewport_bounds(rect, ui.ctx().pixels_per_point()));
        if let Some(Err(error)) = position_result {
            self.embedded_dolphin = None;
            self.log.push(format!(
                "Play in Editor lost the embedded Dolphin window; the managed Dolphin process was stopped: {error}"
            ));
        }
    }

    pub(super) fn stop_play_in_editor(&mut self) {
        let Some(session) = self.embedded_dolphin.take() else {
            return;
        };
        match session.stop() {
            Ok(()) => self.log.push("Stopped Play in Editor.".to_string()),
            Err(error) => self.log.push(error),
        }
    }

    pub(super) fn launch_dolphin(&mut self) {
        if self.dolphin_path.trim().is_empty() || self.game_path.trim().is_empty() {
            self.log
                .push("Dolphin executable and game path are required.".to_string());
            return;
        }

        let mut command = Command::new(&self.dolphin_path);
        Self::configure_dolphin_user_directory(&mut command, &self.dolphin_user_dir);
        command.arg("-b").arg("-e").arg(&self.game_path);

        match command.spawn() {
            Ok(_) => self.log.push("Launched Dolphin.".to_string()),
            Err(err) => self.log.push(format!("Failed to launch Dolphin: {err}")),
        }
    }

    pub(super) fn configure_dolphin_user_directory(
        command: &mut Command,
        configured: &str,
    ) -> Option<PathBuf> {
        let configured = configured.trim();
        if configured.is_empty() {
            return None;
        }

        let path = PathBuf::from(configured);
        command.arg("-u").arg(&path);
        Some(path)
    }

    pub(super) fn configure_play_in_editor_input(command: &mut Command) {
        command
            .arg("-C")
            .arg("Dolphin.Interface.PauseOnFocusLost=False")
            .arg("-C")
            .arg("Dolphin.Input.BackgroundInput=True");
    }

    fn next_available_object_id(&self) -> Option<(String, u32)> {
        let document = self.document.as_ref()?;
        let stage_id = sanitize_id(&self.stage_id);
        let mut serial = self.next_object_serial.max(1);
        loop {
            let id = format!("{stage_id}-obj-{serial:04}");
            if !document.objects.iter().any(|object| object.id == id) {
                return Some((id, serial.saturating_add(1)));
            }
            serial = serial.checked_add(1)?;
        }
    }

    pub(super) fn reconcile_loaded_authored_catalog_resources(&mut self) {
        let mut migrated_shines = Vec::new();
        if let Some(document) = &mut self.document {
            for object in &mut document.objects {
                match migrate_legacy_authored_shine_defaults(object) {
                    Ok(true) => migrated_shines.push(object.id.clone()),
                    Ok(false) => {}
                    Err(error) => self.log.push(format!(
                        "Could not update legacy authored Shine '{}': {error}",
                        object.id
                    )),
                }
            }
        }
        let authored_factories = self
            .document
            .as_ref()
            .map(|document| {
                document
                    .objects
                    .iter()
                    .filter(|object| {
                        matches!(
                            object.placement,
                            Some(sms_scene::PlacementBinding::Authored(_))
                        )
                    })
                    .map(|object| object.factory_name.clone())
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        let templates = authored_factories
            .iter()
            .filter_map(|factory_name| self.object_authoring_catalog.find(factory_name).cloned())
            .collect::<Vec<_>>();
        let repair = self
            .document
            .as_mut()
            .map(|document| repair_authored_catalog_resources(document, &templates))
            .unwrap_or_default();
        let runtime_goop_source = self
            .document
            .as_ref()
            .and_then(|document| document.goop_authoring.as_ref())
            .and_then(|authoring| {
                authoring
                    .layers
                    .iter()
                    .filter_map(|layer| layer.style_source.as_ref())
                    .next()
            })
            .cloned();
        let runtime_goop_repair = runtime_goop_source
            .and_then(|source| {
                self.scene_archives
                    .iter()
                    .find(|archive| archive.stage_id == source.stage_id)
                    .map(|archive| (source, archive.path.clone()))
            })
            .map(|(source, archive_path)| {
                self.document
                    .as_mut()
                    .map(|document| {
                        sync_runtime_actor_goop_textures_from_source(
                            document,
                            &source,
                            &archive_path,
                        )
                    })
                    .unwrap_or(Ok(0))
            })
            .transpose();
        let runtime_goop_writes = match runtime_goop_repair {
            Ok(Some(writes)) => writes,
            Ok(None) => 0,
            Err(error) => {
                self.log.push(format!(
                    "Could not repair the scene-wide actor goop texture: {error}"
                ));
                0
            }
        };

        for error in repair.errors {
            self.log.push(format!(
                "Could not repair authored object runtime resources: {error}"
            ));
        }
        if repair.resource_writes == 0
            && repair.runtime_links_added == 0
            && runtime_goop_writes == 0
            && migrated_shines.is_empty()
        {
            return;
        }
        if let (Some(document), Some(registry)) = (&mut self.document, self.registry.clone()) {
            document.set_registry(registry);
        }
        self.document_dirty = self.document.as_ref().is_some_and(|document| {
            stage_document_differs_from_saved(
                document,
                &self.saved_objects,
                &self.saved_lighting,
                &self.saved_archive_edits,
            )
        });
        self.flush_document_change();
        self.rebuild_model_preview_from_document();
        if repair.resource_writes > 0 {
            self.log.push(format!(
                "Repaired {} missing runtime resource(s) for existing authored class(es): {}. Save the project before launching.",
                repair.resource_writes,
                repair.repaired_factories.join(", ")
            ));
        }
        if repair.runtime_links_added > 0 {
            self.log.push(format!(
                "Reconciled {} decomp-derived runtime actor link(s) for existing authored class(es): {}. Select required targets in the inspector and save before launching.",
                repair.runtime_links_added,
                repair.repaired_factories.join(", ")
            ));
        }
        if runtime_goop_writes > 0 {
            self.log.push(format!(
                "Updated {runtime_goop_writes} scene-wide actor goop texture(s) from the primary goop style. Save the project before launching."
            ));
        }
        if !migrated_shines.is_empty() {
            self.log.push(format!(
                "Updated {} legacy authored Shine(s) to safe standalone spawn defaults: {}. Save the project before launching.",
                migrated_shines.len(),
                migrated_shines.join(", ")
            ));
        }
    }

    pub(super) fn spawn_object_at(&mut self, factory_name: String, translation: [f32; 3]) {
        if self
            .registry
            .as_ref()
            .and_then(|registry| registry.find_object(&factory_name))
            .is_some_and(|definition| definition.unsafe_to_edit)
        {
            self.log.push(format!(
                "Could not place class '{factory_name}': the schema marks it unsafe to edit."
            ));
            return;
        }
        let Some(document) = self.document.as_ref() else {
            self.log
                .push("Open a stage before placing an object class.".to_string());
            return;
        };
        let Some((id, next_object_serial)) = self.next_available_object_id() else {
            self.log.push(format!(
                "Could not place class '{factory_name}': no unique editor object id is available."
            ));
            return;
        };
        let same_stage_template = document
            .objects
            .iter()
            .find(|object| object.factory_name == factory_name && object.placement.is_some())
            .cloned();
        let catalog_template = same_stage_template
            .is_none()
            .then(|| self.object_authoring_catalog.find(&factory_name).cloned())
            .flatten();
        let mut catalog_log = None;
        let mut catalog_resource_deltas = Vec::new();

        let mut object = if factory_name == "Mario" {
            if document
                .objects
                .iter()
                .any(|object| object.factory_name == "Mario")
            {
                self.log.push(
                    "This stage already has a Mario player placement; move the existing Mario instead."
                        .to_string(),
                );
                return;
            }
            let address = match ensure_authored_mario_placement(
                self.document.as_mut().expect("document was checked above"),
                translation,
            ) {
                Ok(address) => address,
                Err(error) => {
                    self.log
                        .push(format!("Could not place the Mario class: {error}"));
                    return;
                }
            };
            let mut object = SceneObject::new(id.clone(), factory_name.clone());
            object.placement = Some(sms_scene::PlacementBinding::Existing(address));
            object
        } else if factory_name == "Sky" {
            if document
                .objects
                .iter()
                .any(|object| object.factory_name == "Sky")
            {
                self.log.push(
                    "This stage already has its stage-global Sky actor; move the existing Sky instead."
                        .to_string(),
                );
                return;
            }
            let address = match ensure_sky_placement(
                self.document.as_mut().expect("document was checked above"),
                translation,
            ) {
                Ok(address) => address,
                Err(error) => {
                    self.log
                        .push(format!("Could not place the Sky class: {error}"));
                    return;
                }
            };
            let mut object = SceneObject::new(id.clone(), factory_name.clone());
            object.placement = Some(sms_scene::PlacementBinding::Existing(address));
            object
        } else if let Some(template) = same_stage_template {
            duplicate_object_for_spawn(template, id.clone(), translation, self.registry.as_ref())
        } else if let Some(template) = catalog_template {
            let preflight = match preflight_catalog_resources(
                self.document.as_ref().expect("document was checked above"),
                &template,
            ) {
                Ok(preflight) => preflight,
                Err(error) => {
                    self.log.push(format!(
                        "Could not place class '{factory_name}' from the retail catalog: {error}"
                    ));
                    return;
                }
            };
            let mut object = match object_from_catalog_template(
                id.clone(),
                factory_name.clone(),
                translation,
                &template,
                &preflight.graph_name_rewrites,
            ) {
                Ok(object) => object,
                Err(error) => {
                    self.log.push(format!(
                        "Could not place class '{factory_name}' from the retail catalog: {error}"
                    ));
                    return;
                }
            };
            add_catalog_preview_hint(
                &mut object,
                self.document.as_ref().expect("document was checked above"),
                &template,
            );
            let authored_resource_count = preflight.writes.len();
            let reused_resource_count = preflight.reused_existing_resources;
            let upgraded_bootstrap_count = preflight.upgraded_bootstrap_proxies;
            catalog_resource_deltas = catalog_resource_edit_deltas(
                self.document.as_ref().expect("document was checked above"),
                preflight.writes,
            );
            catalog_log = Some(format!(
                "Placed '{factory_name}' from retail stage '{}': {} manager/support dependency record(s), {} stage-local character registration(s), {} fixed runtime table dependency record(s), {} required graph(s), {} catalog resource(s), {} existing target resource(s) reused, {} bootstrap proxy resource(s) upgraded, {} resource write(s).",
                template.source_stage,
                template.dependencies.len(),
                template.character_records.len(),
                template.table_dependencies.len(),
                template.required_graph_names.len(),
                template.resources.len(),
                reused_resource_count,
                upgraded_bootstrap_count,
                authored_resource_count
            ));
            object
        } else {
            self.log.push(format!(
                "Could not place class '{factory_name}': this stage has no typed instance to duplicate and the retail authoring catalog has no template."
            ));
            return;
        };

        object.transform.translation = translation;
        if let Err(error) = assign_unique_shine_id_for_spawn(
            &mut object,
            self.document.as_ref().expect("document was checked above"),
        ) {
            self.log
                .push(format!("Could not place class '{factory_name}': {error}."));
            return;
        }
        if let Some(schema) = self
            .registry
            .as_ref()
            .and_then(|registry| registry.find_object(&factory_name))
        {
            object.class_name = Some(schema.class_name.clone());
            if !object
                .asset_hints
                .iter()
                .any(|hint| hint.role == AssetRole::PreviewModel)
            {
                if let Some(model) = &schema.preview_model {
                    object.asset_hints.push(AssetRef {
                        path: model.clone(),
                        role: AssetRole::PreviewModel,
                    });
                }
            }
        }

        self.next_object_serial = next_object_serial;
        let index = self
            .document
            .as_ref()
            .map_or(0, |document| document.objects.len());
        self.apply_object_edit(
            "Added object",
            ObjectUndoRecord {
                deltas: vec![ObjectDelta::Insert { index, object }],
                route_delta: None,
                resource_deltas: catalog_resource_deltas,
            },
        );
        self.selected_object_id = Some(id);
        if let Some(message) = catalog_log {
            self.log.push(message);
        }
        self.rebuild_model_preview_from_document();
    }

    pub(super) fn can_spawn_factory(&self, factory_name: &str) -> bool {
        let Some(document) = self.document.as_ref() else {
            return false;
        };
        if self
            .registry
            .as_ref()
            .and_then(|registry| registry.find_object(factory_name))
            .is_some_and(|definition| definition.unsafe_to_edit)
        {
            return false;
        }
        if factory_name == "Mario" {
            let authored_blank = document.stage_archive.as_ref().is_some_and(|archive| {
                matches!(archive.origin(), sms_scene::StageOrigin::Blank { .. })
            });
            return authored_blank
                && !document
                    .objects
                    .iter()
                    .any(|object| object.factory_name == "Mario");
        }
        if factory_name == "Sky" {
            return document.stage_archive.is_some()
                && !document
                    .objects
                    .iter()
                    .any(|object| object.factory_name == "Sky");
        }
        document
            .objects
            .iter()
            .any(|object| object.factory_name == factory_name && object.placement.is_some())
            || self.object_authoring_catalog.find(factory_name).is_some()
    }

    pub(super) fn update_stage_lighting(&mut self, lighting: sms_scene::StageLighting) {
        let Some(document) = self.document.as_mut() else {
            return;
        };
        if document.lighting == lighting {
            return;
        }
        document.lighting = lighting;
        self.document_dirty = stage_document_differs_from_saved(
            document,
            &self.saved_objects,
            &self.saved_lighting,
            &self.saved_archive_edits,
        );
        self.flush_document_change();
        self.rebuild_gpu_viewport_scene();
        self.clear_viewport_preview_cache();
    }

    pub(super) fn duplicate_selected(&mut self) {
        let Some(source) = self.selected_object().cloned() else {
            return;
        };
        let Some((id, next_object_serial)) = self.next_available_object_id() else {
            self.log.push(
                "Could not duplicate object: no unique editor object id is available.".to_string(),
            );
            return;
        };

        let mut translation = source.transform.translation;
        translation[0] += self.snap_translation.max(25.0);
        translation[2] += self.snap_translation.max(25.0);
        let mut clone =
            duplicate_object_for_spawn(source, id.clone(), translation, self.registry.as_ref());
        if let Err(error) = assign_unique_shine_id_for_spawn(
            &mut clone,
            self.document
                .as_ref()
                .expect("selected object belongs to a document"),
        ) {
            self.log
                .push(format!("Could not duplicate Shine: {error}."));
            return;
        }
        self.next_object_serial = next_object_serial;
        let index = self
            .document
            .as_ref()
            .map_or(0, |document| document.objects.len());
        self.apply_object_edit(
            "Duplicated object",
            ObjectUndoRecord {
                deltas: vec![ObjectDelta::Insert {
                    index,
                    object: clone,
                }],
                resource_deltas: Vec::new(),
                route_delta: None,
            },
        );
        self.selected_object_id = Some(id);
        self.rebuild_model_preview_from_document();
    }

    pub(super) fn delete_selected(&mut self) {
        if self.delete_selected_model_instance() {
            return;
        }
        let Some(selected_id) = self.selected_object_id.clone() else {
            return;
        };
        let Some((index, object)) = self.document.as_ref().and_then(|document| {
            document
                .objects
                .iter()
                .enumerate()
                .find(|(_, object)| object.id == selected_id)
                .map(|(index, object)| (index, object.clone()))
        }) else {
            return;
        };
        self.apply_object_edit(
            "Deleted object",
            ObjectUndoRecord {
                deltas: vec![ObjectDelta::Remove { index, object }],
                resource_deltas: Vec::new(),
                route_delta: None,
            },
        );
        self.selected_object_id = None;
        self.rebuild_model_preview_from_document();
    }

    pub(super) fn update_selected_transform(&mut self, transform: Transform) {
        let Some(selected_id) = self.selected_object_id.clone() else {
            return;
        };
        let Some(old_transform) = self.selected_object().map(|object| object.transform) else {
            return;
        };
        if old_transform == transform {
            return;
        }
        let Some(before) = self.selected_object().cloned() else {
            return;
        };
        let mut after = before.clone();
        after.transform = transform;
        self.apply_object_edit(
            "Updated transform",
            ObjectUndoRecord {
                deltas: vec![ObjectDelta::Update {
                    before: Box::new(before),
                    after: Box::new(after),
                }],
                resource_deltas: Vec::new(),
                route_delta: None,
            },
        );
        let has_rendered_model = self
            .model_preview
            .as_ref()
            .is_some_and(|preview| preview.object_model_indices.contains_key(&selected_id));
        if !has_rendered_model {
            return;
        }
        if let Some(triangle_ranges) =
            self.update_object_preview_transform(&selected_id, old_transform, transform)
        {
            if let (Some(gpu_viewport), Some(preview)) =
                (self.gpu_viewport.as_ref(), self.model_preview.as_ref())
            {
                gpu_viewport.update_geometry(preview, &triangle_ranges);
            }
            self.clear_viewport_preview_cache();
        } else {
            self.rebuild_model_preview_from_document();
        }
    }

    pub(super) fn update_selected_parameter(
        &mut self,
        key: impl Into<String>,
        raw: impl Into<String>,
    ) {
        let key = key.into();
        let raw = raw.into();
        let Some(before) = self.selected_object().cloned() else {
            return;
        };
        let descriptors = match self
            .document
            .as_ref()
            .expect("a selected object belongs to an open document")
            .editable_parameters_for_object(&before)
        {
            Ok(descriptors) => descriptors,
            Err(error) => {
                self.log.push(format!(
                    "Could not edit parameter '{key}' on '{}': {error}",
                    object_display_name(&before)
                ));
                return;
            }
        };
        let Some(descriptor) = descriptors.iter().find(|descriptor| descriptor.key == key) else {
            self.log.push(format!(
                "Could not edit parameter '{key}' on '{}': it is not a canonical typed field.",
                object_display_name(&before)
            ));
            return;
        };
        if let Some(reason) = descriptor.read_only_reason.as_deref() {
            self.log.push(format!(
                "Could not edit parameter '{key}' on '{}': {reason}",
                object_display_name(&before)
            ));
            return;
        }
        if descriptor.raw_value == raw {
            return;
        }

        let old_manager_name = (key == "manager_name")
            .then(|| before.raw_param("manager_name").map(str::to_owned))
            .flatten();
        let mut after = before.clone();
        after.set_raw_param(key.clone(), raw.clone());
        sms_scene::sync_scene_object_parameter_aliases(&mut after);

        if let Some(sms_scene::PlacementBinding::Authored(authored)) = after.placement.as_ref() {
            let mut validation_record = authored.prototype.clone();
            if let Err(error) =
                sms_scene::apply_dirty_object_parameter_edits(&mut validation_record, &after)
            {
                self.log.push(format!(
                    "Could not edit parameter '{key}' on '{}': {error}",
                    object_display_name(&before)
                ));
                return;
            }
        }

        if let (Some(old_manager_name), Some(sms_scene::PlacementBinding::Authored(authored))) =
            (old_manager_name, after.placement.as_mut())
        {
            let mut renamed = 0;
            for dependency in &mut authored.dependencies {
                if dependency.record.name == old_manager_name {
                    dependency.record.name = raw.clone();
                    renamed += 1;
                }
            }
            if renamed == 0 {
                self.log.push(format!(
                    "Parameter update warning for '{}': no authored dependency matched manager name {:?}.",
                    object_display_name(&before), old_manager_name
                ));
            }
        }
        self.apply_object_edit(
            "Updated object parameter",
            ObjectUndoRecord {
                route_delta: None,
                deltas: vec![ObjectDelta::Update {
                    before: Box::new(before),
                    after: Box::new(after),
                }],
                resource_deltas: Vec::new(),
            },
        );
    }
    pub(super) fn update_selected_runtime_reference(
        &mut self,
        reference_index: usize,
        target_object_id: Option<String>,
    ) {
        let Some(before) = self.selected_object().cloned() else {
            return;
        };
        let Some(reference) = before.runtime_references.get(reference_index) else {
            return;
        };
        if let Some(target_id) = target_object_id.as_deref() {
            let Some(target) = self.document.as_ref().and_then(|document| {
                document
                    .objects
                    .iter()
                    .find(|object| object.id == target_id)
            }) else {
                self.log.push(format!(
                    "Could not bind runtime reference {:?}: target '{}' no longer exists.",
                    reference.runtime_name, target_id
                ));
                return;
            };
            if target.factory_name != reference.required_factory_name {
                self.log.push(format!(
                    "Could not bind runtime reference {:?}: '{}' is {}, expected {}.",
                    reference.runtime_name,
                    target.id,
                    target.factory_name,
                    reference.required_factory_name
                ));
                return;
            }
            if let Some(conflicting) = self.document.as_ref().and_then(|document| {
                document
                    .objects
                    .iter()
                    .flat_map(|owner| owner.runtime_references.iter())
                    .find(|binding| {
                        binding.target_object_id.as_deref() == Some(target_id)
                            && binding.runtime_name != reference.runtime_name
                    })
            }) {
                self.log.push(format!(
                    "Could not bind runtime reference {:?}: '{}' already satisfies incompatible runtime lookup {:?}. Choose another {} actor or unassign the optional link.",
                    reference.runtime_name,
                    target_id,
                    conflicting.runtime_name,
                    reference.required_factory_name
                ));
                return;
            }
        }
        if reference.target_object_id == target_object_id {
            return;
        }

        let mut after = before.clone();
        after.runtime_references[reference_index].target_object_id = target_object_id;
        self.apply_object_edit(
            "Updated runtime actor link",
            ObjectUndoRecord {
                deltas: vec![ObjectDelta::Update {
                    before: Box::new(before),
                    after: Box::new(after),
                }],
                resource_deltas: Vec::new(),
                route_delta: None,
            },
        );
    }

    #[cfg(test)]
    pub(super) fn mutate_document(&mut self, label: &str, mutate: impl FnOnce(&mut StageDocument)) {
        let in_transaction = self.undo_transaction.is_some();
        let before = if in_transaction {
            None
        } else {
            self.document
                .as_ref()
                .map(|document| (document.objects.clone(), document.archive_edits.clone()))
        };
        if let Some(document) = &mut self.document {
            mutate(document);
        }
        let undo_record = if in_transaction {
            None
        } else {
            before.as_ref().zip(self.document.as_ref()).map(
                |((before_objects, before_archive_edits), document)| {
                    ObjectUndoRecord::between(
                        before_objects,
                        &document.objects,
                        before_archive_edits,
                        &document.archive_edits,
                    )
                },
            )
        };
        if let Some(record) = undo_record {
            self.push_undo_record(record);
        }
        self.document_dirty = if in_transaction {
            true
        } else {
            self.document.as_ref().is_some_and(|document| {
                stage_document_differs_from_saved(
                    document,
                    &self.saved_objects,
                    &self.saved_lighting,
                    &self.saved_archive_edits,
                )
            })
        };
        if !in_transaction {
            self.flush_document_change();
            self.log.push(format!("{label}."));
        }
    }

    fn apply_object_edit(&mut self, label: &str, record: ObjectUndoRecord) {
        if record.is_empty() {
            return;
        }
        let in_transaction = self.undo_transaction.is_some();
        let registry = (!record.resource_deltas.is_empty())
            .then(|| self.registry.clone())
            .flatten();
        let Some(document) = &mut self.document else {
            return;
        };
        record.apply_forward(document);
        if let Some(registry) = registry {
            document.set_registry(registry);
        }
        self.document_dirty = if in_transaction {
            true
        } else {
            stage_document_differs_from_saved(
                document,
                &self.saved_objects,
                &self.saved_lighting,
                &self.saved_archive_edits,
            )
        };
        if !in_transaction {
            self.push_undo_record(record);
            self.flush_document_change();
            self.log.push(format!("{label}."));
        }
    }

    pub(super) fn flush_document_change(&mut self) {
        let Some(document) = &mut self.document else {
            return;
        };
        if let Err(err) = document.queue_editor_overlay_change() {
            self.log.push(format!("Scene overlay update failed: {err}"));
        }
        self.issues = validation_issues_for_preview(document, self.model_preview.as_ref());
    }

    pub(super) fn begin_undo_transaction(&mut self) {
        self.begin_object_undo_transaction(ObjectUndoTransactionKind::Transform);
    }

    pub(super) fn begin_parameter_undo_transaction(&mut self) {
        self.begin_object_undo_transaction(ObjectUndoTransactionKind::Parameter);
    }

    fn begin_object_undo_transaction(&mut self, kind: ObjectUndoTransactionKind) {
        if self.undo_transaction.is_none() {
            self.undo_transaction = self.selected_object_id.as_ref().and_then(|selected_id| {
                self.document.as_ref().and_then(|document| {
                    document
                        .objects
                        .iter()
                        .enumerate()
                        .find(|(_, object)| &object.id == selected_id)
                        .map(|(index, object)| ObjectUndoTransaction {
                            index,
                            before: object.clone(),
                            kind,
                        })
                })
            });
        }
    }

    pub(super) fn finish_pointer_undo_transaction_if_released(&mut self, primary_down: bool) {
        let is_transform_transaction = self
            .undo_transaction
            .as_ref()
            .is_some_and(|transaction| transaction.kind == ObjectUndoTransactionKind::Transform);
        if is_transform_transaction && !primary_down {
            self.commit_undo_transaction("Updated transform");
        }
    }

    pub(super) fn commit_undo_transaction(&mut self, label: &str) {
        let Some(transaction) = self.undo_transaction.take() else {
            return;
        };
        if let Some(preview) = &mut self.model_preview {
            recompute_model_preview_bounds(preview);
        }
        let record = self.document.as_ref().map(|document| {
            if let Some(after) = document
                .objects
                .iter()
                .find(|object| object.id == transaction.before.id)
            {
                ObjectUndoRecord {
                    deltas: (after != &transaction.before)
                        .then(|| ObjectDelta::Update {
                            before: Box::new(transaction.before.clone()),
                            after: Box::new(after.clone()),
                        })
                        .into_iter()
                        .collect(),
                    resource_deltas: Vec::new(),
                    route_delta: None,
                }
            } else {
                ObjectUndoRecord {
                    deltas: vec![ObjectDelta::Remove {
                        index: transaction.index,
                        object: transaction.before.clone(),
                    }],
                    resource_deltas: Vec::new(),
                    route_delta: None,
                }
            }
        });
        self.document_dirty = self.document.as_ref().is_some_and(|document| {
            stage_document_differs_from_saved(
                document,
                &self.saved_objects,
                &self.saved_lighting,
                &self.saved_archive_edits,
            )
        });
        let Some(record) = record.filter(|record| !record.is_empty()) else {
            return;
        };
        self.push_undo_record(record);
        self.flush_document_change();
        self.log.push(format!("{label}."));
    }

    pub(super) fn push_undo_record(&mut self, record: ObjectUndoRecord) {
        if record.is_empty() {
            return;
        }
        self.undo_stack.push_back(record);
        if self.undo_stack.len() > 80 {
            self.undo_stack.pop_front();
        }
        self.redo_stack.clear();
    }

    pub(super) fn undo(&mut self) {
        if self.tool == EditorTool::Goop && self.undo_goop() {
            return;
        }
        if (self.selected_model_instance_id.is_some()
            || (self.selected_object_id.is_none()
                && self.selected_model_document.is_none()
                && !self.model_instance_undo_stack.is_empty()))
            && self.undo_model_instance()
        {
            return;
        }
        if self.selected_model_document.is_some() && self.undo_model_asset() {
            return;
        }
        if self.document.is_none() {
            return;
        }
        let Some(record) = self.undo_stack.pop_back() else {
            return;
        };
        let registry = (!record.resource_deltas.is_empty())
            .then(|| self.registry.clone())
            .flatten();
        if let Some(document) = &mut self.document {
            record.apply_reverse(document);
            if let Some(registry) = registry {
                document.set_registry(registry);
            }
            self.document_dirty = stage_document_differs_from_saved(
                document,
                &self.saved_objects,
                &self.saved_lighting,
                &self.saved_archive_edits,
            );
        }
        self.redo_stack.push_back(record);
        self.flush_document_change();
        self.ensure_selection_exists();
        self.rebuild_model_preview_from_document();
        self.log.push("Undo.".to_string());
    }

    pub(super) fn redo(&mut self) {
        if self.tool == EditorTool::Goop && self.redo_goop() {
            return;
        }
        if (self.selected_model_instance_id.is_some()
            || (self.selected_object_id.is_none()
                && self.selected_model_document.is_none()
                && !self.model_instance_redo_stack.is_empty()))
            && self.redo_model_instance()
        {
            return;
        }
        if self.selected_model_document.is_some() && self.redo_model_asset() {
            return;
        }
        if self.document.is_none() {
            return;
        }
        let Some(record) = self.redo_stack.pop_back() else {
            return;
        };
        let registry = (!record.resource_deltas.is_empty())
            .then(|| self.registry.clone())
            .flatten();
        if let Some(document) = &mut self.document {
            record.apply_forward(document);
            if let Some(registry) = registry {
                document.set_registry(registry);
            }
            self.document_dirty = stage_document_differs_from_saved(
                document,
                &self.saved_objects,
                &self.saved_lighting,
                &self.saved_archive_edits,
            );
        }
        self.undo_stack.push_back(record);
        self.flush_document_change();
        self.ensure_selection_exists();
        self.rebuild_model_preview_from_document();
        self.log.push("Redo.".to_string());
    }

    pub(super) fn is_dirty(&self) -> bool {
        self.document_dirty || self.asset_dirty || self.model_instances_dirty
    }

    pub(super) fn unsaved_changes_dialog(&mut self, ctx: &egui::Context) {
        if self.pending_stage_open.is_none()
            && !self.close_confirmation_requested
            && !self.pending_project_hub
        {
            return;
        }

        let mut action = None;
        egui::Window::new("Unsaved changes")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label("The current stage or model asset has changes that have not been saved.");
                ui.label(if self.pending_project_hub {
                    "Save the project before returning to the project hub?"
                } else {
                    "Save the editor project before continuing?"
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Save and Continue").clicked() {
                        action = Some(0);
                    }
                    if ui.button("Discard").clicked() {
                        action = Some(1);
                    }
                    if ui.button("Cancel").clicked() {
                        action = Some(2);
                    }
                });
            });

        match action {
            Some(0) if self.save_project() => self.finish_pending_navigation(ctx),
            Some(1) => self.finish_pending_navigation(ctx),
            Some(2) => {
                self.pending_stage_open = None;
                self.close_confirmation_requested = false;
                self.pending_project_hub = false;
            }
            _ => {}
        }
    }

    pub(super) fn finish_pending_navigation(&mut self, ctx: &egui::Context) {
        if self.pending_project_hub {
            self.enter_project_hub();
            return;
        }
        if self.close_confirmation_requested {
            self.close_confirmation_requested = false;
            self.close_authorized = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if let Some(stage_id) = self.pending_stage_open.take() {
            self.stage_id = stage_id;
            self.open_stage();
        }
    }

    pub(super) fn selected_object(&self) -> Option<&SceneObject> {
        let selected_id = self.selected_object_id.as_ref()?;
        self.document
            .as_ref()?
            .objects
            .iter()
            .find(|object| &object.id == selected_id)
    }

    pub(super) fn clear_viewport_preview_cache(&mut self) {
        self.model_framebuffer = None;
        self.model_framebuffer_key = None;
    }

    pub(super) fn rebuild_model_preview_from_document(&mut self) {
        self.refresh_goop_stale_from_final_terrain();
        self.rebuild_audio_cube_helpers_cache();
        let visibility = self.preview_visibility();
        let (render_scene, model_preview) =
            self.document.as_ref().map_or((None, None), |document| {
                (
                    Some(RenderScene::from_document(document)),
                    SmsEditorApp::build_model_preview(document, visibility),
                )
            });
        self.render_scene = render_scene;
        self.reset_authored_model_preview_base();
        self.model_preview = model_preview;
        if let Some(document) = &self.document {
            self.issues = validation_issues_for_preview(document, self.model_preview.as_ref());
        }
        self.last_level_transform_progress_bits = u32::MAX;
        self.rebuild_gpu_viewport_scene();
        self.clear_viewport_preview_cache();
    }

    pub(super) fn update_object_preview_transform(
        &mut self,
        object_id: &str,
        old_transform: Transform,
        new_transform: Transform,
    ) -> Option<Vec<std::ops::Range<usize>>> {
        let defer_full_bounds_recompute = self.undo_transaction.is_some();
        if !transform_has_invertible_scale(old_transform) {
            return None;
        }
        let preview = self.model_preview.as_mut()?;
        let model_index = preview.object_model_indices.get(object_id).copied()?;
        let triangle_ranges = preview_triangle_ranges_for_model_index(preview, model_index);

        let (old_preview_transform, new_preview_transform) = self
            .document
            .as_ref()
            .and_then(|document| {
                let object = document
                    .objects
                    .iter()
                    .find(|object| object.id == object_id)?;
                Some((
                    object,
                    document.registry.as_ref(),
                    document.actor_preview(object),
                ))
            })
            .map(|(object, registry, actor_preview)| {
                (
                    actor_runtime_preview_transform(
                        reset_fruit_preview_transform(object, old_transform, registry),
                        actor_preview,
                    ),
                    actor_runtime_preview_transform(
                        reset_fruit_preview_transform(object, new_transform, registry),
                        actor_preview,
                    ),
                )
            })
            .unwrap_or((old_transform, new_transform));
        preview
            .mirror_actor_positions
            .insert(model_index, new_preview_transform.translation);

        for model in &mut preview.animated_models {
            if let Some(instance) = model
                .instances
                .iter_mut()
                .find(|instance| instance.model_index == model_index)
            {
                instance.transform = new_preview_transform;
            }
        }
        for model in &mut preview.rotating_models {
            if let Some(instance) = model
                .instances
                .iter_mut()
                .find(|instance| instance.model_index == model_index)
            {
                instance.transform = new_preview_transform;
            }
        }
        for particles in &mut preview.actor_particles {
            if particles.model_index == Some(model_index) {
                particles.origin_offset = retransform_preview_point(
                    particles.origin_offset,
                    old_preview_transform,
                    new_preview_transform,
                );
            }
        }

        let mut changed = false;
        for point in &mut preview.points {
            if point.model_index == model_index {
                point.position = retransform_preview_point(
                    point.position,
                    old_preview_transform,
                    new_preview_transform,
                );
                changed = true;
            }
        }
        for range in &triangle_ranges {
            for triangle in &mut preview.triangles[range.clone()] {
                triangle.vertices = triangle.vertices.map(|vertex| {
                    retransform_preview_point(vertex, old_preview_transform, new_preview_transform)
                });
                let normals = if matches!(
                    triangle.render_layer,
                    PreviewRenderLayer::Particle | PreviewRenderLayer::ParticleDistortion
                ) {
                    triangle.normals
                } else {
                    triangle.normals.map(|normals| {
                        normals.map(|normal| {
                            retransform_preview_normal(
                                normal,
                                old_preview_transform,
                                new_preview_transform,
                            )
                        })
                    })
                };
                triangle.billboard = triangle.billboard.and_then(|billboard| {
                    retransform_j3d_billboard(
                        billboard,
                        old_preview_transform,
                        new_preview_transform,
                        normals,
                    )
                });
                triangle.normals = normals;
                changed = true;
            }
        }
        if changed {
            if defer_full_bounds_recompute {
                expand_model_preview_bounds(preview, model_index, &triangle_ranges);
            } else {
                recompute_model_preview_bounds(preview);
            }
        }
        changed.then_some(triangle_ranges)
    }

    pub(super) fn rebuild_gpu_viewport_scene(&mut self) {
        self.sync_authored_model_instance_preview();
        let Some(target_format) = self.gpu_target_format else {
            self.gpu_viewport = None;
            return;
        };
        let Some(preview) = self.model_preview.as_ref() else {
            self.gpu_viewport = None;
            return;
        };
        self.gpu_viewport = Some(gpu_viewport::GpuViewportScene::from_preview(
            preview,
            target_format,
        ));
    }

    pub(super) fn ensure_selection_exists(&mut self) {
        let exists = self.selected_object_id.as_ref().is_some_and(|id| {
            self.document
                .as_ref()
                .is_some_and(|document| document.objects.iter().any(|object| &object.id == id))
        });
        if !exists {
            self.selected_object_id = None;
        }
    }

    pub(super) fn default_spawn_position(&self) -> [f32; 3] {
        self.renderer.camera().focus
    }

    pub(super) fn frame_selected(&mut self) {
        if self.frame_selected_model_instance() {
            return;
        }
        self.stop_camera_fly();
        if let Some(object) = self.selected_object() {
            self.renderer.camera_mut().focus = object.transform.translation;
            self.viewport_pan = egui::Vec2::ZERO;
            self.queue_camera_state_save();
        }
    }

    pub(super) fn reset_camera(&mut self) {
        self.stop_camera_fly();
        self.viewport_pan = egui::Vec2::ZERO;
        self.viewport_zoom = 1.0;
        if let Some(preview) = &self.model_preview {
            let camera = self.renderer.camera_mut();
            camera.focus = preview.center();
            camera.yaw_degrees = self.startup_camera_yaw.unwrap_or(222.0);
            camera.pitch_degrees = self.startup_camera_pitch.unwrap_or(-30.0);
            camera.distance = (preview.radius() * 4.2).clamp(2500.0, 600_000.0);
            self.queue_camera_state_save();
            return;
        }

        let camera = self.renderer.camera_mut();
        camera.focus = [0.0, 0.0, 0.0];
        camera.yaw_degrees = self.startup_camera_yaw.unwrap_or(222.0);
        camera.pitch_degrees = self.startup_camera_pitch.unwrap_or(-30.0);
        camera.distance = 7000.0;
        self.queue_camera_state_save();
    }

    pub(super) fn apply_startup_camera_focus(&mut self) {
        self.stop_camera_fly();
        if let Some(focus) = self.startup_camera_focus {
            let camera = self.renderer.camera_mut();
            camera.focus = focus;
            if let Some(distance) = self.startup_camera_distance {
                camera.distance = distance.max(50.0);
            }
            self.viewport_pan = egui::Vec2::ZERO;
            self.viewport_zoom = 1.0;
            self.log.push(format!(
                "Focused startup camera on {:.1}, {:.1}, {:.1}.",
                focus[0], focus[1], focus[2]
            ));
            return;
        }
        let Some(needle) = self.startup_focus_object.as_deref() else {
            return;
        };
        let Some(document) = &self.document else {
            return;
        };
        let needle = needle.trim().to_ascii_lowercase();
        if needle.is_empty() {
            return;
        }
        if let Some(object) = document
            .objects
            .iter()
            .find(|object| object_matches_focus(object, &needle))
        {
            let camera = self.renderer.camera_mut();
            camera.focus = object.transform.translation;
            camera.distance = self.startup_camera_distance.unwrap_or(2200.0).max(50.0);
            self.viewport_pan = egui::Vec2::ZERO;
            self.viewport_zoom = 1.0;
            self.selected_object_id = Some(object.id.clone());
            self.log.push(format!(
                "Focused startup camera on '{}'.",
                object_display_name(object)
            ));
        }
    }
}

pub(super) fn managed_dolphin_exec_is_directory_main(
    run_root: &std::path::Path,
    launch_dol: &std::path::Path,
) -> bool {
    launch_dol == run_root.join("sys").join("main.dol")
}

#[cfg(test)]
pub(super) fn preview_triangle_ranges_for_model(
    preview: &ModelPreview,
    object_id: &str,
) -> Vec<std::ops::Range<usize>> {
    let Some(model_index) = preview.object_model_indices.get(object_id).copied() else {
        return Vec::new();
    };
    preview_triangle_ranges_for_model_index(preview, model_index)
}

fn preview_triangle_ranges_for_model_index(
    preview: &ModelPreview,
    model_index: usize,
) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = None;
    for (index, triangle) in preview.triangles.iter().enumerate() {
        if triangle.model_index == model_index {
            start.get_or_insert(index);
        } else if let Some(start) = start.take() {
            ranges.push(start..index);
        }
    }
    if let Some(start) = start {
        ranges.push(start..preview.triangles.len());
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;
    use sms_formats::{
        decode_bti_texture, J3dFile, JDramaDocument, JDramaField, JDramaFieldValue, JDramaRecord,
        JDramaRecordPayload, PrmFile,
    };

    fn command_test_document(objects: Vec<SceneObject>) -> StageDocument {
        StageDocument {
            stage_id: "fixture0".to_string(),
            base_root: PathBuf::from("."),
            assets: Vec::new(),
            objects,
            changed_files: BTreeMap::new(),
            stage_archive: None,
            stage_archive_source_path: Some(PathBuf::from("virtual/fixture0.szs")),
            archive_edits: StageArchiveEdits::default(),
            registry: None,
            route_authoring: None,
            goop_authoring: None,
            load_issues: Vec::new(),
            lighting: StageLighting::default(),
            actor_previews: BTreeMap::new(),
            loaded_project: None,
        }
    }

    fn empty_parameter_document() -> StageResourceDocument {
        StageResourceDocument::Parameters(PrmFile {
            entries: Vec::new(),
        })
    }

    fn built_in_proxy_document(raw_resource_path: &[u8]) -> StageResourceDocument {
        let requirement = sms_scene::BLANK_STAGE_BOOTSTRAP_REQUIREMENTS
            .iter()
            .find(|requirement| requirement.raw_path == raw_resource_path)
            .unwrap();
        let proxy = sms_authoring::built_in_blank_stage_proxy(raw_resource_path);
        let bytes = match requirement.kind {
            sms_scene::BlankStageBootstrapKind::Model => proxy.compile_bmd().unwrap(),
            sms_scene::BlankStageBootstrapKind::Collision => {
                proxy.collision.as_ref().unwrap().to_col_bytes().unwrap()
            }
        };
        StageResourceDocument::parse_for_path(raw_resource_path, &bytes).unwrap()
    }

    fn shine_catalog_template() -> sms_scene::ObjectAuthoringTemplate {
        sms_scene::ObjectAuthoringTemplate {
            factory_name: "Shine".to_string(),
            group_index: 4,
            record: JDramaRecord {
                type_name: "Shine".to_string(),
                name: "retail event shine".to_string(),
                payload: JDramaRecordPayload::Actor {
                    transform: sms_formats::JDramaTransform {
                        translation: [0.0; 3],
                        rotation: [0.0; 3],
                        scale: [1.0; 3],
                    },
                    character_name: "??????".to_string(),
                    light_map: sms_formats::JDramaLightMap::default(),
                    fields: vec![
                        JDramaField {
                            name: "resource_name".to_string(),
                            value: JDramaFieldValue::String("shine".to_string()),
                        },
                        JDramaField {
                            name: "collection_type".to_string(),
                            value: JDramaFieldValue::String("demo".to_string()),
                        },
                        JDramaField {
                            name: "shine_id".to_string(),
                            value: JDramaFieldValue::I32(104),
                        },
                        JDramaField {
                            name: "in_stage".to_string(),
                            value: JDramaFieldValue::I32(1),
                        },
                    ],
                },
            },
            dependencies: Vec::new(),
            character_records: Vec::new(),
            table_dependencies: Vec::new(),
            runtime_actor_references: Vec::new(),
            required_graph_names: Vec::new(),
            resources: Vec::new(),
            preview_resource_path: None,
            source_stage: "fixture0".to_string(),
        }
    }

    #[test]
    fn authored_shines_use_standalone_spawn_defaults_and_unique_names() {
        let template = shine_catalog_template();
        let object = object_from_catalog_template(
            "fixture0-obj-0008".to_string(),
            "Shine".to_string(),
            [1.0, 2.0, 3.0],
            &template,
            &BTreeMap::new(),
        )
        .unwrap();

        assert_eq!(
            object.raw_param("name"),
            Some("Graffito-Editor Shine fixture0-obj-0008")
        );
        assert_eq!(object.raw_param("collection_type"), Some("normal"));
        assert_eq!(object.raw_param("shine_id"), Some("104"));
        assert_eq!(object.raw_param("in_stage"), Some("-1"));
        assert_eq!(
            object.authoring_defaults_version,
            sms_scene::OBJECT_AUTHORING_DEFAULTS_VERSION
        );

        let duplicate = duplicate_object_for_spawn(
            object,
            "fixture0-obj-0009".to_string(),
            [4.0, 5.0, 6.0],
            None,
        );
        assert_eq!(
            duplicate.raw_param("name"),
            Some("Graffito-Editor Shine fixture0-obj-0009")
        );
        assert_eq!(duplicate.raw_param("collection_type"), Some("normal"));
        assert_eq!(duplicate.raw_param("shine_id"), Some("104"));
    }

    #[test]
    fn spawned_shines_keep_an_unused_flag_and_allocate_around_conflicts() {
        let template = shine_catalog_template();
        let existing = object_from_catalog_template(
            "fixture0-obj-0008".to_string(),
            "Shine".to_string(),
            [0.0; 3],
            &template,
            &BTreeMap::new(),
        )
        .unwrap();
        let document = command_test_document(vec![existing]);

        let mut new_shine = object_from_catalog_template(
            "fixture0-obj-0009".to_string(),
            "Shine".to_string(),
            [0.0; 3],
            &template,
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(
            assign_unique_shine_id_for_spawn(&mut new_shine, &document).unwrap(),
            Some(0)
        );
        assert_eq!(new_shine.raw_param("shine_id"), Some("0"));

        let empty_document = command_test_document(Vec::new());
        let mut first_shine = object_from_catalog_template(
            "fixture0-obj-0010".to_string(),
            "Shine".to_string(),
            [0.0; 3],
            &template,
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(
            assign_unique_shine_id_for_spawn(&mut first_shine, &empty_document).unwrap(),
            Some(104)
        );
    }

    #[test]
    fn legacy_authored_demo_shine_migrates_once_without_overwriting_new_choices() {
        let template = shine_catalog_template();
        let mut object = SceneObject::new("fixture0-obj-0008", "Shine");
        sms_scene::seed_scene_object_parameters(&mut object, &template.record).unwrap();
        object.placement = Some(sms_scene::PlacementBinding::Authored(
            sms_scene::AuthoredPlacement {
                raw_resource_path: b"map/scene.bin".to_vec(),
                target_group_index: template.group_index,
                prototype: template.record,
                dependencies: Vec::new(),
            },
        ));

        assert!(migrate_legacy_authored_shine_defaults(&mut object).unwrap());
        assert_eq!(object.raw_param("collection_type"), Some("normal"));
        assert_eq!(object.raw_param("in_stage"), Some("-1"));
        assert_eq!(
            object.raw_param("name"),
            Some("Graffito-Editor Shine fixture0-obj-0008")
        );
        assert!(!migrate_legacy_authored_shine_defaults(&mut object).unwrap());

        object.set_raw_param("collection_type", "demo");
        assert!(!migrate_legacy_authored_shine_defaults(&mut object).unwrap());
        assert_eq!(object.raw_param("collection_type"), Some("demo"));

        let template = shine_catalog_template();
        let mut customized = SceneObject::new("fixture0-obj-0010", "Shine");
        sms_scene::seed_scene_object_parameters(&mut customized, &template.record).unwrap();
        customized.set_raw_param("collection_type", "quickly");
        customized.set_raw_param("shine_id", "7");
        customized.set_raw_param("in_stage", "0");
        customized.placement = Some(sms_scene::PlacementBinding::Authored(
            sms_scene::AuthoredPlacement {
                raw_resource_path: b"map/scene.bin".to_vec(),
                target_group_index: template.group_index,
                prototype: template.record,
                dependencies: Vec::new(),
            },
        ));
        assert!(migrate_legacy_authored_shine_defaults(&mut customized).unwrap());
        assert_eq!(customized.raw_param("collection_type"), Some("quickly"));
        assert_eq!(customized.raw_param("shine_id"), Some("7"));
        assert_eq!(customized.raw_param("in_stage"), Some("0"));
    }

    #[test]
    fn spawn_skips_loaded_object_id_collision() {
        let mut template = SceneObject::new("fixture0-obj-0001", "FixtureEnemy");
        template.placement = Some(sms_scene::PlacementBinding::Existing(
            sms_scene::PlacementAddress {
                raw_resource_path: b"map/scene.bin".to_vec(),
                record_path: vec![4, 0],
            },
        ));
        let mut app = SmsEditorApp {
            stage_id: "fixture0".to_string(),
            document: Some(command_test_document(vec![template])),
            ..SmsEditorApp::default()
        };

        app.spawn_object_at("FixtureEnemy".to_string(), [10.0, 20.0, 30.0]);

        let document = app.document.as_ref().unwrap();
        assert_eq!(document.objects.len(), 2);
        assert_eq!(document.objects[1].id, "fixture0-obj-0002");
        assert_eq!(
            document.objects[1].transform.translation,
            [10.0, 20.0, 30.0]
        );
        assert_eq!(app.selected_object_id.as_deref(), Some("fixture0-obj-0002"));
        assert_eq!(app.selected_object().unwrap().factory_name, "FixtureEnemy");
        assert_eq!(app.next_object_serial, 3);
        assert_eq!(
            document
                .objects
                .iter()
                .map(|object| object.id.as_str())
                .collect::<BTreeSet<_>>()
                .len(),
            document.objects.len()
        );
    }

    #[test]
    fn duplicate_skips_loaded_object_id_collisions() {
        let source = SceneObject::new("source-object", "FixtureEnemy");
        let occupied_one = SceneObject::new("fixture0-obj-0001", "OtherFixture");
        let occupied_two = SceneObject::new("fixture0-obj-0002", "OtherFixture");
        let mut app = SmsEditorApp {
            stage_id: "fixture0".to_string(),
            selected_object_id: Some(source.id.clone()),
            document: Some(command_test_document(vec![
                source,
                occupied_one,
                occupied_two,
            ])),
            ..SmsEditorApp::default()
        };

        app.duplicate_selected();

        let document = app.document.as_ref().unwrap();
        assert_eq!(document.objects.len(), 4);
        assert_eq!(document.objects[3].id, "fixture0-obj-0003");
        assert_eq!(app.selected_object_id.as_deref(), Some("fixture0-obj-0003"));
        assert_eq!(app.selected_object().unwrap().id, "fixture0-obj-0003");
        assert_eq!(app.next_object_serial, 4);
        assert_eq!(
            document
                .objects
                .iter()
                .map(|object| object.id.as_str())
                .collect::<BTreeSet<_>>()
                .len(),
            document.objects.len()
        );
    }

    #[test]
    fn authored_mario_placement_uses_typed_player_group_and_constructor() {
        let player_group = JDramaRecord::new(
            "IdxGroup",
            "player group",
            JDramaRecordPayload::Group {
                fields: vec![JDramaField {
                    name: "group_index".to_string(),
                    value: JDramaFieldValue::U32(6),
                }],
                children: Vec::new(),
            },
        )
        .unwrap();
        let root = JDramaRecord::new(
            "GroupObj",
            "scene",
            JDramaRecordPayload::Group {
                fields: Vec::new(),
                children: vec![player_group],
            },
        )
        .unwrap();
        let mut archive = sms_scene::SourceFreeStageArchive::new().unwrap();
        archive
            .insert_resource(
                b"map/scene.bin".to_vec(),
                StageResourceDocument::Placement(JDramaDocument { root }),
            )
            .unwrap();
        archive.set_origin(sms_scene::StageOrigin::Blank {
            target_slot: "custom0".to_string(),
            preset_version: sms_scene::BLANK_STAGE_PRESET_VERSION,
        });
        let mut document = StageDocument {
            stage_id: "custom0".to_string(),
            base_root: PathBuf::from("."),
            assets: Vec::new(),
            objects: Vec::new(),
            changed_files: BTreeMap::new(),
            stage_archive: Some(archive),
            stage_archive_source_path: Some(PathBuf::from("custom0.szs")),
            archive_edits: sms_scene::StageArchiveEdits::default(),
            route_authoring: None,
            goop_authoring: None,
            registry: None,
            load_issues: Vec::new(),
            lighting: sms_scene::StageLighting::default(),
            actor_previews: BTreeMap::new(),
            loaded_project: None,
        };

        let address = ensure_authored_mario_placement(&mut document, [10.0, 20.0, 30.0]).unwrap();
        assert_eq!(address.raw_resource_path, b"map/scene.bin");
        let placements = document.stage_archive.as_ref().unwrap().object_placements();
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].type_name, "Mario");
        assert_eq!(placements[0].record_path, address.record_path);
        assert_eq!(placements[0].transform.translation, [10.0, 20.0, 30.0]);
        assert_eq!(placements[0].transform.scale, [1.0; 3]);
        assert!(authored_runtime_readiness_error(&document, false).is_some());
        document.objects.push(SceneObject {
            id: "mario".to_string(),
            source: None,
            placement: Some(sms_scene::PlacementBinding::Existing(address)),
            factory_name: "Mario".to_string(),
            class_name: None,
            transform: Transform::default(),
            raw_params: BTreeMap::new(),
            asset_hints: Vec::new(),
            runtime_references: Vec::new(),
            manager_capacity_dependencies: Vec::new(),
            authoring_defaults_version: 0,
        });
        assert_eq!(authored_runtime_readiness_error(&document, false), None);
    }

    #[test]
    fn authored_sky_placement_uses_typed_sky_group_and_requires_model() {
        let sky_group = JDramaRecord::new(
            "IdxGroup",
            "sky group",
            JDramaRecordPayload::Group {
                fields: vec![JDramaField {
                    name: "group_index".to_string(),
                    value: JDramaFieldValue::U32(1),
                }],
                children: Vec::new(),
            },
        )
        .unwrap();
        let root = JDramaRecord::new(
            "GroupObj",
            "scene",
            JDramaRecordPayload::Group {
                fields: Vec::new(),
                children: vec![sky_group],
            },
        )
        .unwrap();
        let mut archive = sms_scene::SourceFreeStageArchive::new().unwrap();
        archive
            .insert_resource(
                b"map/scene.bin".to_vec(),
                StageResourceDocument::Placement(JDramaDocument { root }),
            )
            .unwrap();
        archive.set_origin(sms_scene::StageOrigin::Blank {
            target_slot: "custom0".to_string(),
            preset_version: sms_scene::BLANK_STAGE_PRESET_VERSION,
        });
        let mut document = StageDocument {
            stage_id: "custom0".to_string(),
            base_root: PathBuf::from("."),
            assets: Vec::new(),
            objects: vec![SceneObject::new("mario", "Mario")],
            changed_files: BTreeMap::new(),
            stage_archive: Some(archive),
            stage_archive_source_path: Some(PathBuf::from("custom0.szs")),
            route_authoring: None,
            goop_authoring: None,
            archive_edits: sms_scene::StageArchiveEdits::default(),
            registry: None,
            load_issues: Vec::new(),
            lighting: sms_scene::StageLighting::default(),
            actor_previews: BTreeMap::new(),
            loaded_project: None,
        };

        let address = ensure_sky_placement(&mut document, [10.0, 20.0, 30.0]).unwrap();
        assert_eq!(address.raw_resource_path, b"map/scene.bin");
        let placements = document.stage_archive.as_ref().unwrap().object_placements();
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].type_name, "Sky");
        assert_eq!(placements[0].record_path, address.record_path);
        assert_eq!(placements[0].transform.translation, [10.0, 20.0, 30.0]);
        assert_eq!(placements[0].transform.scale, [1.0; 3]);

        let mut sky = SceneObject::new("sky", "Sky");
        sky.placement = Some(sms_scene::PlacementBinding::Existing(address));
        document.objects.push(sky);
        let error = authored_runtime_readiness_error(&document, false)
            .expect("a Sky actor without sky.bmd is not runnable");
        assert!(error.contains("no skybox model"), "{error}");
        assert_eq!(authored_runtime_readiness_error(&document, true), None);
    }

    #[test]
    fn catalog_template_seeds_defaults_and_owns_dependencies() {
        let prototype = JDramaRecord {
            type_name: "FixtureEnemy".to_string(),
            name: "retail fixture".to_string(),
            payload: JDramaRecordPayload::Actor {
                transform: sms_formats::JDramaTransform {
                    translation: [90.0, 80.0, 70.0],
                    rotation: [4.0, 5.0, 6.0],
                    scale: [2.0; 3],
                },
                character_name: "FixtureChara".to_string(),
                light_map: sms_formats::JDramaLightMap::default(),
                fields: vec![
                    JDramaField {
                        name: "manager_name".to_string(),
                        value: JDramaFieldValue::String("fixture manager".to_string()),
                    },
                    JDramaField {
                        name: "behavior".to_string(),
                        value: JDramaFieldValue::U32(17),
                    },
                ],
            },
        };
        let dependency_record = JDramaRecord {
            type_name: "FixtureManager".to_string(),
            name: "fixture manager".to_string(),
            payload: JDramaRecordPayload::Fields { fields: Vec::new() },
        };
        let template = sms_scene::ObjectAuthoringTemplate {
            factory_name: "FixtureEnemy".to_string(),
            group_index: 4,
            record: prototype,
            dependencies: vec![sms_scene::ObjectAuthoringDependency {
                group_index: 2,
                target: sms_scene::AuthoredPlacementDependencyTarget::IndexedGroup {
                    group_index: 2,
                },
                record: dependency_record,
            }],
            character_records: Vec::new(),
            runtime_actor_references: Vec::new(),
            table_dependencies: Vec::new(),
            required_graph_names: Vec::new(),
            resources: Vec::new(),
            preview_resource_path: None,
            source_stage: "fixture0".to_string(),
        };

        let object = object_from_catalog_template(
            "fixture-object".to_string(),
            "FixtureEnemy".to_string(),
            [1.0, 2.0, 3.0],
            &template,
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(object.raw_param("name"), Some("retail fixture"));
        assert_eq!(object.raw_param("manager_name"), Some("fixture manager"));
        assert_eq!(object.raw_param("behavior"), Some("17"));
        let Some(sms_scene::PlacementBinding::Authored(authored)) = object.placement.as_ref()
        else {
            panic!("catalog object must own an authored placement");
        };
        assert_eq!(authored.raw_resource_path, b"map/scene.bin");
        assert_eq!(authored.target_group_index, 4);
        assert_eq!(authored.dependencies.len(), 1);
        assert_eq!(authored.dependencies[0].target_group_index, 2);
        let JDramaRecordPayload::Actor { transform, .. } = &authored.prototype.payload else {
            panic!("expected actor prototype");
        };
        assert_eq!(transform.translation, [1.0, 2.0, 3.0]);
        assert_eq!(transform.rotation, [0.0; 3]);
        assert_eq!(transform.scale, [1.0; 3]);

        let duplicate =
            duplicate_object_for_spawn(object, "fixture-copy".to_string(), [4.0, 5.0, 6.0], None);
        assert_eq!(duplicate.id, "fixture-copy");
        assert!(matches!(
            duplicate.placement,
            Some(sms_scene::PlacementBinding::Authored(_))
        ));
        assert_eq!(duplicate.transform.translation, [4.0, 5.0, 6.0]);
    }

    #[test]
    fn shared_spawn_and_duplicate_path_refreshes_manager_capacity_closure() {
        let mut source = SceneObject::new("launcher", "CommonLauncher");
        source.insert_source_raw_param("launched_enemy_name", "fixture manager");
        source.manager_capacity_dependencies = vec!["stale manager".to_string()];
        source.placement = Some(sms_scene::PlacementBinding::Existing(
            sms_scene::PlacementAddress {
                raw_resource_path: b"map/scene.bin".to_vec(),
                record_path: vec![1, 0],
            },
        ));
        let registry = ObjectRegistry::default();

        let duplicate = duplicate_object_for_spawn(
            source,
            "launcher-copy".to_string(),
            [4.0, 5.0, 6.0],
            Some(&registry),
        );

        assert_eq!(duplicate.manager_capacity_dependencies, ["fixture manager"]);
        assert!(matches!(
            duplicate.placement,
            Some(sms_scene::PlacementBinding::CloneOf(_))
        ));
    }

    #[test]
    fn runtime_table_dependency_merges_into_a_unique_type_compatible_container() {
        let camera_name = "\u{30b7}\u{30e3}\u{30a4}\u{30f3}\u{ff08}\u{3044}\u{304d}\u{306a}\u{308a}\u{51fa}\u{73fe}\u{ff09}\u{30ab}\u{30e1}\u{30e9}";
        let camera = JDramaRecord {
            type_name: "CameraMapInfo".to_string(),
            name: camera_name.to_string(),
            payload: JDramaRecordPayload::Fields {
                fields: vec![JDramaField {
                    name: "demo_length_frames".to_string(),
                    value: JDramaFieldValue::I32(900),
                }],
            },
        };
        let dependency = sms_scene::ObjectAuthoringTableDependency {
            target: sms_scene::AuthoredPlacementDependencyTarget::NamedGroup {
                type_name: "CameraMapToolTable".to_string(),
                name: "camera map tool table".to_string(),
            },
            record: camera.clone(),
        };
        let mut scene = JDramaDocument {
            root: JDramaRecord {
                type_name: "MarScene".to_string(),
                name: "scene".to_string(),
                payload: JDramaRecordPayload::Group {
                    fields: Vec::new(),
                    children: vec![JDramaRecord {
                        type_name: "CameraMapToolTable".to_string(),
                        name: "blank stage camera table".to_string(),
                        payload: JDramaRecordPayload::Group {
                            fields: Vec::new(),
                            children: Vec::new(),
                        },
                    }],
                },
            },
        };

        assert_eq!(
            merge_runtime_table_dependencies(
                Some(&mut scene),
                None,
                std::slice::from_ref(&dependency),
            )
            .unwrap(),
            (true, false)
        );
        assert_eq!(
            matching_record_paths(&scene, |record| {
                record.type_name == "CameraMapInfo" && record.name == camera_name
            })
            .len(),
            1
        );
        assert_eq!(
            merge_runtime_table_dependencies(
                Some(&mut scene),
                None,
                std::slice::from_ref(&dependency),
            )
            .unwrap(),
            (false, false)
        );
    }

    #[test]
    fn catalog_preflight_persists_quick_camera_into_authored_scene_table() {
        let camera_name = "\u{30b7}\u{30e3}\u{30a4}\u{30f3}\u{ff08}\u{3044}\u{304d}\u{306a}\u{308a}\u{51fa}\u{73fe}\u{ff09}\u{30ab}\u{30e1}\u{30e9}";
        let dependency = sms_scene::ObjectAuthoringTableDependency {
            target: sms_scene::AuthoredPlacementDependencyTarget::NamedGroup {
                type_name: "CameraMapToolTable".to_string(),
                name: "camera map tool table".to_string(),
            },
            record: JDramaRecord {
                type_name: "CameraMapInfo".to_string(),
                name: camera_name.to_string(),
                payload: JDramaRecordPayload::Fields {
                    fields: vec![JDramaField {
                        name: "demo_length_frames".to_string(),
                        value: JDramaFieldValue::I32(900),
                    }],
                },
            },
        };
        let scene = JDramaDocument {
            root: JDramaRecord {
                type_name: "MarScene".to_string(),
                name: "scene".to_string(),
                payload: JDramaRecordPayload::Group {
                    fields: Vec::new(),
                    children: vec![JDramaRecord {
                        type_name: "CameraMapToolTable".to_string(),
                        name: "camera map tool table".to_string(),
                        payload: JDramaRecordPayload::Group {
                            fields: Vec::new(),
                            children: Vec::new(),
                        },
                    }],
                },
            },
        };
        let mut archive = sms_scene::SourceFreeStageArchive::new_for_blank(
            "test11",
            sms_scene::BLANK_STAGE_PRESET_VERSION,
        )
        .unwrap();
        archive
            .insert_resource(
                CATALOG_SCENE_PATH.to_vec(),
                StageResourceDocument::Placement(scene),
            )
            .unwrap();
        let mut document = command_test_document(Vec::new());
        document.stage_archive = Some(archive);
        let mut template = shine_catalog_template();
        template.table_dependencies = vec![dependency];

        let preflight = preflight_catalog_resources(&document, &template).unwrap();
        assert_eq!(preflight.writes.len(), 1);
        assert_eq!(preflight.writes[0].raw_resource_path, CATALOG_SCENE_PATH);
        ObjectUndoRecord {
            deltas: Vec::new(),
            resource_deltas: catalog_resource_edit_deltas(&document, preflight.writes),
            route_delta: None,
        }
        .apply_forward(&mut document);

        let Some(StageResourceDocument::Placement(scene)) = document
            .effective_resource_clone(CATALOG_SCENE_PATH)
            .unwrap()
        else {
            panic!("authored scene placement resource is missing");
        };
        assert_eq!(
            matching_record_paths(&scene, |record| {
                semantic_record_type(&record.type_name) == "CameraMapInfo"
                    && record.name == camera_name
            })
            .len(),
            1
        );
    }

    #[test]
    fn runtime_table_dependency_rejects_missing_or_ambiguous_containers() {
        let dependency = sms_scene::ObjectAuthoringTableDependency {
            target: sms_scene::AuthoredPlacementDependencyTarget::NamedGroup {
                type_name: "CameraMapToolTable".to_string(),
                name: "camera map tool table".to_string(),
            },
            record: JDramaRecord {
                type_name: "CameraMapInfo".to_string(),
                name: "quick camera".to_string(),
                payload: JDramaRecordPayload::Fields { fields: Vec::new() },
            },
        };
        let make_document = || JDramaDocument {
            root: JDramaRecord {
                type_name: "CameraMapToolTable".to_string(),
                name: "camera map tool table".to_string(),
                payload: JDramaRecordPayload::Group {
                    fields: Vec::new(),
                    children: Vec::new(),
                },
            },
        };

        let mut scene = make_document();
        let mut tables = make_document();
        let error = merge_runtime_table_dependencies(
            Some(&mut scene),
            Some(&mut tables),
            std::slice::from_ref(&dependency),
        )
        .unwrap_err();
        assert!(error.contains("2 matching target containers"), "{error}");

        let mut missing = JDramaDocument {
            root: JDramaRecord::new(
                "NameRefGrp",
                "empty",
                JDramaRecordPayload::Group {
                    fields: Vec::new(),
                    children: Vec::new(),
                },
            )
            .unwrap(),
        };
        let error = merge_runtime_table_dependencies(
            Some(&mut missing),
            None,
            std::slice::from_ref(&dependency),
        )
        .unwrap_err();
        assert!(error.contains("0 matching target containers"), "{error}");
    }

    #[test]
    fn character_table_merge_is_idempotent_and_rejects_ambiguous_names() {
        let registration = JDramaRecord {
            type_name: "ObjChara".to_string(),
            name: "FixtureChara".to_string(),
            payload: JDramaRecordPayload::Fields {
                fields: vec![JDramaField {
                    name: "resource_folder".to_string(),
                    value: JDramaFieldValue::String("/scene/fixture".to_string()),
                }],
            },
        };
        let (document, changed) =
            merge_character_table(None, std::slice::from_ref(&registration)).unwrap();
        assert!(changed);
        let paths = name_ref_group_paths(&document);
        assert_eq!(paths.len(), 1);

        let (same, changed) = merge_character_table(
            Some(StageResourceDocument::Placement(document.clone())),
            std::slice::from_ref(&registration),
        )
        .unwrap();
        assert!(!changed);
        assert_eq!(same, document);

        let mut duplicate = document.clone();
        let target = jdrama_record_mut(&mut duplicate.root, &paths[0]).unwrap();
        let JDramaRecordPayload::Group { children, .. } = &mut target.payload else {
            unreachable!()
        };
        children.push(registration.clone());
        let error = merge_character_table(
            Some(StageResourceDocument::Placement(duplicate)),
            std::slice::from_ref(&registration),
        )
        .unwrap_err();
        assert!(error.contains("ambiguous"), "{error}");

        let mut conflicting = registration.clone();
        let JDramaRecordPayload::Fields { fields } = &mut conflicting.payload else {
            unreachable!()
        };
        fields[0].value = JDramaFieldValue::String("/scene/different".to_string());
        let error = merge_character_table(
            Some(StageResourceDocument::Placement(document)),
            &[conflicting],
        )
        .unwrap_err();
        assert!(error.contains("conflicts"), "{error}");
    }

    #[test]
    fn graph_conflict_rewrite_survives_canonical_parameter_export() {
        let prototype = JDramaRecord {
            type_name: "FixtureEnemy".to_string(),
            name: "retail fixture".to_string(),
            payload: JDramaRecordPayload::Actor {
                transform: sms_formats::JDramaTransform {
                    translation: [10.0, 20.0, 30.0],
                    rotation: [0.0; 3],
                    scale: [1.0; 3],
                },
                character_name: String::new(),
                light_map: sms_formats::JDramaLightMap::default(),
                fields: vec![JDramaField {
                    name: "graph_name".to_string(),
                    value: JDramaFieldValue::String("route_a".to_string()),
                }],
            },
        };
        let template = sms_scene::ObjectAuthoringTemplate {
            factory_name: "FixtureEnemy".to_string(),
            group_index: 4,
            record: prototype,
            dependencies: Vec::new(),
            runtime_actor_references: Vec::new(),
            character_records: Vec::new(),
            table_dependencies: Vec::new(),
            required_graph_names: vec!["route_a".to_string()],
            resources: Vec::new(),
            preview_resource_path: None,
            source_stage: "fixture0".to_string(),
        };
        let rewrites = BTreeMap::from([("route_a".to_string(), "route_a_authored".to_string())]);
        let object = object_from_catalog_template(
            "fixture-object".to_string(),
            "FixtureEnemy".to_string(),
            [1.0, 2.0, 3.0],
            &template,
            &rewrites,
        )
        .unwrap();
        assert_eq!(object.raw_param("graph_name"), Some("route_a_authored"));
        assert!(!object.raw_params["graph_name"].is_dirty());
        let Some(sms_scene::PlacementBinding::Authored(authored)) = &object.placement else {
            unreachable!()
        };
        let graph_name = |record: &JDramaRecord| {
            let JDramaRecordPayload::Actor { fields, .. } = &record.payload else {
                return None;
            };
            fields.iter().find_map(|field| match &field.value {
                JDramaFieldValue::String(value) if field.name == "graph_name" => {
                    Some(value.clone())
                }
                _ => None,
            })
        };
        assert_eq!(
            graph_name(&authored.prototype),
            Some("route_a_authored".to_string())
        );

        let mut exported = authored.prototype.clone();
        sms_scene::apply_all_object_parameters(&mut exported, &object).unwrap();
        assert_eq!(graph_name(&exported), Some("route_a_authored".to_string()));
    }

    #[test]
    fn existing_authored_objects_repair_new_catalog_runtime_resources_idempotently() {
        let catalog_resource = empty_parameter_document();
        let temp = tempfile::tempdir().unwrap();
        let source_asset_path = temp.path().join("runtime.prm");
        std::fs::write(&source_asset_path, catalog_resource.to_bytes().unwrap()).unwrap();

        let prototype = JDramaRecord {
            type_name: "FixtureEnemy".to_string(),
            name: "fixture".to_string(),
            payload: JDramaRecordPayload::Actor {
                transform: sms_formats::JDramaTransform {
                    translation: [0.0; 3],
                    rotation: [0.0; 3],
                    scale: [1.0; 3],
                },
                character_name: String::new(),
                light_map: sms_formats::JDramaLightMap::default(),
                fields: Vec::new(),
            },
        };
        let mut object = SceneObject::new("fixture-object", "FixtureEnemy");
        object.placement = Some(sms_scene::PlacementBinding::Authored(
            sms_scene::AuthoredPlacement {
                raw_resource_path: b"map/scene.bin".to_vec(),
                target_group_index: 4,
                prototype: prototype.clone(),
                dependencies: Vec::new(),
            },
        ));
        let template = sms_scene::ObjectAuthoringTemplate {
            factory_name: "FixtureEnemy".to_string(),
            group_index: 4,
            record: prototype,
            runtime_actor_references: vec![sms_scene::ObjectAuthoringRuntimeActorReference {
                required_factory_name: "Shine".to_string(),
                runtime_name: "fixture reward".to_string(),
                required: true,
            }],
            dependencies: Vec::new(),
            character_records: Vec::new(),
            table_dependencies: Vec::new(),
            required_graph_names: Vec::new(),
            resources: vec![sms_scene::ObjectAuthoringResource {
                raw_resource_path: b"fixtureanm/runtime.prm".to_vec(),
                source_asset_path,
            }],
            preview_resource_path: None,
            source_stage: "fixture0".to_string(),
        };
        let mut document = command_test_document(vec![object]);

        let repair =
            repair_authored_catalog_resources(&mut document, std::slice::from_ref(&template));
        assert_eq!(repair.resource_writes, 1);
        assert_eq!(repair.runtime_links_added, 1);
        assert_eq!(
            document.objects[0].runtime_references,
            [sms_scene::SceneRuntimeReferenceBinding {
                required_factory_name: "Shine".to_string(),
                runtime_name: "fixture reward".to_string(),
                required: true,
                target_object_id: None,
            }]
        );
        assert_eq!(repair.repaired_factories, ["FixtureEnemy"]);
        assert!(
            document.has_effective_resource(b"fixtureanm/runtime.prm"),
            "repair should add the newly discovered runtime resource"
        );

        let second =
            repair_authored_catalog_resources(&mut document, std::slice::from_ref(&template));
        assert_eq!(second.resource_writes, 0);
        assert_eq!(second.runtime_links_added, 0);
        assert!(second.repaired_factories.is_empty());
        assert_eq!(document.archive_edits.resources.len(), 1);
    }

    #[test]
    #[ignore = "requires SMS_BASE_ROOT, SMS_PROJECT_ROOT, and the neighboring SMS decomp"]
    fn authored_project_stay_pakkun_repairs_and_exports_its_pollution_texture() {
        let base_root = std::env::var_os("SMS_BASE_ROOT")
            .map(PathBuf::from)
            .expect("set SMS_BASE_ROOT to the extracted game root");
        let project_root = std::env::var_os("SMS_PROJECT_ROOT")
            .map(PathBuf::from)
            .expect("set SMS_PROJECT_ROOT to the Graffito project data root");
        let decomp_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let registry = sms_schema::SchemaGenerator::new(decomp_root)
            .generate()
            .expect("generate decomp schema");
        let archives = sms_formats::discover_scene_archives(&base_root)
            .expect("discover retail scene archives");
        let catalog = sms_scene::ObjectAuthoringCatalog::build_with_base_root(
            &archives, &registry, &base_root,
        )
        .catalog;
        let template = catalog
            .find("StayPakkun")
            .expect("StayPakkun authoring template")
            .clone();
        let mut document =
            StageDocument::open_authored_project_stage(&base_root, "goopmap0", &project_root)
                .expect("open authored goopmap0 project stage");
        document.set_registry(registry.clone());

        let repair =
            repair_authored_catalog_resources(&mut document, std::slice::from_ref(&template));

        assert!(repair.errors.is_empty(), "{:?}", repair.errors);
        assert!(document.has_effective_resource(b"map/pollution/h_ma_rak.bti"));
        let rebuilt = document
            .build_stage_archive()
            .expect("build repaired stage");
        let reopened = sms_scene::SourceFreeStageArchive::parse(&rebuilt)
            .expect("reopen repaired stage archive");
        assert!(reopened.resource(b"map/pollution/h_ma_rak.bti").is_some());
        let StageResourceDocument::Texture(texture) = reopened
            .resource(b"map/pollution/h_ma_rak.bti")
            .expect("runtime pollution texture")
        else {
            panic!("runtime pollution resource is a BTI");
        };
        let expected = decode_bti_texture(texture.encode().expect("encode runtime BTI"))
            .expect("decode runtime BTI");
        let StageResourceDocument::Model(model) = reopened
            .resource(b"pakkun/pakun.bmd")
            .expect("Pakkun model")
        else {
            panic!("Pakkun resource is a model");
        };
        let model = J3dFile::parse(model.to_bytes().expect("encode baked Pakkun model"))
            .expect("parse baked Pakkun model");
        let baked = model
            .texture_previews()
            .expect("decode baked Pakkun textures")
            .into_iter()
            .find(|texture| texture.name == "H_ma_rak_dummy")
            .expect("Pakkun dummy texture remains named for runtime replacement");
        assert_eq!(baked.width, expected.width);
        assert_eq!(baked.height, expected.height);
        assert_eq!(baked.format, expected.format);
        assert_eq!(baked.rgba, expected.rgba);
    }

    #[test]
    fn catalog_preflight_reuses_existing_runtime_path_without_overwrite() {
        let raw_resource_path = b"mapobj/shared.prm".to_vec();
        let catalog_resource = empty_parameter_document();
        let temp = tempfile::tempdir().unwrap();
        let source_asset_path = temp.path().join("shared.prm");
        std::fs::write(&source_asset_path, catalog_resource.to_bytes().unwrap()).unwrap();
        let existing_resource = StageResourceDocument::Parameters(PrmFile {
            entries: vec![
                sms_formats::PrmEntry::new("mNum", sms_formats::PrmValue::I32(7)).unwrap(),
            ],
        });
        let mut document = command_test_document(Vec::new());
        document.insert_authored_resource(raw_resource_path.clone(), existing_resource.clone());
        let template = sms_scene::ObjectAuthoringTemplate {
            factory_name: "FixtureMapObj".to_string(),
            group_index: 4,
            record: JDramaRecord::new("FixtureMapObj", "fixture", JDramaRecordPayload::Empty)
                .unwrap(),
            dependencies: Vec::new(),
            character_records: Vec::new(),
            table_dependencies: Vec::new(),
            runtime_actor_references: Vec::new(),
            required_graph_names: Vec::new(),
            resources: vec![sms_scene::ObjectAuthoringResource {
                raw_resource_path: raw_resource_path.clone(),
                source_asset_path,
            }],
            preview_resource_path: None,
            source_stage: "retail-source".to_string(),
        };

        let preflight = preflight_catalog_resources(&document, &template).unwrap();

        assert!(preflight.writes.is_empty());
        assert_eq!(preflight.reused_existing_resources, 1);
        assert_eq!(
            document
                .effective_resource_clone(&raw_resource_path)
                .unwrap(),
            Some(existing_resource)
        );
    }
    #[test]
    fn exact_blank_stage_bootstrap_proxies_are_recognized_for_every_runtime_path() {
        let mut document = command_test_document(Vec::new());
        document.stage_archive = Some(
            sms_scene::SourceFreeStageArchive::new_for_blank(
                "test11",
                sms_scene::BLANK_STAGE_PRESET_VERSION,
            )
            .unwrap(),
        );

        for requirement in sms_scene::BLANK_STAGE_BOOTSTRAP_REQUIREMENTS {
            let proxy = built_in_proxy_document(requirement.raw_path);
            assert!(
                is_exact_blank_stage_bootstrap_proxy(&document, requirement.raw_path, &proxy,)
                    .unwrap(),
                "{} should be recognized as an exact built-in proxy",
                String::from_utf8_lossy(requirement.raw_path)
            );
        }

        let different_model = sms_authoring::built_in_blank_stage_proxy(b"different/model.bmd")
            .compile_bmd()
            .unwrap();
        let different_model =
            StageResourceDocument::parse_for_path(b"mapobj/coin.bmd", &different_model).unwrap();
        assert!(
            !is_exact_blank_stage_bootstrap_proxy(&document, b"mapobj/coin.bmd", &different_model,)
                .unwrap(),
            "a user-authored model at a bootstrap path must remain authoritative"
        );

        let mut imported = sms_scene::SourceFreeStageArchive::new().unwrap();
        imported.set_origin(sms_scene::StageOrigin::ImportedArchive);
        document.stage_archive = Some(imported);
        let coin_proxy = built_in_proxy_document(b"mapobj/coin.bmd");
        assert!(
            !is_exact_blank_stage_bootstrap_proxy(&document, b"mapobj/coin.bmd", &coin_proxy,)
                .unwrap(),
            "imported stages must never opt into authored proxy migration"
        );
    }

    #[test]
    fn catalog_preflight_upgrades_exact_coin_proxy_and_undo_restores_it() {
        let raw_resource_path = b"mapobj/coin.bmd".to_vec();
        let proxy_document = built_in_proxy_document(&raw_resource_path);
        let retail_bytes = sms_authoring::built_in_blank_stage_proxy(b"retail/coin.bmd")
            .compile_bmd()
            .unwrap();
        let retail_document =
            StageResourceDocument::parse_for_path(&raw_resource_path, &retail_bytes).unwrap();
        assert_ne!(proxy_document, retail_document);

        let temp = tempfile::tempdir().unwrap();
        let source_asset_path = temp.path().join("coin.bmd");
        std::fs::write(&source_asset_path, retail_bytes).unwrap();

        let mut archive = sms_scene::SourceFreeStageArchive::new_for_blank(
            "test11",
            sms_scene::BLANK_STAGE_PRESET_VERSION,
        )
        .unwrap();
        archive
            .insert_resource(raw_resource_path.clone(), proxy_document.clone())
            .unwrap();
        let mut document = command_test_document(Vec::new());
        document.stage_archive = Some(archive);

        let template = sms_scene::ObjectAuthoringTemplate {
            factory_name: "Coin".to_string(),
            runtime_actor_references: Vec::new(),
            group_index: 4,
            record: JDramaRecord::new("Coin", "coin", JDramaRecordPayload::Empty).unwrap(),
            dependencies: Vec::new(),
            character_records: Vec::new(),
            table_dependencies: Vec::new(),
            required_graph_names: Vec::new(),
            resources: vec![sms_scene::ObjectAuthoringResource {
                raw_resource_path: raw_resource_path.clone(),
                source_asset_path,
            }],
            preview_resource_path: Some(raw_resource_path.clone()),
            source_stage: "retail-source".to_string(),
        };

        let preflight = preflight_catalog_resources(&document, &template).unwrap();
        assert_eq!(preflight.reused_existing_resources, 0);
        assert_eq!(preflight.upgraded_bootstrap_proxies, 1);
        assert_eq!(preflight.writes.len(), 1);
        assert!(preflight.writes[0].upsert);
        assert_eq!(preflight.writes[0].document, retail_document);

        let edit = ObjectUndoRecord {
            deltas: Vec::new(),
            resource_deltas: catalog_resource_edit_deltas(&document, preflight.writes),
            route_delta: None,
        };
        edit.apply_forward(&mut document);
        assert_eq!(
            document
                .effective_resource_clone(&raw_resource_path)
                .unwrap(),
            Some(retail_document.clone())
        );
        let rebuilt = document.build_stage_archive().unwrap();
        let reopened = sms_scene::SourceFreeStageArchive::parse(&rebuilt).unwrap();
        assert_eq!(
            reopened.resource(&raw_resource_path),
            Some(&retail_document)
        );

        edit.apply_reverse(&mut document);
        assert_eq!(
            document
                .effective_resource_clone(&raw_resource_path)
                .unwrap(),
            Some(proxy_document)
        );
        assert!(document.archive_edits.resources.is_empty());
    }

    #[test]
    fn catalog_resource_import_is_atomic_with_object_undo_and_redo() {
        let mut app = SmsEditorApp {
            document: Some(command_test_document(Vec::new())),
            ..SmsEditorApp::default()
        };
        let raw_resource_path = b"map/catalog.prm".to_vec();
        let resource_deltas = catalog_resource_edit_deltas(
            app.document.as_ref().unwrap(),
            vec![CatalogResourceWrite {
                raw_resource_path: raw_resource_path.clone(),
                document: empty_parameter_document(),
                upsert: false,
            }],
        );
        let object = SceneObject::new("catalog-object", "FixtureEnemy");
        app.apply_object_edit(
            "Added catalog object",
            ObjectUndoRecord {
                deltas: vec![ObjectDelta::Insert { index: 0, object }],
                resource_deltas,
                route_delta: None,
            },
        );

        let document = app.document.as_ref().unwrap();
        assert_eq!(document.objects.len(), 1);
        assert!(document.has_effective_resource(&raw_resource_path));
        assert_eq!(document.archive_edits.resources.len(), 1);
        assert!(app.document_dirty);

        app.undo();
        let document = app.document.as_ref().unwrap();
        assert!(document.objects.is_empty());
        assert!(!document.has_effective_resource(&raw_resource_path));
        assert!(document.archive_edits.resources.is_empty());
        assert!(!document.assets.iter().any(|asset| {
            asset
                .path
                .to_string_lossy()
                .replace('\\', "/")
                .ends_with("!/map/catalog.prm")
        }));
        assert!(!app.document_dirty);

        app.redo();
        let document = app.document.as_ref().unwrap();
        assert_eq!(document.objects.len(), 1);
        assert!(document.has_effective_resource(&raw_resource_path));
        assert_eq!(document.archive_edits.resources.len(), 1);
        assert!(app.document_dirty);
    }

    #[test]
    fn catalog_preflight_restores_removed_baseline_resource_with_upsert_and_exact_undo() {
        let raw_resource_path = b"map/catalog.prm".to_vec();
        let catalog_resource = empty_parameter_document();
        let temp = tempfile::tempdir().unwrap();
        let source_asset_path = temp.path().join("catalog.prm");
        std::fs::write(&source_asset_path, catalog_resource.to_bytes().unwrap()).unwrap();

        let mut archive = sms_scene::SourceFreeStageArchive::new().unwrap();
        archive
            .insert_resource(raw_resource_path.clone(), catalog_resource.clone())
            .unwrap();
        let mut document = command_test_document(Vec::new());
        document.stage_archive = Some(archive);
        document
            .archive_edits
            .remove_resource(raw_resource_path.clone());
        let removed_overlay = document.archive_edits.clone();
        assert!(!document.has_effective_resource(&raw_resource_path));

        let template = sms_scene::ObjectAuthoringTemplate {
            factory_name: "FixtureEnemy".to_string(),
            runtime_actor_references: Vec::new(),
            group_index: 7,
            record: JDramaRecord::new("FixtureEnemy", "fixture enemy", JDramaRecordPayload::Empty)
                .unwrap(),
            dependencies: Vec::new(),
            character_records: Vec::new(),
            table_dependencies: Vec::new(),
            required_graph_names: Vec::new(),
            resources: vec![sms_scene::ObjectAuthoringResource {
                raw_resource_path: raw_resource_path.clone(),
                source_asset_path,
            }],
            preview_resource_path: None,
            source_stage: "fixture0".to_string(),
        };
        let preflight = preflight_catalog_resources(&document, &template).unwrap();
        assert_eq!(preflight.writes.len(), 1);
        assert!(preflight.writes[0].upsert);
        let resource_deltas = catalog_resource_edit_deltas(&document, preflight.writes);

        let mut app = SmsEditorApp {
            saved_archive_edits: removed_overlay.clone(),
            document: Some(document),
            ..SmsEditorApp::default()
        };
        app.apply_object_edit(
            "Restored catalog resource",
            ObjectUndoRecord {
                deltas: Vec::new(),
                resource_deltas,
                route_delta: None,
            },
        );

        let document = app.document.as_ref().unwrap();
        assert!(document.archive_edits.resource_removals.is_empty());
        assert_eq!(document.archive_edits.resources.len(), 1);
        assert_eq!(
            document.archive_edits.resources[0].mode,
            sms_scene::StageResourceEditMode::Upsert
        );
        let rebuilt = document.build_stage_archive().unwrap();
        let reopened = sms_scene::SourceFreeStageArchive::parse(&rebuilt).unwrap();
        assert_eq!(
            reopened.resource(&raw_resource_path),
            Some(&catalog_resource)
        );

        app.undo();

        let document = app.document.as_ref().unwrap();
        assert_eq!(document.archive_edits, removed_overlay);
        assert!(!document.has_effective_resource(&raw_resource_path));
        let rebuilt = document.build_stage_archive().unwrap();
        let reopened = sms_scene::SourceFreeStageArchive::parse(&rebuilt).unwrap();
        assert!(reopened.resource(&raw_resource_path).is_none());
        assert!(!app.document_dirty);
    }

    #[test]
    fn undo_restores_saved_resource_edit_order_exactly() {
        let mut document = command_test_document(Vec::new());
        document.insert_authored_resource(b"map/z_saved.prm".to_vec(), empty_parameter_document());
        document.insert_authored_resource(b"map/a_saved.prm".to_vec(), empty_parameter_document());
        let saved_archive_edits = document.archive_edits.clone();
        let mut app = SmsEditorApp {
            saved_archive_edits: saved_archive_edits.clone(),
            document: Some(document),
            ..SmsEditorApp::default()
        };
        let resource_deltas = catalog_resource_edit_deltas(
            app.document.as_ref().unwrap(),
            vec![CatalogResourceWrite {
                raw_resource_path: b"map/z_saved.prm".to_vec(),
                document: empty_parameter_document(),
                upsert: true,
            }],
        );
        app.apply_object_edit(
            "Updated saved catalog resource",
            ObjectUndoRecord {
                deltas: Vec::new(),
                resource_deltas,
                route_delta: None,
            },
        );
        assert!(app.document_dirty);

        app.undo();

        assert_eq!(
            app.document.as_ref().unwrap().archive_edits,
            saved_archive_edits
        );
        assert!(!app.document_dirty);
        assert_eq!(
            app.document.as_ref().unwrap().archive_edits.resources[0].raw_resource_path,
            b"map/z_saved.prm"
        );
    }

    #[test]
    fn archive_edit_saved_baseline_participates_in_dirty_equality() {
        let mut document = command_test_document(Vec::new());
        document.upsert_authored_resource(b"map/saved.prm".to_vec(), empty_parameter_document());
        let saved_archive_edits = document.archive_edits.clone();
        assert!(!stage_document_differs_from_saved(
            &document,
            &document.objects,
            &document.lighting,
            &saved_archive_edits,
        ));

        document.archive_edits.resources[0].mode = sms_scene::StageResourceEditMode::Insert;
        assert!(stage_document_differs_from_saved(
            &document,
            &document.objects,
            &document.lighting,
            &saved_archive_edits,
        ));
    }

    #[test]
    fn update_parameter_rejects_linked_read_only_fields() {
        let prototype = JDramaRecord {
            type_name: "FixtureEnemy".to_string(),
            name: "fixture".to_string(),
            payload: JDramaRecordPayload::Actor {
                transform: sms_formats::JDramaTransform {
                    translation: [0.0; 3],
                    rotation: [0.0; 3],
                    scale: [1.0; 3],
                },
                character_name: String::new(),
                light_map: sms_formats::JDramaLightMap::default(),
                fields: vec![JDramaField {
                    name: "graph_name".to_string(),
                    value: JDramaFieldValue::String("route_a".to_string()),
                }],
            },
        };
        let mut object = SceneObject::new("fixture-object", "FixtureEnemy");
        sms_scene::seed_scene_object_parameters(&mut object, &prototype).unwrap();
        object.placement = Some(sms_scene::PlacementBinding::Authored(
            sms_scene::AuthoredPlacement {
                raw_resource_path: b"map/scene.bin".to_vec(),
                target_group_index: 4,
                prototype,
                dependencies: Vec::new(),
            },
        ));
        let mut app = SmsEditorApp {
            selected_object_id: Some(object.id.clone()),
            document: Some(command_test_document(vec![object])),
            ..SmsEditorApp::default()
        };

        app.update_selected_parameter("graph_name", "route_b");

        let object = &app.document.as_ref().unwrap().objects[0];
        assert_eq!(object.raw_param("graph_name"), Some("route_a"));
        assert!(app.undo_stack.is_empty());
        assert!(app.log.iter().any(|message| {
            message.contains("Could not edit parameter 'graph_name'") && message.contains("respawn")
        }));
    }

    #[test]
    fn parameter_keyboard_transaction_survives_pointer_up_and_commits_once() {
        let prototype = JDramaRecord {
            type_name: "FixtureEnemy".to_string(),
            name: "fixture".to_string(),
            payload: JDramaRecordPayload::Actor {
                transform: sms_formats::JDramaTransform {
                    translation: [0.0; 3],
                    rotation: [0.0; 3],
                    scale: [1.0; 3],
                },
                character_name: String::new(),
                light_map: sms_formats::JDramaLightMap::default(),
                fields: vec![JDramaField {
                    name: "ordinary".to_string(),
                    value: JDramaFieldValue::U32(7),
                }],
            },
        };
        let mut object = SceneObject::new("fixture-object", "FixtureEnemy");
        sms_scene::seed_scene_object_parameters(&mut object, &prototype).unwrap();
        object.placement = Some(sms_scene::PlacementBinding::Authored(
            sms_scene::AuthoredPlacement {
                raw_resource_path: b"map/scene.bin".to_vec(),
                target_group_index: 4,
                prototype,
                dependencies: Vec::new(),
            },
        ));
        let mut app = SmsEditorApp {
            selected_object_id: Some(object.id.clone()),
            saved_objects: vec![object.clone()],
            document: Some(command_test_document(vec![object])),
            ..SmsEditorApp::default()
        };

        app.begin_parameter_undo_transaction();
        app.update_selected_parameter("ordinary", "8");
        app.finish_pointer_undo_transaction_if_released(false);
        assert!(
            app.undo_transaction.is_some(),
            "pointer-up fallback must not split a focused keyboard edit"
        );
        assert!(app.undo_stack.is_empty());

        app.update_selected_parameter("ordinary", "9");
        app.commit_undo_transaction("Updated object parameter");

        assert_eq!(app.undo_stack.len(), 1);
        assert_eq!(
            app.selected_object().unwrap().raw_param("ordinary"),
            Some("9")
        );
        app.undo();
        assert_eq!(
            app.selected_object().unwrap().raw_param("ordinary"),
            Some("7")
        );
    }

    #[test]
    fn unsafe_registry_classes_cannot_spawn_from_existing_templates() {
        let prototype = JDramaRecord {
            type_name: "UnsafeFixture".to_string(),
            name: "unsafe fixture".to_string(),
            payload: JDramaRecordPayload::Actor {
                transform: sms_formats::JDramaTransform {
                    translation: [0.0; 3],
                    rotation: [0.0; 3],
                    scale: [1.0; 3],
                },
                character_name: String::new(),
                light_map: sms_formats::JDramaLightMap::default(),
                fields: Vec::new(),
            },
        };
        let mut object = SceneObject::new("unsafe-object", "UnsafeFixture");
        object.placement = Some(sms_scene::PlacementBinding::Authored(
            sms_scene::AuthoredPlacement {
                raw_resource_path: b"map/scene.bin".to_vec(),
                target_group_index: 4,
                prototype,
                dependencies: Vec::new(),
            },
        ));
        let registry = ObjectRegistry {
            objects: vec![sms_schema::ObjectDefinition {
                factory_name: "UnsafeFixture".to_string(),
                class_name: "TUnsafeFixture".to_string(),
                category: "Fixture".to_string(),
                source: sms_schema::SchemaSource::MarNameRefGen,
                display_name: None,
                preview_model: None,
                hidden: false,
                unsafe_to_edit: true,
            }],
            ..ObjectRegistry::default()
        };
        let mut document = command_test_document(vec![object]);
        document.registry = Some(registry.clone());
        let app = SmsEditorApp {
            registry: Some(registry),
            document: Some(document),
            ..SmsEditorApp::default()
        };
        assert!(!app.can_spawn_factory("UnsafeFixture"));
    }

    #[test]
    fn graph_rewrites_cover_dependency_records() {
        let prototype = JDramaRecord {
            type_name: "FixtureEnemy".to_string(),
            name: "fixture".to_string(),
            payload: JDramaRecordPayload::Actor {
                transform: sms_formats::JDramaTransform {
                    translation: [0.0; 3],
                    rotation: [0.0; 3],
                    scale: [1.0; 3],
                },
                character_name: String::new(),
                light_map: sms_formats::JDramaLightMap::default(),
                fields: Vec::new(),
            },
        };
        let dependency = JDramaRecord {
            type_name: "FixtureManager".to_string(),
            name: "fixture manager".to_string(),
            payload: JDramaRecordPayload::Fields {
                fields: vec![JDramaField {
                    name: "graph_name".to_string(),
                    value: JDramaFieldValue::String("manager_route".to_string()),
                }],
            },
        };
        let template = sms_scene::ObjectAuthoringTemplate {
            factory_name: "FixtureEnemy".to_string(),
            group_index: 4,
            record: prototype,
            dependencies: vec![sms_scene::ObjectAuthoringDependency {
                group_index: 2,
                target: sms_scene::AuthoredPlacementDependencyTarget::IndexedGroup {
                    group_index: 2,
                },
                record: dependency,
            }],
            character_records: Vec::new(),
            table_dependencies: Vec::new(),
            runtime_actor_references: Vec::new(),
            required_graph_names: vec!["manager_route".to_string()],
            resources: Vec::new(),
            preview_resource_path: None,
            source_stage: "fixture0".to_string(),
        };
        let object = object_from_catalog_template(
            "fixture-object".to_string(),
            "FixtureEnemy".to_string(),
            [1.0, 2.0, 3.0],
            &template,
            &BTreeMap::from([(
                "manager_route".to_string(),
                "manager_route_authored".to_string(),
            )]),
        )
        .unwrap();
        let Some(sms_scene::PlacementBinding::Authored(authored)) = object.placement else {
            unreachable!()
        };
        let JDramaRecordPayload::Fields { fields } = &authored.dependencies[0].record.payload
        else {
            unreachable!()
        };
        assert!(fields.iter().any(|field| {
            field.name == "graph_name"
                && field.value == JDramaFieldValue::String("manager_route_authored".to_string())
        }));
    }
}
