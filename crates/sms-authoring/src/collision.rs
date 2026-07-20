use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sms_formats::{ColFile, ColGroup, ColTriangle, ColVertex};

use crate::math::{cross, sub};
use crate::{
    AuthoringError, AuthoringResult, CollisionDocument, CollisionGroup, Diagnostic, DiagnosticCode,
};

const MAX_RUNTIME_VERTEX_COUNT: usize = i16::MAX as usize + 1;
const MAX_GROUP_TRIANGLES: usize = i16::MAX as usize;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollisionCleanupReport {
    pub input_vertices: usize,
    pub output_vertices: usize,
    pub welded_vertices: usize,
    pub removed_unused_vertices: usize,
    pub removed_degenerate_triangles: usize,
    pub removed_duplicate_triangles: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollisionImportResult {
    pub collision: CollisionDocument,
    pub diagnostics: Vec<Diagnostic>,
    pub cleanup: CollisionCleanupReport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simplification: Option<CollisionSimplificationReport>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct CollisionSimplificationReport {
    pub input_triangles: usize,
    pub target_triangles: usize,
    pub output_triangles: usize,
    pub collapsed_edges: usize,
    pub maximum_applied_error: f32,
    pub stopped_at_error_limit: bool,
}

impl CollisionDocument {
    pub fn validate(&self) -> AuthoringResult<()> {
        if self
            .vertices
            .iter()
            .flatten()
            .any(|value| !value.is_finite())
        {
            return Err(AuthoringError::Collision(
                "collision contains a non-finite position".to_string(),
            ));
        }
        for (group_index, group) in self.groups.iter().enumerate() {
            for (triangle_index, triangle) in group.triangles.iter().enumerate() {
                if triangle
                    .iter()
                    .any(|&index| index as usize >= self.vertices.len())
                {
                    return Err(AuthoringError::Collision(format!(
                        "group {group_index} triangle {triangle_index} has an out-of-range vertex"
                    )));
                }
            }
        }
        Ok(())
    }

    /// Welds bit-identical positions and removes zero-area and duplicate faces.
    /// The first appearance of every position and triangle wins, which makes
    /// the result deterministic for a fixed source document.
    pub fn cleanup_exact(&mut self) -> AuthoringResult<CollisionCleanupReport> {
        self.validate()?;
        let input_vertices = self.vertices.len();
        let mut vertex_map = BTreeMap::<[u32; 3], u32>::new();
        let mut remap = Vec::with_capacity(self.vertices.len());
        let mut vertices = Vec::new();
        for position in &self.vertices {
            let key = position.map(|component| {
                if component == 0.0 {
                    0.0f32.to_bits()
                } else {
                    component.to_bits()
                }
            });
            let next_index = u32::try_from(vertices.len()).map_err(|_| {
                AuthoringError::Collision("collision vertex count exceeds u32".to_string())
            })?;
            let index = *vertex_map.entry(key).or_insert_with(|| {
                vertices.push(*position);
                next_index
            });
            remap.push(index);
        }

        let mut removed_degenerate_triangles = 0;
        let mut removed_duplicate_triangles = 0;
        for group in &mut self.groups {
            let mut seen = BTreeSet::new();
            group.triangles.retain_mut(|triangle| {
                *triangle = triangle.map(|index| remap[index as usize]);
                if triangle[0] == triangle[1]
                    || triangle[1] == triangle[2]
                    || triangle[2] == triangle[0]
                    || is_zero_area(*triangle, &vertices)
                {
                    removed_degenerate_triangles += 1;
                    return false;
                }
                let mut identity = *triangle;
                identity.sort_unstable();
                if !seen.insert(identity) {
                    removed_duplicate_triangles += 1;
                    return false;
                }
                true
            });
        }
        let unique_vertex_count = vertices.len();
        let mut used = vec![false; vertices.len()];
        for group in &self.groups {
            for triangle in &group.triangles {
                for &index in triangle {
                    used[index as usize] = true;
                }
            }
        }
        let mut compact_remap = vec![0u32; vertices.len()];
        let mut compact_vertices = Vec::new();
        for (index, position) in vertices.into_iter().enumerate() {
            if used[index] {
                compact_remap[index] = compact_vertices.len() as u32;
                compact_vertices.push(position);
            }
        }
        for group in &mut self.groups {
            for triangle in &mut group.triangles {
                *triangle = triangle.map(|index| compact_remap[index as usize]);
            }
        }
        self.vertices = compact_vertices;
        let output_vertices = self.vertices.len();
        Ok(CollisionCleanupReport {
            input_vertices,
            output_vertices,
            welded_vertices: input_vertices - unique_vertex_count,
            removed_unused_vertices: unique_vertex_count - output_vertices,
            removed_degenerate_triangles,
            removed_duplicate_triangles,
        })
    }

    pub fn to_col_file(&self) -> AuthoringResult<ColFile> {
        self.validate()?;
        if self.vertices.len() > MAX_RUNTIME_VERTEX_COUNT {
            return Err(AuthoringError::Collision(format!(
                "{} vertices exceed the retail signed-index limit of {MAX_RUNTIME_VERTEX_COUNT}",
                self.vertices.len()
            )));
        }
        let vertices = self
            .vertices
            .iter()
            .copied()
            .map(ColVertex::from)
            .collect::<Vec<_>>();
        let mut groups = Vec::new();
        for group in &self.groups {
            let chunks = if group.triangles.is_empty() {
                vec![&[][..]]
            } else {
                group
                    .triangles
                    .chunks(MAX_GROUP_TRIANGLES)
                    .collect::<Vec<_>>()
            };
            for triangles in chunks {
                let has_per_triangle_data = group.surface.data.is_some();
                let triangles = triangles
                    .iter()
                    .map(|triangle| {
                        let vertex_indices = triangle.map(|index| {
                            u16::try_from(index)
                                .expect("runtime signed-index validation also guarantees u16")
                        });
                        ColTriangle {
                            vertex_indices,
                            attribute_0: group.surface.attribute_0,
                            attribute_1: group.surface.attribute_1,
                            data: group.surface.data,
                        }
                    })
                    .collect();
                groups.push(ColGroup {
                    surface_type: group.surface.surface_type,
                    has_per_triangle_data,
                    triangles,
                });
            }
        }
        Ok(ColFile::new(vertices, groups))
    }

    pub fn to_col_bytes(&self) -> AuthoringResult<Vec<u8>> {
        self.to_col_file()?
            .encode()
            .map_err(|error| AuthoringError::Collision(error.to_string()))
    }

    /// Reduces triangle count with deterministic quadric-error edge collapses.
    /// Edges whose vertices participate in different surface groups are never
    /// collapsed, preserving authored surface boundaries.
    pub fn simplify(
        &mut self,
        options: &crate::CollisionSimplificationOptions,
    ) -> AuthoringResult<CollisionSimplificationReport> {
        if !options.target_ratio.is_finite()
            || options.target_ratio <= 0.0
            || options.target_ratio > 1.0
        {
            return Err(AuthoringError::Collision(
                "simplification target_ratio must be finite and in (0, 1]".to_string(),
            ));
        }
        if !options.max_error.is_finite() || options.max_error < 0.0 {
            return Err(AuthoringError::Collision(
                "simplification max_error must be finite and non-negative".to_string(),
            ));
        }
        self.cleanup_exact()?;
        let input_triangles = triangle_count(self);
        let target_triangles = ((input_triangles as f64 * f64::from(options.target_ratio)).ceil()
            as usize)
            .min(input_triangles);
        let mut report = CollisionSimplificationReport {
            input_triangles,
            target_triangles,
            output_triangles: input_triangles,
            ..CollisionSimplificationReport::default()
        };
        while triangle_count(self) > target_triangles {
            let quadrics = build_vertex_quadrics(self);
            let incident_groups = vertex_incident_groups(self);
            let mut candidates = build_edge_candidates(self, &quadrics, &incident_groups);
            candidates.sort_by(|left, right| {
                left.error
                    .total_cmp(&right.error)
                    .then_with(|| left.edge.cmp(&right.edge))
            });
            let Some(candidate) = candidates
                .into_iter()
                .find(|candidate| collapse_preserves_winding(self, candidate))
            else {
                break;
            };
            if candidate.error > options.max_error {
                report.stopped_at_error_limit = true;
                break;
            }
            collapse_edge(self, candidate);
            report.collapsed_edges += 1;
            report.maximum_applied_error = report.maximum_applied_error.max(candidate.error);
        }
        self.cleanup_exact()?;
        report.output_triangles = triangle_count(self);
        Ok(report)
    }
}

#[derive(Clone, Copy, Default)]
struct Quadric {
    // Symmetric 4x4 matrix: xx, xy, xz, xw, yy, yz, yw, zz, zw, ww.
    values: [f64; 10],
}

impl Quadric {
    fn from_plane(plane: [f64; 4]) -> Self {
        let [x, y, z, w] = plane;
        Self {
            values: [
                x * x,
                x * y,
                x * z,
                x * w,
                y * y,
                y * z,
                y * w,
                z * z,
                z * w,
                w * w,
            ],
        }
    }

    fn add(self, other: Self) -> Self {
        let mut result = self;
        for (value, other) in result.values.iter_mut().zip(other.values) {
            *value += other;
        }
        result
    }

    fn error(self, point: [f32; 3]) -> f64 {
        let [x, y, z] = point.map(f64::from);
        let q = self.values;
        q[0] * x * x
            + 2.0 * q[1] * x * y
            + 2.0 * q[2] * x * z
            + 2.0 * q[3] * x
            + q[4] * y * y
            + 2.0 * q[5] * y * z
            + 2.0 * q[6] * y
            + q[7] * z * z
            + 2.0 * q[8] * z
            + q[9]
    }

    fn optimal(self, first: [f32; 3], second: [f32; 3]) -> ([f32; 3], f32) {
        let q = self.values;
        let matrix = [[q[0], q[1], q[2]], [q[1], q[4], q[5]], [q[2], q[5], q[7]]];
        let rhs = [-q[3], -q[6], -q[8]];
        let mut points = vec![first, second, midpoint(first, second)];
        if let Some(point) = solve3(matrix, rhs) {
            if point.iter().all(|value| value.is_finite())
                && point
                    .iter()
                    .all(|value| *value >= f64::from(f32::MIN) && *value <= f64::from(f32::MAX))
            {
                points.push(point.map(|value| value as f32));
            }
        }
        points
            .into_iter()
            .map(|point| (point, self.error(point).max(0.0) as f32))
            .min_by(|left, right| {
                left.1
                    .total_cmp(&right.1)
                    .then_with(|| point_bits(left.0).cmp(&point_bits(right.0)))
            })
            .expect("at least the two edge endpoints are candidates")
    }
}

#[derive(Clone, Copy)]
struct EdgeCandidate {
    edge: [u32; 2],
    position: [f32; 3],
    error: f32,
}

fn build_vertex_quadrics(document: &CollisionDocument) -> Vec<Quadric> {
    let mut quadrics = vec![Quadric::default(); document.vertices.len()];
    for group in &document.groups {
        for triangle in &group.triangles {
            let a = document.vertices[triangle[0] as usize];
            let b = document.vertices[triangle[1] as usize];
            let c = document.vertices[triangle[2] as usize];
            let normal = cross(sub(b, a), sub(c, a));
            let length = f64::from(normal[0])
                .hypot(f64::from(normal[1]))
                .hypot(f64::from(normal[2]));
            if length == 0.0 {
                continue;
            }
            let plane = [
                f64::from(normal[0]) / length,
                f64::from(normal[1]) / length,
                f64::from(normal[2]) / length,
                -(f64::from(normal[0]) * f64::from(a[0])
                    + f64::from(normal[1]) * f64::from(a[1])
                    + f64::from(normal[2]) * f64::from(a[2]))
                    / length,
            ];
            let face = Quadric::from_plane(plane);
            for &vertex in triangle {
                quadrics[vertex as usize] = quadrics[vertex as usize].add(face);
            }
        }
    }
    quadrics
}

fn vertex_incident_groups(document: &CollisionDocument) -> Vec<BTreeSet<usize>> {
    let mut groups = vec![BTreeSet::new(); document.vertices.len()];
    for (group_index, group) in document.groups.iter().enumerate() {
        for triangle in &group.triangles {
            for &vertex in triangle {
                groups[vertex as usize].insert(group_index);
            }
        }
    }
    groups
}

fn build_edge_candidates(
    document: &CollisionDocument,
    quadrics: &[Quadric],
    incident_groups: &[BTreeSet<usize>],
) -> Vec<EdgeCandidate> {
    let mut edges = BTreeSet::new();
    for group in &document.groups {
        for triangle in &group.triangles {
            for pair in [
                [triangle[0], triangle[1]],
                [triangle[1], triangle[2]],
                [triangle[2], triangle[0]],
            ] {
                let edge = if pair[0] < pair[1] {
                    pair
                } else {
                    [pair[1], pair[0]]
                };
                if incident_groups[edge[0] as usize] == incident_groups[edge[1] as usize]
                    && incident_groups[edge[0] as usize].len() == 1
                {
                    edges.insert(edge);
                }
            }
        }
    }
    edges
        .into_iter()
        .map(|edge| {
            let quadric = quadrics[edge[0] as usize].add(quadrics[edge[1] as usize]);
            let (position, error) = quadric.optimal(
                document.vertices[edge[0] as usize],
                document.vertices[edge[1] as usize],
            );
            EdgeCandidate {
                edge,
                position,
                error,
            }
        })
        .collect()
}

fn collapse_preserves_winding(document: &CollisionDocument, candidate: &EdgeCandidate) -> bool {
    for group in &document.groups {
        for triangle in &group.triangles {
            if !triangle.contains(&candidate.edge[0]) && !triangle.contains(&candidate.edge[1]) {
                continue;
            }
            if triangle.contains(&candidate.edge[0]) && triangle.contains(&candidate.edge[1]) {
                continue;
            }
            let old = triangle.map(|index| document.vertices[index as usize]);
            let new = triangle.map(|index| {
                if candidate.edge.contains(&index) {
                    candidate.position
                } else {
                    document.vertices[index as usize]
                }
            });
            let old_normal = cross(sub(old[1], old[0]), sub(old[2], old[0]));
            let new_normal = cross(sub(new[1], new[0]), sub(new[2], new[0]));
            let dot = old_normal[0] * new_normal[0]
                + old_normal[1] * new_normal[1]
                + old_normal[2] * new_normal[2];
            if dot <= 0.0 || !dot.is_finite() {
                return false;
            }
        }
    }
    true
}

fn collapse_edge(document: &mut CollisionDocument, candidate: EdgeCandidate) {
    document.vertices[candidate.edge[0] as usize] = candidate.position;
    for group in &mut document.groups {
        for triangle in &mut group.triangles {
            for index in triangle {
                if *index == candidate.edge[1] {
                    *index = candidate.edge[0];
                }
            }
        }
        group.triangles.retain(|triangle| {
            triangle[0] != triangle[1] && triangle[1] != triangle[2] && triangle[2] != triangle[0]
        });
    }
}

fn solve3(mut matrix: [[f64; 3]; 3], mut rhs: [f64; 3]) -> Option<[f64; 3]> {
    for pivot in 0..3 {
        let best = (pivot..3).max_by(|&left, &right| {
            matrix[left][pivot]
                .abs()
                .total_cmp(&matrix[right][pivot].abs())
        })?;
        if matrix[best][pivot].abs() <= f64::EPSILON {
            return None;
        }
        matrix.swap(pivot, best);
        rhs.swap(pivot, best);
        let pivot_row = matrix[pivot];
        for row in pivot + 1..3 {
            let factor = matrix[row][pivot] / matrix[pivot][pivot];
            for (column, value) in matrix[row].iter_mut().enumerate().skip(pivot) {
                *value -= factor * pivot_row[column];
            }
            rhs[row] -= factor * rhs[pivot];
        }
    }
    let mut result = [0.0; 3];
    for row in (0..3).rev() {
        result[row] = (rhs[row]
            - (row + 1..3)
                .map(|column| matrix[row][column] * result[column])
                .sum::<f64>())
            / matrix[row][row];
    }
    Some(result)
}

fn midpoint(first: [f32; 3], second: [f32; 3]) -> [f32; 3] {
    [
        (first[0] + second[0]) * 0.5,
        (first[1] + second[1]) * 0.5,
        (first[2] + second[2]) * 0.5,
    ]
}

fn point_bits(point: [f32; 3]) -> [u32; 3] {
    point.map(f32::to_bits)
}

fn triangle_count(document: &CollisionDocument) -> usize {
    document
        .groups
        .iter()
        .map(|group| group.triangles.len())
        .sum()
}

pub(crate) fn cleanup_diagnostics(report: CollisionCleanupReport) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    if report.welded_vertices != 0 {
        diagnostics.push(Diagnostic::warning(
            DiagnosticCode::CollisionVertexWelded,
            format!(
                "welded {} duplicate collision vertices",
                report.welded_vertices
            ),
            None,
            false,
        ));
    }
    if report.removed_unused_vertices != 0 {
        diagnostics.push(Diagnostic::warning(
            DiagnosticCode::CollisionUnusedVertexRemoved,
            format!(
                "removed {} unreferenced collision vertices",
                report.removed_unused_vertices
            ),
            None,
            false,
        ));
    }
    if report.removed_degenerate_triangles != 0 {
        diagnostics.push(Diagnostic::warning(
            DiagnosticCode::CollisionDegenerateRemoved,
            format!(
                "removed {} zero-area collision triangles",
                report.removed_degenerate_triangles
            ),
            None,
            false,
        ));
    }
    if report.removed_duplicate_triangles != 0 {
        diagnostics.push(Diagnostic::warning(
            DiagnosticCode::CollisionDuplicateRemoved,
            format!(
                "removed {} duplicate collision triangles",
                report.removed_duplicate_triangles
            ),
            None,
            false,
        ));
    }
    diagnostics
}

fn is_zero_area(triangle: [u32; 3], vertices: &[[f32; 3]]) -> bool {
    let a = vertices[triangle[0] as usize];
    let b = vertices[triangle[1] as usize];
    let c = vertices[triangle[2] as usize];
    let normal = cross(sub(b, a), sub(c, a));
    normal == [0.0, 0.0, 0.0]
}

pub(crate) fn collision_group(
    name: String,
    surface: crate::CollisionSurface,
    triangles: Vec<[u32; 3]>,
) -> CollisionGroup {
    CollisionGroup {
        name,
        surface,
        triangles,
    }
}
