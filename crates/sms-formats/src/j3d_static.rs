//! Canonical eight-section BMD3 construction for rigid static meshes.

use encoding_rs::SHIFT_JIS;
use serde::{Deserialize, Serialize};

use crate::{
    compile_material_section, compile_texture_section, FormatError, GxEncodedTexture, GxMaterial,
    J3dDrawMatrixSection, J3dEnvelopeSection, J3dGxCommand, J3dGxVertexOperand,
    J3dHierarchyCommand, J3dInformationSection, J3dJointRecord, J3dJointSection, J3dNameEntry,
    J3dNameTable, J3dRebuildDocument, J3dRebuildSection, J3dRebuildSectionData, J3dScalarArray,
    J3dShapeDraw, J3dShapeMatrixGroup, J3dShapeRecord, J3dShapeSection, J3dVertexArray,
    J3dVertexArrayAttribute, J3dVertexAttributeFormat, J3dVertexDescriptor, J3dVertexDescriptorSet,
    J3dVertexSection, Result,
};

const FORMAT: &str = "static BMD3 authoring";
const CANONICAL_SECTION_TAGS: [[u8; 4]; 8] = [
    *b"INF1", *b"VTX1", *b"EVP1", *b"DRW1", *b"JNT1", *b"SHP1", *b"MAT3", *b"TEX1",
];

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct StaticModelVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    /// Optional GX_NBT triplet in normal, binormal, tangent order.
    pub normal_binormal_tangent: Option<[[f32; 3]; 3]>,
    pub colors: [Option<[u8; 4]>; 2],
    pub tex_coords: [Option<[f32; 2]>; 8],
}

