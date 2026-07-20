use super::*;

const MAX_PARTICLE_CAPACITY: usize = 512;
const DEFAULT_TRANSFORM_FRAMES: f32 = 600.0;
const JPA_VOLUME_CUBE: u8 = 0;
const JPA_VOLUME_SPHERE: u8 = 1;
const JPA_VOLUME_CYLINDER: u8 = 2;
const JPA_VOLUME_TORUS: u8 = 3;
const JPA_VOLUME_POINT: u8 = 4;
const JPA_VOLUME_CIRCLE: u8 = 5;
const JPA_VOLUME_LINE: u8 = 6;

#[derive(Debug, Clone, Copy, PartialEq)]
struct SampledParticle {
    position: [f32; 3],
    half_size: [f32; 2],
    rotation: f32,
    color: [u8; 4],
    environment_color: [u8; 4],
    particle_type: u8,
    direction: [f32; 3],
    velocity: [f32; 3],
    trail_id: u32,
    quad_vertices: Option<[[f32; 3]; 4]>,
    quad_uvs: Option<[[f32; 2]; 4]>,
}

pub(super) fn load_actor_particle_effects(document: &StageDocument) -> BTreeMap<u16, JpaEffect> {
    let Some(registry) = document.registry.as_ref() else {
        return BTreeMap::new();
    };
    let actor_classes = document
        .objects
        .iter()
        .flat_map(|object| cpp_class_names_for_object(registry, object))
        .collect::<BTreeSet<_>>();
    let requested_ids = registry
        .actor_particle_bindings
        .iter()
        .filter(|binding| actor_classes.contains(binding.class_name.as_str()))
        .map(|binding| binding.effect_id)
        .collect::<BTreeSet<_>>();
    if requested_ids.is_empty() {
        return BTreeMap::new();
    }
    let requested_resources = registry
        .particle_resources
        .iter()
        .filter(|resource| requested_ids.contains(&resource.effect_id))
        .map(|resource| (resource.effect_id, resource.path.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut effects = BTreeMap::new();

    for asset in document
        .assets
        .iter()
        .filter(|asset| asset.kind == StageAssetKind::Particle)
    {
        resolve_actor_particle_asset(document, &requested_resources, asset, &mut effects);
    }
    if effects.len() == requested_resources.len() {
        return effects;
    }

    let mut files = Vec::new();
    let data_roots = [
        document.base_root.join("files").join("data"),
        document.base_root.join("data"),
    ];
    for root in data_roots.iter().filter(|root| root.exists()) {
        collect_particle_resource_candidates(root, root, &mut files);
    }
    files.sort_by_key(|path| {
        std::fs::metadata(path)
            .map(|metadata| metadata.len())
            .unwrap_or(u64::MAX)
    });

    for path in files
        .iter()
        .filter(|path| path.extension().is_some_and(|extension| extension == "jpa"))
    {
        let asset = StageAsset {
            path: path.clone(),
            kind: StageAssetKind::Particle,
        };
        resolve_actor_particle_asset(document, &requested_resources, &asset, &mut effects);
    }
    for archive in files.iter().filter(|path| {
        path.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("szs") || extension.eq_ignore_ascii_case("arc")
            })
    }) {
        let Ok(assets) = mount_scene_archive(archive) else {
            continue;
        };
        for asset in assets
            .iter()
            .filter(|asset| asset.kind == StageAssetKind::Particle)
        {
            resolve_actor_particle_asset(document, &requested_resources, asset, &mut effects);
        }
        if effects.len() == requested_resources.len() {
            break;
        }
    }
    effects
}

fn collect_particle_resource_candidates(
    root: &std::path::Path,
    path: &std::path::Path,
    files: &mut Vec<PathBuf>,
) {
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path
                .strip_prefix(root)
                .ok()
                .and_then(|relative| relative.components().next())
                .is_some_and(|component| component.as_os_str().eq_ignore_ascii_case("scene"))
            {
                continue;
            }
            collect_particle_resource_candidates(root, &path, files);
        } else if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("jpa")
                    || extension.eq_ignore_ascii_case("szs")
                    || extension.eq_ignore_ascii_case("arc")
            })
        {
            files.push(path);
        }
    }
}

fn resolve_actor_particle_asset(
    document: &StageDocument,
    requested: &BTreeMap<u16, String>,
    asset: &StageAsset,
    effects: &mut BTreeMap<u16, JpaEffect>,
) {
    let normalized = normalized_preview_asset_path(&asset.path.to_string_lossy());
    let internal = normalized
        .split_once("!/")
        .map(|(_, internal)| internal)
        .unwrap_or(normalized.as_str());
    for (effect_id, resource_path) in requested {
        if effects.contains_key(effect_id) {
            continue;
        }
        let resource = normalized_preview_asset_path(resource_path);
        let matches = if resource.contains('/') {
            internal.ends_with(resource.trim_start_matches('/'))
        } else {
            internal
                .rsplit('/')
                .next()
                .is_some_and(|name| name.eq_ignore_ascii_case(&resource))
        };
        if !matches {
            continue;
        }
        let Ok(bytes) = document.read_asset_bytes(&asset.path) else {
            continue;
        };
        if let Ok(effect) = JpaEffect::parse(&bytes) {
            effects.insert(*effect_id, effect);
        }
    }
}

fn cpp_class_names_for_object<'a>(
    registry: &'a ObjectRegistry,
    object: &'a SceneObject,
) -> Vec<&'a str> {
    let mut classes = Vec::new();
    if let Some(class_name) = registry
        .find_object(&object.factory_name)
        .map(|definition| definition.class_name.as_str())
        .filter(|class_name| *class_name != "Unknown")
    {
        classes.push(class_name);
    }
    if let Some(class_name) = object
        .class_name
        .as_deref()
        .filter(|class_name| class_name.starts_with('T'))
    {
        classes.push(class_name);
    }
    let object_names = [
        object.factory_name.as_str(),
        object.class_name.as_deref().unwrap_or(""),
    ];
    for binding in &registry.actor_particle_bindings {
        let short_class = binding
            .class_name
            .rsplit("::")
            .next()
            .unwrap_or(&binding.class_name);
        let without_prefix = short_class.strip_prefix('T').unwrap_or(short_class);
        if object_names.contains(&without_prefix) {
            classes.push(binding.class_name.as_str());
        }
    }
    classes.sort_unstable();
    classes.dedup();
    classes
}

#[allow(clippy::too_many_arguments)]
pub(super) fn push_actor_particle_previews(
    document: &StageDocument,
    object: &SceneObject,
    effects: &BTreeMap<u16, JpaEffect>,
    joint_matrices: &[J3dMatrix34],
    transform: Transform,
    model_index: usize,
    textures: &mut Vec<PreviewTexture>,
    triangles: &mut Vec<PreviewTriangle>,
    next_packet_index: &mut usize,
    previews: &mut Vec<LevelTransformParticlePreview>,
) {
    let Some(registry) = document.registry.as_ref() else {
        return;
    };
    let class_names = cpp_class_names_for_object(registry, object);
    if class_names.is_empty() {
        return;
    }
    for binding in registry
        .actor_particle_bindings
        .iter()
        .filter(|binding| class_names.contains(&binding.class_name.as_str()))
    {
        let Some(effect) = effects.get(&binding.effect_id) else {
            continue;
        };
        let origin = match binding.target {
            ParticleBindingTarget::ActorOrigin => transform.translation,
            ParticleBindingTarget::ModelJoint(index) => {
                let Some(matrix) = joint_matrices.get(index).copied() else {
                    continue;
                };
                transform_preview_point([matrix[0][3], matrix[1][3], matrix[2][3]], transform)
            }
        };
        let preview_start = previews.len();
        append_particle_preview(
            effect.clone(),
            Some(binding.effect_id),
            origin,
            textures,
            triangles,
            next_packet_index,
            previews,
        );
        for preview in &mut previews[preview_start..] {
            preview.model_index = Some(model_index);
            for triangle in &mut triangles[preview.triangle_range.clone()] {
                triangle.model_index = model_index;
            }
        }
    }
}

