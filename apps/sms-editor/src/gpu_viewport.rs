use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bytemuck::{Pod, Zeroable};
use eframe::egui;
use eframe::egui_wgpu::{Callback, CallbackResources, CallbackTrait, ScreenDescriptor};
use eframe::wgpu::{self, util::DeviceExt};
use sms_formats::{
    J3dAlphaCompare, J3dBillboardMode, J3dBlendMode, J3dMaterial, J3dTevStage, J3dTexMatrix,
    J3dTextureSrtAnimation, J3dZMode,
};

use super::{
    preview_solid_triangle_colors, preview_triangle_normal, ModelPreview, PreviewRenderLayer,
    PreviewTexture, PreviewTriangle,
};

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth24Plus;
const GX_COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const WAVE_MASK_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;
const TEXTURE_SLOT_COUNT: usize = 8;
const TEV_STAGE_COUNT: usize = 16;
const TEX_MATRIX_ROW_COUNT: usize = TEXTURE_SLOT_COUNT * 3;
const SMS_SCREEN_TEXTURE_SIZE: [f32; 2] = [320.0, 224.0];
static NEXT_SCENE_GENERATION: AtomicU64 = AtomicU64::new(1);

const J3D_SHADER: &str = include_str!("shaders/j3d.wgsl");
const COMPOSITE_SHADER: &str = include_str!("shaders/composite.wgsl");

#[derive(Clone)]
pub struct GpuViewportScene {
    shared: Arc<Mutex<GpuViewportShared>>,
}

impl GpuViewportScene {
    pub fn from_preview(preview: &ModelPreview, target_format: wgpu::TextureFormat) -> Self {
        let scene = GpuSceneData::from_preview(preview);
        let dirty_vertex_ranges = vec![None; scene.batches.len()];
        Self {
            shared: Arc::new(Mutex::new(GpuViewportShared {
                scene,
                frame: GpuViewportFrame::default(),
                generation: NEXT_SCENE_GENERATION.fetch_add(1, Ordering::Relaxed),
                geometry_generation: 0,
                dirty_vertex_ranges,
                dirty_materials: BTreeSet::new(),
                target_format,
            })),
        }
    }

    pub fn set_frame(&self, frame: GpuViewportFrame) {
        if let Ok(mut shared) = self.shared.lock() {
            shared.frame = frame;
        }
    }

    pub fn update_geometry(
        &self,
        preview: &ModelPreview,
        triangle_ranges: &[std::ops::Range<usize>],
    ) {
        if let Ok(mut shared) = self.shared.lock() {
            let GpuViewportShared {
                scene,
                geometry_generation,
                dirty_vertex_ranges,
                ..
            } = &mut *shared;
            if scene.update_geometry(preview, triangle_ranges, dirty_vertex_ranges) {
                *geometry_generation = (*geometry_generation).wrapping_add(1);
            }
        }
    }

    pub fn update_materials(&self, preview: &ModelPreview, material_indices: &[usize]) {
        if let Ok(mut shared) = self.shared.lock() {
            for index in material_indices.iter().copied() {
                let Some(material) = preview.materials.get(index) else {
                    continue;
                };
                if index >= shared.scene.materials.len() {
                    continue;
                }
                shared.scene.materials[index] = GpuMaterialData::from_j3d(material, preview);
                shared.dirty_materials.insert(index);
            }
        }
    }

    pub fn paint_callback(&self, rect: egui::Rect) -> egui::PaintCallback {
        Callback::new_paint_callback(
            rect,
            GpuViewportCallback {
                shared: self.shared.clone(),
            },
        )
    }
}

#[cfg(test)]
pub(super) fn render_preview_offscreen(
    preview: &ModelPreview,
    frame: GpuViewportFrame,
    size: [u32; 2],
) -> Result<egui::ColorImage, String> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .map_err(|error| format!("request WGPU adapter: {error}"))?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("Noki rendering test device"),
        ..Default::default()
    }))
    .map_err(|error| format!("request WGPU device: {error}"))?;

    let scene = GpuSceneData::from_preview(preview);
    let mut resources = GpuViewportResources::new(&device, GX_COLOR_FORMAT);
    resources.ensure_viewport_target(&device, size);
    resources.ensure_scene(&device, &queue, &scene, 1, 0);
    resources.write_frame(&queue, frame, &scene);

    let bytes_per_pixel = 4u32;
    let unpadded_row = size[0] * bytes_per_pixel;
    let alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded_row = unpadded_row.div_ceil(alignment) * alignment;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Noki rendering test readback"),
        size: u64::from(padded_row) * u64::from(size[1]),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("Noki rendering test encoder"),
    });
    resources.render_scene(&mut encoder);
    let target = resources
        .viewport_target
        .as_ref()
        .ok_or_else(|| "missing WGPU viewport target".to_string())?;
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target.color,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_row),
                rows_per_image: Some(size[1]),
            },
        },
        target.extent(),
    );
    queue.submit([encoder.finish()]);

    let slice = readback.slice(..);
    let (sender, receiver) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|error| format!("poll WGPU device: {error}"))?;
    receiver
        .recv()
        .map_err(|error| format!("receive WGPU map result: {error}"))?
        .map_err(|error| format!("map WGPU readback: {error}"))?;

    let mapped = slice.get_mapped_range();
    let mut image =
        egui::ColorImage::filled([size[0] as usize, size[1] as usize], egui::Color32::BLACK);
    for y in 0..size[1] as usize {
        let row = &mapped[y * padded_row as usize..y * padded_row as usize + unpadded_row as usize];
        for (x, rgba) in row.chunks_exact(4).enumerate() {
            image.pixels[y * size[0] as usize + x] =
                egui::Color32::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3]);
        }
    }
    drop(mapped);
    readback.unmap();
    Ok(image)
}

#[derive(Clone, Copy, Debug)]
pub struct GpuViewportFrame {
    pub camera_position: [f32; 3],
    pub right: [f32; 3],
    pub up: [f32; 3],
    pub forward: [f32; 3],
    pub focal: f32,
    pub viewport_size: [f32; 2],
    pub viewport_pan: [f32; 2],
    pub near: f32,
    pub animation_seconds: f32,
    pub light_position: [f32; 3],
    pub light_color: [f32; 4],
    pub ambient_color: Option<[f32; 4]>,
}

impl Default for GpuViewportFrame {
    fn default() -> Self {
        Self {
            camera_position: [0.0; 3],
            right: [1.0, 0.0, 0.0],
            up: [0.0, 1.0, 0.0],
            forward: [0.0, 0.0, 1.0],
            focal: 1.0,
            viewport_size: [1.0, 1.0],
            viewport_pan: [0.0; 2],
            near: 8.0,
            animation_seconds: 0.0,
            light_position: [200_000.0, 500_000.0, 200_000.0],
            light_color: [1.0; 4],
            ambient_color: None,
        }
    }
}

struct GpuViewportShared {
    scene: GpuSceneData,
    frame: GpuViewportFrame,
    generation: u64,
    geometry_generation: u64,
    dirty_vertex_ranges: Vec<Option<std::ops::Range<usize>>>,
    dirty_materials: BTreeSet<usize>,
    target_format: wgpu::TextureFormat,
}

struct GpuViewportCallback {
    shared: Arc<Mutex<GpuViewportShared>>,
}

impl CallbackTrait for GpuViewportCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen_descriptor: &ScreenDescriptor,
        egui_encoder: &mut wgpu::CommandEncoder,
        callback_resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Ok(mut shared) = self.shared.lock() else {
            return Vec::new();
        };
        let resources = callback_resources
            .entry::<GpuViewportResources>()
            .or_insert_with(|| GpuViewportResources::new(device, shared.target_format));
        let target_size = [
            (shared.frame.viewport_size[0] * screen_descriptor.pixels_per_point)
                .round()
                .max(1.0) as u32,
            (shared.frame.viewport_size[1] * screen_descriptor.pixels_per_point)
                .round()
                .max(1.0) as u32,
        ];
        let target_changed = resources.ensure_viewport_target(device, target_size);
        let scene_rebuilt = resources.ensure_scene(
            device,
            queue,
            &shared.scene,
            shared.generation,
            shared.geometry_generation,
        );
        if target_changed && !scene_rebuilt {
            resources.rebuild_target_material_bind_groups(device, &shared.scene);
        }
        let geometry_changed = resources.write_geometry(
            queue,
            &shared.scene,
            &shared.dirty_vertex_ranges,
            shared.geometry_generation,
        );
        if geometry_changed || scene_rebuilt {
            shared.dirty_vertex_ranges.fill(None);
        }
        let materials_changed =
            resources.write_materials(device, queue, &shared.scene, &shared.dirty_materials);
        shared.dirty_materials.clear();
        let frame_state =
            GpuOffscreenFrameState::new(shared.frame, shared.scene.mirror_plane_y, target_size);
        let invalidation = GpuOffscreenInvalidation {
            target: target_changed,
            scene: scene_rebuilt,
            geometry: geometry_changed,
            materials: materials_changed,
            time_animation: !resources.animated_materials.is_empty(),
        };
        if offscreen_render_required(resources.offscreen_frame_state, frame_state, invalidation) {
            resources.write_frame(queue, shared.frame, &shared.scene);
            resources.render_scene(egui_encoder);
            resources.offscreen_frame_state = Some(frame_state);
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        callback_resources: &CallbackResources,
    ) {
        if let Some(resources) = callback_resources.get::<GpuViewportResources>() {
            resources.composite(render_pass);
        }
    }
}

#[derive(Clone)]
struct GpuSceneData {
    textures: Vec<GpuTextureData>,
    materials: Vec<GpuMaterialData>,
    batches: Vec<GpuBatchData>,
    triangle_vertices: Vec<Option<GpuTriangleLocation>>,
    mirror_plane_y: Option<f32>,
}

#[derive(Clone, Copy)]
struct GpuTriangleLocation {
    batch_index: usize,
    vertex_offset: usize,
    billboard_attributes_dynamic: bool,
    surface_attributes_dynamic: bool,
}

