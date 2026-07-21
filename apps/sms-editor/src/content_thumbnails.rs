use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;

use sms_authoring::{AssetId, ModelAssetCatalog};
use sms_formats::{
    decode_bti_texture, decode_yaz0, BmpFile, GameFileId, GameFileMetadata, J3dFile, RarcArchive,
};

use super::*;

const MAX_THUMBNAILS: usize = 64;
const MAX_IN_FLIGHT: usize = 4;
const MODEL_THUMBNAIL_WIDTH: usize = 256;
const MODEL_THUMBNAIL_HEIGHT: usize = 144;
const MAX_MODEL_TRIANGLES: usize = 6_000;

pub(super) struct ContentThumbnailService {
    entries: BTreeMap<String, ThumbnailEntry>,
    sender: mpsc::Sender<ThumbnailMessage>,
    receiver: Receiver<ThumbnailMessage>,
    serial: u64,
}

enum ThumbnailEntry {
    Pending {
        touched: u64,
    },
    Ready {
        texture: egui::TextureHandle,
        touched: u64,
    },
    Failed {
        message: String,
        touched: u64,
    },
}

struct ThumbnailMessage {
    key: String,
    result: Result<ThumbnailPixels, String>,
}

struct ThumbnailPixels {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
}

impl Default for ContentThumbnailService {
    fn default() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            entries: BTreeMap::new(),
            sender,
            receiver,
            serial: 0,
        }
    }
}

impl ContentThumbnailService {
    fn request_raw(
        &mut self,
        key: String,
        base_root: PathBuf,
        metadata: GameFileMetadata,
        prioritized: bool,
    ) {
        self.request_job(key, prioritized, move || {
            decode_thumbnail(&base_root, &metadata)
        });
    }

    fn request_skybox(
        &mut self,
        key: String,
        base_root: PathBuf,
        metadata: GameFileMetadata,
        prioritized: bool,
    ) {
        self.request_job(key, prioritized, move || {
            decode_skybox_thumbnail(&base_root, &metadata)
        });
    }

    fn request_project_model(
        &mut self,
        key: String,
        content_root: PathBuf,
        asset_id: AssetId,
        prioritized: bool,
    ) {
        self.request_job(key, prioritized, move || {
            let catalog = ModelAssetCatalog::open_content_root(&content_root)
                .map_err(|error| error.to_string())?;
            let document = catalog
                .load_asset(asset_id)
                .map_err(|error| error.to_string())?;
            let geometry = build_authored_model_preview(&document, SMS_MAP_MODEL_LOAD_FLAGS)?;
            rasterize_model_thumbnail(&geometry.preview.triangles)
        });
    }

    fn request_job<F>(&mut self, key: String, prioritized: bool, job: F)
    where
        F: FnOnce() -> Result<ThumbnailPixels, String> + Send + 'static,
    {
        self.serial = self.serial.wrapping_add(1);
        if let Some(entry) = self.entries.get_mut(&key) {
            match entry {
                ThumbnailEntry::Pending { touched }
                | ThumbnailEntry::Ready { touched, .. }
                | ThumbnailEntry::Failed { touched, .. } => *touched = self.serial,
            }
            return;
        }
        let pending = self
            .entries
            .values()
            .filter(|entry| matches!(entry, ThumbnailEntry::Pending { .. }))
            .count();
        let limit = MAX_IN_FLIGHT + usize::from(prioritized);
        if pending >= limit {
            return;
        }
        self.evict_if_needed();
        self.entries.insert(
            key.clone(),
            ThumbnailEntry::Pending {
                touched: self.serial,
            },
        );
        let sender = self.sender.clone();
        thread::spawn(move || {
            let result = job();
            let _ = sender.send(ThumbnailMessage { key, result });
        });
    }
    fn evict_if_needed(&mut self) {
        while self.entries.len() >= MAX_THUMBNAILS {
            let candidate = self
                .entries
                .iter()
                .filter(|(_, entry)| !matches!(entry, ThumbnailEntry::Pending { .. }))
                .min_by_key(|(_, entry)| match entry {
                    ThumbnailEntry::Ready { touched, .. }
                    | ThumbnailEntry::Failed { touched, .. } => *touched,
                    ThumbnailEntry::Pending { touched } => *touched,
                })
                .map(|(key, _)| key.clone());
            let Some(candidate) = candidate else {
                break;
            };
            self.entries.remove(&candidate);
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(message) = self.receiver.try_recv() {
            self.serial = self.serial.wrapping_add(1);
            let entry = match message.result {
                Ok(pixels) => {
                    let image = egui::ColorImage::from_rgba_unmultiplied(
                        [pixels.width, pixels.height],
                        &pixels.rgba,
                    );
                    let texture = ctx.load_texture(
                        format!("content-thumbnail:{}", message.key),
                        image,
                        egui::TextureOptions::LINEAR,
                    );
                    ThumbnailEntry::Ready {
                        texture,
                        touched: self.serial,
                    }
                }
                Err(message) => ThumbnailEntry::Failed {
                    message,
                    touched: self.serial,
                },
            };
            self.entries.insert(message.key, entry);
            ctx.request_repaint();
        }
    }
}

impl SmsEditorApp {
    pub(super) fn poll_content_thumbnails(&mut self, ctx: &egui::Context) {
        self.content_thumbnails.poll(ctx);
    }