pub(super) fn push_level_transform_particle_previews(
    document: &StageDocument,
    transform_models: &[LevelTransformModelPreview],
    textures: &mut Vec<PreviewTexture>,
    triangles: &mut Vec<PreviewTriangle>,
    next_packet_index: &mut usize,
    previews: &mut Vec<LevelTransformParticlePreview>,
) {
    let target_centers = transform_models
        .iter()
        .flat_map(level_transform_target_centers)
        .collect::<Vec<_>>();
    if target_centers.is_empty() {
        return;
    }

    let mut effect_groups = BTreeMap::<String, Vec<JpaEffect>>::new();
    for asset in document
        .assets
        .iter()
        .filter(|asset| asset.kind == StageAssetKind::Particle)
    {
        let normalized = normalized_preview_asset_path(&asset.path.to_string_lossy());
        let Some(group) = particle_pair_group(&normalized) else {
            continue;
        };
        let Ok(bytes) = document.read_asset_bytes(&asset.path) else {
            continue;
        };
        let Ok(effect) = JpaEffect::parse(&bytes) else {
            continue;
        };
        effect_groups.entry(group).or_default().push(effect);
    }

    let effect_groups = effect_groups
        .into_values()
        .filter(|effects| {
            effects.len() >= 2
                && effects.iter().any(|effect| effect.uses_screen_texture)
                && effects.iter().any(|effect| !effect.uses_screen_texture)
                && effects.iter().all(|effect| effect.emitter.max_frame > 0)
        })
        .collect::<Vec<_>>();
    let assignments = if effect_groups.len() == 1 {
        target_centers
            .iter()
            .copied()
            .map(|center| (&effect_groups[0], center))
            .collect::<Vec<_>>()
    } else {
        effect_groups
            .iter()
            .zip(target_centers.iter().copied())
            .collect::<Vec<_>>()
    };

    for (effects, target_center) in assignments {
        for effect in effects {
            append_particle_preview(
                effect.clone(),
                None,
                target_center,
                textures,
                triangles,
                next_packet_index,
                previews,
            );
        }
    }
}

fn level_transform_target_centers(model: &LevelTransformModelPreview) -> Vec<[f32; 3]> {
    let triangles = model
        .file
        .triangles_with_joint_overrides(model.loader_flags, &[])
        .ok();
    model
        .targets
        .iter()
        .filter(|target| target.behavior == LevelTransformBehavior::Linear)
        .filter_map(|target| {
            let shapes = model
                .file
                .joint_subtree_shape_indices(target.joint_index)
                .ok()?;
            let mut minimum = [f32::INFINITY; 3];
            let mut maximum = [f32::NEG_INFINITY; 3];
            for vertex in triangles
                .iter()
                .flatten()
                .filter(|triangle| shapes.contains(&triangle.shape_index))
                .flat_map(|triangle| triangle.vertices)
            {
                for axis in 0..3 {
                    minimum[axis] = minimum[axis].min(vertex[axis]);
                    maximum[axis] = maximum[axis].max(vertex[axis]);
                }
            }
            if minimum.iter().all(|value| value.is_finite())
                && maximum.iter().all(|value| value.is_finite())
            {
                Some(std::array::from_fn(|axis| {
                    (minimum[axis] + maximum[axis]) * 0.5
                }))
            } else {
                model
                    .file
                    .joint_bounds(target.joint_index)
                    .ok()
                    .map(|(minimum, maximum)| {
                        std::array::from_fn(|axis| (minimum[axis] + maximum[axis]) * 0.5)
                    })
            }
        })
        .collect()
}

fn append_particle_preview(
    effect: JpaEffect,
    shared_simulation_id: Option<u16>,
    target_center: [f32; 3],
    textures: &mut Vec<PreviewTexture>,
    triangles: &mut Vec<PreviewTriangle>,
    next_packet_index: &mut usize,
    previews: &mut Vec<LevelTransformParticlePreview>,
) {
    let origin_offset = if effect
        .emitter
        .translation
        .iter()
        .all(|component| component.abs() <= 0.01)
    {
        target_center
    } else {
        [0.0; 3]
    };
    let source_texture_index = if effect.uses_screen_texture {
        usize::from(effect.indirect_texture_index.unwrap_or(1))
    } else {
        usize::from(effect.base_shape.texture_index)
    };
    if effect.child_shape.is_none_or(|child| child.draw_parent) {
        append_particle_shape_preview(
            effect.clone(),
            JpaParticleKind::Parent,
            shared_simulation_id,
            origin_offset,
            source_texture_index,
            effect.uses_screen_texture,
            textures,
            triangles,
            next_packet_index,
            previews,
        );
    }
    if let Some(child) = effect.child_shape {
        append_particle_shape_preview(
            effect,
            JpaParticleKind::Child,
            shared_simulation_id,
            origin_offset,
            usize::from(child.texture_index),
            false,
            textures,
            triangles,
            next_packet_index,
            previews,
        );
    }
}

fn particle_capacity(effect: &JpaEffect, kind: JpaParticleKind) -> usize {
    let emitter = effect.emitter;
    let maximum_spawn_rate = maximum_keyframed_value(effect, 0, emitter.spawn_rate)
        * (1.0 + emitter.spawn_rate_variance.abs());
    let maximum_lifetime = maximum_keyframed_value(effect, 4, f32::from(emitter.base_lifetime))
        * (1.0 + (-emitter.lifetime_random_scale).max(0.0));
    let interval = usize::from(emitter.emit_interval) + 1;
    let parent_capacity = particle_birth_capacity(maximum_spawn_rate, maximum_lifetime, interval);

    let capacity = match kind {
        JpaParticleKind::Parent => parent_capacity,
        JpaParticleKind::Child => {
            let Some(child) = effect.child_shape else {
                return 1;
            };
            let child_lifetime = f32::from(child.lifetime.max(1));
            let contributing_parents = particle_birth_capacity(
                maximum_spawn_rate,
                maximum_lifetime + child_lifetime,
                interval,
            );
            let child_interval = usize::from(child.spawn_step) + 1;
            let births_per_parent =
                (((maximum_lifetime.min(child_lifetime) / child_interval as f32).ceil() as usize)
                    + 1)
                .saturating_mul(usize::try_from(child.spawn_count.max(0)).unwrap_or(0));
            contributing_parents.saturating_mul(births_per_parent)
        }
    };
    let stripe_multiplier = match kind {
        JpaParticleKind::Parent if effect.base_shape.particle_type == 6 => 2,
        JpaParticleKind::Child
            if effect
                .child_shape
                .is_some_and(|child| child.particle_type == 6) =>
        {
            2
        }
        _ => 1,
    };
    capacity
        .saturating_mul(stripe_multiplier)
        .clamp(1, MAX_PARTICLE_CAPACITY)
}

fn particle_birth_capacity(maximum_spawn_rate: f32, lifetime: f32, interval: usize) -> usize {
    if maximum_spawn_rate <= 0.0 || lifetime <= 0.0 {
        return 1;
    }
    let births_per_event = maximum_spawn_rate.ceil().max(1.0) as usize;
    let concurrent_events = (lifetime / interval.max(1) as f32).ceil() as usize + 1;
    births_per_event.saturating_mul(concurrent_events)
}

fn maximum_keyframed_value(effect: &JpaEffect, parameter_index: u8, fallback: f32) -> f32 {
    let Some(curve) = effect
        .keyframes
        .iter()
        .find(|curve| curve.parameter_index == parameter_index)
    else {
        return fallback.max(0.0);
    };
    let endpoint_maximum = curve.keys.iter().map(|key| key[1]).fold(0.0f32, f32::max);
    curve
        .keys
        .windows(2)
        .fold(endpoint_maximum, |maximum, pair| {
            let span = (pair[1][0] - pair[0][0]).abs();
            // Hermite tangent basis functions stay within roughly +/-0.148.
            // Using 0.15 gives a conservative bound without sampling a timeline.
            let tangent_overshoot = 0.15 * span * (pair[0][3].abs() + pair[1][2].abs());
            maximum.max(pair[0][1].max(pair[1][1]) + tangent_overshoot)
        })
}