impl GpuSceneData {
    fn from_preview(preview: &ModelPreview) -> Self {
        let mut textures = vec![GpuTextureData::white()];
        textures.extend(
            preview
                .textures
                .iter()
                .map(GpuTextureData::from_preview_texture),
        );
        let mut materials = preview
            .materials
            .iter()
            .map(|material| GpuMaterialData::from_j3d(material, preview))
            .collect::<Vec<_>>();
        let mut fallback_materials = BTreeMap::<(usize, usize, u8, u8, u32), usize>::new();
        let mirror_actor_models = preview
            .object_model_indices
            .values()
            .copied()
            .collect::<BTreeSet<_>>();
        let mut batch_map = BTreeMap::<(usize, usize, u8, bool, GpuPipelineKey), usize>::new();
        let mut batches = Vec::<GpuBatchData>::new();
        let mut triangle_vertices = vec![None; preview.triangles.len()];

        for (triangle_index, triangle) in preview.triangles.iter().enumerate() {
            let material_index = triangle
                .material_index
                .filter(|index| *index < materials.len())
                .unwrap_or_else(|| {
                    let texture = triangle.texture_index.unwrap_or(usize::MAX);
                    let mask = triangle.mask_texture_index.unwrap_or(usize::MAX);
                    let layer = render_layer_id(triangle.render_layer);
                    let particle_color_mode = triangle.particle_color_mode.unwrap_or(u8::MAX);
                    let particle_environment_color = triangle
                        .particle_environment_color
                        .map(u32::from_be_bytes)
                        .unwrap_or(0);
                    *fallback_materials
                        .entry((
                            texture,
                            mask,
                            layer,
                            particle_color_mode,
                            particle_environment_color,
                        ))
                        .or_insert_with(|| {
                            let index = materials.len();
                            materials.push(GpuMaterialData::fallback(triangle, preview));
                            index
                        })
                });
            let material = &materials[material_index];
            if material.state.cull == GpuCullMode::All {
                continue;
            }
            let pipeline_key = material.state.pipeline_key(triangle.render_layer);
            let mirror_visible = triangle_is_mirror_visible(triangle, &mirror_actor_models);
            let batch_index = *batch_map
                .entry((
                    triangle.packet_index,
                    material_index,
                    render_layer_id(triangle.render_layer),
                    mirror_visible,
                    pipeline_key,
                ))
                .or_insert_with(|| {
                    let index = batches.len();
                    batches.push(GpuBatchData {
                        pipeline_key,
                        material_index,
                        packet_index: triangle.packet_index,
                        render_layer: triangle.render_layer,
                        mirror_visible,
                        vertices: Vec::new(),
                    });
                    index
                });
            let batch = &mut batches[batch_index];
            let base = batch.vertices.len() as u32;
            triangle_vertices[triangle_index] = Some(GpuTriangleLocation {
                batch_index,
                vertex_offset: base as usize,
                billboard_attributes_dynamic: triangle.billboard.is_some()
                    || triangle.particle_type.is_some()
                    || triangle.particle_pivot.is_some()
                    || triangle.particle_direction.is_some(),
                surface_attributes_dynamic: triangle.particle_type.is_some(),
            });
            let face_normal = preview_triangle_normal(triangle);
            let legacy_colors = legacy_vertex_colors(triangle, face_normal);
            for vertex_index in 0..3 {
                let normal = triangle
                    .billboard
                    .and_then(|billboard| billboard.normals)
                    .map(|normals| normals[vertex_index])
                    .or_else(|| triangle.normals.map(|normals| normals[vertex_index]))
                    .unwrap_or(face_normal);
                let color0 = triangle.color_channels[0]
                    .map(|colors| color_u8_to_f32(colors[vertex_index]))
                    .or_else(|| {
                        triangle
                            .vertex_colors
                            .map(|colors| color_u8_to_f32(colors[vertex_index]))
                    })
                    .unwrap_or(legacy_colors[vertex_index]);
                let color1 = triangle.color_channels[1]
                    .map(|colors| color_u8_to_f32(colors[vertex_index]))
                    .unwrap_or([1.0; 4]);
                let tex_coords: [[f32; 2]; TEXTURE_SLOT_COUNT] = std::array::from_fn(|slot| {
                    triangle.tex_coord_sets[slot]
                        .map(|coords| coords[vertex_index])
                        .or_else(|| {
                            (slot == 0)
                                .then_some(triangle.tex_coords)
                                .flatten()
                                .map(|coords| coords[vertex_index])
                        })
                        .unwrap_or([0.0; 2])
                });
                batch.vertices.push(GpuVertex {
                    position: triangle.vertices[vertex_index],
                    normal,
                    color0,
                    color1,
                    uv0: tex_coords[0],
                    uv1: tex_coords[1],
                    uv2: tex_coords[2],
                    uv3: tex_coords[3],
                    uv4: tex_coords[4],
                    uv5: tex_coords[5],
                    uv6: tex_coords[6],
                    uv7: tex_coords[7],
                    coordinate_space: coordinate_space_for_render_layer(triangle.render_layer),
                    billboard_center_mode: triangle
                        .billboard
                        .map(|billboard| {
                            [
                                billboard.center[0],
                                billboard.center[1],
                                billboard.center[2],
                                match billboard.mode {
                                    J3dBillboardMode::Full => 1.0,
                                    J3dBillboardMode::YAxis => 2.0,
                                },
                            ]
                        })
                        .unwrap_or([
                            triangle.particle_pivot.map_or(0.0, |pivot| pivot[0]),
                            triangle.particle_pivot.map_or(0.0, |pivot| pivot[1]),
                            0.0,
                            triangle.particle_type.map_or(0.0, f32::from),
                        ]),
                    billboard_offset: triangle
                        .billboard
                        .map(|billboard| billboard.offsets[vertex_index])
                        .unwrap_or([0.0; 3]),
                    billboard_axis_y: triangle
                        .particle_direction
                        .or_else(|| triangle.billboard.map(|billboard| billboard.axes[1]))
                        .unwrap_or([0.0, 1.0, 0.0]),
                });
            }
        }

        for batch in &batches {
            if render_layer_uses_efb_copy(batch.render_layer) {
                if let Some(material) = materials.get_mut(batch.material_index) {
                    material.uniform.texture_sizes[1] =
                        texture_size_uniform(SMS_SCREEN_TEXTURE_SIZE);
                }
            }
            if batch.render_layer == PreviewRenderLayer::WaveFoam {
                if let Some(material) = materials.get_mut(batch.material_index) {
                    material.runtime_wave = true;
                }
            }
        }

        let mirror_plane_y = mirror_plane_y_from_preview(preview);

        Self {
            textures,
            materials,
            batches,
            triangle_vertices,
            mirror_plane_y,
        }
    }

    fn update_geometry(
        &mut self,
        preview: &ModelPreview,
        triangle_ranges: &[std::ops::Range<usize>],
        dirty_vertex_ranges: &mut [Option<std::ops::Range<usize>>],
    ) -> bool {
        let mut geometry_changed = false;
        let mut mirror_surface_updated = false;
        for triangle_index in triangle_ranges.iter().flat_map(|range| range.clone()) {
            let Some(triangle) = preview.triangles.get(triangle_index) else {
                continue;
            };
            mirror_surface_updated |= triangle.render_layer == PreviewRenderLayer::MirrorSurface;
            let Some(location) = self
                .triangle_vertices
                .get(triangle_index)
                .copied()
                .flatten()
            else {
                continue;
            };
            let Some(dirty_range) = dirty_vertex_ranges.get_mut(location.batch_index) else {
                continue;
            };
            let Some(batch) = self.batches.get_mut(location.batch_index) else {
                continue;
            };
            let Some(vertex_end) = location.vertex_offset.checked_add(3) else {
                continue;
            };
            let Some(vertices) = batch.vertices.get_mut(location.vertex_offset..vertex_end) else {
                continue;
            };
            update_gpu_triangle_geometry(
                vertices,
                triangle,
                location.billboard_attributes_dynamic,
                location.surface_attributes_dynamic,
            );
            extend_dirty_vertex_range(dirty_range, location.vertex_offset..vertex_end);
            geometry_changed = true;
        }
        if mirror_surface_updated {
            self.mirror_plane_y = mirror_plane_y_from_preview(preview);
        }
        geometry_changed
    }
}

fn update_gpu_triangle_geometry(
    vertices: &mut [GpuVertex],
    triangle: &PreviewTriangle,
    billboard_attributes_dynamic: bool,
    surface_attributes_dynamic: bool,
) {
    debug_assert_eq!(vertices.len(), 3);
    let billboard = triangle.billboard;
    let explicit_normals = billboard
        .and_then(|billboard| billboard.normals)
        .or(triangle.normals);
    let procedural_color = triangle.color_channels[0].is_none()
        && triangle.vertex_colors.is_none()
        && triangle.color.is_none();
    let face_normal = if explicit_normals.is_none() || procedural_color {
        preview_triangle_normal(triangle)
    } else {
        [0.0; 3]
    };
    let normals = explicit_normals.unwrap_or([face_normal; 3]);
    for (vertex_index, vertex) in vertices.iter_mut().enumerate() {
        vertex.position = triangle.vertices[vertex_index];
        vertex.normal = normals[vertex_index];
    }

    if billboard_attributes_dynamic
        || billboard.is_some()
        || triangle.particle_type.is_some()
        || triangle.particle_pivot.is_some()
        || triangle.particle_direction.is_some()
    {
        let billboard_center_mode = billboard
            .map(|billboard| {
                [
                    billboard.center[0],
                    billboard.center[1],
                    billboard.center[2],
                    match billboard.mode {
                        J3dBillboardMode::Full => 1.0,
                        J3dBillboardMode::YAxis => 2.0,
                    },
                ]
            })
            .unwrap_or([
                triangle.particle_pivot.map_or(0.0, |pivot| pivot[0]),
                triangle.particle_pivot.map_or(0.0, |pivot| pivot[1]),
                0.0,
                triangle.particle_type.map_or(0.0, f32::from),
            ]);
        let billboard_offsets = billboard.map_or([[0.0; 3]; 3], |billboard| billboard.offsets);
        let billboard_axis_y = triangle
            .particle_direction
            .or_else(|| billboard.map(|billboard| billboard.axes[1]))
            .unwrap_or([0.0, 1.0, 0.0]);
        for (vertex_index, vertex) in vertices.iter_mut().enumerate() {
            vertex.billboard_center_mode = billboard_center_mode;
            vertex.billboard_offset = billboard_offsets[vertex_index];
            vertex.billboard_axis_y = billboard_axis_y;
        }
    }

    // Skeletal, rotation, level-transform, and object-transform updates only
    // change the geometry fields above. JPA particle slots also animate their
    // colors and texture coordinates, so retain full surface updates for any
    // slot that was or is a particle. Procedural fallback color is geometry-
    // dependent and must likewise be refreshed for otherwise-static surfaces.
    if surface_attributes_dynamic || triangle.particle_type.is_some() {
        update_gpu_triangle_surface(vertices, triangle, face_normal);
    } else if procedural_color {
        let colors = legacy_vertex_colors(triangle, face_normal);
        for (vertex, color) in vertices.iter_mut().zip(colors) {
            vertex.color0 = color;
        }
    }
}

fn update_gpu_triangle_surface(
    vertices: &mut [GpuVertex],
    triangle: &PreviewTriangle,
    face_normal: [f32; 3],
) {
    let color0 = triangle.color_channels[0]
        .map(|colors| colors.map(color_u8_to_f32))
        .or_else(|| {
            triangle
                .vertex_colors
                .map(|colors| colors.map(color_u8_to_f32))
        })
        .unwrap_or_else(|| legacy_vertex_colors(triangle, face_normal));
    let color1 = triangle.color_channels[1]
        .map(|colors| colors.map(color_u8_to_f32))
        .unwrap_or([[1.0; 4]; 3]);
    let tex_coords: [[[f32; 2]; 3]; TEXTURE_SLOT_COUNT] = std::array::from_fn(|slot| {
        triangle.tex_coord_sets[slot]
            .or_else(|| (slot == 0).then_some(triangle.tex_coords).flatten())
            .unwrap_or([[0.0; 2]; 3])
    });

    for (vertex_index, vertex) in vertices.iter_mut().enumerate() {
        vertex.color0 = color0[vertex_index];
        vertex.color1 = color1[vertex_index];
        vertex.uv0 = tex_coords[0][vertex_index];
        vertex.uv1 = tex_coords[1][vertex_index];
        vertex.uv2 = tex_coords[2][vertex_index];
        vertex.uv3 = tex_coords[3][vertex_index];
        vertex.uv4 = tex_coords[4][vertex_index];
        vertex.uv5 = tex_coords[5][vertex_index];
        vertex.uv6 = tex_coords[6][vertex_index];
        vertex.uv7 = tex_coords[7][vertex_index];
    }
}