    pub(super) fn queue_raw_content_thumbnail(&mut self, metadata: GameFileMetadata) {
        self.queue_game_content_thumbnail(metadata, false);
    }

    pub(super) fn queue_selected_content_thumbnail(&mut self, metadata: GameFileMetadata) {
        self.queue_game_content_thumbnail(metadata, true);
    }

    pub(super) fn queue_skybox_content_thumbnail(
        &mut self,
        metadata: GameFileMetadata,
        prioritized: bool,
    ) -> String {
        let key = skybox_thumbnail_cache_key(&self.game_content_index.root_identity, &metadata);
        let base_root = self
            .current_project
            .as_ref()
            .map(|project| project.descriptor.base_game_root.clone())
            .unwrap_or_else(|| PathBuf::from(self.base_root.trim()));
        if !base_root.as_os_str().is_empty() {
            self.content_thumbnails
                .request_skybox(key.clone(), base_root, metadata, prioritized);
        }
        key
    }

    fn queue_game_content_thumbnail(&mut self, metadata: GameFileMetadata, prioritized: bool) {
        let key = thumbnail_cache_key(&self.game_content_index.root_identity, &metadata);
        let base_root = self
            .current_project
            .as_ref()
            .map(|project| project.descriptor.base_game_root.clone())
            .unwrap_or_else(|| PathBuf::from(self.base_root.trim()));
        if !base_root.as_os_str().is_empty() {
            self.content_thumbnails
                .request_raw(key, base_root, metadata, prioritized);
        }
    }

    pub(super) fn queue_project_model_thumbnail(
        &mut self,
        asset_id: AssetId,
        prioritized: bool,
    ) -> Option<String> {
        let content_root = self.model_content_root()?;
        let entry = self
            .model_catalog_entries
            .iter()
            .find(|entry| entry.id == asset_id)?;
        let key = project_model_thumbnail_key(&content_root, entry);
        self.content_thumbnails.request_project_model(
            key.clone(),
            content_root,
            asset_id,
            prioritized,
        );
        Some(key)
    }

    pub(super) fn content_thumbnail_texture_for_metadata(
        &mut self,
        metadata: &GameFileMetadata,
    ) -> Option<egui::TextureHandle> {
        let key = thumbnail_cache_key(&self.game_content_index.root_identity, metadata);
        self.content_thumbnail_texture_by_key(&key)
    }

    pub(super) fn content_thumbnail_texture_by_key(
        &mut self,
        key: &str,
    ) -> Option<egui::TextureHandle> {
        self.content_thumbnails.serial = self.content_thumbnails.serial.wrapping_add(1);
        let touched = self.content_thumbnails.serial;
        let ThumbnailEntry::Ready {
            texture,
            touched: entry_touched,
        } = self.content_thumbnails.entries.get_mut(key)?
        else {
            return None;
        };
        *entry_touched = touched;
        Some(texture.clone())
    }