#[allow(clippy::too_many_arguments)]
fn append_particle_shape_preview(
    effect: JpaEffect,
    kind: JpaParticleKind,
    shared_simulation_id: Option<u16>,
    origin_offset: [f32; 3],
    source_texture_index: usize,
    screen_distortion: bool,
    textures: &mut Vec<PreviewTexture>,
    triangles: &mut Vec<PreviewTriangle>,
    next_packet_index: &mut usize,
    previews: &mut Vec<LevelTransformParticlePreview>,
) {
    let Some(source_texture) = effect.textures.get(source_texture_index) else {
        return;
    };
    let texture_index = push_j3d_preview_textures(textures, std::slice::from_ref(source_texture));
    let draw_shape = match kind {
        JpaParticleKind::Parent => effect.base_shape,
        JpaParticleKind::Child => {
            let mut shape = effect.base_shape;
            if let Some(child) = effect.child_shape {
                shape.particle_type = child.particle_type;
                shape.direction_type = child.direction_type;
                shape.rotation_type = child.rotation_type;
                shape.environment_color = child.environment_color;
            }
            shape
        }
    };
    let extra_shape = matches!(kind, JpaParticleKind::Parent)
        .then_some(effect.extra_shape)
        .flatten();
    let triangle_start = triangles.len();
    let packet_index = *next_packet_index;
    *next_packet_index += 1;
    let particle_capacity = particle_capacity(&effect, kind);
    for _ in 0..particle_capacity {
        push_particle_slot(
            triangles,
            packet_index,
            texture_index,
            draw_shape,
            extra_shape,
            screen_distortion,
        );
    }
    previews.push(LevelTransformParticlePreview {
        origin_offset,
        effect,
        kind,
        shared_simulation_id,
        triangle_range: triangle_start..triangles.len(),
        particle_capacity,
        model_index: None,
    });
}

pub(super) fn level_transform_duration_frames(previews: &[LevelTransformParticlePreview]) -> f32 {
    if previews.is_empty() {
        return DEFAULT_TRANSFORM_FRAMES;
    }

    previews.iter().fold(1.0, |duration, preview| {
        duration.max(jpa_emission_end_frame(&preview.effect))
    })
}

pub(super) fn level_transform_duration_seconds(duration_frames: f32) -> f32 {
    duration_frames.max(1.0) / SMS_ANIMATION_FRAMES_PER_SECOND
}

pub(super) fn level_transform_particle_end_frames(
    previews: &[LevelTransformParticlePreview],
) -> f32 {
    previews.iter().fold(0.0, |end, preview| {
        end.max(jpa_particle_end_frame(&preview.effect))
    })
}

fn jpa_emission_end_frame(effect: &JpaEffect) -> f32 {
    let emitter = effect.emitter;
    // Map-event geometry advances during the effect's active emitter window.
    // Existing particles can outlive the emitter, but that death tail must not
    // be stretched across the map transformation itself.
    emission_end_frame(emitter.start_frame, emitter.max_frame)
}

fn emission_end_frame(start_frame: i16, max_frame: i16) -> f32 {
    f32::from(start_frame.max(0)) + f32::from(max_frame.max(1))
}

fn jpa_particle_end_frame(effect: &JpaEffect) -> f32 {
    let emitter = effect.emitter;
    let maximum_lifetime = effect
        .keyframes
        .iter()
        .filter(|curve| curve.parameter_index == 4)
        .flat_map(|curve| curve.keys.iter().map(|key| key[1]))
        .fold(f32::from(emitter.base_lifetime), f32::max)
        .max(1.0);
    let child_lifetime = effect
        .child_shape
        .map(|child| f32::from(child.lifetime.max(0)))
        .unwrap_or(0.0);
    particle_end_frame(
        emitter.start_frame,
        emitter.max_frame,
        maximum_lifetime + child_lifetime,
    )
}

fn particle_end_frame(start_frame: i16, max_frame: i16, maximum_lifetime: f32) -> f32 {
    emission_end_frame(start_frame, max_frame) + maximum_lifetime.max(1.0)
}

pub(super) fn level_transform_sample_progress(
    progress: f32,
    duration_frames: f32,
    playing: bool,
) -> f32 {
    if !playing {
        return progress.clamp(0.0, 1.0);
    }
    let frame_count = duration_frames.max(1.0);
    (progress.clamp(0.0, 1.0) * frame_count).floor() / frame_count
}

fn particle_pair_group(path: &str) -> Option<String> {
    let stem = path.rsplit('/').next()?.strip_suffix(".jpa")?;
    stem.strip_suffix("_a")
        .or_else(|| stem.strip_suffix("_b"))
        .map(str::to_string)
}

pub(super) fn apply_level_transform_particles(
    previews: &[LevelTransformParticlePreview],
    retail_frame: f32,
    triangles: &mut [PreviewTriangle],
) {
    for preview in previews {
        let samples = sample_particle_preview(preview, preview.origin_offset, retail_frame);
        apply_particle_samples(preview, &samples, [0.0; 3], triangles);
    }
}

/// Actor-bound emitters with the same retail effect ID are deterministic and
/// phase-aligned. Simulate each effect once, then translate that exact result
/// to every actor instead of replaying its full history for every instance.
pub(super) fn apply_actor_particles(
    previews: &[LevelTransformParticlePreview],
    retail_frame: f32,
    triangles: &mut [PreviewTriangle],
) {
    let mut shared_samples = BTreeMap::<(u16, JpaParticleKind), Vec<SampledParticle>>::new();
    for preview in previews {
        let Some(effect_id) = preview.shared_simulation_id else {
            let samples = sample_particle_preview(preview, preview.origin_offset, retail_frame);
            apply_particle_samples(preview, &samples, [0.0; 3], triangles);
            continue;
        };
        let samples = shared_samples
            .entry((effect_id, preview.kind))
            .or_insert_with(|| sample_particle_preview(preview, [0.0; 3], retail_frame));
        apply_particle_samples(preview, samples, preview.origin_offset, triangles);
    }
}

fn sample_particle_preview(
    preview: &LevelTransformParticlePreview,
    origin_offset: [f32; 3],
    retail_frame: f32,
) -> Vec<SampledParticle> {
    let mut samples = match preview.kind {
        JpaParticleKind::Parent => sample_particles(
            &preview.effect,
            origin_offset,
            retail_frame,
            preview.particle_capacity,
        ),
        JpaParticleKind::Child => sample_child_particles(
            &preview.effect,
            origin_offset,
            retail_frame,
            preview.particle_capacity,
        ),
    };
    let particle_type = match preview.kind {
        JpaParticleKind::Parent => preview.effect.base_shape.particle_type,
        JpaParticleKind::Child => preview
            .effect
            .child_shape
            .map_or(preview.effect.base_shape.particle_type, |child| {
                child.particle_type
            }),
    };
    if matches!(particle_type, 5 | 6) {
        samples = stripe_particle_segments(
            &samples,
            particle_type == 6,
            preview.effect.base_shape.flags & 1 != 0,
            preview.particle_capacity,
        );
    }
    samples
}

fn apply_particle_samples(
    preview: &LevelTransformParticlePreview,
    samples: &[SampledParticle],
    translation: [f32; 3],
    triangles: &mut [PreviewTriangle],
) {
    let slots = triangles[preview.triangle_range.clone()].chunks_exact_mut(2);
    for (index, slot) in slots.enumerate() {
        if let Some(sample) = samples.get(index).copied() {
            update_particle_slot(slot, sample, translation);
        } else {
            hide_particle_slot(slot);
        }
    }
}