fn extend_dirty_vertex_range(
    dirty: &mut Option<std::ops::Range<usize>>,
    update: std::ops::Range<usize>,
) {
    match dirty {
        Some(dirty) => {
            dirty.start = dirty.start.min(update.start);
            dirty.end = dirty.end.max(update.end);
        }
        None => *dirty = Some(update),
    }
}

fn mirror_plane_y_from_preview(preview: &ModelPreview) -> Option<f32> {
    preview
        .triangles
        .iter()
        .filter(|triangle| triangle.render_layer == PreviewRenderLayer::MirrorSurface)
        .flat_map(|triangle| triangle.vertices)
        .map(|vertex| vertex[1])
        .find(|height| height.is_finite())
}

fn triangle_is_mirror_visible(
    triangle: &PreviewTriangle,
    mirror_actor_models: &BTreeSet<usize>,
) -> bool {
    triangle.render_layer == PreviewRenderLayer::MirrorScene
        || triangle.render_layer == PreviewRenderLayer::Sky
        || (triangle.render_layer == PreviewRenderLayer::Main
            && mirror_actor_models.contains(&triangle.model_index))
}

fn legacy_vertex_colors(triangle: &PreviewTriangle, normal: [f32; 3]) -> [[f32; 4]; 3] {
    if let Some(color) = triangle.color {
        return [color_u8_to_f32(color); 3];
    }
    let average_y = triangle
        .vertices
        .iter()
        .map(|vertex| vertex[1])
        .sum::<f32>()
        / 3.0;
    preview_solid_triangle_colors(triangle, normal, average_y).map(color32_to_f32)
}

fn color32_to_f32(color: egui::Color32) -> [f32; 4] {
    let [r, g, b, a] = color.to_srgba_unmultiplied();
    color_u8_to_f32([r, g, b, a])
}

pub(super) fn color_u8_to_f32(color: [u8; 4]) -> [f32; 4] {
    color.map(|value| value as f32 / 255.0)
}

fn render_layer_id(layer: PreviewRenderLayer) -> u8 {
    match layer {
        PreviewRenderLayer::Sky => 0,
        PreviewRenderLayer::Main => 1,
        PreviewRenderLayer::Water => 2,
        PreviewRenderLayer::MirrorSurface => 3,
        PreviewRenderLayer::MirrorScene => 4,
        PreviewRenderLayer::Goop => 5,
        PreviewRenderLayer::Shadow => 6,
        PreviewRenderLayer::Heatwave => 7,
        PreviewRenderLayer::Particle => 8,
        PreviewRenderLayer::ParticleDistortion => 9,
        PreviewRenderLayer::IndirectWater => 10,
        PreviewRenderLayer::WaveFoam => 11,
    }
}

fn coordinate_space_for_render_layer(layer: PreviewRenderLayer) -> u32 {
    match layer {
        PreviewRenderLayer::Sky => 1,
        PreviewRenderLayer::Heatwave => 2,
        PreviewRenderLayer::Particle => 3,
        PreviewRenderLayer::ParticleDistortion => 4,
        PreviewRenderLayer::MirrorSurface => 5,
        PreviewRenderLayer::IndirectWater => 6,
        PreviewRenderLayer::WaveFoam => 7,
        _ => 0,
    }
}

fn render_layer_uses_efb_copy(layer: PreviewRenderLayer) -> bool {
    matches!(
        layer,
        PreviewRenderLayer::Heatwave
            | PreviewRenderLayer::IndirectWater
            | PreviewRenderLayer::ParticleDistortion
    )
}

fn wave_mask_source_layer(layer: PreviewRenderLayer) -> bool {
    layer == PreviewRenderLayer::Water
}

#[derive(Clone)]
struct GpuTextureData {
    mips: Vec<GpuTextureMip>,
    format: wgpu::TextureFormat,
    address_mode_u: wgpu::AddressMode,
    address_mode_v: wgpu::AddressMode,
    mag_filter: wgpu::FilterMode,
    min_filter: wgpu::FilterMode,
    mipmap_filter: wgpu::MipmapFilterMode,
    lod_min_clamp: f32,
    lod_max_clamp: f32,
}

#[derive(Clone)]
struct GpuTextureMip {
    size: [u32; 2],
    rgba: Vec<u8>,
}

impl GpuTextureData {
    fn white() -> Self {
        Self {
            mips: vec![GpuTextureMip {
                size: [1, 1],
                rgba: vec![255; 4],
            }],
            format: GX_COLOR_FORMAT,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            lod_min_clamp: 0.0,
            lod_max_clamp: 0.0,
        }
    }

    fn from_preview_texture(texture: &PreviewTexture) -> Self {
        let max_levels = if texture.mipmap_enabled {
            texture.mipmap_count.max(1) as usize
        } else {
            1
        };
        let mut mips = texture
            .mips
            .iter()
            .take(max_levels)
            .map(gpu_mip_from_color_image)
            .collect::<Vec<_>>();
        if mips.is_empty() {
            mips.push(gpu_mip_from_color_image(&texture.image));
        }
        let mip_filter_enabled = texture.mipmap_enabled
            && texture.mipmap_count > 1
            && matches!(texture.min_filter, 2..=5);
        let last_mip = mips.len().saturating_sub(1) as f32;
        let (lod_min_clamp, lod_max_clamp) = if mip_filter_enabled {
            let min = texture.min_lod.clamp(0.0, last_mip);
            (min, texture.max_lod.clamp(min, last_mip))
        } else {
            (0.0, 0.0)
        };
        Self {
            mips,
            format: gpu_texture_format_for_j3d(texture.format),
            address_mode_u: sampler_address_mode(texture.wrap_s),
            address_mode_v: sampler_address_mode(texture.wrap_t),
            mag_filter: sampler_mag_filter(texture.mag_filter),
            min_filter: sampler_min_filter(texture.min_filter),
            mipmap_filter: sampler_mipmap_filter(texture.min_filter, mip_filter_enabled),
            lod_min_clamp,
            lod_max_clamp,
        }
    }
}

fn gpu_texture_format_for_j3d(_format: u8) -> wgpu::TextureFormat {
    // GX has no per-texture color-space conversion. Intensity and color BTI
    // channels alike enter TEV as their stored numeric values, so sampling a
    // color texture through an sRGB view changes every tint, fog, and blend.
    GX_COLOR_FORMAT
}

fn gpu_mip_from_color_image(image: &egui::ColorImage) -> GpuTextureMip {
    let mut rgba = Vec::with_capacity(image.pixels.len() * 4);
    for pixel in &image.pixels {
        rgba.extend_from_slice(&pixel.to_srgba_unmultiplied());
    }
    GpuTextureMip {
        size: [image.size[0].max(1) as u32, image.size[1].max(1) as u32],
        rgba,
    }
}

fn sampler_address_mode(wrap: u8) -> wgpu::AddressMode {
    match wrap {
        0 => wgpu::AddressMode::ClampToEdge,
        2 => wgpu::AddressMode::MirrorRepeat,
        _ => wgpu::AddressMode::Repeat,
    }
}

fn sampler_mag_filter(filter: u8) -> wgpu::FilterMode {
    if filter == 1 {
        wgpu::FilterMode::Linear
    } else {
        wgpu::FilterMode::Nearest
    }
}

fn sampler_min_filter(filter: u8) -> wgpu::FilterMode {
    match filter {
        1 | 3 | 5 => wgpu::FilterMode::Linear,
        _ => wgpu::FilterMode::Nearest,
    }
}

fn sampler_mipmap_filter(filter: u8, mipmap_enabled: bool) -> wgpu::MipmapFilterMode {
    if !mipmap_enabled {
        return wgpu::MipmapFilterMode::Nearest;
    }
    match filter {
        4 | 5 => wgpu::MipmapFilterMode::Linear,
        _ => wgpu::MipmapFilterMode::Nearest,
    }
}

#[derive(Clone)]
struct GpuMaterialData {
    uniform: GpuMaterialUniform,
    texture_indices: [usize; TEXTURE_SLOT_COUNT],
    state: GpuMaterialState,
    tex_matrices: [Option<J3dTexMatrix>; TEXTURE_SLOT_COUNT],
    animations: Vec<GpuMaterialAnimation>,
    runtime_wave: bool,
}

#[derive(Clone)]
struct GpuMaterialAnimation {
    animation: J3dTextureSrtAnimation,
    binding_index: usize,
}

impl GpuMaterialData {
    fn from_j3d(material: &J3dMaterial, preview: &ModelPreview) -> Self {
        let texture_indices = std::array::from_fn(|slot| {
            material.texture_indices[slot]
                .filter(|index| *index < preview.textures.len())
                .map(|index| index + 1)
                .unwrap_or(0)
        });
        let animations = preview
            .material_animation_bindings
            .get(material.material_index)
            .into_iter()
            .flatten()
            .filter_map(|binding| {
                preview
                    .texture_srt_animations
                    .get(binding.animation_index)
                    .cloned()
                    .map(|animation| GpuMaterialAnimation {
                        animation,
                        binding_index: binding.binding_index,
                    })
            })
            .collect();
        let mut uniform = GpuMaterialUniform::from_j3d(material);
        for (slot, texture_index) in material.texture_indices.iter().enumerate() {
            if let Some(texture) = texture_index.and_then(|index| preview.textures.get(index)) {
                uniform.texture_sizes[slot] = texture_size_uniform([
                    texture.image.size[0] as f32,
                    texture.image.size[1] as f32,
                ]);
                uniform.texture_lod_parameters[slot] = texture_lod_uniform(texture);
            }
        }
        Self {
            uniform,
            texture_indices,
            state: GpuMaterialState::from_j3d(material),
            tex_matrices: material.tex_matrices,
            animations,
            runtime_wave: false,
        }
    }