    pub(super) fn request_raw_content_preview(&mut self, raw_id: &str) {
        let Some(metadata) = self
            .game_content_index
            .by_stable_id
            .get(raw_id)
            .and_then(|index| self.game_content_index.entries.get(*index))
            .cloned()
        else {
            self.log
                .push("The selected Game File is no longer indexed.".to_string());
            return;
        };
        let base_root = self
            .current_project
            .as_ref()
            .map(|project| project.descriptor.base_game_root.clone())
            .unwrap_or_else(|| PathBuf::from(self.base_root.trim()));
        if base_root.as_os_str().is_empty() {
            self.log
                .push("Choose a Sunshine base game before previewing content.".to_string());
            return;
        }
        let key = thumbnail_cache_key(&self.game_content_index.root_identity, &metadata);
        self.content_thumbnails
            .request_raw(key, base_root, metadata, true);
    }

    fn content_thumbnail_cache_key(&self, raw_id: &str) -> Option<String> {
        self.game_content_index
            .by_stable_id
            .get(raw_id)
            .and_then(|index| self.game_content_index.entries.get(*index))
            .map(|metadata| thumbnail_cache_key(&self.game_content_index.root_identity, metadata))
    }

    pub(super) fn raw_content_preview_panel(&mut self, ui: &mut egui::Ui, raw_id: &str) {
        let Some(key) = self.content_thumbnail_cache_key(raw_id) else {
            ui.small("This Game File is no longer present in the current index.");
            return;
        };
        self.content_thumbnails.serial = self.content_thumbnails.serial.wrapping_add(1);
        let touched = self.content_thumbnails.serial;
        let Some(entry) = self.content_thumbnails.entries.get_mut(&key) else {
            ui.small("Choose Preview to decode this asset without retaining retail payload bytes.");
            return;
        };
        match entry {
            ThumbnailEntry::Pending {
                touched: entry_touched,
            } => {
                *entry_touched = touched;
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.small("Decoding preview off the UI thread...");
                });
            }
            ThumbnailEntry::Ready {
                texture,
                touched: entry_touched,
            } => {
                *entry_touched = touched;
                let available = ui.available_width().max(64.0);
                let source = texture.size_vec2();
                let scale = (available / source.x).min(220.0 / source.y).min(1.0);
                ui.image((texture.id(), source * scale));
            }
            ThumbnailEntry::Failed {
                message,
                touched: entry_touched,
            } => {
                *entry_touched = touched;
                ui.colored_label(
                    egui::Color32::from_rgb(241, 126, 104),
                    format!("Preview unavailable: {message}"),
                );
            }
        }
    }
}

fn project_model_thumbnail_key(
    content_root: &Path,
    entry: &sms_authoring::CatalogAssetEntry,
) -> String {
    let manifest = content_root.join(&entry.relative_path);
    let modified = fs::metadata(&manifest)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos());
    format!(
        "project-model|{}|{}|{}",
        content_root.display(),
        entry.id,
        modified
    )
}

fn thumbnail_cache_key(root_identity: &str, metadata: &GameFileMetadata) -> String {
    format!(
        "{}|{}|{}|{}",
        root_identity,
        metadata.size_bytes,
        metadata.modified_unix_nanos.unwrap_or_default(),
        raw_game_file_id(&metadata.id)
    )
}

fn skybox_thumbnail_cache_key(root_identity: &str, metadata: &GameFileMetadata) -> String {
    format!(
        "skybox-texture|{}",
        thumbnail_cache_key(root_identity, metadata)
    )
}

fn decode_thumbnail(
    base_root: &Path,
    metadata: &GameFileMetadata,
) -> Result<ThumbnailPixels, String> {
    let bytes = read_game_file_bytes(base_root, metadata)?;
    let path = metadata.display_path.to_ascii_lowercase();
    if path.ends_with(".bti") {
        let preview = decode_bti_texture(&bytes).map_err(|error| error.to_string())?;
        return Ok(ThumbnailPixels {
            width: usize::from(preview.width),
            height: usize::from(preview.height),
            rgba: preview.rgba,
        });
    }
    if path.ends_with(".bmp") {
        return decode_bmp_thumbnail(&bytes);
    }
    if path.ends_with(".bmd") || path.ends_with(".bdl") {
        let file = J3dFile::parse(&bytes).map_err(|error| error.to_string())?;
        let geometry = file.geometry_preview().map_err(|error| error.to_string())?;
        return rasterize_model_thumbnail(&geometry.triangles);
    }
    Err("this resource type has no compatible preview decoder".to_string())
}