impl StaticModelVertex {
    pub fn new(position: [f32; 3], normal: [f32; 3]) -> Self {
        Self {
            position,
            normal,
            normal_binormal_tangent: None,
            colors: [None; 2],
            tex_coords: [None; 8],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StaticModelMesh {
    pub name: String,
    pub material_index: u16,
    pub vertices: Vec<StaticModelVertex>,
    pub triangles: Vec<[u32; 3]>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StaticModel {
    pub root_joint_name: String,
    pub meshes: Vec<StaticModelMesh>,
    pub materials: Vec<GxMaterial>,
    pub textures: Vec<GxEncodedTexture>,
}

/// Compiles an authoring model into a canonical, source-free J3D2 `bmd3`.
pub fn compile_static_bmd3(model: &StaticModel) -> Result<J3dRebuildDocument> {
    validate_static_model(model)?;
    // J3DMaterial is also the runtime hierarchy node that owns one shape and
    // one `next` link. Reusing a MAT3 material index for multiple SHP1 shapes
    // makes J3DJoint::addMesh link that material to itself, so Sunshine loops
    // forever while walking the joint's material list. Keep authored material
    // state shared in StaticModel, but instantiate one runtime material per
    // shape in the compiled BMD.
    let runtime_materials = model
        .meshes
        .iter()
        .map(|mesh| model.materials[mesh.material_index as usize].clone())
        .collect::<Vec<_>>();
    let attributes = used_attributes(model);
    let mut global_vertices = Vec::new();
    let mut mesh_ranges = Vec::with_capacity(model.meshes.len());
    for mesh in &model.meshes {
        let start = global_vertices.len();
        global_vertices.extend_from_slice(&mesh.vertices);
        mesh_ranges.push(start);
    }
    let bounds = bounds(global_vertices.iter().map(|vertex| vertex.position))?;

    let information = information_section(model)?;
    let vertices = vertex_section(&global_vertices, &attributes);
    let envelopes = empty_envelope_section();
    let draw_matrices = rigid_draw_matrix_section();
    let joints = joint_section(&model.root_joint_name, bounds)?;
    let shapes = shape_section(model, &mesh_ranges, &attributes)?;
    let materials = compile_material_section(&runtime_materials)?;
    let textures = compile_texture_section(&model.textures)?;

    let mut document = J3dRebuildDocument {
        file_type: *b"bmd3",
        version_tag: *b"SVR3",
        reserved_words: [u32::MAX; 3],
        declared_section_count: 8,
        sections: vec![
            information,
            vertices,
            envelopes,
            draw_matrices,
            joints,
            shapes,
            materials,
            textures,
        ],
    };
    document.canonicalize_geometry_layout()?;
    validate_canonical_bmd3(&document)?;
    Ok(document)
}

pub fn validate_canonical_bmd3(document: &J3dRebuildDocument) -> Result<()> {
    if document.file_type != *b"bmd3" {
        return Err(unsupported(format!(
            "expected bmd3 file type, found {:?}",
            String::from_utf8_lossy(&document.file_type)
        )));
    }
    if document.declared_section_count != 8 || document.sections.len() != 8 {
        return Err(unsupported(format!(
            "canonical BMD3 requires eight sections; declared {}, stored {}",
            document.declared_section_count,
            document.sections.len()
        )));
    }
    for (index, (section, expected)) in document
        .sections
        .iter()
        .zip(CANONICAL_SECTION_TAGS)
        .enumerate()
    {
        if section.tag() != expected {
            return Err(unsupported(format!(
                "section {index} is {:?}, expected {:?}",
                String::from_utf8_lossy(&section.tag()),
                String::from_utf8_lossy(&expected)
            )));
        }
        if section.declared_size < 8 || !section.declared_size.is_multiple_of(0x20) {
            return Err(unsupported(format!(
                "section {index} size {:#x} is not a nonempty 32-byte allocation",
                section.declared_size
            )));
        }
    }
    let bytes = document.to_bytes()?;
    let reopened = J3dRebuildDocument::parse(&bytes)?;
    if reopened.to_bytes()? != bytes {
        return Err(unsupported(
            "canonical BMD3 is not stable through source-free parse/encode",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct UsedAttributes {
    normal_binormal_tangent: bool,
    colors: [bool; 2],
    tex_coords: [bool; 8],
}

fn used_attributes(model: &StaticModel) -> UsedAttributes {
    let mut used = UsedAttributes {
        normal_binormal_tangent: false,
        colors: [false; 2],
        tex_coords: [false; 8],
    };
    for vertex in model.meshes.iter().flat_map(|mesh| mesh.vertices.iter()) {
        used.normal_binormal_tangent |= vertex.normal_binormal_tangent.is_some();
        for (slot, color) in vertex.colors.iter().enumerate() {
            used.colors[slot] |= color.is_some();
        }
        for (slot, coord) in vertex.tex_coords.iter().enumerate() {
            used.tex_coords[slot] |= coord.is_some();
        }
    }
    used
}

fn validate_static_model(model: &StaticModel) -> Result<()> {
    validate_name("root joint", &model.root_joint_name)?;
    if model.meshes.is_empty() {
        return Err(unsupported("static BMD3 needs at least one mesh"));
    }
    if model.meshes.len() > u16::MAX as usize {
        return Err(limit("meshes", model.meshes.len(), u16::MAX as usize));
    }
    if model.materials.is_empty() {
        return Err(unsupported("static BMD3 needs at least one material"));
    }
    let total_vertices = model
        .meshes
        .iter()
        .try_fold(0usize, |count, mesh| count.checked_add(mesh.vertices.len()))
        .ok_or_else(|| limit("vertices", usize::MAX, u16::MAX as usize + 1))?;
    if total_vertices > u16::MAX as usize + 1 {
        return Err(limit(
            "vertices addressable by GX index16",
            total_vertices,
            u16::MAX as usize + 1,
        ));
    }
    for (mesh_index, mesh) in model.meshes.iter().enumerate() {
        validate_name("shape", &mesh.name)?;
        if mesh.material_index as usize >= model.materials.len() {
            return Err(unsupported(format!(
                "mesh {mesh_index} references material {}, but only {} exist",
                mesh.material_index,
                model.materials.len()
            )));
        }
        if mesh.vertices.is_empty() || mesh.triangles.is_empty() {
            return Err(unsupported(format!(
                "mesh {mesh_index} must contain vertices and triangles"
            )));
        }
        let operand_count = mesh
            .triangles
            .len()
            .checked_mul(3)
            .ok_or_else(|| limit("triangle operands", usize::MAX, u16::MAX as usize))?;
        if operand_count > u16::MAX as usize {
            return Err(limit(
                "triangle operands in one GX primitive",
                operand_count,
                u16::MAX as usize,
            ));
        }
        for (vertex_index, vertex) in mesh.vertices.iter().enumerate() {
            if vertex
                .position
                .into_iter()
                .chain(vertex.normal)
                .chain(
                    vertex
                        .normal_binormal_tangent
                        .iter()
                        .flatten()
                        .flatten()
                        .copied(),
                )
                .chain(vertex.tex_coords.iter().flatten().flatten().copied())
                .any(|value| !value.is_finite())
            {
                return Err(unsupported(format!(
                    "mesh {mesh_index} vertex {vertex_index} contains a non-finite component"
                )));
            }
        }
        for (triangle_index, triangle) in mesh.triangles.iter().enumerate() {
            if triangle
                .iter()
                .any(|index| *index as usize >= mesh.vertices.len())
            {
                return Err(unsupported(format!(
                    "mesh {mesh_index} triangle {triangle_index} has an out-of-range index"
                )));
            }
            if triangle[0] == triangle[1]
                || triangle[1] == triangle[2]
                || triangle[2] == triangle[0]
            {
                return Err(unsupported(format!(
                    "mesh {mesh_index} triangle {triangle_index} repeats a vertex"
                )));
            }
            let [a, b, c] = triangle.map(|index| mesh.vertices[index as usize].position);
            let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
            let cross = [
                ab[1] * ac[2] - ab[2] * ac[1],
                ab[2] * ac[0] - ab[0] * ac[2],
                ab[0] * ac[1] - ab[1] * ac[0],
            ];
            if cross.iter().map(|value| value * value).sum::<f32>() <= f32::EPSILON {
                return Err(unsupported(format!(
                    "mesh {mesh_index} triangle {triangle_index} has zero area"
                )));
            }
        }
    }
    for (material_index, material) in model.materials.iter().enumerate() {
        for (slot, texture) in material.texture_numbers.iter().enumerate() {
            if let Some(texture) = texture {
                if *texture as usize >= model.textures.len() {
                    return Err(unsupported(format!(
                        "material {material_index} texture slot {slot} references {texture}, but only {} textures exist",
                        model.textures.len()
                    )));
                }
            }
        }
    }
    Ok(())
}

fn information_section(model: &StaticModel) -> Result<J3dRebuildSection> {
    let mut hierarchy = Vec::with_capacity(4 + model.meshes.len() * 2);
    hierarchy.push(J3dHierarchyCommand {
        node_type: 0x10,
        index: 0,
    });
    hierarchy.push(J3dHierarchyCommand {
        node_type: 1,
        index: 0,
    });
    for (shape_index, _) in model.meshes.iter().enumerate() {
        hierarchy.push(J3dHierarchyCommand {
            node_type: 0x11,
            index: shape_index as u16,
        });
        hierarchy.push(J3dHierarchyCommand {
            node_type: 0x12,
            index: shape_index as u16,
        });
    }
    hierarchy.push(J3dHierarchyCommand {
        node_type: 2,
        index: 0,
    });
    hierarchy.push(J3dHierarchyCommand {
        node_type: 0,
        index: 0,
    });
    let size = align(0x18 + hierarchy.len() * 4, 0x20)?;
    Ok(J3dRebuildSection {
        declared_size: size as u32,
        data: J3dRebuildSectionData::Information(J3dInformationSection {
            flags: 0,
            reserved: u16::MAX,
            packet_count: 0,
            vertex_count: 0,
            hierarchy_offset: 0x18,
            hierarchy,
        }),
        padding: Vec::new(),
    })
}

fn vertex_section(
    vertices: &[StaticModelVertex],
    attributes: &UsedAttributes,
) -> J3dRebuildSection {
    let mut formats = vec![vertex_format(9, 1, 4), vertex_format(10, 0, 4)];
    let mut arrays = vec![
        J3dVertexArray {
            attribute: J3dVertexArrayAttribute::Position,
            offset: 0,
            values: J3dScalarArray::Float32Bits(
                vertices
                    .iter()
                    .flat_map(|vertex| vertex.position.map(f32::to_bits))
                    .collect(),
            ),
        },
        J3dVertexArray {
            attribute: J3dVertexArrayAttribute::Normal,
            offset: 0,
            values: J3dScalarArray::Float32Bits(
                vertices
                    .iter()
                    .flat_map(|vertex| vertex.normal.map(f32::to_bits))
                    .collect(),
            ),
        },
    ];
    if attributes.normal_binormal_tangent {
        formats.push(vertex_format(25, 1, 4));
        arrays.push(J3dVertexArray {
            attribute: J3dVertexArrayAttribute::NormalBinormalTangent,
            offset: 0,
            values: J3dScalarArray::Float32Bits(
                vertices
                    .iter()
                    .flat_map(|vertex| {
                        vertex
                            .normal_binormal_tangent
                            .unwrap_or_else(|| fallback_nbt(vertex.normal))
                            .into_iter()
                            .flatten()
                            .map(f32::to_bits)
                    })
                    .collect(),
            ),
        });
    }
    for slot in 0..2 {
        if attributes.colors[slot] {
            formats.push(vertex_format(11 + slot as u32, 1, 5));
            arrays.push(J3dVertexArray {
                attribute: if slot == 0 {
                    J3dVertexArrayAttribute::Color0
                } else {
                    J3dVertexArrayAttribute::Color1
                },
                offset: 0,
                values: J3dScalarArray::PackedColor(
                    vertices
                        .iter()
                        .flat_map(|vertex| vertex.colors[slot].unwrap_or([255; 4]))
                        .collect(),
                ),
            });
        }
    }
    for slot in 0..8 {
        if attributes.tex_coords[slot] {
            formats.push(vertex_format(13 + slot as u32, 1, 4));
            arrays.push(J3dVertexArray {
                attribute: J3dVertexArrayAttribute::TexCoord(slot as u8),
                offset: 0,
                values: J3dScalarArray::Float32Bits(
                    vertices
                        .iter()
                        .flat_map(|vertex| {
                            vertex.tex_coords[slot]
                                .unwrap_or([0.0; 2])
                                .map(f32::to_bits)
                        })
                        .collect(),
                ),
            });
        }
    }
    formats.push(J3dVertexAttributeFormat {
        attribute: 0xff,
        component_count: u32::MAX,
        component_type: u32::MAX,
        fractional_bits: 0xff,
        reserved: [0xff; 3],
    });
    J3dRebuildSection {
        declared_size: 8,
        data: J3dRebuildSectionData::Vertices(J3dVertexSection {
            offsets: [0; 14],
            formats,
            arrays,
        }),
        padding: Vec::new(),
    }
}

fn vertex_format(
    attribute: u32,
    component_count: u32,
    component_type: u32,
) -> J3dVertexAttributeFormat {
    J3dVertexAttributeFormat {
        attribute,
        component_count,
        component_type,
        fractional_bits: 0,
        reserved: [0xff; 3],
    }
}

fn empty_envelope_section() -> J3dRebuildSection {
    J3dRebuildSection {
        declared_size: 0x20,
        data: J3dRebuildSectionData::Envelopes(J3dEnvelopeSection {
            envelope_count: 0,
            reserved: u16::MAX,
            offsets: [0; 4],
            mix_counts: Vec::new(),
            joint_indices: Vec::new(),
            weights: Vec::new(),
            inverse_bind_matrices: Vec::new(),
        }),
        padding: Vec::new(),
    }
}

fn rigid_draw_matrix_section() -> J3dRebuildSection {
    J3dRebuildSection {
        declared_size: 0x20,
        data: J3dRebuildSectionData::DrawMatrices(J3dDrawMatrixSection {
            matrix_count: 1,
            reserved: u16::MAX,
            flag_offset: 0x14,
            index_offset: 0x16,
            weighted_flags: vec![0],
            indices: vec![0],
        }),
        padding: Vec::new(),
    }
}

fn joint_section(name: &str, bounds: ([f32; 3], [f32; 3], f32)) -> Result<J3dRebuildSection> {
    let (min, max, radius) = bounds;
    let mut names = build_name_table([name])?;
    let name_size = canonicalize_name_table(&mut names)?;
    let name_offset = 0x5cusize;
    let size = align(name_offset + name_size, 0x20)?;
    Ok(J3dRebuildSection {
        declared_size: size as u32,
        data: J3dRebuildSectionData::Joints(J3dJointSection {
            joint_count: 1,
            reserved: u16::MAX,
            init_offset: 0x18,
            remap_offset: 0x58,
            name_table_offset: name_offset as u32,
            joints: vec![J3dJointRecord {
                matrix_type: 0,
                scale_compensate: 0,
                reserved: 0xff,
                scale: [1.0; 3],
                rotation: [0; 3],
                rotation_padding: -1,
                translation: [0.0; 3],
                radius,
                bounds_min: min,
                bounds_max: max,
            }],
            remap: vec![0],
            names,
        }),
        padding: Vec::new(),
    })
}

fn shape_section(
    model: &StaticModel,
    mesh_ranges: &[usize],
    attributes: &UsedAttributes,
) -> Result<J3dRebuildSection> {
    let descriptors = descriptors(attributes);
    let descriptor_set = J3dVertexDescriptorSet {
        relative_offset: 0,
        descriptors,
    };
    let mut shapes = Vec::with_capacity(model.meshes.len());
    let mut groups = Vec::with_capacity(model.meshes.len());
    let mut draws = Vec::with_capacity(model.meshes.len());
    for (index, mesh) in model.meshes.iter().enumerate() {
        let mesh_bounds = bounds(mesh.vertices.iter().map(|vertex| vertex.position))?;
        shapes.push(J3dShapeRecord {
            matrix_type: 0,
            reserved_01: 0xff,
            matrix_group_count: 1,
            vertex_descriptor_offset: 0,
            matrix_group_start: index as u16,
            draw_start: index as u16,
            reserved_0a: u16::MAX,
            radius: mesh_bounds.2,
            bounds_min: mesh_bounds.0,
            bounds_max: mesh_bounds.1,
        });
        groups.push(J3dShapeMatrixGroup {
            matrix_index: 0,
            matrix_count: 0,
            first_matrix: 0,
        });
        let vertices = mesh
            .triangles
            .iter()
            // J3D/GX BMD display lists use Sunshine's clockwise front-face
            // convention. Keep the authoring document's geometric winding
            // (and matching normals/collision) unchanged, and invert only the
            // final SHP1 operand order written for the runtime.
            .flat_map(|triangle| [triangle[0], triangle[2], triangle[1]])
            .map(|local_index| {
                let global_index = mesh_ranges[index] + local_index as usize;
                let mut operands = vec![J3dGxVertexOperand::Index16(global_index as u16); 2];
                if attributes.normal_binormal_tangent {
                    operands.push(J3dGxVertexOperand::Index16(global_index as u16));
                }
                for used in attributes.colors {
                    if used {
                        operands.push(J3dGxVertexOperand::Index16(global_index as u16));
                    }
                }
                for used in attributes.tex_coords {
                    if used {
                        operands.push(J3dGxVertexOperand::Index16(global_index as u16));
                    }
                }
                operands
            })
            .collect::<Vec<_>>();
        draws.push(J3dShapeDraw {
            display_list_size: 0,
            display_list_offset: 0,
            commands: vec![J3dGxCommand::Primitive {
                opcode: 0x90,
                vertex_count: vertices.len() as u16,
                vertices,
            }],
        });
    }
    Ok(J3dRebuildSection {
        declared_size: 8,
        data: J3dRebuildSectionData::Shapes(J3dShapeSection {
            shape_count: model.meshes.len() as u16,
            reserved: u16::MAX,
            offsets: [0; 8],
            shapes,
            remap: (0..model.meshes.len()).map(|index| index as u16).collect(),
            names: Some(build_name_table(
                model.meshes.iter().map(|mesh| mesh.name.as_str()),
            )?),
            vertex_descriptor_sets: vec![descriptor_set],
            matrix_table: Vec::new(),
            matrix_groups: groups,
            draws,
        }),
        padding: Vec::new(),
    })
}

fn descriptors(attributes: &UsedAttributes) -> Vec<J3dVertexDescriptor> {
    let mut descriptors = vec![
        J3dVertexDescriptor {
            attribute: 9,
            input_type: 3,
        },
        J3dVertexDescriptor {
            attribute: 10,
            input_type: 3,
        },
    ];
    if attributes.normal_binormal_tangent {
        descriptors.push(J3dVertexDescriptor {
            attribute: 25,
            input_type: 3,
        });
    }
    for (slot, used) in attributes.colors.iter().copied().enumerate() {
        if used {
            descriptors.push(J3dVertexDescriptor {
                attribute: 11 + slot as u32,
                input_type: 3,
            });
        }
    }
    for (slot, used) in attributes.tex_coords.iter().copied().enumerate() {
        if used {
            descriptors.push(J3dVertexDescriptor {
                attribute: 13 + slot as u32,
                input_type: 3,
            });
        }
    }
    descriptors.push(J3dVertexDescriptor {
        attribute: 0xff,
        input_type: 0,
    });
    descriptors
}

fn bounds(positions: impl IntoIterator<Item = [f32; 3]>) -> Result<([f32; 3], [f32; 3], f32)> {
    let mut positions = positions.into_iter();
    let first = positions
        .next()
        .ok_or_else(|| unsupported("cannot compute bounds of empty geometry"))?;
    let mut min = first;
    let mut max = first;
    let mut radius_squared = first.iter().map(|value| value * value).sum::<f32>();
    for position in positions {
        for component in 0..3 {
            min[component] = min[component].min(position[component]);
            max[component] = max[component].max(position[component]);
        }
        radius_squared = radius_squared.max(position.iter().map(|value| value * value).sum());
    }
    Ok((min, max, radius_squared.sqrt()))
}

fn fallback_nbt(normal: [f32; 3]) -> [[f32; 3]; 3] {
    let reference = if normal[1].abs() < 0.999 {
        [0.0, 1.0, 0.0]
    } else {
        [1.0, 0.0, 0.0]
    };
    let tangent = normalize(cross(reference, normal));
    let binormal = normalize(cross(normal, tangent));
    [normal, binormal, tangent]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize(value: [f32; 3]) -> [f32; 3] {
    let length = value
        .iter()
        .map(|component| component * component)
        .sum::<f32>()
        .sqrt();
    if length > f32::EPSILON {
        value.map(|component| component / length)
    } else {
        [1.0, 0.0, 0.0]
    }
}

fn build_name_table<'a>(names: impl IntoIterator<Item = &'a str>) -> Result<J3dNameTable> {
    Ok(J3dNameTable {
        reserved: u16::MAX,
        entries: names
            .into_iter()
            .map(|name| {
                validate_name("J3D", name)?;
                Ok(J3dNameEntry {
                    hash: 0,
                    string_offset: 0,
                    name: name.to_string(),
                })
            })
            .collect::<Result<Vec<_>>>()?,
    })
}

fn canonicalize_name_table(table: &mut J3dNameTable) -> Result<usize> {
    let mut cursor = 4 + table.entries.len() * 4;
    for entry in &mut table.entries {
        let (encoded, _, errors) = SHIFT_JIS.encode(&entry.name);
        if errors {
            return Err(unsupported("J3D name cannot be encoded as Shift-JIS"));
        }
        entry.hash = encoded.iter().fold(0u16, |hash, byte| {
            hash.wrapping_mul(3).wrapping_add(*byte as u16)
        });
        entry.string_offset = u16::try_from(cursor)
            .map_err(|_| limit("J3D name table bytes", cursor, u16::MAX as usize))?;
        cursor = cursor
            .checked_add(encoded.len() + 1)
            .ok_or_else(|| limit("J3D name table bytes", usize::MAX, u16::MAX as usize))?;
    }
    Ok(cursor)
}

fn validate_name(kind: &str, name: &str) -> Result<()> {
    let (_, _, errors) = SHIFT_JIS.encode(name);
    if errors {
        Err(unsupported(format!(
            "{kind} name {name:?} cannot be encoded as Shift-JIS"
        )))
    } else {
        Ok(())
    }
}

fn align(value: usize, alignment: usize) -> Result<usize> {
    value
        .checked_add(alignment - 1)
        .map(|value| value & !(alignment - 1))
        .ok_or_else(|| unsupported("BMD3 layout overflow"))
}

fn limit(resource: &'static str, requested: usize, limit: usize) -> FormatError {
    FormatError::ResourceLimit {
        format: FORMAT,
        resource,
        requested,
        limit,
    }
}

fn unsupported(message: impl Into<String>) -> FormatError {
    FormatError::Unsupported {
        format: FORMAT,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::J3dFile;

    fn triangle_model() -> StaticModel {
        let mut vertices = vec![
            StaticModelVertex::new([0.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
            StaticModelVertex::new([100.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
            StaticModelVertex::new([0.0, 0.0, 100.0], [0.0, 1.0, 0.0]),
        ];
        for (index, vertex) in vertices.iter_mut().enumerate() {
            vertex.normal_binormal_tangent =
                Some([[0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]]);
            vertex.colors[0] = Some([index as u8, 2, 3, 255]);
            vertex.tex_coords[0] = Some([index as f32 / 2.0, index as f32 / 2.0]);
        }
        StaticModel {
            root_joint_name: "root".to_string(),
            meshes: vec![StaticModelMesh {
                name: "triangle".to_string(),
                material_index: 0,
                vertices,
                triangles: vec![[0, 2, 1]],
            }],
            materials: vec![GxMaterial::default()],
            textures: Vec::new(),
        }
    }

    #[test]
    fn static_triangle_compiles_as_canonical_eight_section_bmd3() {
        let document = compile_static_bmd3(&triangle_model()).unwrap();
        assert_eq!(document.sections.len(), 8);
        let first = document.to_bytes().unwrap();
        let independently_compiled = compile_static_bmd3(&triangle_model())
            .unwrap()
            .to_bytes()
            .unwrap();
        assert_eq!(first, independently_compiled);
        let reopened = J3dRebuildDocument::parse(&first).unwrap();
        assert_eq!(reopened.to_bytes().unwrap(), first);
        let preview = J3dFile::parse(first).unwrap().geometry_preview().unwrap();
        assert_eq!(preview.triangles.len(), 1);
    }

    #[test]
    fn each_shape_gets_a_distinct_runtime_material_node() {
        let mut model = triangle_model();
        let mut second_mesh = model.meshes[0].clone();
        second_mesh.name = "second_triangle".to_string();
        model.meshes.push(second_mesh);

        let document = compile_static_bmd3(&model).unwrap();
        let reopened = J3dRebuildDocument::parse(document.to_bytes().unwrap()).unwrap();
        let information = reopened
            .sections
            .iter()
            .find_map(|section| match &section.data {
                J3dRebuildSectionData::Information(information) => Some(information),
                _ => None,
            })
            .expect("compiled BMD3 has INF1");
        let material_shape_pairs = information
            .hierarchy
            .windows(2)
            .filter_map(|commands| {
                (commands[0].node_type == 0x11 && commands[1].node_type == 0x12)
                    .then_some((commands[0].index, commands[1].index))
            })
            .collect::<Vec<_>>();
        assert_eq!(material_shape_pairs, [(0, 0), (1, 1)]);

        let shapes = reopened
            .sections
            .iter()
            .find_map(|section| match &section.data {
                J3dRebuildSectionData::Shapes(shapes) => Some(shapes),
                _ => None,
            })
            .expect("compiled BMD3 has SHP1");
        assert_eq!(shapes.shape_count, 2);

        let materials = reopened
            .sections
            .iter()
            .find_map(|section| match &section.data {
                J3dRebuildSectionData::Materials(materials) => Some(materials),
                _ => None,
            })
            .expect("compiled BMD3 has MAT3");
        assert_eq!(materials.material_count, 2);
        assert_eq!(materials.material_init_records.len(), 2);
        assert_eq!(
            materials.material_init_records[0],
            materials.material_init_records[1]
        );
        let remap = materials
            .tables
            .iter()
            .find(|table| table.kind == crate::J3dMaterialTableKind::MaterialRemap)
            .expect("compiled MAT3 has a material remap");
        assert_eq!(remap.allocation, J3dScalarArray::Unsigned16(vec![0, 1]));
    }

    #[test]
    fn static_triangle_emits_sunshine_clockwise_display_list_winding() {
        let mut model = triangle_model();
        model.meshes[0].vertices.swap(1, 2);
        model.meshes[0].triangles[0] = [0, 1, 2];
        let source_triangle = model.meshes[0].triangles[0];
        assert_eq!(source_triangle, [0, 1, 2]);

        let source_positions =
            source_triangle.map(|index| model.meshes[0].vertices[index as usize].position);
        let source_edges = [
            [
                source_positions[1][0] - source_positions[0][0],
                source_positions[1][1] - source_positions[0][1],
                source_positions[1][2] - source_positions[0][2],
            ],
            [
                source_positions[2][0] - source_positions[0][0],
                source_positions[2][1] - source_positions[0][1],
                source_positions[2][2] - source_positions[0][2],
            ],
        ];
        let source_face_normal = cross(source_edges[0], source_edges[1]);
        assert!(source_face_normal[1] > 0.0);

        let document = compile_static_bmd3(&model).unwrap();
        let shapes = document
            .sections
            .iter()
            .find_map(|section| match &section.data {
                J3dRebuildSectionData::Shapes(shapes) => Some(shapes),
                _ => None,
            })
            .expect("compiled BMD3 has SHP1");
        let J3dGxCommand::Primitive { vertices, .. } = &shapes.draws[0].commands[0] else {
            panic!("static BMD3 shape starts with a GX primitive");
        };
        let emitted_position_indices = vertices
            .iter()
            .take(3)
            .map(|vertex| match vertex[0] {
                J3dGxVertexOperand::Index16(index) => index,
                _ => panic!("static BMD3 positions use GX index16"),
            })
            .collect::<Vec<_>>();
        assert_eq!(emitted_position_indices, [0, 2, 1]);

        let preview = J3dFile::parse(document.to_bytes().unwrap())
            .unwrap()
            .geometry_preview()
            .unwrap();
        let emitted = &preview.triangles[0];
        assert_eq!(
            emitted.vertices,
            [
                model.meshes[0].vertices[0].position,
                model.meshes[0].vertices[2].position,
                model.meshes[0].vertices[1].position,
            ]
        );
        assert_eq!(emitted.normals, Some([[0.0, 1.0, 0.0]; 3]));
        let emitted_edges = [
            [
                emitted.vertices[1][0] - emitted.vertices[0][0],
                emitted.vertices[1][1] - emitted.vertices[0][1],
                emitted.vertices[1][2] - emitted.vertices[0][2],
            ],
            [
                emitted.vertices[2][0] - emitted.vertices[0][0],
                emitted.vertices[2][1] - emitted.vertices[0][1],
                emitted.vertices[2][2] - emitted.vertices[0][2],
            ],
        ];
        let emitted_face_normal = cross(emitted_edges[0], emitted_edges[1]);
        assert!(emitted_face_normal[1] < 0.0);
    }

    #[test]
    fn invalid_cross_section_references_are_rejected() {
        let mut model = triangle_model();
        model.meshes[0].material_index = 1;
        assert!(compile_static_bmd3(&model).is_err());
        let mut model = triangle_model();
        model.materials[0].texture_numbers[0] = Some(0);
        assert!(compile_static_bmd3(&model).is_err());
    }

    #[test]
    fn non_finite_and_degenerate_geometry_is_rejected() {
        let mut model = triangle_model();
        model.meshes[0].vertices[0].position[0] = f32::NAN;
        assert!(compile_static_bmd3(&model).is_err());
        let mut model = triangle_model();
        model.meshes[0].triangles[0] = [0, 0, 1];
        assert!(compile_static_bmd3(&model).is_err());
    }
}