    fn fallback(triangle: &PreviewTriangle, preview: &ModelPreview) -> Self {
        let mut uniform = GpuMaterialUniform::fallback(triangle.texture_index.is_some());
        if let Some(mode) = triangle.particle_color_mode {
            uniform.fog_meta[3] = 1;
            uniform.tev_color_args[0] = match mode {
                0 => [15, 8, 12, 15],
                1 => [15, 10, 8, 15],
                2 => [10, 12, 8, 15],
                3 => [4, 10, 8, 15],
                4 => [15, 8, 10, 4],
                5 => [15, 15, 15, 10],
                _ => [15, 10, 8, 15],
            };
            if let Some(environment) = triangle.particle_environment_color {
                uniform.tev_colors[1] = color_u8_to_f32(environment);
            }
        }
        if let Some(compare) = triangle.alpha_compare {
            uniform.set_alpha_compare(compare);
        }
        let mut texture_indices = [0; TEXTURE_SLOT_COUNT];
        texture_indices[0] = triangle
            .texture_index
            .filter(|index| *index < preview.textures.len())
            .map(|index| index + 1)
            .unwrap_or(0);
        texture_indices[1] = triangle
            .mask_texture_index
            .filter(|index| *index < preview.textures.len())
            .map(|index| index + 1)
            .unwrap_or(0);
        if let Some(texture) = triangle
            .texture_index
            .and_then(|index| preview.textures.get(index))
        {
            uniform.texture_sizes[0] =
                texture_size_uniform([texture.image.size[0] as f32, texture.image.size[1] as f32]);
        }
        if let Some(texture) = triangle
            .mask_texture_index
            .and_then(|index| preview.textures.get(index))
        {
            uniform.texture_sizes[1] =
                texture_size_uniform([texture.image.size[0] as f32, texture.image.size[1] as f32]);
        }
        Self {
            uniform,
            texture_indices,
            state: GpuMaterialState {
                cull: gpu_cull_mode(triangle.cull_mode.unwrap_or(0)),
                alpha_compare: triangle.alpha_compare.unwrap_or(always_alpha_compare()),
                blend: triangle.blend_mode.unwrap_or(J3dBlendMode {
                    mode: 0,
                    src_factor: 1,
                    dst_factor: 0,
                    logic_op: 3,
                }),
                depth: triangle.z_mode.unwrap_or(default_z_mode()),
                draw_mode: None,
            },
            tex_matrices: [None; TEXTURE_SLOT_COUNT],
            animations: Vec::new(),
            runtime_wave: false,
        }
    }

    fn uniform_at_time(&self, elapsed_seconds: f32) -> GpuMaterialUniform {
        let mut uniform = self.uniform;
        if self.runtime_wave {
            uniform.runtime_parameters[0] = elapsed_seconds;
        }
        for animated in &self.animations {
            let Some(binding) = animated.animation.bindings.get(animated.binding_index) else {
                continue;
            };
            let slot = binding.texture_matrix_index as usize;
            let Some(mut matrix) = self.tex_matrices.get(slot).copied().flatten() else {
                continue;
            };
            let frame = animated.animation.playback_frame(elapsed_seconds);
            let srt = binding.sample(frame);
            matrix.center = binding.center;
            matrix.scale = srt.scale;
            matrix.rotation = srt.rotation;
            matrix.translation = srt.translation;
            let rows = texture_srt_rows(matrix);
            uniform.tex_matrix_rows[slot * 3..slot * 3 + 3].copy_from_slice(&rows);
        }
        uniform
    }
}

#[derive(Clone, Copy)]
struct GpuMaterialState {
    cull: GpuCullMode,
    alpha_compare: J3dAlphaCompare,
    blend: J3dBlendMode,
    depth: J3dZMode,
    draw_mode: Option<u8>,
}

impl GpuMaterialState {
    fn from_j3d(material: &J3dMaterial) -> Self {
        Self {
            cull: gpu_cull_mode(material.cull_mode),
            alpha_compare: material.alpha_compare,
            blend: material.blend_mode,
            depth: material.z_mode,
            draw_mode: Some(material.mode),
        }
    }

    fn pipeline_key(self, render_layer: PreviewRenderLayer) -> GpuPipelineKey {
        let pass = if matches!(
            render_layer,
            PreviewRenderLayer::Heatwave | PreviewRenderLayer::IndirectWater
        ) {
            GpuBatchPass::Heatwave
        } else if render_layer == PreviewRenderLayer::WaveFoam {
            GpuBatchPass::WaveFoam
        } else if matches!(
            render_layer,
            PreviewRenderLayer::Particle | PreviewRenderLayer::ParticleDistortion
        ) {
            GpuBatchPass::Particle
        } else if render_layer == PreviewRenderLayer::Sky {
            GpuBatchPass::Sky
        } else if self
            .draw_mode
            .map_or(self.blend.mode == 1 || self.blend.mode == 3, |mode| {
                mode & 3 == 0
            })
        {
            GpuBatchPass::Translucent
        } else if !alpha_compare_is_always(self.alpha_compare) {
            GpuBatchPass::AlphaTest
        } else {
            GpuBatchPass::Opaque
        };
        let depth = if pass == GpuBatchPass::Sky {
            // The retail sky model is camera-relative and some of its opaque
            // materials write depth. That is safe inside Sunshine's gameplay
            // camera range, but a free editor camera can put level geometry
            // beyond the finite sky sphere. Keep sky as a true background so
            // it cannot occlude world geometry at long viewing distances.
            GpuDepthState {
                write: false,
                compare: GpuDepthCompare::Always,
            }
        } else {
            GpuDepthState {
                write: self.depth.update_enable != 0,
                compare: if self.depth.compare_enable == 0 {
                    GpuDepthCompare::Always
                } else {
                    gx_compare_to_gpu(self.depth.func)
                },
            }
        };
        GpuPipelineKey {
            pass,
            depth,
            cull: self.cull,
            blend: GpuBlendKey {
                mode: self.blend.mode,
                src_factor: self.blend.src_factor,
                dst_factor: self.blend.dst_factor,
                logic_op: self.blend.logic_op,
            },
        }
    }
}

fn always_alpha_compare() -> J3dAlphaCompare {
    J3dAlphaCompare {
        comp0: 7,
        ref0: 0,
        op: 0,
        comp1: 7,
        ref1: 0,
    }
}

fn default_z_mode() -> J3dZMode {
    J3dZMode {
        compare_enable: 1,
        func: 3,
        update_enable: 1,
    }
}