fn decode_skybox_thumbnail(
    base_root: &Path,
    metadata: &GameFileMetadata,
) -> Result<ThumbnailPixels, String> {
    let bytes = read_game_file_bytes(base_root, metadata)?;
    let file = J3dFile::parse(&bytes).map_err(|error| error.to_string())?;
    let geometry = file.geometry_preview().map_err(|error| error.to_string())?;
    rasterize_skybox_view(&geometry)
}

fn rasterize_skybox_view(
    geometry: &sms_formats::J3dGeometryPreview,
) -> Result<ThumbnailPixels, String> {
    if geometry.triangles.is_empty() {
        return Err("the skybox model contains no preview triangles".to_string());
    }
    let width = MODEL_THUMBNAIL_WIDTH;
    let height = MODEL_THUMBNAIL_HEIGHT;
    let mut rgba = vec![0_u8; width * height * 4];
    for (index, pixel) in rgba.chunks_exact_mut(4).enumerate() {
        let y = index / width;
        let t = y as f32 / height.saturating_sub(1).max(1) as f32;
        let top = [82.0, 137.0, 174.0];
        let bottom = [190.0, 206.0, 204.0];
        pixel.copy_from_slice(&[
            (top[0] + (bottom[0] - top[0]) * t) as u8,
            (top[1] + (bottom[1] - top[1]) * t) as u8,
            (top[2] + (bottom[2] - top[2]) * t) as u8,
            255,
        ]);
    }
    let mut depth = vec![f32::INFINITY; width * height];
    let step = geometry
        .triangles
        .len()
        .div_ceil(MAX_MODEL_TRIANGLES)
        .max(1);
    let triangles = geometry
        .triangles
        .iter()
        .step_by(step)
        .filter(|triangle| {
            triangle.texture_index.is_none()
                && (triangle.vertex_colors.is_some() || triangle.color.is_some())
        })
        .chain(
            geometry
                .triangles
                .iter()
                .step_by(step)
                .filter(|triangle| triangle.texture_index.is_some()),
        );
    for triangle in triangles {
        let texture = triangle
            .texture_index
            .and_then(|index| geometry.textures.get(index));
        if texture.is_none() && triangle.vertex_colors.is_none() && triangle.color.is_none() {
            continue;
        }
        let mut projected = [[0.0_f32; 3]; 3];
        let mut visible = true;
        for (index, vertex) in triangle.vertices.iter().enumerate() {
            if vertex[2] <= 1.0 {
                visible = false;
                break;
            }
            let aspect = width as f32 / height as f32;
            projected[index] = [
                (0.5 + vertex[0] / vertex[2] / (2.0 * aspect)) * width as f32,
                (0.5 - vertex[1] / vertex[2] / 2.0) * height as f32,
                if texture.is_some() { 0.0 } else { vertex[2] },
            ];
        }
        if !visible {
            continue;
        }
        rasterize_sky_triangle(
            &mut rgba, &mut depth, width, height, projected, triangle, texture,
        );
    }
    Ok(ThumbnailPixels {
        width,
        height,
        rgba,
    })
}

#[allow(clippy::too_many_arguments)]
fn rasterize_sky_triangle(
    rgba: &mut [u8],
    depth: &mut [f32],
    width: usize,
    height: usize,
    points: [[f32; 3]; 3],
    triangle: &sms_formats::J3dTriangle,
    texture: Option<&sms_formats::J3dTexturePreview>,
) {
    let min_x = points
        .iter()
        .map(|point| point[0].floor() as i32)
        .min()
        .unwrap_or_default()
        .clamp(0, width as i32 - 1);
    let max_x = points
        .iter()
        .map(|point| point[0].ceil() as i32)
        .max()
        .unwrap_or_default()
        .clamp(0, width as i32 - 1);
    let min_y = points
        .iter()
        .map(|point| point[1].floor() as i32)
        .min()
        .unwrap_or_default()
        .clamp(0, height as i32 - 1);
    let max_y = points
        .iter()
        .map(|point| point[1].ceil() as i32)
        .max()
        .unwrap_or_default()
        .clamp(0, height as i32 - 1);
    let area = edge(points[0], points[1], points[2][0], points[2][1]);
    if area.abs() < 0.0001 {
        return;
    }
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let sample_x = x as f32 + 0.5;
            let sample_y = y as f32 + 0.5;
            let w0 = edge(points[1], points[2], sample_x, sample_y) / area;
            let w1 = edge(points[2], points[0], sample_x, sample_y) / area;
            let w2 = 1.0 - w0 - w1;
            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                continue;
            }
            let radius = w0 * points[0][2] + w1 * points[1][2] + w2 * points[2][2];
            let pixel_index = y as usize * width + x as usize;
            if radius >= depth[pixel_index] {
                continue;
            }
            let sampled = skybox_fragment_color(triangle, texture, [w0, w1, w2]);
            let alpha = sampled[3];
            if alpha < 8 {
                continue;
            }
            depth[pixel_index] = radius;
            let target = pixel_index * 4;
            if alpha == 255 {
                rgba[target..target + 4].copy_from_slice(&sampled);
            } else {
                let alpha = u16::from(alpha);
                for channel in 0..3 {
                    rgba[target + channel] = ((u16::from(sampled[channel]) * alpha
                        + u16::from(rgba[target + channel]) * (255 - alpha))
                        / 255) as u8;
                }
                rgba[target + 3] = 255;
            }
        }
    }
}