fn sample_child_particles(
    effect: &JpaEffect,
    origin_offset: [f32; 3],
    retail_frame: f32,
    capacity: usize,
) -> Vec<SampledParticle> {
    let Some(child) = effect.child_shape else {
        return Vec::new();
    };
    let emitter = effect.emitter;
    let emitter_frame = retail_frame - f32::from(emitter.start_frame.max(0));
    if emitter_frame < 0.0 {
        return Vec::new();
    }

    let final_birth_frame = final_emission_birth_frame(emitter_frame, emitter.max_frame);
    let interval = u32::from(emitter.emit_interval) + 1;
    let mut spawn_accumulator = 0.0f32;
    let mut birth_serial = 0u32;
    let mut result = Vec::new();

    for birth_frame in (0..=final_birth_frame).step_by(interval as usize) {
        let spawn_rate = keyframe_value(effect, 0, birth_frame as f32)
            .unwrap_or(emitter.spawn_rate)
            * (1.0
                + emitter.spawn_rate_variance * (random01(birth_frame ^ 0x510e_527f) * 2.0 - 1.0));
        let spawn_rate = spawn_rate.max(0.0);
        spawn_accumulator += spawn_rate;
        let mut parent_count = spawn_accumulator.floor() as u32;
        spawn_accumulator -= parent_count as f32;
        if birth_frame == 0 && parent_count == 0 && spawn_rate > 0.0 {
            parent_count = 1;
        }

        for _ in 0..parent_count {
            let parent_seed = birth_serial
                .wrapping_mul(0x9e37_79b9)
                .wrapping_add(0x6d2b_79f5);
            birth_serial = birth_serial.wrapping_add(1);
            let life_random = random01(parent_seed ^ 0xa511_e9b3);
            let base_lifetime = keyframe_value(effect, 4, birth_frame as f32)
                .unwrap_or(f32::from(emitter.base_lifetime));
            let parent_lifetime =
                base_lifetime * (1.0 - emitter.lifetime_random_scale * life_random);
            let parent_age_now = emitter_frame - birth_frame as f32;
            if parent_age_now < 0.0 {
                continue;
            }
            let first_child_age = (child.spawn_timing.clamp(0.0, 1.0)
                * (parent_lifetime - 1.0).max(0.0))
            .ceil() as u32;
            let last_child_age =
                parent_age_now.floor().min((parent_lifetime - 1.0).max(0.0)) as u32;
            if first_child_age > last_child_age {
                continue;
            }

            let local = volume_position(effect, parent_seed);
            let parent_velocity = initial_velocity(effect, local, parent_seed, birth_frame as f32);
            for child_birth_age in
                (first_child_age..=last_child_age).step_by(usize::from(child.spawn_step) + 1)
            {
                for child_index in 0..u32::try_from(child.spawn_count.max(0)).unwrap_or(0) {
                    let child_age = parent_age_now - child_birth_age as f32;
                    let child_lifetime = f32::from(child.lifetime.max(1));
                    if child_age < 0.0 || child_age >= child_lifetime {
                        continue;
                    }
                    let seed = parent_seed
                        .wrapping_add(child_birth_age.wrapping_mul(0x85eb_ca6b))
                        .wrapping_add(child_index.wrapping_mul(0xc2b2_ae35));
                    let parent_age = child_birth_age as f32;
                    let mut child_velocity = [0.0; 3];
                    let (parent_position, parent_current_velocity) = simulate_particle_motion(
                        effect,
                        std::array::from_fn(|axis| local[axis] * emitter.scale[axis]),
                        parent_velocity,
                        parent_seed,
                        parent_age,
                        parent_lifetime,
                    );
                    let random_offset = random_unit3(seed ^ 0x27d4_eb2d)
                        .map(|value| value * child.position_random * random01(seed ^ 0x1656_67b1));
                    let random_velocity = random_unit3(seed ^ 0xd3a2_646c);
                    let velocity_scale = child.velocity
                        * (1.0
                            + child.velocity_random * (random01(seed ^ 0xfd70_46c5) * 2.0 - 1.0));
                    for axis in 0..3 {
                        child_velocity[axis] = parent_current_velocity[axis]
                            * child.inherit_velocity
                            + random_velocity[axis] * velocity_scale;
                    }
                    let child_start =
                        std::array::from_fn(|axis| parent_position[axis] + random_offset[axis]);
                    let (child_position, child_current_velocity) =
                        if child.children_affected_by_fields {
                            simulate_particle_motion(
                                effect,
                                child_start,
                                child_velocity,
                                seed,
                                child_age,
                                child_lifetime,
                            )
                        } else {
                            (
                                std::array::from_fn(|axis| {
                                    child_start[axis] + child_velocity[axis] * child_age
                                }),
                                child_velocity,
                            )
                        };
                    let position = std::array::from_fn(|axis| {
                        origin_offset[axis] + emitter.translation[axis] + child_position[axis]
                    });
                    let parent_life = (parent_age / parent_lifetime.max(1.0)).clamp(0.0, 1.0);
                    let inherited_scale = if child.inherit_flags & 1 != 0 {
                        let parent =
                            particle_scale(effect, parent_life, parent_seed, birth_frame as f32);
                        parent.map(|value| value * child.inherit_scale)
                    } else {
                        [1.0; 2]
                    };
                    let mut color = child.color;
                    if child.inherit_flags & 4 != 0 {
                        for (channel, parent) in color[..3]
                            .iter_mut()
                            .zip(effect.base_shape.color[..3].iter().copied())
                        {
                            *channel =
                                lerp(f32::from(*channel), f32::from(parent), child.inherit_rgb)
                                    .round()
                                    .clamp(0.0, 255.0) as u8;
                        }
                    }
                    if child.inherit_flags & 2 != 0 {
                        color[3] = lerp(
                            f32::from(color[3]),
                            f32::from(effect.base_shape.color[3]),
                            child.inherit_alpha,
                        )
                        .round()
                        .clamp(0.0, 255.0) as u8;
                    }
                    let life = (child_age / child_lifetime).clamp(0.0, 1.0);
                    if child.alpha_out_enabled {
                        color[3] = (f32::from(color[3]) * (1.0 - life))
                            .round()
                            .clamp(0.0, 255.0) as u8;
                    }
                    let child_base_size = if child.inherit_flags & 1 != 0 {
                        effect.base_shape.size
                    } else {
                        child.size
                    };
                    let scale_out = if child.scale_out_enabled {
                        1.0 - life
                    } else {
                        1.0
                    };
                    result.push(SampledParticle {
                        position,
                        half_size: [
                            child_base_size[0] * 25.0 * inherited_scale[0] * scale_out,
                            child_base_size[1] * 25.0 * inherited_scale[1] * scale_out,
                        ],
                        rotation: if child.rotate_enabled {
                            particle_rotation(effect, parent_age, parent_seed)
                                + child.rotate_speed * child_age
                        } else {
                            particle_rotation(effect, parent_age, parent_seed)
                        },
                        color,
                        environment_color: child.environment_color,
                        particle_type: child.particle_type,
                        direction: particle_draw_direction(
                            child.direction_type,
                            child_current_velocity,
                            random_offset,
                            effect.emitter.direction,
                        ),
                        velocity: child_current_velocity,
                        trail_id: parent_seed,
                        quad_vertices: None,
                        quad_uvs: None,
                    });
                    if result.len() == capacity {
                        return result;
                    }
                }
            }
        }
    }
    result
}