fn alpha_compare_is_always(compare: J3dAlphaCompare) -> bool {
    match compare.op {
        0 => compare.comp0 == 7 && compare.comp1 == 7,
        1 => compare.comp0 == 7 || compare.comp1 == 7,
        2 => false,
        3 => compare.comp0 == compare.comp1 && compare.ref0 == compare.ref1,
        _ => false,
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuMaterialUniform {
    counts: [u32; 4],
    alpha_compare: [u32; 4],
    alpha_refs: [f32; 4],
    texture_sizes: [[f32; 4]; TEXTURE_SLOT_COUNT],
    texture_lod_parameters: [[f32; 4]; TEXTURE_SLOT_COUNT],
    material_colors: [[f32; 4]; 2],
    ambient_colors: [[f32; 4]; 2],
    tev_colors: [[f32; 4]; 4],
    tev_k_colors: [[f32; 4]; 4],
    color_channels: [[u32; 4]; 4],
    tex_gens: [[u32; 4]; TEXTURE_SLOT_COUNT],
    tex_matrix_rows: [[f32; 4]; TEX_MATRIX_ROW_COUNT],
    tex_effect_rows: [[f32; 4]; TEXTURE_SLOT_COUNT * 4],
    tev_orders: [[u32; 4]; TEV_STAGE_COUNT],
    tev_color_args: [[u32; 4]; TEV_STAGE_COUNT],
    tev_color_ops: [[u32; 4]; TEV_STAGE_COUNT],
    tev_alpha_args: [[u32; 4]; TEV_STAGE_COUNT],
    tev_alpha_ops: [[u32; 4]; TEV_STAGE_COUNT],
    tev_selectors: [[u32; 4]; TEV_STAGE_COUNT],
    swap_tables: [[u32; 4]; 4],
    indirect_orders: [[u32; 4]; 3],
    indirect_matrix_rows: [[f32; 4]; 6],
    indirect_matrix_meta: [[u32; 4]; 3],
    indirect_stages0: [[u32; 4]; TEV_STAGE_COUNT],
    indirect_stages1: [[u32; 4]; TEV_STAGE_COUNT],
    indirect_stages2: [[u32; 4]; TEV_STAGE_COUNT],
    fog_meta: [u32; 4],
    fog_params: [f32; 4],
    fog_color: [f32; 4],
    runtime_parameters: [f32; 4],
}

impl GpuMaterialUniform {
    fn from_j3d(material: &J3dMaterial) -> Self {
        let mut uniform = Self::zeroed();
        uniform.counts = [
            material.tev_stages.len().min(TEV_STAGE_COUNT) as u32,
            material.tex_gen_count.min(TEXTURE_SLOT_COUNT as u8) as u32,
            material.color_channel_count.min(4) as u32,
            material.indirect.stage_count.min(3) as u32,
        ];
        uniform.set_alpha_compare(material.alpha_compare);
        uniform.texture_sizes = [texture_size_uniform([1.0, 1.0]); TEXTURE_SLOT_COUNT];
        uniform.material_colors = material.material_colors.map(color_u8_to_f32);
        uniform.ambient_colors = material.ambient_colors.map(color_u8_to_f32);
        uniform.tev_colors = material
            .tev_colors
            .map(|color| color.map(|value| value as f32 / 255.0));
        uniform.tev_k_colors = material.tev_k_colors.map(color_u8_to_f32);
        uniform.color_channels = material.color_channels.map(|channel| {
            [
                channel.enable as u32,
                channel.mat_src as u32,
                channel.amb_src as u32,
                channel.diffuse_fn as u32
                    | ((channel.attenuation_fn as u32) << 8)
                    | ((channel.light_mask as u32) << 16),
            ]
        });
        for slot in 0..TEXTURE_SLOT_COUNT {
            let tex_gen = material.tex_gens[slot];
            let matrix_slot = tex_matrix_slot(tex_gen.matrix)
                .filter(|slot| *slot < TEXTURE_SLOT_COUNT)
                .map(|slot| slot as u32 + 1)
                .unwrap_or(0);
            let matrix = tex_matrix_slot(tex_gen.matrix)
                .and_then(|slot| material.tex_matrices.get(slot).copied().flatten());
            uniform.tex_gens[slot] = [
                tex_gen.gen_type as u32,
                tex_gen.source as u32,
                matrix_slot,
                matrix
                    .map(|matrix| {
                        matrix.mode as u32
                            | ((matrix.projection as u32) << 8)
                            | ((u32::from(matrix.maya)) << 16)
                    })
                    .unwrap_or(0),
            ];
            if let Some(matrix) = material.tex_matrices[slot] {
                let rows = texture_srt_rows(matrix);
                uniform.tex_matrix_rows[slot * 3..slot * 3 + 3].copy_from_slice(&rows);
                uniform.tex_effect_rows[slot * 4..slot * 4 + 4]
                    .copy_from_slice(&matrix.effect_matrix);
            } else {
                uniform.tex_matrix_rows[slot * 3] = [1.0, 0.0, 0.0, 0.0];
                uniform.tex_matrix_rows[slot * 3 + 1] = [0.0, 1.0, 0.0, 0.0];
                uniform.tex_matrix_rows[slot * 3 + 2] = [0.0, 0.0, 1.0, 0.0];
                uniform.tex_effect_rows[slot * 4] = [1.0, 0.0, 0.0, 0.0];
                uniform.tex_effect_rows[slot * 4 + 1] = [0.0, 1.0, 0.0, 0.0];
                uniform.tex_effect_rows[slot * 4 + 2] = [0.0, 0.0, 1.0, 0.0];
                uniform.tex_effect_rows[slot * 4 + 3] = [0.0, 0.0, 0.0, 1.0];
            }
        }
        for (stage_index, stage) in material.tev_stages.iter().take(TEV_STAGE_COUNT).enumerate() {
            uniform.write_tev_stage(stage_index, *stage);
        }
        uniform.swap_tables = material.swap_tables.map(|table| table.map(u32::from));
        for slot in 0..3 {
            if let Some(order) = material.indirect.orders[slot] {
                let scale = material.indirect.scales[slot];
                uniform.indirect_orders[slot] = [
                    order.tex_coord.map(|value| value as u32 + 1).unwrap_or(0),
                    order.tex_map.map(|value| value as u32 + 1).unwrap_or(0),
                    scale.map(|value| value.scale_s as u32).unwrap_or(0),
                    scale.map(|value| value.scale_t as u32).unwrap_or(0),
                ];
            }
            if let Some(matrix) = material.indirect.matrices[slot] {
                let scale = 2.0f32.powi(matrix.scale_exponent as i32);
                uniform.indirect_matrix_rows[slot * 2] = [
                    matrix.rows[0][0] * scale,
                    matrix.rows[0][1] * scale,
                    matrix.rows[0][2] * scale,
                    0.0,
                ];
                uniform.indirect_matrix_rows[slot * 2 + 1] = [
                    matrix.rows[1][0] * scale,
                    matrix.rows[1][1] * scale,
                    matrix.rows[1][2] * scale,
                    0.0,
                ];
                uniform.indirect_matrix_meta[slot][0] = matrix.scale_exponent as i32 as u32;
            }
        }
        if let Some(fog) = material.fog {
            uniform.fog_meta = [
                fog.fog_type as u32,
                fog.adjustment_enabled as u32,
                fog.center as u32,
                0,
            ];
            uniform.fog_params = [fog.start_z, fog.end_z, fog.near_z, fog.far_z];
            uniform.fog_color = color_u8_to_f32(fog.color);
        }
        uniform
    }

    fn fallback(textured: bool) -> Self {
        let mut uniform = Self::zeroed();
        uniform.counts = [1, 1, 0, 0];
        uniform.set_alpha_compare(always_alpha_compare());
        uniform.texture_sizes = [texture_size_uniform([1.0, 1.0]); TEXTURE_SLOT_COUNT];
        uniform.material_colors = [[1.0; 4]; 2];
        uniform.ambient_colors = [[1.0; 4]; 2];
        uniform.tev_k_colors = [[1.0; 4]; 4];
        uniform.color_channels = [[0, 1, 1, 0], [0, 1, 1, 0], [0, 1, 1, 0], [0, 1, 1, 0]];
        uniform.tex_gens[0] = [1, 4, 0, 0];
        uniform.tex_matrix_rows[0] = [1.0, 0.0, 0.0, 0.0];
        uniform.tex_matrix_rows[1] = [0.0, 1.0, 0.0, 0.0];
        uniform.tex_matrix_rows[2] = [0.0, 0.0, 1.0, 0.0];
        uniform.tev_orders[0] = if textured { [1, 1, 4, 0] } else { [0, 0, 4, 0] };
        uniform.tev_color_args[0] = if textured {
            [15, 8, 10, 15]
        } else {
            [15, 15, 15, 10]
        };
        uniform.tev_alpha_args[0] = if textured { [7, 4, 5, 7] } else { [7, 7, 7, 5] };
        uniform.tev_color_ops[0] = [0, 0, 0, 1];
        uniform.tev_alpha_ops[0] = [0, 0, 0, 1];
        uniform.swap_tables = [[0, 1, 2, 3], [0, 0, 0, 0], [1, 1, 1, 1], [2, 2, 2, 2]];
        uniform
    }

    fn set_alpha_compare(&mut self, compare: J3dAlphaCompare) {
        self.alpha_compare = [
            compare.comp0 as u32,
            compare.op as u32,
            compare.comp1 as u32,
            1,
        ];
        self.alpha_refs = [
            compare.ref0 as f32 / 255.0,
            compare.ref1 as f32 / 255.0,
            0.0,
            0.0,
        ];
    }

    fn write_tev_stage(&mut self, index: usize, stage: J3dTevStage) {
        self.tev_orders[index] = [
            stage
                .order
                .tex_coord
                .map(|value| value as u32 + 1)
                .unwrap_or(0),
            stage
                .order
                .tex_map
                .map(|value| value as u32 + 1)
                .unwrap_or(0),
            stage.order.color_channel as u32,
            0,
        ];
        self.tev_color_args[index] = stage.color_args.map(u32::from);
        self.tev_color_ops[index] = [
            stage.color_op as u32,
            stage.color_bias as u32,
            stage.color_scale as u32,
            stage.color_clamp as u32 | ((stage.color_register as u32) << 8),
        ];
        self.tev_alpha_args[index] = stage.alpha_args.map(u32::from);
        self.tev_alpha_ops[index] = [
            stage.alpha_op as u32,
            stage.alpha_bias as u32,
            stage.alpha_scale as u32,
            stage.alpha_clamp as u32 | ((stage.alpha_register as u32) << 8),
        ];
        self.tev_selectors[index] = [
            stage.konst_color as u32,
            stage.konst_alpha as u32,
            stage.raster_swap as u32,
            stage.texture_swap as u32,
        ];
        self.indirect_stages0[index] = [
            stage.indirect.stage as u32,
            stage.indirect.format as u32,
            stage.indirect.bias as u32,
            stage.indirect.matrix as u32,
        ];
        self.indirect_stages1[index] = [
            stage.indirect.wrap_s as u32,
            stage.indirect.wrap_t as u32,
            stage.indirect.add_previous as u32,
            stage.indirect.use_original_lod as u32,
        ];
        self.indirect_stages2[index][0] = stage.indirect.alpha as u32;
    }
}

fn texture_size_uniform(size: [f32; 2]) -> [f32; 4] {
    let width = size[0].max(1.0);
    let height = size[1].max(1.0);
    [width, height, 1.0 / width, 1.0 / height]
}

fn texture_lod_uniform(texture: &PreviewTexture) -> [f32; 4] {
    let flags = u32::from(texture.mipmap_enabled)
        | (u32::from(texture.do_edge_lod) << 1)
        | (u32::from(texture.bias_clamp) << 2)
        | (u32::from(texture.max_anisotropy) << 8);
    [
        gx_lod_bias(texture.lod_bias),
        texture.min_lod,
        texture.max_lod,
        flags as f32,
    ]
}

fn gx_lod_bias(lod_bias: f32) -> f32 {
    if !lod_bias.is_finite() {
        return 0.0;
    }
    // GXInitTexObjLOD clamps the float and stores it in a signed 8-bit BP
    // field with five fractional bits. The float-to-integer conversion
    // truncates toward zero.
    (lod_bias.clamp(-4.0, 3.99) * 32.0).trunc() / 32.0
}

fn tex_matrix_slot(matrix: u8) -> Option<usize> {
    if matrix < 30 || matrix == 60 {
        return None;
    }
    let offset = matrix - 30;
    offset.is_multiple_of(3).then_some((offset / 3) as usize)
}

fn texture_srt_rows(matrix: J3dTexMatrix) -> [[f32; 4]; 3] {
    let radians = matrix.rotation as f32 * std::f32::consts::TAU / 65536.0;
    let (sin, cos) = radians.sin_cos();
    let mut rows = if matrix.maya {
        [
            [
                matrix.scale[0] * cos,
                matrix.scale[1] * sin,
                0.0,
                (matrix.translation[0] - 0.5) * cos
                    - sin * ((matrix.translation[1] - 0.5) + matrix.scale[1])
                    + 0.5,
            ],
            [
                -matrix.scale[0] * sin,
                matrix.scale[1] * cos,
                0.0,
                -(matrix.translation[0] - 0.5) * sin
                    - cos * ((matrix.translation[1] - 0.5) + matrix.scale[1])
                    + 0.5,
            ],
            [0.0, 0.0, 1.0, 0.0],
        ]
    } else {
        [
            [
                matrix.scale[0] * cos,
                -matrix.scale[0] * sin,
                0.0,
                -matrix.scale[0] * cos * matrix.center[0]
                    + matrix.scale[0] * sin * matrix.center[1]
                    + matrix.center[0]
                    + matrix.translation[0],
            ],
            [
                matrix.scale[1] * sin,
                matrix.scale[1] * cos,
                0.0,
                -matrix.scale[1] * sin * matrix.center[0]
                    - matrix.scale[1] * cos * matrix.center[1]
                    + matrix.center[1]
                    + matrix.translation[1],
            ],
            [0.0, 0.0, 1.0, 0.0],
        ]
    };
    if matches!(matrix.mode, 7 | 8 | 9 | 11) {
        for row in rows.iter_mut().take(2) {
            row[2] = row[3];
            row[3] = 0.0;
        }
    }
    rows
}

#[derive(Clone)]
struct GpuBatchData {
    pipeline_key: GpuPipelineKey,
    material_index: usize,
    packet_index: usize,
    render_layer: PreviewRenderLayer,
    mirror_visible: bool,
    vertices: Vec<GpuVertex>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct GpuPipelineKey {
    pass: GpuBatchPass,
    depth: GpuDepthState,
    cull: GpuCullMode,
    blend: GpuBlendKey,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum GpuBatchPass {
    Sky,
    Opaque,
    AlphaTest,
    Translucent,
    Heatwave,
    WaveFoam,
    Particle,
}

fn gpu_batch_pass_is_post_snapshot(pass: GpuBatchPass) -> bool {
    matches!(
        pass,
        GpuBatchPass::Heatwave | GpuBatchPass::WaveFoam | GpuBatchPass::Particle
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct GpuDepthState {
    write: bool,
    compare: GpuDepthCompare,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum GpuDepthCompare {
    Never,
    Less,
    Equal,
    LessEqual,
    Greater,
    NotEqual,
    GreaterEqual,
    Always,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum GpuCullMode {
    None,
    Front,
    Back,
    All,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct GpuBlendKey {
    mode: u8,
    src_factor: u8,
    dst_factor: u8,
    logic_op: u8,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuVertex {
    position: [f32; 3],
    normal: [f32; 3],
    color0: [f32; 4],
    color1: [f32; 4],
    uv0: [f32; 2],
    uv1: [f32; 2],
    uv2: [f32; 2],
    uv3: [f32; 2],
    uv4: [f32; 2],
    uv5: [f32; 2],
    uv6: [f32; 2],
    uv7: [f32; 2],
    coordinate_space: u32,
    billboard_center_mode: [f32; 4],
    billboard_offset: [f32; 3],
    billboard_axis_y: [f32; 3],
}

impl GpuVertex {
    const ATTRIBUTES: [wgpu::VertexAttribute; 16] = wgpu::vertex_attr_array![
        0 => Float32x3,
        1 => Float32x3,
        2 => Float32x4,
        3 => Float32x4,
        4 => Float32x2,
        5 => Float32x2,
        6 => Float32x2,
        7 => Float32x2,
        8 => Float32x2,
        9 => Float32x2,
        10 => Float32x2,
        11 => Float32x2,
        12 => Uint32,
        13 => Float32x4,
        14 => Float32x3,
        15 => Float32x3
    ];

    fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBUTES,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
struct GpuCameraUniform {
    camera_position: [f32; 4],
    right: [f32; 4],
    up: [f32; 4],
    forward: [f32; 4],
    projection: [f32; 4],
    clip: [f32; 4],
    light_position: [f32; 4],
    light_color: [f32; 4],
    ambient_color: [f32; 4],
    lighting_meta: [f32; 4],
    render_target_size: [f32; 4],
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct GpuOffscreenFrameState {
    camera: GpuCameraUniform,
    mirror_camera: GpuCameraUniform,
}

impl GpuOffscreenFrameState {
    fn new(
        frame: GpuViewportFrame,
        mirror_plane_y: Option<f32>,
        render_target_size: [u32; 2],
    ) -> Self {
        Self {
            camera: GpuCameraUniform::from_frame(frame, render_target_size),
            mirror_camera: GpuCameraUniform::from_frame(
                mirror_viewport_frame(frame, mirror_plane_y),
                render_target_size,
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct GpuOffscreenInvalidation {
    target: bool,
    scene: bool,
    geometry: bool,
    materials: bool,
    time_animation: bool,
}

fn offscreen_render_required(
    previous: Option<GpuOffscreenFrameState>,
    current: GpuOffscreenFrameState,
    invalidation: GpuOffscreenInvalidation,
) -> bool {
    previous != Some(current)
        || invalidation.target
        || invalidation.scene
        || invalidation.geometry
        || invalidation.materials
        || invalidation.time_animation
}

impl GpuCameraUniform {
    fn from_frame(frame: GpuViewportFrame, render_target_size: [u32; 2]) -> Self {
        let half_width = (frame.viewport_size[0] * 0.5).max(1.0);
        let half_height = (frame.viewport_size[1] * 0.5).max(1.0);
        let render_width = render_target_size[0].max(1) as f32;
        let render_height = render_target_size[1].max(1) as f32;
        Self {
            camera_position: vec4(frame.camera_position, 0.0),
            right: vec4(frame.right, 0.0),
            up: vec4(frame.up, 0.0),
            forward: vec4(frame.forward, 0.0),
            projection: [
                frame.focal / half_width,
                frame.focal / half_height,
                frame.viewport_pan[0] / half_width,
                -frame.viewport_pan[1] / half_height,
            ],
            clip: [frame.near, 0.0, 0.0, 0.0],
            light_position: vec4(frame.light_position, 0.0),
            light_color: frame.light_color,
            ambient_color: frame.ambient_color.unwrap_or([1.0; 4]),
            lighting_meta: [f32::from(frame.ambient_color.is_some()), 0.0, 0.0, 0.0],
            render_target_size: [
                render_width,
                render_height,
                1.0 / render_width,
                1.0 / render_height,
            ],
        }
    }
}

fn mirror_viewport_frame(mut frame: GpuViewportFrame, plane_y: Option<f32>) -> GpuViewportFrame {
    let Some(plane_y) = plane_y else {
        return frame;
    };
    frame.camera_position[1] = plane_y * 2.0 - frame.camera_position[1];
    frame.forward[1] = -frame.forward[1];
    frame.up[1] = -frame.up[1];
    // C_MTXLookAt rebuilds the right axis after reflecting eye, target, and up.
    // The cross product introduces this extra sign versus reflecting right directly.
    frame.right = [-frame.right[0], frame.right[1], -frame.right[2]];

    // TMirrorCamera widens the main camera's vertical field of view by 1.3.
    let half_height = (frame.viewport_size[1] * 0.5).max(1.0);
    let half_fovy = (half_height / frame.focal.max(0.000_001)).atan();
    frame.focal = half_height / (half_fovy * 1.3).tan().max(0.000_001);
    frame
}

struct GpuViewportResources {
    pipeline_layout: wgpu::PipelineLayout,
    material_layout: wgpu::BindGroupLayout,
    wave_mask_pipeline: wgpu::RenderPipeline,
    composite_layout: wgpu::BindGroupLayout,
    composite_pipeline: wgpu::RenderPipeline,
    composite_sampler: wgpu::Sampler,
    viewport_target: Option<GpuViewportTarget>,
    pipelines: BTreeMap<GpuPipelineKey, wgpu::RenderPipeline>,
    camera_buffer: wgpu::Buffer,
    mirror_camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    mirror_camera_bind_group: wgpu::BindGroup,
    textures: Vec<GpuTextureResource>,
    material_buffers: Vec<wgpu::Buffer>,
    material_bind_groups: Vec<wgpu::BindGroup>,
    efb_copy_materials: BTreeSet<usize>,
    mirror_surface_materials: BTreeSet<usize>,
    wave_mask_materials: BTreeSet<usize>,
    has_wave_mask_sources: bool,
    animated_materials: BTreeSet<usize>,
    batches: Vec<GpuBatchResources>,
    draw_order: Vec<GpuDrawCommand>,
    generation: u64,
    geometry_generation: u64,
    offscreen_frame_state: Option<GpuOffscreenFrameState>,
}

impl GpuViewportResources {
    fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sms viewport camera layout"),
            entries: &[0, 1].map(|binding| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: camera_binding_visibility(binding),
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }),
        });
        let mut material_entries = vec![wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }];
        for slot in 0..TEXTURE_SLOT_COUNT {
            material_entries.push(wgpu::BindGroupLayoutEntry {
                binding: 1 + slot as u32 * 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            });
            material_entries.push(wgpu::BindGroupLayoutEntry {
                binding: 2 + slot as u32 * 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            });
        }
        let material_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sms viewport J3D material layout"),
            entries: &material_entries,
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sms viewport J3D pipeline layout"),
            bind_group_layouts: &[Some(&camera_layout), Some(&material_layout)],
            immediate_size: 0,
        });
        let wave_mask_pipeline = create_wave_mask_pipeline(device, &pipeline_layout);
        let composite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sms viewport composite layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let composite_pipeline =
            create_composite_pipeline(device, &composite_layout, target_format);
        let composite_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("sms viewport composite sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sms viewport camera buffer"),
            size: std::mem::size_of::<GpuCameraUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mirror_camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sms viewport mirror camera buffer"),
            size: std::mem::size_of::<GpuCameraUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sms viewport camera bind group"),
            layout: &camera_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: camera_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: mirror_camera_buffer.as_entire_binding(),
                },
            ],
        });
        let mirror_camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sms viewport mirror camera bind group"),
            layout: &camera_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: mirror_camera_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: mirror_camera_buffer.as_entire_binding(),
                },
            ],
        });
        Self {
            pipeline_layout,
            material_layout,
            wave_mask_pipeline,
            composite_layout,
            composite_pipeline,
            composite_sampler,
            viewport_target: None,
            pipelines: BTreeMap::new(),
            camera_buffer,
            mirror_camera_buffer,
            camera_bind_group,
            mirror_camera_bind_group,
            textures: Vec::new(),
            material_buffers: Vec::new(),
            material_bind_groups: Vec::new(),
            efb_copy_materials: BTreeSet::new(),
            mirror_surface_materials: BTreeSet::new(),
            wave_mask_materials: BTreeSet::new(),
            has_wave_mask_sources: false,
            animated_materials: BTreeSet::new(),
            batches: Vec::new(),
            draw_order: Vec::new(),
            generation: 0,
            geometry_generation: 0,
            offscreen_frame_state: None,
        }
    }

    fn ensure_scene(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &GpuSceneData,
        generation: u64,
        geometry_generation: u64,
    ) -> bool {
        if self.generation == generation {
            return false;
        }
        for batch in &scene.batches {
            self.ensure_pipeline(device, batch.pipeline_key);
        }
        self.textures = scene
            .textures
            .iter()
            .map(|texture| GpuTextureResource::new(device, queue, texture))
            .collect();
        self.material_buffers.clear();
        self.material_bind_groups.clear();
        self.efb_copy_materials = scene
            .batches
            .iter()
            .filter(|batch| render_layer_uses_efb_copy(batch.render_layer))
            .map(|batch| batch.material_index)
            .collect();
        self.mirror_surface_materials = scene
            .batches
            .iter()
            .filter(|batch| batch.render_layer == PreviewRenderLayer::MirrorSurface)
            .map(|batch| batch.material_index)
            .collect();
        self.wave_mask_materials = scene
            .batches
            .iter()
            .filter(|batch| batch.render_layer == PreviewRenderLayer::WaveFoam)
            .map(|batch| batch.material_index)
            .collect();
        self.has_wave_mask_sources = !self.wave_mask_materials.is_empty()
            && scene
                .batches
                .iter()
                .any(|batch| wave_mask_source_layer(batch.render_layer));
        self.animated_materials = scene
            .materials
            .iter()
            .enumerate()
            .filter_map(|(index, material)| {
                (!material.animations.is_empty() || material.runtime_wave).then_some(index)
            })
            .collect();
        for (index, material) in scene.materials.iter().enumerate() {
            let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("sms viewport J3D material uniform"),
                contents: bytemuck::bytes_of(&material.uniform),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
            let bind_group = self.create_material_bind_group(device, index, material, &buffer);
            self.material_buffers.push(buffer);
            self.material_bind_groups.push(bind_group);
        }
        self.batches = scene
            .batches
            .iter()
            .filter(|batch| !batch.vertices.is_empty())
            .map(|batch| GpuBatchResources::new(device, batch))
            .collect();
        self.draw_order = sorted_gpu_draw_order(&self.batches);
        self.generation = generation;
        self.geometry_generation = geometry_generation;
        true
    }

    fn write_geometry(
        &mut self,
        queue: &wgpu::Queue,
        scene: &GpuSceneData,
        dirty_vertex_ranges: &[Option<std::ops::Range<usize>>],
        geometry_generation: u64,
    ) -> bool {
        if self.geometry_generation == geometry_generation {
            return false;
        }
        for (batch_index, dirty_range) in dirty_vertex_ranges.iter().enumerate() {
            let Some(dirty_range) = dirty_range.as_ref() else {
                continue;
            };
            let Some(data) = scene.batches.get(batch_index) else {
                continue;
            };
            let Some(resources) = self.batches.get(batch_index) else {
                continue;
            };
            let Some(vertices) = data.vertices.get(dirty_range.clone()) else {
                continue;
            };
            queue.write_buffer(
                &resources.vertex_buffer,
                (dirty_range.start * std::mem::size_of::<GpuVertex>()) as wgpu::BufferAddress,
                bytemuck::cast_slice(vertices),
            );
        }
        self.geometry_generation = geometry_generation;
        true
    }

    fn write_materials(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &GpuSceneData,
        dirty_materials: &BTreeSet<usize>,
    ) -> bool {
        let materials_changed = !dirty_materials.is_empty();
        for index in dirty_materials.iter().copied() {
            let Some(material) = scene.materials.get(index) else {
                continue;
            };
            if index >= self.material_buffers.len() || index >= self.material_bind_groups.len() {
                continue;
            }
            let buffer = &self.material_buffers[index];
            queue.write_buffer(buffer, 0, bytemuck::bytes_of(&material.uniform));
            let bind_group = self.create_material_bind_group(device, index, material, buffer);
            self.material_bind_groups[index] = bind_group;
            if material.animations.is_empty() && !material.runtime_wave {
                self.animated_materials.remove(&index);
            } else {
                self.animated_materials.insert(index);
            }
        }
        materials_changed
    }

    fn rebuild_target_material_bind_groups(&mut self, device: &wgpu::Device, scene: &GpuSceneData) {
        let target_materials = self
            .efb_copy_materials
            .iter()
            .chain(&self.mirror_surface_materials)
            .chain(&self.wave_mask_materials)
            .copied()
            .collect::<Vec<_>>();
        for index in target_materials {
            let Some(material) = scene.materials.get(index) else {
                continue;
            };
            let Some(buffer) = self.material_buffers.get(index) else {
                continue;
            };
            if index >= self.material_bind_groups.len() {
                continue;
            }
            self.material_bind_groups[index] =
                self.create_material_bind_group(device, index, material, buffer);
        }
    }

    fn create_material_bind_group(
        &self,
        device: &wgpu::Device,
        index: usize,
        material: &GpuMaterialData,
        buffer: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        let mut entries = vec![wgpu::BindGroupEntry {
            binding: 0,
            resource: buffer.as_entire_binding(),
        }];
        let uses_efb_copy = self.efb_copy_materials.contains(&index);
        let mirror_surface = self.mirror_surface_materials.contains(&index);
        let wave_mask = self.has_wave_mask_sources && self.wave_mask_materials.contains(&index);
        for slot in 0..TEXTURE_SLOT_COUNT {
            let texture_index = material.texture_indices[slot].min(self.textures.len() - 1);
            let texture = &self.textures[texture_index];
            let view = if mirror_surface && slot == 0 {
                self.viewport_target
                    .as_ref()
                    .map(|target| &target.mirror_view)
                    .unwrap_or(&texture.view)
            } else if uses_efb_copy && slot == 1 {
                self.viewport_target
                    .as_ref()
                    .map(|target| &target.efb_copy_view)
                    .unwrap_or(&texture.view)
            } else if wave_mask && slot == 1 {
                self.viewport_target
                    .as_ref()
                    .map(|target| &target.wave_mask_view)
                    .unwrap_or(&texture.view)
            } else {
                &texture.view
            };
            entries.push(wgpu::BindGroupEntry {
                binding: 1 + slot as u32 * 2,
                resource: wgpu::BindingResource::TextureView(view),
            });
            entries.push(wgpu::BindGroupEntry {
                binding: 2 + slot as u32 * 2,
                resource: wgpu::BindingResource::Sampler(&texture.sampler),
            });
        }
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(&format!("sms viewport J3D material {index}")),
            layout: &self.material_layout,
            entries: &entries,
        })
    }

    fn ensure_pipeline(&mut self, device: &wgpu::Device, key: GpuPipelineKey) {
        if !self.pipelines.contains_key(&key) {
            let pipeline = create_pipeline(device, &self.pipeline_layout, GX_COLOR_FORMAT, key);
            self.pipelines.insert(key, pipeline);
        }
    }

    fn ensure_viewport_target(&mut self, device: &wgpu::Device, size: [u32; 2]) -> bool {
        if self
            .viewport_target
            .as_ref()
            .is_some_and(|target| target.size == size)
        {
            return false;
        }
        self.viewport_target = Some(GpuViewportTarget::new(
            device,
            &self.composite_layout,
            &self.composite_sampler,
            size,
        ));
        true
    }

    fn write_frame(&mut self, queue: &wgpu::Queue, frame: GpuViewportFrame, scene: &GpuSceneData) {
        let mirror_frame = mirror_viewport_frame(frame, scene.mirror_plane_y);
        let render_target_size = self
            .viewport_target
            .as_ref()
            .map_or([1, 1], |target| target.size);
        queue.write_buffer(
            &self.camera_buffer,
            0,
            bytemuck::bytes_of(&GpuCameraUniform::from_frame(frame, render_target_size)),
        );
        queue.write_buffer(
            &self.mirror_camera_buffer,
            0,
            bytemuck::bytes_of(&GpuCameraUniform::from_frame(
                mirror_frame,
                render_target_size,
            )),
        );
        for index in self.animated_materials.iter().copied() {
            let Some(material) = scene.materials.get(index) else {
                continue;
            };
            let Some(buffer) = self.material_buffers.get(index) else {
                continue;
            };
            let uniform = material.uniform_at_time(frame.animation_seconds);
            queue.write_buffer(buffer, 0, bytemuck::bytes_of(&uniform));
        }
    }

    fn render_scene(&self, encoder: &mut wgpu::CommandEncoder) {
        let Some(target) = &self.viewport_target else {
            return;
        };
        let has_mirror = self
            .batches
            .iter()
            .any(|batch| batch.render_layer == PreviewRenderLayer::MirrorSurface);
        if has_mirror {
            let mut mirror_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("sms GX mirror scene render"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.mirror_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &target.mirror_depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            mirror_pass.set_bind_group(0, &self.mirror_camera_bind_group, &[]);
            for command in &self.draw_order {
                let Some(batch) = self.batches.get(command.batch_index) else {
                    continue;
                };
                if !batch.mirror_visible {
                    continue;
                }
                let Some(pipeline) = self.pipelines.get(&batch.pipeline_key) else {
                    continue;
                };
                let Some(material) = self.material_bind_groups.get(batch.material_index) else {
                    continue;
                };
                mirror_pass.set_pipeline(pipeline);
                mirror_pass.set_bind_group(1, material, &[]);
                mirror_pass.set_vertex_buffer(0, batch.vertex_buffer.slice(..));
                mirror_pass.draw(0..batch.vertex_count, 0..1);
            }
        }
        let has_post_snapshot = self
            .draw_order
            .iter()
            .filter_map(|command| self.batches.get(command.batch_index))
            .any(|batch| gpu_batch_pass_is_post_snapshot(batch.pipeline_key.pass));
        let needs_efb_copy = self
            .draw_order
            .iter()
            .filter_map(|command| self.batches.get(command.batch_index))
            .any(|batch| render_layer_uses_efb_copy(batch.render_layer));
        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("sms GX viewport scene render"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.color_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // TDisplay initializes Sunshine's display clear color to black.
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &target.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: if has_post_snapshot {
                            wgpu::StoreOp::Store
                        } else {
                            wgpu::StoreOp::Discard
                        },
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            render_pass.set_bind_group(0, &self.camera_bind_group, &[]);
            for command in &self.draw_order {
                let Some(batch) = self.batches.get(command.batch_index) else {
                    continue;
                };
                if gpu_batch_pass_is_post_snapshot(batch.pipeline_key.pass)
                    || batch.render_layer == PreviewRenderLayer::MirrorScene
                {
                    continue;
                }
                let Some(pipeline) = self.pipelines.get(&batch.pipeline_key) else {
                    continue;
                };
                let Some(material) = self.material_bind_groups.get(batch.material_index) else {
                    continue;
                };
                render_pass.set_pipeline(pipeline);
                render_pass.set_bind_group(1, material, &[]);
                render_pass.set_vertex_buffer(0, batch.vertex_buffer.slice(..));
                render_pass.draw(0..batch.vertex_count, 0..1);
            }
        }

        if self.has_wave_mask_sources {
            let mut mask_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("sms GX visible-water wave mask"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.wave_mask_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &target.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            mask_pass.set_bind_group(0, &self.camera_bind_group, &[]);
            mask_pass.set_pipeline(&self.wave_mask_pipeline);
            for command in &self.draw_order {
                let Some(batch) = self.batches.get(command.batch_index) else {
                    continue;
                };
                if !wave_mask_source_layer(batch.render_layer) {
                    continue;
                }
                let Some(material) = self.material_bind_groups.get(batch.material_index) else {
                    continue;
                };
                mask_pass.set_bind_group(1, material, &[]);
                mask_pass.set_vertex_buffer(0, batch.vertex_buffer.slice(..));
                mask_pass.draw(0..batch.vertex_count, 0..1);
            }
        }

        if !has_post_snapshot {
            return;
        }

        if needs_efb_copy {
            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &target.color,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &target.efb_copy,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                target.extent(),
            );
        }

        let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("sms GX post-snapshot render"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target.color_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &target.depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        render_pass.set_bind_group(0, &self.camera_bind_group, &[]);
        for command in &self.draw_order {
            let Some(batch) = self.batches.get(command.batch_index) else {
                continue;
            };
            if !gpu_batch_pass_is_post_snapshot(batch.pipeline_key.pass) {
                continue;
            }
            let Some(pipeline) = self.pipelines.get(&batch.pipeline_key) else {
                continue;
            };
            let Some(material) = self.material_bind_groups.get(batch.material_index) else {
                continue;
            };
            render_pass.set_pipeline(pipeline);
            render_pass.set_bind_group(1, material, &[]);
            render_pass.set_vertex_buffer(0, batch.vertex_buffer.slice(..));
            render_pass.draw(0..batch.vertex_count, 0..1);
        }
    }

    fn composite(&self, render_pass: &mut wgpu::RenderPass<'static>) {
        let Some(target) = &self.viewport_target else {
            return;
        };
        render_pass.set_pipeline(&self.composite_pipeline);
        render_pass.set_bind_group(0, &target.composite_bind_group, &[]);
        render_pass.draw(0..3, 0..1);
    }
}

fn camera_binding_visibility(binding: u32) -> wgpu::ShaderStages {
    if binding == 0 {
        // Fragment sampling uses the physical target size to reproduce
        // Sunshine's fixed-EFB mip selection.
        wgpu::ShaderStages::VERTEX_FRAGMENT
    } else {
        wgpu::ShaderStages::VERTEX
    }
}

struct GpuViewportTarget {
    size: [u32; 2],
    color: wgpu::Texture,
    color_view: wgpu::TextureView,
    efb_copy: wgpu::Texture,
    efb_copy_view: wgpu::TextureView,
    _wave_mask: wgpu::Texture,
    wave_mask_view: wgpu::TextureView,
    _mirror: wgpu::Texture,
    mirror_view: wgpu::TextureView,
    _mirror_depth: wgpu::Texture,
    mirror_depth_view: wgpu::TextureView,
    _depth: wgpu::Texture,
    depth_view: wgpu::TextureView,
    composite_bind_group: wgpu::BindGroup,
}

impl GpuViewportTarget {
    fn new(
        device: &wgpu::Device,
        composite_layout: &wgpu::BindGroupLayout,
        composite_sampler: &wgpu::Sampler,
        size: [u32; 2],
    ) -> Self {
        let extent = wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        };
        let color = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("sms GX viewport color"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: GX_COLOR_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
        let efb_copy = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("sms GX shimmer EFB copy"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: GX_COLOR_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let efb_copy_view = efb_copy.create_view(&wgpu::TextureViewDescriptor::default());
        let wave_mask = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("sms GX visible-water wave mask"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: WAVE_MASK_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let wave_mask_view = wave_mask.create_view(&wgpu::TextureViewDescriptor::default());
        // Sunshine uses a fixed 256x256 RGB5A3 mirror buffer. The editor keeps
        // the same mirror camera and projection but renders at the viewport's
        // physical pixel size so the preview remains sharp on modern displays.
        let mirror_extent = extent;
        let mirror = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("sms GX mirror texture"),
            size: mirror_extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: GX_COLOR_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let mirror_view = mirror.create_view(&wgpu::TextureViewDescriptor::default());
        let mirror_depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("sms GX mirror depth"),
            size: mirror_extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let mirror_depth_view = mirror_depth.create_view(&wgpu::TextureViewDescriptor::default());
        let depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("sms GX viewport depth"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());
        let composite_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sms GX viewport composite bind group"),
            layout: composite_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&color_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(composite_sampler),
                },
            ],
        });
        Self {
            size,
            color,
            color_view,
            efb_copy,
            efb_copy_view,
            _wave_mask: wave_mask,
            wave_mask_view,
            _mirror: mirror,
            mirror_view,
            _mirror_depth: mirror_depth,
            mirror_depth_view,
            _depth: depth,
            depth_view,
            composite_bind_group,
        }
    }

    fn extent(&self) -> wgpu::Extent3d {
        wgpu::Extent3d {
            width: self.size[0],
            height: self.size[1],
            depth_or_array_layers: 1,
        }
    }
}