fn skybox_fragment_color(
    triangle: &sms_formats::J3dTriangle,
    texture: Option<&sms_formats::J3dTexturePreview>,
    weights: [f32; 3],
) -> [u8; 4] {
    let interpolate = |colors: [[u8; 4]; 3]| {
        std::array::from_fn(|channel| {
            (weights[0] * f32::from(colors[0][channel])
                + weights[1] * f32::from(colors[1][channel])
                + weights[2] * f32::from(colors[2][channel]))
            .round()
            .clamp(0.0, 255.0) as u8
        })
    };
    let vertex = triangle.vertex_colors.map(interpolate).unwrap_or([255; 4]);
    let material = triangle.color.unwrap_or([255; 4]);
    let sampled = texture
        .zip(triangle.tex_coords)
        .map(|(texture, tex_coords)| {
            let uv = [
                weights[0] * tex_coords[0][0]
                    + weights[1] * tex_coords[1][0]
                    + weights[2] * tex_coords[2][0],
                weights[0] * tex_coords[0][1]
                    + weights[1] * tex_coords[1][1]
                    + weights[2] * tex_coords[2][1],
            ];
            sample_j3d_texture(texture, uv)
        })
        .unwrap_or([255; 4]);
    let modulate = |left: [u8; 4], right: [u8; 4], keep_left_alpha: bool| {
        let mut result = std::array::from_fn(|channel| {
            (u16::from(left[channel]) * u16::from(right[channel]) / 255) as u8
        });
        if keep_left_alpha {
            result[3] = left[3];
        }
        result
    };
    match triangle.combine_mode {
        sms_formats::J3dPreviewCombineMode::TextureOnly => sampled,
        sms_formats::J3dPreviewCombineMode::TextureModulateMaterial => {
            modulate(sampled, material, true)
        }
        sms_formats::J3dPreviewCombineMode::TextureModulateVertex => {
            modulate(sampled, vertex, false)
        }
        sms_formats::J3dPreviewCombineMode::MaterialOnly => material,
        sms_formats::J3dPreviewCombineMode::VertexOnly => vertex,
    }
}

fn sample_j3d_texture(texture: &sms_formats::J3dTexturePreview, uv: [f32; 2]) -> [u8; 4] {
    fn wrap(value: f32, mode: u8) -> f32 {
        match mode {
            1 => value.rem_euclid(1.0),
            2 => {
                let value = value.rem_euclid(2.0);
                if value > 1.0 {
                    2.0 - value
                } else {
                    value
                }
            }
            _ => value.clamp(0.0, 1.0),
        }
    }
    let width = usize::from(texture.width).max(1);
    let height = usize::from(texture.height).max(1);
    let x = (wrap(uv[0], texture.wrap_s) * (width - 1) as f32).round() as usize;
    let y = (wrap(uv[1], texture.wrap_t) * (height - 1) as f32).round() as usize;
    let offset = (y * width + x) * 4;
    texture
        .rgba
        .get(offset..offset + 4)
        .and_then(|pixel| <[u8; 4]>::try_from(pixel).ok())
        .unwrap_or([255, 0, 255, 255])
}