fn sample_particles(
    effect: &JpaEffect,
    origin_offset: [f32; 3],
    retail_frame: f32,
    capacity: usize,
) -> Vec<SampledParticle> {
    let emitter = effect.emitter;
    let emitter_frame = retail_frame - f32::from(emitter.start_frame.max(0));
    if emitter_frame < 0.0 {
        return Vec::new();
    }
    let final_birth_frame = final_emission_birth_frame(emitter_frame, emitter.max_frame);
    let interval = u32::from(emitter.emit_interval) + 1;
    let mut spawn_accumulator = 0.0f32;
    let mut birth_serial = 0u32;
    let mut result = Vec::new();

    for birth_frame in (0..=final_birth_frame).step_by(interval as usize) {
        let spawn_rate = effect
            .keyframes
            .iter()
            .find(|curve| curve.parameter_index == 0)
            .and_then(|curve| curve.sample(birth_frame as f32))
            .unwrap_or(emitter.spawn_rate)
            * (1.0
                + emitter.spawn_rate_variance * (random01(birth_frame ^ 0x510e_527f) * 2.0 - 1.0));
        let spawn_rate = spawn_rate.max(0.0);
        spawn_accumulator += spawn_rate;
        let mut spawn_count = spawn_accumulator.floor() as u32;
        spawn_accumulator -= spawn_count as f32;
        if birth_frame == 0 && spawn_count == 0 && spawn_rate > 0.0 {
            spawn_count = 1;
        }

        for _ in 0..spawn_count {
            let seed = birth_serial
                .wrapping_mul(0x9e37_79b9)
                .wrapping_add(0x6d2b_79f5);
            birth_serial = birth_serial.wrapping_add(1);
            let life_random = random01(seed ^ 0xa511_e9b3);
            let base_lifetime = keyframe_value(effect, 4, birth_frame as f32)
                .unwrap_or(f32::from(emitter.base_lifetime));
            let lifetime = base_lifetime * (1.0 - emitter.lifetime_random_scale * life_random);
            let age = emitter_frame - birth_frame as f32;
            if age < 0.0 || age >= lifetime.max(1.0) {
                continue;
            }
            let life = (age / lifetime.max(1.0)).clamp(0.0, 1.0);
            let local = volume_position(effect, seed);
            let velocity = initial_velocity(effect, local, seed, birth_frame as f32);
            let (local_position, current_velocity) = simulate_particle_motion(
                effect,
                std::array::from_fn(|axis| local[axis] * emitter.scale[axis]),
                velocity,
                seed,
                age,
                lifetime,
            );
            let position = std::array::from_fn(|axis| {
                origin_offset[axis] + emitter.translation[axis] + local_position[axis]
            });
            let scale = particle_scale(effect, life, seed, birth_frame as f32);
            let (mut color, environment_color) =
                particle_colors(effect, age, life, seed, emitter_frame);
            if effect.uses_screen_texture {
                color = [190, 220, 255, 92];
            }
            color[3] = ((f32::from(color[3]) * particle_alpha(effect, life))
                .round()
                .clamp(0.0, 255.0)) as u8;
            result.push(SampledParticle {
                position,
                half_size: [
                    effect.base_shape.size[0] * 25.0 * scale[0],
                    effect.base_shape.size[1] * 25.0 * scale[1],
                ],
                rotation: particle_rotation(effect, age, seed),
                color,
                environment_color,
                particle_type: effect.base_shape.particle_type,
                direction: particle_draw_direction(
                    effect.base_shape.direction_type,
                    current_velocity,
                    local,
                    emitter.direction,
                ),
                velocity: current_velocity,
                trail_id: seed,
                quad_vertices: None,
                quad_uvs: None,
            });
            if result.len() == capacity {
                return result;
            }
        }
    }
    result
}

fn final_emission_birth_frame(emitter_frame: f32, max_frame: i16) -> u32 {
    let current = emitter_frame.floor().max(0.0) as u32;
    if max_frame == 0 {
        current
    } else if max_frame < 0 {
        0
    } else {
        current.min(max_frame as u32)
    }
}

fn volume_position(effect: &JpaEffect, seed: u32) -> [f32; 3] {
    let size = f32::from(effect.emitter.volume_size);
    let distance = if effect.emitter.flags & 1 != 0 {
        random01(seed ^ 0x6a09_e667).sqrt()
    } else {
        random01(seed ^ 0x6a09_e667)
    };
    let radius = size
        * (effect.emitter.volume_min_radius + distance * (1.0 - effect.emitter.volume_min_radius));
    let yaw = (random01(seed ^ 0xbb67_ae85) * 2.0 - 1.0)
        * std::f32::consts::PI
        * effect.emitter.volume_yaw_sweep;
    match effect.emitter.volume_type {
        JPA_VOLUME_CUBE => [
            (random01(seed ^ 0x68bc_21eb) - 0.5) * size,
            (random01(seed ^ 0x02e5_be93) - 0.5) * size,
            (random01(seed ^ 0x967a_889b) - 0.5) * size,
        ],
        JPA_VOLUME_SPHERE => random_unit3(seed ^ 0x3c6e_f372).map(|value| value * radius),
        JPA_VOLUME_CYLINDER => [
            yaw.sin() * radius,
            (random01(seed ^ 0x02e5_be93) - 0.5) * 2.0 * size,
            yaw.cos() * radius,
        ],
        JPA_VOLUME_TORUS => {
            let tube_angle = random01(seed ^ 0xa54f_f53a) * std::f32::consts::TAU;
            let tube_radius = size * effect.emitter.volume_min_radius;
            [
                yaw.sin() * (size + tube_radius * tube_angle.cos()),
                tube_radius * tube_angle.sin(),
                yaw.cos() * (size + tube_radius * tube_angle.cos()),
            ]
        }
        JPA_VOLUME_POINT => [0.0; 3],
        JPA_VOLUME_CIRCLE => [yaw.sin() * radius, 0.0, yaw.cos() * radius],
        JPA_VOLUME_LINE => [0.0, 0.0, (random01(seed ^ 0x967a_889b) - 0.5) * size],
        _ => [0.0; 3],
    }
}

fn initial_velocity(effect: &JpaEffect, local: [f32; 3], seed: u32, birth_frame: f32) -> [f32; 3] {
    let emitter = effect.emitter;
    let spread = emitter.direction_spread.clamp(0.0, 1.0);
    let random_direction = random_unit3(seed ^ 0x243f_6a88);
    let direction = normalize3(std::array::from_fn(|axis| {
        emitter.direction[axis] * (1.0 - spread) + random_direction[axis] * spread
    }));
    let volume = if emitter.volume_type == JPA_VOLUME_POINT {
        random_unit3(seed ^ 0x1319_8a2e)
    } else {
        normalize3(std::array::from_fn(|axis| {
            local[axis] * emitter.scale[axis]
        }))
    };
    let radial = if emitter.volume_type == JPA_VOLUME_POINT {
        normalize3([
            random01(seed ^ 0xbb67_ae85) - 0.5,
            0.0,
            random01(seed ^ 0x9b05_688c) - 0.5,
        ])
    } else {
        normalize3([local[0], 0.0, local[2]])
    };
    let random = [
        random01(seed ^ 0x3c6e_f372) - 0.5,
        random01(seed ^ 0xa54f_f53a) - 0.5,
        random01(seed ^ 0x510e_527f) - 0.5,
    ];
    let direction_speed =
        keyframe_value(effect, 8, birth_frame).unwrap_or(emitter.initial_velocity[3]);
    let velocity = std::array::from_fn(|axis| {
        volume[axis] * emitter.initial_velocity[0]
            + radial[axis] * emitter.initial_velocity[1]
            + random[axis] * emitter.initial_velocity[2]
            + direction[axis] * direction_speed
    });
    let velocity_random =
        1.0 + emitter.initial_velocity_random_scale * (random01(seed ^ 0x1f83_d9ab) * 2.0 - 1.0);
    velocity.map(|component| component * velocity_random)
}

