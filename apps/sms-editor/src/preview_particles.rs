use super::*;

const PARTICLE_CAPACITY: usize = 512;
const DEFAULT_TRANSFORM_FRAMES: f32 = 600.0;

#[derive(Debug, Clone, Copy, PartialEq)]
struct SampledParticle {
    position: [f32; 3],
    half_size: [f32; 2],
    rotation: f32,
    color: [u8; 4],
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
        let Ok(bytes) = read_stage_asset_bytes(&asset.path) else {
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

#[allow(clippy::too_many_arguments)]
fn append_particle_shape_preview(
    effect: JpaEffect,
    kind: JpaParticleKind,
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
    let triangle_start = triangles.len();
    let packet_index = *next_packet_index;
    *next_packet_index += 1;
    for _ in 0..PARTICLE_CAPACITY {
        push_particle_slot(
            triangles,
            packet_index,
            texture_index,
            effect.base_shape,
            screen_distortion,
        );
    }
    previews.push(LevelTransformParticlePreview {
        origin_offset,
        effect,
        kind,
        triangle_range: triangle_start..triangles.len(),
        particle_capacity: PARTICLE_CAPACITY,
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
        let samples = match preview.kind {
            JpaParticleKind::Parent => sample_particles(
                &preview.effect,
                preview.origin_offset,
                retail_frame,
                preview.particle_capacity,
            ),
            JpaParticleKind::Child => sample_child_particles(
                &preview.effect,
                preview.origin_offset,
                retail_frame,
                preview.particle_capacity,
            ),
        };
        let slots = triangles[preview.triangle_range.clone()].chunks_exact_mut(2);
        for (index, slot) in slots.enumerate() {
            if let Some(sample) = samples.get(index).copied() {
                update_particle_slot(slot, sample);
            } else {
                hide_particle_slot(slot);
            }
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

    let final_birth_frame = emitter_frame
        .floor()
        .min(f32::from(emitter.max_frame.max(0))) as u32;
    let interval = u32::from(emitter.emit_interval) + 1;
    let mut spawn_accumulator = 0.0f32;
    let mut birth_serial = 0u32;
    let mut result = Vec::new();

    for birth_frame in (0..=final_birth_frame).step_by(interval as usize) {
        let spawn_rate = keyframe_value(effect, 0, birth_frame as f32)
            .unwrap_or(emitter.spawn_rate)
            .max(0.0);
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
            let acceleration = field_acceleration(effect);
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
                    let mut position = [0.0; 3];
                    let random_offset = random_unit3(seed ^ 0x27d4_eb2d)
                        .map(|value| value * child.position_random * random01(seed ^ 0x1656_67b1));
                    let random_velocity = random_unit3(seed ^ 0xd3a2_646c);
                    let velocity_scale = child.velocity
                        * (1.0
                            + child.velocity_random * (random01(seed ^ 0xfd70_46c5) * 2.0 - 1.0));
                    for axis in 0..3 {
                        let parent_position = origin_offset[axis]
                            + emitter.translation[axis]
                            + local[axis] * emitter.scale[axis]
                            + parent_velocity[axis] * parent_age
                            + 0.5
                                * acceleration[axis]
                                * emitter.base_weight
                                * parent_age
                                * parent_age;
                        let parent_current_velocity = parent_velocity[axis]
                            + acceleration[axis] * emitter.base_weight * parent_age;
                        let child_velocity = parent_current_velocity * child.inherit_velocity
                            + random_velocity[axis] * velocity_scale;
                        position[axis] =
                            parent_position + random_offset[axis] + child_velocity * child_age;
                    }
                    let parent_life = (parent_age / parent_lifetime.max(1.0)).clamp(0.0, 1.0);
                    let inherited_scale = if child.inherit_flags & 1 != 0 {
                        let parent =
                            particle_scale(effect, parent_life, parent_seed, birth_frame as f32);
                        parent.map(|value| lerp(1.0, value, child.inherit_scale))
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
                    color[3] = (f32::from(color[3]) * (1.0 - life))
                        .round()
                        .clamp(0.0, 255.0) as u8;
                    result.push(SampledParticle {
                        position,
                        half_size: [
                            child.size[0] * 25.0 * inherited_scale[0],
                            child.size[1] * 25.0 * inherited_scale[1],
                        ],
                        rotation: if child.rotate_enabled {
                            child.rotate_speed * child_age
                        } else {
                            0.0
                        },
                        color,
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
    let final_birth_frame = emitter_frame
        .floor()
        .min(f32::from(emitter.max_frame.max(0))) as u32;
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
            .max(0.0);
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
            let mut position = [
                origin_offset[0] + emitter.translation[0] + local[0] * emitter.scale[0],
                origin_offset[1] + emitter.translation[1] + local[1] * emitter.scale[1],
                origin_offset[2] + emitter.translation[2] + local[2] * emitter.scale[2],
            ];
            let velocity = initial_velocity(effect, local, seed, birth_frame as f32);
            let acceleration = field_acceleration(effect);
            for axis in 0..3 {
                position[axis] += velocity[axis] * age
                    + 0.5 * acceleration[axis] * emitter.base_weight * age * age;
            }
            let scale = particle_scale(effect, life, seed, birth_frame as f32);
            let mut color = effect.base_shape.color;
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
            });
            if result.len() == capacity {
                return result;
            }
        }
    }
    result
}

fn volume_position(effect: &JpaEffect, seed: u32) -> [f32; 3] {
    let size = f32::from(effect.emitter.volume_size);
    match effect.emitter.volume_type {
        4 => [0.0; 3],
        _ => [
            (random01(seed ^ 0x68bc_21eb) - 0.5) * size,
            (random01(seed ^ 0x02e5_be93) - 0.5) * size,
            (random01(seed ^ 0x967a_889b) - 0.5) * size,
        ],
    }
}

fn initial_velocity(effect: &JpaEffect, local: [f32; 3], seed: u32, birth_frame: f32) -> [f32; 3] {
    let emitter = effect.emitter;
    let direction = normalize3(emitter.direction);
    let radial = normalize3([local[0], 0.0, local[2]]);
    let random = normalize3([
        random01(seed ^ 0x3c6e_f372) - 0.5,
        random01(seed ^ 0xa54f_f53a) - 0.5,
        random01(seed ^ 0x510e_527f) - 0.5,
    ]);
    let direction_speed =
        keyframe_value(effect, 8, birth_frame).unwrap_or(emitter.initial_velocity[3]);
    [
        radial[0] * (emitter.initial_velocity[0] + emitter.initial_velocity[1])
            + random[0] * emitter.initial_velocity[2]
            + direction[0] * direction_speed,
        radial[1] * (emitter.initial_velocity[0] + emitter.initial_velocity[1])
            + random[1] * emitter.initial_velocity[2]
            + direction[1] * direction_speed,
        radial[2] * (emitter.initial_velocity[0] + emitter.initial_velocity[1])
            + random[2] * emitter.initial_velocity[2]
            + direction[2] * direction_speed,
    ]
}

fn field_acceleration(effect: &JpaEffect) -> [f32; 3] {
    let mut result = [0.0; 3];
    for field in effect.fields.iter().filter(|field| field.kind == 0) {
        let direction = normalize3(field.direction);
        for axis in 0..3 {
            result[axis] += direction[axis] * field.magnitude;
        }
    }
    result
}

fn particle_scale(effect: &JpaEffect, life: f32, seed: u32, birth_frame: f32) -> [f32; 2] {
    let global_scale = keyframe_value(effect, 10, birth_frame).unwrap_or(1.0);
    let Some(extra) = effect.extra_shape.filter(|extra| extra.scale_enabled) else {
        return [global_scale; 2];
    };
    let random_scale = 1.0 + (random01(seed ^ 0xbb67_ae85) - 0.5) * 0.3;
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

fn particle_rotation(effect: &JpaEffect, age: f32, seed: u32) -> f32 {
    let Some(extra) = effect.extra_shape.filter(|extra| extra.rotate_enabled) else {
        return 0.0;
    };
    let start =
        extra.rotate_angle + (random01(seed ^ 0x1f83_d9ab) - 0.5) * 2.0 * extra.rotate_random_angle;
    let speed = extra.rotate_speed
        * (1.0 + (random01(seed ^ 0x5be0_cd19) - 0.5) * 2.0 * extra.rotate_random_speed);
    start + speed * age
}

fn push_particle_slot(
    triangles: &mut Vec<PreviewTriangle>,
    packet_index: usize,
    texture_index: usize,
    shape: sms_formats::JpaBaseShape,
    screen_distortion: bool,
) {
    let triangle = |uv: [[f32; 2]; 3]| PreviewTriangle {
        vertices: [[0.0; 3]; 3],
        normals: Some([[0.0; 3]; 3]),
        color_channels: [Some([[255, 255, 255, 0]; 3]), None],
        tex_coord_sets: [None; 8],
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
    };
    triangles.push(triangle([[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]]));
    triangles.push(triangle([[0.0, 0.0], [1.0, 1.0], [0.0, 1.0]]));
}

fn update_particle_slot(slot: &mut [PreviewTriangle], sample: SampledParticle) {
    for triangle in slot {
        triangle.vertices = [sample.position; 3];
        triangle.normals = Some([[sample.half_size[0], sample.half_size[1], sample.rotation]; 3]);
        triangle.color_channels[0] = Some([sample.color; 3]);
    }
}

fn hide_particle_slot(slot: &mut [PreviewTriangle]) {
    for triangle in slot {
        triangle.vertices = [[0.0; 3]; 3];
        triangle.normals = Some([[0.0; 3]; 3]);
        triangle.color_channels[0] = Some([[255, 255, 255, 0]; 3]);
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
    fn asset_timeline_uses_sunshines_animation_clock() {
        assert_eq!(level_transform_duration_seconds(360.0), 6.0);
    }

    #[test]
    fn particle_tail_ends_after_the_last_possible_particle_lifetime() {
        assert_eq!(particle_end_frame(30, 150, 300.0), 480.0);
    }
}