struct GpuTextureResource {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    sampler: wgpu::Sampler,
}

impl GpuTextureResource {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, data: &GpuTextureData) -> Self {
        let base_mip = data.mips.first().expect("viewport texture has a base mip");
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("sms viewport J3D texture"),
            size: wgpu::Extent3d {
                width: base_mip.size[0],
                height: base_mip.size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: data.mips.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: data.format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (level, mip) in data.mips.iter().enumerate() {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: level as u32,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &mip.rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(mip.size[0] * 4),
                    rows_per_image: Some(mip.size[1]),
                },
                wgpu::Extent3d {
                    width: mip.size[0],
                    height: mip.size[1],
                    depth_or_array_layers: 1,
                },
            );
        }
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("sms viewport J3D sampler"),
            address_mode_u: data.address_mode_u,
            address_mode_v: data.address_mode_v,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: data.mag_filter,
            min_filter: data.min_filter,
            mipmap_filter: data.mipmap_filter,
            lod_min_clamp: data.lod_min_clamp,
            lod_max_clamp: data.lod_max_clamp,
            ..Default::default()
        });
        Self {
            _texture: texture,
            view,
            sampler,
        }
    }
}

struct GpuBatchResources {
    pipeline_key: GpuPipelineKey,
    material_index: usize,
    packet_index: usize,
    render_layer: PreviewRenderLayer,
    mirror_visible: bool,
    vertex_buffer: wgpu::Buffer,
    vertex_count: u32,
}