fn simulate_particle_motion(
    effect: &JpaEffect,
    mut position: [f32; 3],
    mut base_velocity: [f32; 3],
    seed: u32,
    age: f32,
    lifetime: f32,
) -> ([f32; 3], [f32; 3]) {
    let emitter = effect.emitter;
    let air = (emitter.base_air_resistance
        + emitter.air_resistance_variance * (random01(seed ^ 0x428a_2f98) - 0.5))
        .min(1.0);
    let weight =
        emitter.base_weight * (1.0 - emitter.weight_random_scale * random01(seed ^ 0x7137_4491));
    let mut field_acceleration = [0.0; 3];
    let mut velocity = base_velocity.map(|value| value * weight);
    for frame in 0..age.floor().max(0.0) as u32 {
        let progress = frame as f32 / lifetime.max(1.0);
        let mut field_velocity = field_acceleration;
        for (field_index, field) in effect.fields.iter().enumerate() {
            let fade = field_fade_scale(*field, progress);
            if fade <= 0.0 {
                continue;
            }
            let force = match field.kind {
                0 => normalize3(field.direction).map(|value| value * field.magnitude * fade),
                3 => newton_field_force(*field, position).map(|value| value * fade),
                5 if frame == 0 || (field.cycle != 0 && frame % u32::from(field.cycle) == 0) => {
                    let field_seed = seed
                        .wrapping_add(frame.wrapping_mul(0x9e37_79b9))
                        .wrapping_add((field_index as u32).wrapping_mul(0x85eb_ca6b));
                    [
                        random01(field_seed ^ 0x243f_6a88) - 0.5,
                        random01(field_seed ^ 0x85a3_08d3) - 0.5,
                        random01(field_seed ^ 0x1319_8a2e) - 0.5,
                    ]
                    .map(|value| value * field.magnitude * fade)
                }
                _ => [0.0; 3],
            };
            match field.add_type {
                1 => {
                    for axis in 0..3 {
                        base_velocity[axis] += force[axis];
                    }
                }
                2 => {
                    for axis in 0..3 {
                        field_velocity[axis] += force[axis];
                    }
                }
                _ => {
                    for axis in 0..3 {
                        field_acceleration[axis] += force[axis];
                    }
                }
            }
        }
        if air < 1.0 {
            base_velocity = base_velocity.map(|value| value * air);
        }
        velocity =
            std::array::from_fn(|axis| (base_velocity[axis] + field_velocity[axis]) * weight);
        for axis in 0..3 {
            position[axis] += velocity[axis];
        }
    }
    (position, velocity)
}

fn field_fade_scale(field: sms_formats::JpaField, progress: f32) -> f32 {
    let [fade_in_end, fade_out_start, fade_in_start, fade_out_end] = field.fade;
    if field.status & 0x08 != 0 && progress < fade_in_start {
        return 0.0;
    }
    if field.status & 0x10 != 0 && progress >= fade_out_end {
        return 0.0;
    }
    if field.status & 0x40 != 0 && progress >= fade_out_start {
        let span = fade_out_end - fade_out_start;
        if span > 0.0 {
            return ((fade_out_end - progress) / span).clamp(0.0, 1.0);
        }
    } else if field.status & 0x20 != 0 && progress < fade_in_end {
        let span = fade_in_end - fade_in_start;
        if span > 0.0 {
            return ((progress - fade_in_start) / span).clamp(0.0, 1.0);
        }
    }
    1.0
}

fn newton_field_force(field: sms_formats::JpaField, position: [f32; 3]) -> [f32; 3] {
    let direction = std::array::from_fn(|axis| field.position[axis] - position[axis]);
    let length_sq = direction.iter().map(|value| value * value).sum::<f32>();
    if length_sq <= 0.000001 {
        return [0.0; 3];
    }
    let scale = if field.status & 0x100 != 0 {
        field.magnitude
    } else {
        let radius_sq = field.parameter[0] * field.parameter[0];
        let attenuation = if length_sq > radius_sq {
            (radius_sq * 10.0) / length_sq
        } else {
            10.0
        };
        field.magnitude * attenuation
    };
    normalize3(direction).map(|value| value * scale)
}

fn particle_draw_direction(
    direction_type: u8,
    velocity: [f32; 3],
    local_position: [f32; 3],
    emitter_direction: [f32; 3],
) -> [f32; 3] {
    match direction_type {
        1 => local_position,
        2 => local_position.map(|value| -value),
        3 => emitter_direction,
        // Previous-particle direction needs the emitter list; velocity is the
        // stable fallback used by JParticle when no previous point exists.
        4 => velocity,
        _ => velocity,
    }
}

fn stripe_particle_segments(
    particles: &[SampledParticle],
    crossed: bool,
    reverse_order: bool,
    capacity: usize,
) -> Vec<SampledParticle> {
    let ordered = if reverse_order {
        particles.iter().collect::<Vec<_>>()
    } else {
        particles.iter().rev().collect::<Vec<_>>()
    };
    let mut result = Vec::new();
    let stripe_color = ordered.first().map(|particle| particle.color);
    let stripe_environment_color = ordered.first().map(|particle| particle.environment_color);
    let denominator = ordered.len().saturating_sub(1).max(1) as f32;
    for (index, pair) in ordered.windows(2).enumerate() {
        let start_v = index as f32 / denominator;
        let end_v = (index + 1) as f32 / denominator;
        for cross_index in 0..=usize::from(crossed) {
            let mut segment = *pair[1];
            if let Some(color) = stripe_color {
                segment.color = color;
            }
            if let Some(environment_color) = stripe_environment_color {
                segment.environment_color = environment_color;
            }
            let (start_left, start_right) = stripe_edges(pair[0], cross_index == 1);
            let (end_left, end_right) = stripe_edges(pair[1], cross_index == 1);
            segment.quad_vertices = Some([start_left, start_right, end_left, end_right]);
            segment.quad_uvs = Some([[0.0, start_v], [1.0, start_v], [0.0, end_v], [1.0, end_v]]);
            // 255 is the internal pre-expanded particle geometry marker. The
            // GPU must preserve these authored strip vertices verbatim.
            segment.particle_type = 255;
            result.push(segment);
            if result.len() >= capacity {
                return result;
            }
        }
    }
    result
}

fn stripe_edges(particle: &SampledParticle, second_cross: bool) -> ([f32; 3], [f32; 3]) {
    let direction = normalize3(if second_cross {
        particle.velocity
    } else {
        particle.direction
    });
    let direction = if direction == [0.0; 3] {
        [0.0, 1.0, 0.0]
    } else {
        direction
    };
    let mut side = cross3([0.0, 1.0, 0.0], direction);
    if side == [0.0; 3] {
        side = [0.0, 1.0, 0.0];
    } else {
        side = normalize3(side);
    }
    let across = normalize3(cross3(direction, side));
    let angle = particle.rotation * std::f32::consts::PI;
    let (sin, cos) = angle.sin_cos();
    let (basis_x, basis_y, width) = if second_cross {
        (across.map(|value| -value), side, particle.half_size[1])
    } else {
        (across, side, particle.half_size[0])
    };
    let axis = normalize3(std::array::from_fn(|component| {
        basis_x[component] * sin + basis_y[component] * cos
    }));
    (
        std::array::from_fn(|component| particle.position[component] - axis[component] * width),
        std::array::from_fn(|component| particle.position[component] + axis[component] * width),
    )
}

fn particle_scale(effect: &JpaEffect, life: f32, seed: u32, birth_frame: f32) -> [f32; 2] {
    let global_scale = keyframe_value(effect, 10, birth_frame).unwrap_or(1.0);
    let Some(extra) = effect.extra_shape.filter(|extra| extra.scale_enabled) else {
        return [global_scale; 2];
    };
    let random_scale = 1.0 + (random01(seed ^ 0xbb67_ae85) * 2.0 - 1.0) * extra.random_scale;
    let axis = |index: usize| {
        let value = if life < extra.scale_in_timing && extra.scale_in_timing > 0.0 {
            lerp(
                extra.scale_in_value[index],
                1.0,
                life / extra.scale_in_timing,
            )
        } else if life > extra.scale_out_timing && extra.scale_out_timing < 1.0 {
            lerp(
                1.0,
                extra.scale_out_value[index],
                (life - extra.scale_out_timing) / (1.0 - extra.scale_out_timing),
            )
        } else {
            1.0
        };
        value.max(0.0) * random_scale * global_scale
    };
    [axis(0), axis(1)]
}

fn particle_alpha(effect: &JpaEffect, life: f32) -> f32 {
    let Some(extra) = effect.extra_shape.filter(|extra| extra.alpha_enabled) else {
        return 1.0;
    };
    if life < extra.alpha_in_timing && extra.alpha_in_timing > 0.0 {
        lerp(
            extra.alpha_in_value,
            extra.alpha_base_value,
            life / extra.alpha_in_timing,
        )
    } else if life > extra.alpha_out_timing && extra.alpha_out_timing < 1.0 {
        lerp(
            extra.alpha_base_value,
            extra.alpha_out_value,
            (life - extra.alpha_out_timing) / (1.0 - extra.alpha_out_timing),
        )
    } else {
        extra.alpha_base_value
    }
    .clamp(0.0, 1.0)
}