fn read_game_file_bytes(base_root: &Path, metadata: &GameFileMetadata) -> Result<Vec<u8>, String> {
    let physical_path = base_root.join(&metadata.physical_relative_path);
    let bytes = fs::read(&physical_path)
        .map_err(|error| format!("read '{}': {error}", physical_path.display()))?;
    let GameFileId::ArchiveEntry { raw_entry_path, .. } = &metadata.id else {
        return Ok(bytes);
    };
    let archive_bytes = if bytes.starts_with(b"Yaz0") {
        decode_yaz0(&bytes).map_err(|error| error.to_string())?
    } else {
        bytes
    };
    let archive = RarcArchive::parse(&archive_bytes).map_err(|error| error.to_string())?;
    archive
        .file_bytes_raw(raw_entry_path)
        .map_err(|error| error.to_string())
}

fn decode_bmp_thumbnail(bytes: &[u8]) -> Result<ThumbnailPixels, String> {
    let bitmap = BmpFile::parse(bytes).map_err(|error| error.to_string())?;
    let width = usize::try_from(bitmap.width).map_err(|_| "invalid BMP width".to_string())?;
    let height = usize::try_from(bitmap.height.unsigned_abs())
        .map_err(|_| "invalid BMP height".to_string())?;
    let stride = bitmap.row_stride().map_err(|error| error.to_string())?;
    let mut rgba = vec![0_u8; width * height * 4];
    for output_y in 0..height {
        let source_y = if bitmap.height > 0 {
            height - 1 - output_y
        } else {
            output_y
        };
        let source = &bitmap.encoded_pixels[source_y * stride..source_y * stride + width];
        for (x, palette_index) in source.iter().enumerate() {
            let palette = bitmap
                .palette
                .get(usize::from(*palette_index))
                .copied()
                .unwrap_or([0, 0, 0, 0]);
            let target = (output_y * width + x) * 4;
            rgba[target..target + 4].copy_from_slice(&[palette[2], palette[1], palette[0], 255]);
        }
    }
    Ok(ThumbnailPixels {
        width,
        height,
        rgba,
    })
}

fn rasterize_model_thumbnail(
    triangles: &[sms_formats::J3dTriangle],
) -> Result<ThumbnailPixels, String> {
    if triangles.is_empty() {
        return Err("the model contains no preview triangles".to_string());
    }
    let step = triangles.len().div_ceil(MAX_MODEL_TRIANGLES).max(1);
    let mut projected = Vec::new();
    for triangle in triangles.iter().step_by(step) {
        let mut points = [[0.0_f32; 3]; 3];
        for (index, vertex) in triangle.vertices.iter().enumerate() {
            points[index] = [
                0.707 * (vertex[0] - vertex[2]),
                -0.408 * (vertex[0] + vertex[2]) + 0.816 * vertex[1],
                0.577 * (vertex[0] + vertex[1] + vertex[2]),
            ];
        }
        let a = triangle.vertices[0];
        let b = triangle.vertices[1];
        let c = triangle.vertices[2];
        let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let normal = [
            ab[1] * ac[2] - ab[2] * ac[1],
            ab[2] * ac[0] - ab[0] * ac[2],
            ab[0] * ac[1] - ab[1] * ac[0],
        ];
        let length = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
        let light = if length > 0.0001 {
            ((normal[0] * 0.35 + normal[1] * 0.82 + normal[2] * 0.45) / length)
                .abs()
                .clamp(0.12, 1.0)
        } else {
            0.35
        };
        projected.push((points, light));
    }
    let mut min = [f32::INFINITY; 2];
    let mut max = [f32::NEG_INFINITY; 2];
    for (triangle, _) in &projected {
        for point in triangle {
            min[0] = min[0].min(point[0]);
            min[1] = min[1].min(point[1]);
            max[0] = max[0].max(point[0]);
            max[1] = max[1].max(point[1]);
        }
    }
    let span_x = (max[0] - min[0]).max(0.001);
    let span_y = (max[1] - min[1]).max(0.001);
    let scale = ((MODEL_THUMBNAIL_WIDTH as f32 - 28.0) / span_x)
        .min((MODEL_THUMBNAIL_HEIGHT as f32 - 28.0) / span_y);
    let mut rgba = vec![0_u8; MODEL_THUMBNAIL_WIDTH * MODEL_THUMBNAIL_HEIGHT * 4];
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.copy_from_slice(&[27, 34, 37, 255]);
    }
    let mut depth = vec![f32::NEG_INFINITY; MODEL_THUMBNAIL_WIDTH * MODEL_THUMBNAIL_HEIGHT];
    for (triangle, light) in projected {
        let points = triangle.map(|point| {
            [
                (point[0] - (min[0] + max[0]) * 0.5) * scale + MODEL_THUMBNAIL_WIDTH as f32 * 0.5,
                (point[1] - (min[1] + max[1]) * 0.5) * scale + MODEL_THUMBNAIL_HEIGHT as f32 * 0.5,
                point[2],
            ]
        });
        rasterize_triangle(&mut rgba, &mut depth, points, light);
    }
    Ok(ThumbnailPixels {
        width: MODEL_THUMBNAIL_WIDTH,
        height: MODEL_THUMBNAIL_HEIGHT,
        rgba,
    })
}