impl GpuBatchResources {
    fn new(device: &wgpu::Device, batch: &GpuBatchData) -> Self {
        Self {
            pipeline_key: batch.pipeline_key,
            material_index: batch.material_index,
            packet_index: batch.packet_index,
            render_layer: batch.render_layer,
            mirror_visible: batch.mirror_visible,
            vertex_buffer: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("sms viewport J3D vertex buffer"),
                contents: bytemuck::cast_slice(&batch.vertices),
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            }),
            vertex_count: batch.vertices.len() as u32,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GpuDrawCommand {
    batch_index: usize,
}

fn sorted_gpu_draw_order(batches: &[GpuBatchResources]) -> Vec<GpuDrawCommand> {
    sorted_gpu_draw_order_from_info(batches.iter().enumerate().map(|(batch_index, batch)| {
        GpuDrawBatchInfo {
            batch_index,
            pass: batch.pipeline_key.pass,
            material_index: batch.material_index,
            packet_index: batch.packet_index,
        }
    }))
}

#[derive(Clone, Copy)]
struct GpuDrawBatchInfo {
    batch_index: usize,
    pass: GpuBatchPass,
    material_index: usize,
    packet_index: usize,
}

fn sorted_gpu_draw_order_from_info(
    batches: impl IntoIterator<Item = GpuDrawBatchInfo>,
) -> Vec<GpuDrawCommand> {
    let mut solid = Vec::<(usize, usize, usize)>::new();
    let mut translucent = Vec::<(usize, usize, usize)>::new();
    let mut sky = Vec::<(usize, usize, usize)>::new();
    let mut heatwave = Vec::<(usize, usize, usize)>::new();
    let mut wave_foam = Vec::<(usize, usize, usize)>::new();
    let mut particles = Vec::<(usize, usize, usize)>::new();
    for batch in batches {
        let entry = (batch.material_index, batch.packet_index, batch.batch_index);
        match batch.pass {
            GpuBatchPass::Sky => sky.push(entry),
            GpuBatchPass::Translucent => translucent.push(entry),
            GpuBatchPass::Heatwave => heatwave.push(entry),
            GpuBatchPass::WaveFoam => wave_foam.push(entry),
            GpuBatchPass::Particle => particles.push(entry),
            GpuBatchPass::Opaque | GpuBatchPass::AlphaTest => solid.push(entry),
        }
    }
    sky.sort_unstable();
    solid.sort_unstable();
    translucent.sort_unstable();
    heatwave.sort_unstable();
    wave_foam.sort_unstable();
    particles.sort_unstable();
    sky.into_iter()
        .chain(solid)
        .chain(translucent)
        .chain(heatwave)
        .chain(wave_foam)
        .chain(particles)
        .map(|(_, _, batch_index)| GpuDrawCommand { batch_index })
        .collect()
}

fn create_composite_pipeline(
    device: &wgpu::Device,
    composite_layout: &wgpu::BindGroupLayout,
    target_format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("sms viewport composite pipeline layout"),
        bind_group_layouts: &[Some(composite_layout)],
        immediate_size: 0,
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("sms viewport composite shader"),
        source: wgpu::ShaderSource::Wgsl(COMPOSITE_SHADER.into()),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("sms viewport composite pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some(composite_fragment_entry(target_format)),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: target_format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: Some(composite_depth_stencil_state()),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn composite_fragment_entry(target_format: wgpu::TextureFormat) -> &'static str {
    if target_format.is_srgb() {
        "fs_srgb_target"
    } else {
        "fs_unorm_target"
    }
}

fn composite_depth_stencil_state() -> wgpu::DepthStencilState {
    // eframe's main render pass includes the depth buffer requested in
    // NativeOptions. wgpu requires every pipeline used by that pass to declare
    // the same attachment format, even when the draw ignores depth entirely.
    wgpu::DepthStencilState {
        format: DEPTH_FORMAT,
        depth_write_enabled: Some(false),
        depth_compare: Some(wgpu::CompareFunction::Always),
        stencil: Default::default(),
        bias: Default::default(),
    }
}

fn create_wave_mask_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("sms viewport visible-water mask shader"),
        source: wgpu::ShaderSource::Wgsl(J3D_SHADER.into()),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("sms viewport visible-water mask pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[GpuVertex::layout()],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_wave_mask"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: WAVE_MASK_FORMAT,
                blend: None,
                write_mask: wgpu::ColorWrites::RED,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Cw,
            cull_mode: None,
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(false),
            depth_compare: Some(wgpu::CompareFunction::LessEqual),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn create_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    target_format: wgpu::TextureFormat,
    key: GpuPipelineKey,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("sms viewport J3D TEV shader"),
        source: wgpu::ShaderSource::Wgsl(J3D_SHADER.into()),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("sms viewport J3D pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[GpuVertex::layout()],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: target_format,
                blend: gpu_blend_state(key.blend),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            // The editor's positive-depth camera basis mirrors GX clip-space winding.
            // Preserve J3D's GX_CULL_FRONT/BACK semantics by declaring GX faces CW.
            front_face: wgpu::FrontFace::Cw,
            cull_mode: wgpu_cull_mode(key.cull),
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(key.depth.write),
            depth_compare: Some(wgpu_depth_compare(key.depth.compare)),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn gpu_blend_state(key: GpuBlendKey) -> Option<wgpu::BlendState> {
    match key.mode {
        1 => Some(wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: gx_blend_factor(key.src_factor),
                dst_factor: gx_blend_factor(key.dst_factor),
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: gx_blend_factor(key.src_factor),
                dst_factor: gx_blend_factor(key.dst_factor),
                operation: wgpu::BlendOperation::Add,
            },
        }),
        3 => Some(wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::ReverseSubtract,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::ReverseSubtract,
            },
        }),
        _ => None,
    }
}