fn particle_colors(
    effect: &JpaEffect,
    age: f32,
    life: f32,
    seed: u32,
    emitter_frame: f32,
) -> ([u8; 4], [u8; 4]) {
    let Some(animation) = effect.color_animation.as_ref() else {
        return (effect.base_shape.color, effect.base_shape.environment_color);
    };
    let max_frame = animation.max_frame.max(0) as u32;
    let source_frame = if animation.global { emitter_frame } else { age };
    let frame = match animation.mode {
        0 => source_frame.floor().clamp(0.0, max_frame as f32),
        1 if max_frame > 0 => source_frame.floor() % (max_frame + 1) as f32,
        2 if max_frame > 0 => {
            let period = max_frame * 2;
            let position = source_frame.floor().max(0.0) as u32 % period;
            position.min(period - position) as f32
        }
        3 => ((life * (max_frame + 1) as f32).floor() as u32 % (max_frame + 1)) as f32,
        4 if animation.random_offset => {
            (random01(seed ^ 0x9b05_688c) * (max_frame + 1) as f32).floor()
        }
        _ => 0.0,
    };
    (
        sample_color_keys(&animation.primary, frame).unwrap_or(effect.base_shape.color),
        sample_color_keys(&animation.environment, frame)
            .unwrap_or(effect.base_shape.environment_color),
    )
}

fn sample_color_keys(keys: &[sms_formats::JpaColorKey], frame: f32) -> Option<[u8; 4]> {
    let first = *keys.first()?;
    let last = *keys.last()?;
    if frame <= f32::from(first.frame) {
        return Some(first.color);
    }
    if frame >= f32::from(last.frame) {
        return Some(last.color);
    }
    let pair = keys
        .windows(2)
        .find(|pair| frame >= f32::from(pair[0].frame) && frame < f32::from(pair[1].frame))?;
    let span = f32::from(pair[1].frame - pair[0].frame).max(1.0);
    let amount = (frame - f32::from(pair[0].frame)) / span;
    Some(std::array::from_fn(|channel| {
        lerp(
            f32::from(pair[0].color[channel]),
            f32::from(pair[1].color[channel]),
            amount,
        )
        .round()
        .clamp(0.0, 255.0) as u8
    }))
}

fn particle_rotation(effect: &JpaEffect, age: f32, seed: u32) -> f32 {
    let Some(extra) = effect.extra_shape.filter(|extra| extra.rotate_enabled) else {
        return 0.0;
    };
    // JParticle stores rotation as an s16 angle. The authored base angle and
    // speed use half-turn units, while random angle is multiplied by 65536 and
    // therefore contributes twice as much in those units.
    let start = extra.rotate_angle
        + (random01(seed ^ 0x1f83_d9ab) * 2.0 - 1.0) * extra.rotate_random_angle * 2.0;
    let random_speed = 1.0 + random01(seed ^ 0x5be0_cd19) * extra.rotate_random_speed;
    let direction = if random01(seed ^ 0x243f_6a88) < extra.rotate_direction {
        1.0
    } else {
        -1.0
    };
    let speed = extra.rotate_speed * random_speed * direction;
    start + speed * age
}