fn rasterize_triangle(rgba: &mut [u8], depth: &mut [f32], points: [[f32; 3]; 3], light: f32) {
    let min_x = points
        .iter()
        .map(|point| point[0].floor() as i32)
        .min()
        .unwrap_or_default()
        .clamp(0, MODEL_THUMBNAIL_WIDTH as i32 - 1);
    let max_x = points
        .iter()
        .map(|point| point[0].ceil() as i32)
        .max()
        .unwrap_or_default()
        .clamp(0, MODEL_THUMBNAIL_WIDTH as i32 - 1);
    let min_y = points
        .iter()
        .map(|point| point[1].floor() as i32)
        .min()
        .unwrap_or_default()
        .clamp(0, MODEL_THUMBNAIL_HEIGHT as i32 - 1);
    let max_y = points
        .iter()
        .map(|point| point[1].ceil() as i32)
        .max()
        .unwrap_or_default()
        .clamp(0, MODEL_THUMBNAIL_HEIGHT as i32 - 1);
    let area = edge(points[0], points[1], points[2][0], points[2][1]);
    if area.abs() < 0.0001 {
        return;
    }
    let color = [
        (58.0 + 42.0 * light) as u8,
        (111.0 + 91.0 * light) as u8,
        (119.0 + 96.0 * light) as u8,
        255,
    ];
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let sample_x = x as f32 + 0.5;
            let sample_y = y as f32 + 0.5;
            let w0 = edge(points[1], points[2], sample_x, sample_y) / area;
            let w1 = edge(points[2], points[0], sample_x, sample_y) / area;
            let w2 = 1.0 - w0 - w1;
            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                continue;
            }
            let sample_depth = w0 * points[0][2] + w1 * points[1][2] + w2 * points[2][2];
            let pixel_index = y as usize * MODEL_THUMBNAIL_WIDTH + x as usize;
            if sample_depth <= depth[pixel_index] {
                continue;
            }
            depth[pixel_index] = sample_depth;
            let rgba_index = pixel_index * 4;
            rgba[rgba_index..rgba_index + 4].copy_from_slice(&color);
        }
    }
}