fn gx_blend_factor(factor: u8) -> wgpu::BlendFactor {
    match factor {
        0 => wgpu::BlendFactor::Zero,
        1 => wgpu::BlendFactor::One,
        2 => wgpu::BlendFactor::Src,
        3 => wgpu::BlendFactor::OneMinusSrc,
        4 => wgpu::BlendFactor::SrcAlpha,
        5 => wgpu::BlendFactor::OneMinusSrcAlpha,
        6 => wgpu::BlendFactor::DstAlpha,
        7 => wgpu::BlendFactor::OneMinusDstAlpha,
        _ => wgpu::BlendFactor::One,
    }
}

fn gpu_cull_mode(cull_mode: u8) -> GpuCullMode {
    match cull_mode {
        1 => GpuCullMode::Front,
        2 => GpuCullMode::Back,
        3 => GpuCullMode::All,
        _ => GpuCullMode::None,
    }
}

fn gx_compare_to_gpu(compare: u8) -> GpuDepthCompare {
    match compare {
        0 => GpuDepthCompare::Never,
        1 => GpuDepthCompare::Less,
        2 => GpuDepthCompare::Equal,
        3 => GpuDepthCompare::LessEqual,
        4 => GpuDepthCompare::Greater,
        5 => GpuDepthCompare::NotEqual,
        6 => GpuDepthCompare::GreaterEqual,
        7 => GpuDepthCompare::Always,
        _ => GpuDepthCompare::LessEqual,
    }
}

fn wgpu_depth_compare(compare: GpuDepthCompare) -> wgpu::CompareFunction {
    match compare {
        GpuDepthCompare::Never => wgpu::CompareFunction::Never,
        GpuDepthCompare::Less => wgpu::CompareFunction::Less,
        GpuDepthCompare::Equal => wgpu::CompareFunction::Equal,
        GpuDepthCompare::LessEqual => wgpu::CompareFunction::LessEqual,
        GpuDepthCompare::Greater => wgpu::CompareFunction::Greater,
        GpuDepthCompare::NotEqual => wgpu::CompareFunction::NotEqual,
        GpuDepthCompare::GreaterEqual => wgpu::CompareFunction::GreaterEqual,
        GpuDepthCompare::Always => wgpu::CompareFunction::Always,
    }
}

fn wgpu_cull_mode(cull_mode: GpuCullMode) -> Option<wgpu::Face> {
    match cull_mode {
        GpuCullMode::None | GpuCullMode::All => None,
        GpuCullMode::Front => Some(wgpu::Face::Front),
        GpuCullMode::Back => Some(wgpu::Face::Back),
    }
}

fn vec4(value: [f32; 3], w: f32) -> [f32; 4] {
    [value[0], value[1], value[2], w]
}

#[cfg(test)]
#[path = "gpu_viewport_tests.rs"]
mod tests;