fn push_particle_slot(
    triangles: &mut Vec<PreviewTriangle>,
    packet_index: usize,
    texture_index: usize,
    shape: sms_formats::JpaBaseShape,
    extra_shape: Option<sms_formats::JpaExtraShape>,
    screen_distortion: bool,
) {
    let triangle = |uv: [[f32; 2]; 3], corner_uv: [[f32; 2]; 3]| PreviewTriangle {
        vertices: [[0.0; 3]; 3],
        normals: Some([[0.0; 3]; 3]),
        color_channels: [
            Some([[255, 255, 255, 0]; 3]),
            Some([shape.environment_color; 3]),
        ],
        tex_coord_sets: std::array::from_fn(|slot| (slot == 1).then_some(corner_uv)),
        material_index: None,
        packet_index,
        model_index: 0,
        render_layer: if screen_distortion {
            PreviewRenderLayer::ParticleDistortion
        } else {
            PreviewRenderLayer::Particle
        },
        color: None,
        vertex_colors: None,
        combine_mode: J3dPreviewCombineMode::TextureModulateVertex,
        tex_coords: Some(uv),
        texture_index: Some(texture_index),
        mask_tex_coords: None,
        mask_texture_index: None,
        cull_mode: Some(0),
        alpha_compare: Some(J3dAlphaCompare {
            comp0: shape.alpha_compare[0],
            ref0: shape.alpha_compare[1],
            op: shape.alpha_compare[2],
            comp1: shape.alpha_compare[3],
            ref1: shape.alpha_compare[4],
        }),
        blend_mode: Some(J3dBlendMode {
            mode: shape.blend_mode,
            src_factor: shape.source_blend_factor,
            dst_factor: shape.destination_blend_factor,
            logic_op: 3,
        }),
        z_mode: Some(J3dZMode {
            compare_enable: u8::from(shape.z_compare_enable),
            func: shape.z_compare_function,
            update_enable: 0,
        }),
        billboard: None,
        particle_type: Some(shape.particle_type),
        particle_pivot: Some(
            extra_shape.map_or([1.0, 1.0], |extra| extra.scale_pivot.map(f32::from)),
        ),
        particle_direction: Some([0.0; 3]),
        particle_color_mode: Some(shape.color_mode),
        particle_environment_color: Some(shape.environment_color),
    };
    let [tiling_x, tiling_y] = shape.tiling;
    triangles.push(triangle(
        [[0.0, 0.0], [tiling_x, 0.0], [tiling_x, tiling_y]],
        [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]],
    ));
    triangles.push(triangle(
        [[0.0, 0.0], [tiling_x, tiling_y], [0.0, tiling_y]],
        [[0.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
    ));
}

fn update_particle_slot(
    slot: &mut [PreviewTriangle],
    sample: SampledParticle,
    translation: [f32; 3],
) {
    let sample = translated_particle_sample(sample, translation);
    if let (Some(vertices), Some(uvs)) = (sample.quad_vertices, sample.quad_uvs) {
        if let [first, second] = slot {
            first.vertices = [vertices[0], vertices[1], vertices[3]];
            second.vertices = [vertices[0], vertices[3], vertices[2]];
            first.tex_coords = Some([uvs[0], uvs[1], uvs[3]]);
            second.tex_coords = Some([uvs[0], uvs[3], uvs[2]]);
        }
    }
    for triangle in slot {
        if sample.quad_vertices.is_none() {
            triangle.vertices = [sample.position; 3];
        }
        triangle.normals = Some([[sample.half_size[0], sample.half_size[1], sample.rotation]; 3]);
        triangle.color_channels[0] = Some([sample.color; 3]);
        triangle.color_channels[1] = Some([sample.environment_color; 3]);
        triangle.particle_type = Some(sample.particle_type);
        triangle.particle_direction = Some(sample.direction);
    }
}

fn translated_particle_sample(
    mut sample: SampledParticle,
    translation: [f32; 3],
) -> SampledParticle {
    sample.position = std::array::from_fn(|axis| sample.position[axis] + translation[axis]);
    if let Some(vertices) = &mut sample.quad_vertices {
        for vertex in vertices {
            *vertex = std::array::from_fn(|axis| vertex[axis] + translation[axis]);
        }
    }
    sample
}

fn hide_particle_slot(slot: &mut [PreviewTriangle]) {
    for triangle in slot {
        triangle.vertices = [[0.0; 3]; 3];
        triangle.normals = Some([[0.0; 3]; 3]);
        triangle.color_channels[0] = Some([[255, 255, 255, 0]; 3]);
        triangle.particle_direction = Some([0.0; 3]);
    }
}

fn random01(mut value: u32) -> f32 {
    value ^= value >> 16;
    value = value.wrapping_mul(0x7feb_352d);
    value ^= value >> 15;
    value = value.wrapping_mul(0x846c_a68b);
    value ^= value >> 16;
    value as f32 / u32::MAX as f32
}

fn normalize3(value: [f32; 3]) -> [f32; 3] {
    let length = (value[0] * value[0] + value[1] * value[1] + value[2] * value[2]).sqrt();
    if length <= f32::EPSILON {
        [0.0; 3]
    } else {
        value.map(|component| component / length)
    }
}

fn cross3(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

fn random_unit3(seed: u32) -> [f32; 3] {
    normalize3([
        random01(seed ^ 0x68bc_21eb) - 0.5,
        random01(seed ^ 0x02e5_be93) - 0.5,
        random01(seed ^ 0x967a_889b) - 0.5,
    ])
}

fn lerp(a: f32, b: f32, amount: f32) -> f32 {
    a + (b - a) * amount.clamp(0.0, 1.0)
}

fn keyframe_value(effect: &JpaEffect, parameter_index: u8, frame: f32) -> Option<f32> {
    effect
        .keyframes
        .iter()
        .find(|curve| curve.parameter_index == parameter_index)
        .and_then(|curve| curve.sample(frame))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_random_stays_normalized() {
        assert_eq!(random01(42), random01(42));
        assert!((0.0..=1.0).contains(&random01(42)));
    }

    #[test]
    fn sparse_emitters_allocate_only_the_slots_they_can_reach() {
        assert_eq!(particle_birth_capacity(0.15, 30.0, 1), 31);
        assert_eq!(particle_birth_capacity(2.0, 10.0, 2), 12);
        assert_eq!(particle_birth_capacity(0.0, 30.0, 1), 1);
    }

    #[test]
    fn shared_actor_samples_translate_points_and_stripe_geometry_equally() {
        let sample = SampledParticle {
            position: [1.0, 2.0, 3.0],
            half_size: [1.0; 2],
            rotation: 0.0,
            color: [255; 4],
            environment_color: [255; 4],
            particle_type: 6,
            direction: [0.0, 1.0, 0.0],
            velocity: [0.0; 3],
            trail_id: 0,
            quad_vertices: Some([[1.0, 2.0, 3.0]; 4]),
            quad_uvs: Some([[0.0; 2]; 4]),
        };
        let translated = translated_particle_sample(sample, [10.0, 20.0, 30.0]);
        assert_eq!(translated.position, [11.0, 22.0, 33.0]);
        assert!(translated
            .quad_vertices
            .unwrap()
            .iter()
            .all(|vertex| *vertex == [11.0, 22.0, 33.0]));
    }

    #[test]
    fn directional_shapes_follow_the_requested_jparticle_vector() {
        let velocity = [1.0, 2.0, 3.0];
        let position = [4.0, 5.0, 6.0];
        let emitter = [7.0, 8.0, 9.0];
        assert_eq!(
            particle_draw_direction(0, velocity, position, emitter),
            velocity
        );
        assert_eq!(
            particle_draw_direction(1, velocity, position, emitter),
            position
        );
        assert_eq!(
            particle_draw_direction(2, velocity, position, emitter),
            [-4.0, -5.0, -6.0]
        );
        assert_eq!(
            particle_draw_direction(3, velocity, position, emitter),
            emitter
        );
    }

    #[test]
    fn particle_color_tables_interpolate_retail_keyframes() {
        let keys = [
            sms_formats::JpaColorKey {
                frame: 0,
                color: [0, 20, 40, 60],
            },
            sms_formats::JpaColorKey {
                frame: 10,
                color: [100, 120, 140, 160],
            },
        ];
        assert_eq!(sample_color_keys(&keys, 5.0), Some([50, 70, 90, 110]));
    }

    #[test]
    fn stripe_cross_builds_two_continuous_strip_quads_per_particle_segment() {
        let particle = |position, color| SampledParticle {
            position,
            half_size: [2.0, 3.0],
            rotation: 0.0,
            color,
            environment_color: [255; 4],
            particle_type: 6,
            direction: [1.0, 0.0, 0.0],
            velocity: [0.0, 0.0, 1.0],
            trail_id: 7,
            quad_vertices: None,
            quad_uvs: None,
        };
        let segments = stripe_particle_segments(
            &[
                particle([0.0, 0.0, 0.0], [255, 255, 255, 32]),
                particle([0.0, 10.0, 0.0], [255, 255, 255, 255]),
            ],
            true,
            false,
            8,
        );
        assert_eq!(segments.len(), 2);
        assert!(segments.iter().all(|segment| segment.particle_type == 255));
        assert!(segments
            .iter()
            .all(|segment| segment.color == [255, 255, 255, 255]));
        assert_eq!(segments[0].quad_uvs.unwrap()[0], [0.0, 0.0]);
        assert_eq!(segments[0].quad_uvs.unwrap()[3], [1.0, 1.0]);
        assert_ne!(segments[0].quad_vertices, segments[1].quad_vertices);
    }

    #[test]
    fn jparticle_field_fade_and_newton_force_follow_fld1_parameters() {
        let field = sms_formats::JpaField {
            kind: 3,
            add_type: 1,
            cycle: 0,
            status: 0x20,
            magnitude: 0.1,
            secondary_magnitude: 0.0,
            max_distance: 0.0,
            position: [0.0, 10.0, 0.0],
            direction: [0.0; 3],
            parameter: [200.0, 40_000.0, 0.0],
            fade: [0.5, 1.0, 0.0, 1.0],
        };
        assert!((field_fade_scale(field, 0.25) - 0.5).abs() < 0.0001);
        assert_eq!(newton_field_force(field, [0.0; 3]), [0.0, 1.0, 0.0]);
    }

    #[test]
    fn effect_pair_groups_are_derived_from_asset_names() {
        assert_eq!(
            particle_pair_group("archive!/map/map/custom_raise_a.jpa"),
            Some("custom_raise".to_string())
        );
        assert_eq!(
            particle_pair_group("archive!/map/map/custom_raise_b.jpa"),
            Some("custom_raise".to_string())
        );
        assert_eq!(particle_pair_group("archive!/map/map/unpaired.jpa"), None);
    }

    #[test]
    fn playback_samples_once_per_retail_frame() {
        let duration = 720.0;
        assert_eq!(
            level_transform_sample_progress(1.0 / duration, duration, true),
            1.0 / duration
        );
        assert_eq!(
            level_transform_sample_progress(1.9 / duration, duration, true),
            1.0 / duration
        );
        assert_eq!(
            level_transform_sample_progress(0.123, duration, false),
            0.123
        );
    }

    #[test]
    fn asset_timeline_uses_the_emitter_window_without_the_particle_death_tail() {
        assert_eq!(emission_end_frame(30, 150), 180.0);
        assert_eq!(emission_end_frame(0, 180), 180.0);
    }

    #[test]
    fn continuous_emitters_keep_accepting_births_after_frame_zero() {
        assert_eq!(final_emission_birth_frame(240.75, 0), 240);
        assert_eq!(final_emission_birth_frame(240.75, -1), 0);
        assert_eq!(final_emission_birth_frame(240.75, 60), 60);
    }

    #[test]
    fn placement_names_resolve_cpp_particle_classes_without_overlays() {
        let mut registry = ObjectRegistry::default();
        registry
            .actor_particle_bindings
            .push(sms_schema::ActorParticleBinding {
                class_name: "TExampleActor".to_string(),
                effect_id: 7,
                target: ParticleBindingTarget::ActorOrigin,
                source_file: "src/Example.cpp".to_string(),
            });
        let object = SceneObject::new("example", "ExampleActor");

        assert_eq!(
            cpp_class_names_for_object(&registry, &object),
            vec!["TExampleActor"]
        );
    }

    #[test]
    fn asset_timeline_uses_sunshines_animation_clock() {
        assert_eq!(level_transform_duration_seconds(360.0), 6.0);
    }

    #[test]
    fn particle_tail_ends_after_the_last_possible_particle_lifetime() {
        assert_eq!(particle_end_frame(30, 150, 300.0), 480.0);
    }
}