fn edge(from: [f32; 3], to: [f32; 3], x: f32, y: f32) -> f32 {
    (x - from[0]) * (to[1] - from[1]) - (y - from[1]) * (to[0] - from[0])
}
#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(stamp: u128) -> GameFileMetadata {
        GameFileMetadata {
            id: GameFileId::Physical {
                relative_path: "files/test.bti".into(),
            },
            display_path: "files/test.bti".to_string(),
            physical_relative_path: "files/test.bti".into(),
            kind: sms_formats::GameResourceKind::Texture,
            size_bytes: 32,
            modified_unix_nanos: Some(stamp),
            archive_entry: None,
        }
    }

    #[test]
    fn thumbnail_keys_are_scoped_to_root_and_file_stamp() {
        let first = thumbnail_cache_key("root-a", &metadata(1));
        assert_ne!(first, thumbnail_cache_key("root-b", &metadata(1)));
        assert_ne!(first, thumbnail_cache_key("root-a", &metadata(2)));
    }

    #[test]
    fn selected_request_gets_one_priority_slot() {
        let mut service = ContentThumbnailService::default();
        for index in 0..MAX_IN_FLIGHT {
            service.entries.insert(
                format!("visible-{index}"),
                ThumbnailEntry::Pending { touched: 0 },
            );
        }
        service.request_job("normal".to_string(), false, || {
            Err("not expected to run".to_string())
        });
        assert!(!service.entries.contains_key("normal"));

        service.request_job("selected".to_string(), true, || {
            Err("test preview".to_string())
        });
        assert!(service.entries.contains_key("selected"));
        service.request_job("selected-two".to_string(), true, || {
            Err("not expected to run".to_string())
        });
        assert!(!service.entries.contains_key("selected-two"));
    }

    #[test]
    fn model_thumbnail_uses_filled_surfaces_instead_of_wireframe_edges() {
        let triangle = sms_formats::J3dTriangle {
            vertices: [[-1.0, -1.0, 0.0], [1.0, -1.0, 0.0], [0.0, 1.0, 0.0]],
            normals: None,
            color_channels: [None; 2],
            tex_coord_sets: [None; 8],
            material_index: None,
            shape_index: 0,
            packet_index: 0,
            color: None,
            vertex_colors: None,
            combine_mode: sms_formats::J3dPreviewCombineMode::MaterialOnly,
            tex_coords: None,
            texture_index: None,
            mask_tex_coords: None,
            mask_texture_index: None,
            cull_mode: None,
            alpha_compare: None,
            blend_mode: None,
            z_mode: None,
            z_comp_loc: None,
            billboard: None,
        };
        let image = rasterize_model_thumbnail(&[triangle]).unwrap();
        let filled_pixels = image
            .rgba
            .chunks_exact(4)
            .filter(|pixel| *pixel != [27, 34, 37, 255])
            .count();
        assert!(
            filled_pixels > 1_000,
            "only {filled_pixels} pixels were filled"
        );
    }

    #[test]
    fn skybox_fragment_preserves_interpolated_vertex_color() {
        let triangle = sms_formats::J3dTriangle {
            vertices: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            normals: None,
            color_channels: [None; 2],
            tex_coord_sets: [None; 8],
            material_index: None,
            shape_index: 0,
            packet_index: 0,
            color: Some([128, 128, 128, 50]),
            vertex_colors: Some([[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255]]),
            combine_mode: sms_formats::J3dPreviewCombineMode::VertexOnly,
            tex_coords: None,
            texture_index: None,
            mask_tex_coords: None,
            mask_texture_index: None,
            cull_mode: None,
            alpha_compare: None,
            blend_mode: None,
            z_mode: None,
            z_comp_loc: None,
            billboard: None,
        };
        assert_eq!(
            skybox_fragment_color(&triangle, None, [0.25, 0.5, 0.25]),
            [64, 128, 64, 255]
        );
    }

    #[test]
    #[ignore = "requires an extracted retail Sunshine base in SMS_BASE_ROOT"]
    fn renders_retail_skybox_vertex_colors_and_textures() {
        let root = PathBuf::from(std::env::var_os("SMS_BASE_ROOT").expect("SMS_BASE_ROOT"));
        let relative_path = PathBuf::from("files/data/scene/bianco0.szs");
        let metadata = GameFileMetadata {
            id: GameFileId::ArchiveEntry {
                archive_relative_path: relative_path.clone(),
                raw_entry_path: b"map/map/sky.bmd".to_vec(),
            },
            display_path: "files/data/scene/bianco0.szs!/map/map/sky.bmd".to_string(),
            physical_relative_path: relative_path.clone(),
            kind: sms_formats::GameResourceKind::Model,
            size_bytes: fs::metadata(root.join(&relative_path)).unwrap().len(),
            modified_unix_nanos: None,
            archive_entry: None,
        };
        let image = decode_skybox_thumbnail(&root, &metadata).unwrap();
        let represented = image
            .rgba
            .chunks_exact(4)
            .enumerate()
            .filter(|(index, pixel)| {
                let y = index / image.width;
                let t = y as f32 / image.height.saturating_sub(1).max(1) as f32;
                let expected = [
                    (82.0 + (190.0 - 82.0) * t) as u8,
                    (137.0 + (206.0 - 137.0) * t) as u8,
                    (174.0 + (204.0 - 174.0) * t) as u8,
                    255,
                ];
                **pixel != expected
            })
            .count();
        if let Some(output) = std::env::var_os("SKYBOX_THUMBNAIL_OUT") {
            fs::write(output, &image.rgba).unwrap();
        }
        assert!(represented > image.width * image.height / 3);
    }
}
