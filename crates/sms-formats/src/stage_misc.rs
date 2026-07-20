//! Source-free semantic codecs for small stage-side runtime formats.

use encoding_rs::SHIFT_JIS;
use serde::{Deserialize, Serialize};

use crate::binary::{be_f32, be_i16, be_u16, be_u32, require_len, require_magic};
use crate::{FormatError, Result};

const FORMAT: &str = "stage misc";
const RETAIL_PADDING: &[u8] = b"This is padding data to alignment";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StageMiscPaddingStyle {
    Zero,
    Ff,
    RetailPhrase,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageMiscPaddingRegion {
    pub offset: u32,
    pub length: u32,
    pub style: StageMiscPaddingStyle,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RalNode {
    pub position: [i16; 3],
    pub connection_count: i16,
    pub flags: u32,
    pub pitch: u16,
    pub yaw: u16,
    pub roll: u16,
    pub speed: u16,
    pub connections: [u16; 8],
    pub periods: [f32; 8],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RalGraph {
    pub name_offset: u32,
    pub nodes_offset: u32,
    pub name: String,
    pub nodes: Vec<RalNode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RalGraphMerge {
    pub source_name: String,
    pub target_name: String,
    pub inserted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RalDocument {
    pub graphs: Vec<RalGraph>,
    pub file_size: u32,
    pub padding: Vec<StageMiscPaddingRegion>,
}

impl RalDocument {
    pub fn parse(bytes: impl AsRef<[u8]>) -> Result<Self> {
        const DESC_SIZE: usize = 12;
        const NODE_SIZE: usize = 0x44;
        let bytes = bytes.as_ref();
        require_len(FORMAT, bytes, DESC_SIZE)?;
        let mut coverage = Coverage::new(bytes.len());
        let mut descriptors = Vec::new();
        let mut offset = 0usize;
        loop {
            coverage.mark(offset, DESC_SIZE)?;
            let node_count = be_u32(bytes, offset, FORMAT)? as usize;
            let name_offset = be_u32(bytes, offset + 4, FORMAT)?;
            let nodes_offset = be_u32(bytes, offset + 8, FORMAT)?;
            offset += DESC_SIZE;
            if node_count == 0 && name_offset == 0 && nodes_offset == 0 {
                break;
            }
            if node_count > 0x10_0000 {
                return Err(resource("RAL nodes", node_count, 0x10_0000));
            }
            descriptors.push((node_count, name_offset, nodes_offset));
        }

        let mut graphs = Vec::with_capacity(descriptors.len());
        for (node_count, name_offset, nodes_offset) in descriptors {
            let name = read_string(bytes, &mut coverage, name_offset as usize)?;
            coverage.mark(
                nodes_offset as usize,
                node_count
                    .checked_mul(NODE_SIZE)
                    .ok_or_else(|| invalid(nodes_offset as usize, bytes.len()))?,
            )?;
            let mut nodes = Vec::with_capacity(node_count);
            for node_index in 0..node_count {
                let base = nodes_offset as usize + node_index * NODE_SIZE;
                let connection_count = be_i16(bytes, base + 6, FORMAT)?;
                if !(0..=8).contains(&connection_count) {
                    return Err(unsupported(format!(
                        "RAL node {node_index} has {connection_count} connections"
                    )));
                }
                let connections = std::array::from_fn(|index| {
                    be_u16(bytes, base + 0x14 + index * 2, FORMAT).expect("covered RAL connection")
                });
                for connection in &connections[..connection_count as usize] {
                    if *connection as usize >= node_count {
                        return Err(unsupported(format!(
                            "RAL node {node_index} connects to out-of-range node {connection}"
                        )));
                    }
                }
                nodes.push(RalNode {
                    position: [
                        be_i16(bytes, base, FORMAT)?,
                        be_i16(bytes, base + 2, FORMAT)?,
                        be_i16(bytes, base + 4, FORMAT)?,
                    ],
                    connection_count,
                    flags: be_u32(bytes, base + 8, FORMAT)?,
                    pitch: be_u16(bytes, base + 0x0C, FORMAT)?,
                    yaw: be_u16(bytes, base + 0x0E, FORMAT)?,
                    roll: be_u16(bytes, base + 0x10, FORMAT)?,
                    speed: be_u16(bytes, base + 0x12, FORMAT)?,
                    connections,
                    periods: std::array::from_fn(|index| {
                        be_f32(bytes, base + 0x24 + index * 4, FORMAT).expect("covered RAL period")
                    }),
                });
            }
            graphs.push(RalGraph {
                name_offset,
                nodes_offset,
                name,
                nodes,
            });
        }
        Ok(Self {
            graphs,
            file_size: usize_u32(bytes.len(), "RAL file size")?,
            padding: coverage.classify(bytes)?,
        })
    }

    /// Creates an empty source-free RAL document ready for graph insertion.
    pub fn empty_canonical() -> Self {
        Self {
            graphs: Vec::new(),
            file_size: 12,
            padding: Vec::new(),
        }
    }

    /// Rebuilds graph descriptors, Shift-JIS names, node arrays, and padding
    /// into one deterministic source-free layout.
    pub fn canonicalize_layout(&mut self) -> Result<()> {
        const DESC_SIZE: usize = 12;
        const NODE_SIZE: usize = 0x44;
        let mut canonical = self.clone();
        let descriptor_end = canonical
            .graphs
            .len()
            .checked_add(1)
            .and_then(|count| count.checked_mul(DESC_SIZE))
            .ok_or_else(|| {
                resource("RAL descriptors", canonical.graphs.len(), u32::MAX as usize)
            })?;
        let mut cursor = descriptor_end;
        for graph in &mut canonical.graphs {
            if graph.name.contains('\0') {
                return Err(unsupported("RAL graph name contains a null byte"));
            }
            let (encoded, _, had_errors) = SHIFT_JIS.encode(&graph.name);
            if had_errors {
                return Err(unsupported(format!(
                    "RAL graph name {:?} cannot be encoded as Shift-JIS",
                    graph.name
                )));
            }
            graph.name_offset = usize_u32(cursor, "RAL graph name offset")?;
            cursor = cursor
                .checked_add(encoded.len() + 1)
                .ok_or_else(|| resource("RAL bytes", usize::MAX, u32::MAX as usize))?;
        }
        let names_end = cursor;
        cursor = align_stage_misc(cursor, 4)?;
        let mut padding = Vec::new();
        if cursor > names_end {
            padding.push(StageMiscPaddingRegion {
                offset: usize_u32(names_end, "RAL padding offset")?,
                length: usize_u32(cursor - names_end, "RAL padding length")?,
                style: StageMiscPaddingStyle::Zero,
            });
        }
        for graph in &mut canonical.graphs {
            graph.nodes_offset = usize_u32(cursor, "RAL graph nodes offset")?;
            cursor =
                cursor
                    .checked_add(graph.nodes.len().checked_mul(NODE_SIZE).ok_or_else(|| {
                        resource("RAL nodes", graph.nodes.len(), u32::MAX as usize)
                    })?)
                    .ok_or_else(|| resource("RAL bytes", usize::MAX, u32::MAX as usize))?;
        }
        canonical.file_size = usize_u32(cursor, "RAL file size")?;
        canonical.padding = padding;
        // Prove that all graph/node invariants and the derived allocations are
        // encodable before committing the transactional update.
        let bytes = canonical.encode()?;
        let reparsed = Self::parse(&bytes)?;
        if reparsed.encode()? != bytes {
            return Err(unsupported("canonical RAL layout is not byte-stable"));
        }
        *self = canonical;
        Ok(())
    }

    /// Merges exact named source graphs. Equal target graphs are reused;
    /// conflicting names receive a deterministic Shift-JIS-safe authored name
    /// which callers must write back to the actor's typed `graph_name` field.
    pub fn merge_named_graphs(
        &mut self,
        source: &Self,
        names: &[String],
    ) -> Result<Vec<RalGraphMerge>> {
        let mut merged = self.clone();
        let mut used_names = merged
            .graphs
            .iter()
            .chain(source.graphs.iter())
            .map(|graph| graph.name.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let mut outcomes = Vec::new();
        for name in names.iter().collect::<std::collections::BTreeSet<_>>() {
            let mut source_matches = source.graphs.iter().filter(|graph| graph.name == *name);
            let source_graph = source_matches.next().ok_or_else(|| {
                unsupported(format!(
                    "required RAL graph {name:?} was not found in the source"
                ))
            })?;
            if source_matches.any(|graph| graph.nodes != source_graph.nodes) {
                return Err(unsupported(format!(
                    "source RAL graph {name:?} has conflicting duplicate definitions"
                )));
            }
            let target_matches = merged
                .graphs
                .iter()
                .filter(|graph| graph.name == *name)
                .collect::<Vec<_>>();
            let authored_prefix = format!("{name}_authored");
            let reusable_authored = merged
                .graphs
                .iter()
                .filter(|graph| {
                    graph.nodes == source_graph.nodes
                        && (graph.name == authored_prefix
                            || graph
                                .name
                                .strip_prefix(&format!("{authored_prefix}_"))
                                .is_some_and(|suffix| suffix.parse::<u16>().is_ok()))
                })
                .min_by(|a, b| a.name.cmp(&b.name));
            let (target_name, inserted) = if !target_matches.is_empty()
                && target_matches
                    .iter()
                    .all(|graph| graph.nodes == source_graph.nodes)
            {
                ((*name).clone(), false)
            } else if let Some(existing) = reusable_authored {
                (existing.name.clone(), false)
            } else {
                let target_name = if target_matches.is_empty() {
                    (*name).clone()
                } else {
                    unique_authored_graph_name(name, &used_names)?
                };
                let mut graph = source_graph.clone();
                graph.name = target_name.clone();
                graph.name_offset = 0;
                graph.nodes_offset = 0;
                used_names.insert(target_name.clone());
                merged.graphs.push(graph);
                (target_name, true)
            };
            outcomes.push(RalGraphMerge {
                source_name: (*name).clone(),
                target_name,
                inserted,
            });
        }
        if outcomes.iter().any(|outcome| outcome.inserted) {
            merged.canonicalize_layout()?;
            *self = merged;
        }
        Ok(outcomes)
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        const DESC_SIZE: usize = 12;
        const NODE_SIZE: usize = 0x44;
        let mut bytes = vec![0; self.file_size as usize];
        fill_padding(&mut bytes, &self.padding)?;
        let descriptor_bytes = self
            .graphs
            .len()
            .checked_add(1)
            .and_then(|count| count.checked_mul(DESC_SIZE))
            .ok_or_else(|| invalid(self.graphs.len(), bytes.len()))?;
        require_range(&bytes, 0, descriptor_bytes)?;
        for (index, graph) in self.graphs.iter().enumerate() {
            let base = index * DESC_SIZE;
            put_u32(
                &mut bytes,
                base,
                usize_u32(graph.nodes.len(), "RAL node count")?,
            )?;
            put_u32(&mut bytes, base + 4, graph.name_offset)?;
            put_u32(&mut bytes, base + 8, graph.nodes_offset)?;
            write_string(&mut bytes, graph.name_offset as usize, &graph.name)?;
            for (node_index, node) in graph.nodes.iter().enumerate() {
                if !(0..=8).contains(&node.connection_count) {
                    return Err(unsupported("RAL connection count must be 0..=8"));
                }
                let node_offset = graph.nodes_offset as usize + node_index * NODE_SIZE;
                put_i16(&mut bytes, node_offset, node.position[0])?;
                put_i16(&mut bytes, node_offset + 2, node.position[1])?;
                put_i16(&mut bytes, node_offset + 4, node.position[2])?;
                put_i16(&mut bytes, node_offset + 6, node.connection_count)?;
                put_u32(&mut bytes, node_offset + 8, node.flags)?;
                put_u16(&mut bytes, node_offset + 0x0C, node.pitch)?;
                put_u16(&mut bytes, node_offset + 0x0E, node.yaw)?;
                put_u16(&mut bytes, node_offset + 0x10, node.roll)?;
                put_u16(&mut bytes, node_offset + 0x12, node.speed)?;
                for (connection_index, connection) in node.connections.iter().enumerate() {
                    put_u16(
                        &mut bytes,
                        node_offset + 0x14 + connection_index * 2,
                        *connection,
                    )?;
                }
                for (period_index, period) in node.periods.iter().enumerate() {
                    put_u32(
                        &mut bytes,
                        node_offset + 0x24 + period_index * 4,
                        period.to_bits(),
                    )?;
                }
            }
        }
        Ok(bytes)
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        self.encode()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct YmpLayer {
    pub layer_type: u16,
    pub subtype: u16,
    pub flags: u16,
    pub reserved: u16,
    pub vertical_offset: f32,
    pub vertical_scale: f32,
    pub min_x: f32,
    pub min_z: f32,
    pub max_x: f32,
    pub max_z: f32,
    pub width_log2: u16,
    pub height_log2: u16,
    pub user_value: u32,
    pub map_offset: u32,
    pub depth_map: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct YmpDocument {
    pub header_reserved: u16,
    pub layer_info_offset: u32,
    pub layers: Vec<YmpLayer>,
    pub file_size: u32,
    pub padding: Vec<StageMiscPaddingRegion>,
}

impl YmpDocument {
    pub fn parse(bytes: impl AsRef<[u8]>) -> Result<Self> {
        const HEADER_SIZE: usize = 8;
        const LAYER_SIZE: usize = 0x2C;
        let bytes = bytes.as_ref();
        require_len(FORMAT, bytes, HEADER_SIZE)?;
        let layer_count = be_u16(bytes, 0, FORMAT)? as usize;
        let layer_info_offset = be_u32(bytes, 4, FORMAT)?;
        let mut coverage = Coverage::new(bytes.len());
        coverage.mark(0, HEADER_SIZE)?;
        coverage.mark(
            layer_info_offset as usize,
            layer_count
                .checked_mul(LAYER_SIZE)
                .ok_or_else(|| invalid(layer_count, bytes.len()))?,
        )?;
        let mut layers = Vec::with_capacity(layer_count);
        for index in 0..layer_count {
            let base = layer_info_offset as usize + index * LAYER_SIZE;
            let width_log2 = be_u16(bytes, base + 0x20, FORMAT)?;
            let height_log2 = be_u16(bytes, base + 0x22, FORMAT)?;
            if width_log2 > 15 || height_log2 > 15 {
                return Err(unsupported(format!(
                    "YMP layer {index} has unsupported dimensions 2^{width_log2} x 2^{height_log2}"
                )));
            }
            let map_len = (1usize << width_log2)
                .checked_mul(1usize << height_log2)
                .ok_or_else(|| invalid(width_log2 as usize, bytes.len()))?;
            let map_offset = be_u32(bytes, base + 0x28, FORMAT)?;
            coverage.mark(map_offset as usize, map_len)?;
            layers.push(YmpLayer {
                layer_type: be_u16(bytes, base, FORMAT)?,
                subtype: be_u16(bytes, base + 2, FORMAT)?,
                flags: be_u16(bytes, base + 4, FORMAT)?,
                reserved: be_u16(bytes, base + 6, FORMAT)?,
                vertical_offset: be_f32(bytes, base + 8, FORMAT)?,
                vertical_scale: be_f32(bytes, base + 0x0C, FORMAT)?,
                min_x: be_f32(bytes, base + 0x10, FORMAT)?,
                min_z: be_f32(bytes, base + 0x14, FORMAT)?,
                max_x: be_f32(bytes, base + 0x18, FORMAT)?,
                max_z: be_f32(bytes, base + 0x1C, FORMAT)?,
                width_log2,
                height_log2,
                user_value: be_u32(bytes, base + 0x24, FORMAT)?,
                map_offset,
                depth_map: bytes[map_offset as usize..map_offset as usize + map_len].to_vec(),
            });
        }
        Ok(Self {
            header_reserved: be_u16(bytes, 2, FORMAT)?,
            layer_info_offset,
            layers,
            file_size: usize_u32(bytes.len(), "YMP file size")?,
            padding: coverage.classify(bytes)?,
        })
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        const LAYER_SIZE: usize = 0x2C;
        let mut bytes = vec![0; self.file_size as usize];
        fill_padding(&mut bytes, &self.padding)?;
        put_u16(
            &mut bytes,
            0,
            usize_u16(self.layers.len(), "YMP layer count")?,
        )?;
        put_u16(&mut bytes, 2, self.header_reserved)?;
        put_u32(&mut bytes, 4, self.layer_info_offset)?;
        for (index, layer) in self.layers.iter().enumerate() {
            if layer.width_log2 > 15 || layer.height_log2 > 15 {
                return Err(unsupported(
                    "YMP dimensions exceed the supported exponent range",
                ));
            }
            let expected_len = (1usize << layer.width_log2) * (1usize << layer.height_log2);
            if layer.depth_map.len() != expected_len {
                return Err(unsupported(format!(
                    "YMP depth map has {} bytes, expected {expected_len}",
                    layer.depth_map.len()
                )));
            }
            let base = self.layer_info_offset as usize + index * LAYER_SIZE;
            put_u16(&mut bytes, base, layer.layer_type)?;
            put_u16(&mut bytes, base + 2, layer.subtype)?;
            put_u16(&mut bytes, base + 4, layer.flags)?;
            put_u16(&mut bytes, base + 6, layer.reserved)?;
            for (field, value) in [
                (8, layer.vertical_offset),
                (0x0C, layer.vertical_scale),
                (0x10, layer.min_x),
                (0x14, layer.min_z),
                (0x18, layer.max_x),
                (0x1C, layer.max_z),
            ] {
                put_u32(&mut bytes, base + field, value.to_bits())?;
            }
            put_u16(&mut bytes, base + 0x20, layer.width_log2)?;
            put_u16(&mut bytes, base + 0x22, layer.height_log2)?;
            put_u32(&mut bytes, base + 0x24, layer.user_value)?;
            put_u32(&mut bytes, base + 0x28, layer.map_offset)?;
            put_bytes(&mut bytes, layer.map_offset as usize, &layer.depth_map)?;
        }
        Ok(bytes)
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        self.encode()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MeDocument {
    DummyDot,
    DummyCrLf,
}

impl MeDocument {
    pub fn parse(bytes: impl AsRef<[u8]>) -> Result<Self> {
        match bytes.as_ref() {
            b"dummy." => Ok(Self::DummyDot),
            b"dummy\r\n" => Ok(Self::DummyCrLf),
            bytes => Err(unsupported(format!(
                "unknown .me deletion marker ({} bytes)",
                bytes.len()
            ))),
        }
    }

    pub fn encode(self) -> Vec<u8> {
        match self {
            Self::DummyDot => b"dummy.".to_vec(),
            Self::DummyCrLf => b"dummy\r\n".to_vec(),
        }
    }

    pub fn to_bytes(self) -> Vec<u8> {
        self.encode()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayLinkRow {
    pub name: String,
    /// Target node indices for the row's three replay exits. `None` is the
    /// on-disc `*` sentinel.
    pub targets: [Option<u8>; 3],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayLinkDocument {
    pub name: String,
    pub rows: Vec<ReplayLinkRow>,
}

impl ReplayLinkDocument {
    pub fn parse(bytes: impl AsRef<[u8]>) -> Result<Self> {
        const TYPE_NAME: &str = "ReplayLink";
        const ROW_TYPE_NAME: &str = "Link";
        let bytes = bytes.as_ref();
        require_len(FORMAT, bytes, 4)?;
        let declared_size = be_u32(bytes, 0, FORMAT)? as usize;
        if declared_size != bytes.len() {
            return Err(unsupported(format!(
                "ReplayLink declares {declared_size} bytes, file has {}",
                bytes.len()
            )));
        }
        let mut offset = 4usize;
        let type_name = read_hashed_sized_string(bytes, &mut offset)?;
        if type_name != TYPE_NAME {
            return Err(unsupported(format!(
                "ReplayLink root type is {type_name:?}, expected {TYPE_NAME:?}"
            )));
        }
        let name = read_hashed_sized_string(bytes, &mut offset)?;
        let row_count = be_u32(bytes, offset, FORMAT)? as usize;
        offset += 4;
        if row_count > 0x10000 {
            return Err(resource("ReplayLink rows", row_count, 0x10000));
        }
        let mut rows = Vec::with_capacity(row_count);
        for row_index in 0..row_count {
            let row_size = be_u32(bytes, offset, FORMAT)? as usize;
            if row_size < 4 {
                return Err(unsupported(format!(
                    "ReplayLink row {row_index} declares only {row_size} bytes"
                )));
            }
            let row_end = offset
                .checked_add(row_size)
                .ok_or_else(|| invalid(offset, bytes.len()))?;
            require_range(bytes, offset, row_size)?;
            let row_bytes = &bytes[offset..row_end];
            let mut row_offset = 4usize;
            let row_type = read_hashed_sized_string(row_bytes, &mut row_offset)?;
            if row_type != ROW_TYPE_NAME {
                return Err(unsupported(format!(
                    "ReplayLink row {row_index} type is {row_type:?}, expected {ROW_TYPE_NAME:?}"
                )));
            }
            let row_name = read_hashed_sized_string(row_bytes, &mut row_offset)?;
            let mut targets = [None; 3];
            for (column, target) in targets.iter_mut().enumerate() {
                let length = be_u16(row_bytes, row_offset, FORMAT)?;
                row_offset += 2;
                if length != 1 {
                    return Err(unsupported(format!(
                        "ReplayLink row {row_index} column {column} has {length} bytes"
                    )));
                }
                require_range(row_bytes, row_offset, 1)?;
                let value = row_bytes[row_offset];
                row_offset += 1;
                *target = match value {
                    b'*' => None,
                    b'A'..=b'Z' => Some(value - b'A'),
                    _ => {
                        return Err(unsupported(format!(
                            "ReplayLink row {row_index} column {column} has target {value:#x}"
                        )))
                    }
                };
            }
            if row_offset != row_bytes.len() {
                return Err(unsupported(format!(
                    "ReplayLink row {row_index} has {} trailing bytes",
                    row_bytes.len() - row_offset
                )));
            }
            rows.push(ReplayLinkRow {
                name: row_name,
                targets,
            });
            offset = row_end;
        }
        if offset != bytes.len() {
            return Err(unsupported(format!(
                "ReplayLink has {} trailing bytes",
                bytes.len() - offset
            )));
        }
        Ok(Self { name, rows })
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut bytes = vec![0; 4];
        push_hashed_sized_string(&mut bytes, "ReplayLink")?;
        push_hashed_sized_string(&mut bytes, &self.name)?;
        bytes.extend_from_slice(&usize_u32(self.rows.len(), "ReplayLink row count")?.to_be_bytes());
        for (row_index, row) in self.rows.iter().enumerate() {
            let mut row_bytes = vec![0; 4];
            push_hashed_sized_string(&mut row_bytes, "Link")?;
            push_hashed_sized_string(&mut row_bytes, &row.name)?;
            for (column, target) in row.targets.iter().enumerate() {
                row_bytes.extend_from_slice(&1u16.to_be_bytes());
                let value = match target {
                    None => b'*',
                    Some(index @ 0..=25) => b'A' + index,
                    Some(index) => {
                        return Err(unsupported(format!(
                            "ReplayLink row {row_index} column {column} target {index} exceeds Z"
                        )))
                    }
                };
                row_bytes.push(value);
            }
            let row_size = usize_u32(row_bytes.len(), "ReplayLink row size")?;
            row_bytes[..4].copy_from_slice(&row_size.to_be_bytes());
            bytes.extend_from_slice(&row_bytes);
        }
        let file_size = usize_u32(bytes.len(), "ReplayLink file size")?;
        bytes[..4].copy_from_slice(&file_size.to_be_bytes());
        Ok(bytes)
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        self.encode()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SpcInstruction {
    Int(i32),
    Float(f32),
    String(u32),
    Address(u32),
    Variable {
        layer: u32,
        variable: u32,
    },
    Nop,
    Increment {
        reserved: u8,
        layer: u32,
        variable: u32,
    },
    Decrement {
        reserved: u8,
        layer: u32,
        variable: u32,
    },
    Add,
    Subtract,
    Multiply,
    Divide,
    Modulo,
    Assign {
        reserved: u8,
        layer: u32,
        variable: u32,
    },
    Equal,
    NotEqual,
    Greater,
    Less,
    GreaterEqual,
    LessEqual,
    Negate,
    Not,
    LogicalAnd,
    LogicalOr,
    BitAnd,
    BitOr,
    ShiftLeft,
    ShiftRight,
    Call {
        address: u32,
        argument_count: i32,
    },
    Builtin {
        symbol_index: u32,
        argument_count: u32,
    },
    MakeFrame(i32),
    MakeDisplay(i32),
    Return,
    ReturnZero,
    JumpIfZero(u32),
    Jump(u32),
    Pop,
    IntZero,
    IntOne,
    End,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpcDataEntry {
    pub offset: u32,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpcSymbol {
    pub symbol_type: u32,
    pub name_offset: u32,
    pub data: u32,
    pub name_hash: u32,
    pub native_call: u32,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpcDocument {
    pub text_offset: u32,
    pub text_length: u32,
    pub data_offset: u32,
    pub symbol_offset: u32,
    pub initial_storage_count: i32,
    pub instructions: Vec<SpcInstruction>,
    pub data: Vec<SpcDataEntry>,
    pub symbols: Vec<SpcSymbol>,
    pub file_size: u32,
    pub padding: Vec<StageMiscPaddingRegion>,
}

impl SpcDocument {
    pub fn parse(bytes: impl AsRef<[u8]>) -> Result<Self> {
        const HEADER_SIZE: usize = 0x1C;
        const SYMBOL_SIZE: usize = 0x14;
        let bytes = bytes.as_ref();
        require_len(FORMAT, bytes, HEADER_SIZE)?;
        require_magic(FORMAT, bytes, b"SPCB")?;
        let text_offset = be_u32(bytes, 4, FORMAT)?;
        let data_offset = be_u32(bytes, 8, FORMAT)?;
        let data_count = be_u32(bytes, 0x0C, FORMAT)? as usize;
        let symbol_offset = be_u32(bytes, 0x10, FORMAT)?;
        let symbol_count = be_u32(bytes, 0x14, FORMAT)? as usize;
        if !(HEADER_SIZE..=data_offset as usize).contains(&(text_offset as usize))
            || data_offset > symbol_offset
            || symbol_offset as usize > bytes.len()
        {
            return Err(unsupported(format!(
                "SPC section order is invalid: text={text_offset:#x}, data={data_offset:#x}, symbols={symbol_offset:#x}"
            )));
        }
        let mut coverage = Coverage::new(bytes.len());
        coverage.mark(0, HEADER_SIZE)?;
        let text_region = &bytes[text_offset as usize..data_offset as usize];
        let (instructions, text_len) = parse_spc_text_aligned(text_region)?;
        coverage.mark(text_offset as usize, text_len)?;

        let data_table_len = data_count
            .checked_mul(4)
            .ok_or_else(|| invalid(data_count, bytes.len()))?;
        coverage.mark(data_offset as usize, data_table_len)?;
        let data_blob = data_offset as usize + data_table_len;
        if data_blob > symbol_offset as usize {
            return Err(invalid(data_blob, symbol_offset as usize));
        }
        let mut data = Vec::with_capacity(data_count);
        for index in 0..data_count {
            let offset = be_u32(bytes, data_offset as usize + index * 4, FORMAT)?;
            let value = read_string(bytes, &mut coverage, data_blob + offset as usize)?;
            data.push(SpcDataEntry { offset, value });
        }

        let symbol_table_len = symbol_count
            .checked_mul(SYMBOL_SIZE)
            .ok_or_else(|| invalid(symbol_count, bytes.len()))?;
        coverage.mark(symbol_offset as usize, symbol_table_len)?;
        let name_blob = symbol_offset as usize + symbol_table_len;
        let mut symbols = Vec::with_capacity(symbol_count);
        for index in 0..symbol_count {
            let base = symbol_offset as usize + index * SYMBOL_SIZE;
            let name_offset = be_u32(bytes, base + 4, FORMAT)?;
            let name = read_string(bytes, &mut coverage, name_blob + name_offset as usize)?;
            symbols.push(SpcSymbol {
                symbol_type: be_u32(bytes, base, FORMAT)?,
                name_offset,
                data: be_u32(bytes, base + 8, FORMAT)?,
                name_hash: be_u32(bytes, base + 0x0C, FORMAT)?,
                native_call: be_u32(bytes, base + 0x10, FORMAT)?,
                name,
            });
        }
        Ok(Self {
            text_offset,
            text_length: usize_u32(text_len, "SPC text length")?,
            data_offset,
            symbol_offset,
            initial_storage_count: be_u32(bytes, 0x18, FORMAT)? as i32,
            instructions,
            data,
            symbols,
            file_size: usize_u32(bytes.len(), "SPC file size")?,
            padding: coverage.classify(bytes)?,
        })
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        const SYMBOL_SIZE: usize = 0x14;
        let mut bytes = vec![0; self.file_size as usize];
        fill_padding(&mut bytes, &self.padding)?;
        put_bytes(&mut bytes, 0, b"SPCB")?;
        put_u32(&mut bytes, 4, self.text_offset)?;
        put_u32(&mut bytes, 8, self.data_offset)?;
        put_u32(
            &mut bytes,
            0x0C,
            usize_u32(self.data.len(), "SPC data count")?,
        )?;
        put_u32(&mut bytes, 0x10, self.symbol_offset)?;
        put_u32(
            &mut bytes,
            0x14,
            usize_u32(self.symbols.len(), "SPC symbol count")?,
        )?;
        put_u32(&mut bytes, 0x18, self.initial_storage_count as u32)?;
        let text = encode_spc_text(&self.instructions)?;
        let expected_text_len = self.text_length as usize;
        if text.len() != expected_text_len {
            return Err(unsupported(format!(
                "SPC instruction stream encodes to {} bytes, layout requires {expected_text_len}",
                text.len()
            )));
        }
        if self
            .text_offset
            .checked_add(self.text_length)
            .is_none_or(|end| end > self.data_offset)
        {
            return Err(unsupported("SPC text length exceeds the text region"));
        }
        put_bytes(&mut bytes, self.text_offset as usize, &text)?;
        let data_blob = self.data_offset as usize + self.data.len() * 4;
        for (index, entry) in self.data.iter().enumerate() {
            put_u32(
                &mut bytes,
                self.data_offset as usize + index * 4,
                entry.offset,
            )?;
            write_string(&mut bytes, data_blob + entry.offset as usize, &entry.value)?;
        }
        let name_blob = self.symbol_offset as usize + self.symbols.len() * SYMBOL_SIZE;
        for (index, symbol) in self.symbols.iter().enumerate() {
            let base = self.symbol_offset as usize + index * SYMBOL_SIZE;
            put_u32(&mut bytes, base, symbol.symbol_type)?;
            put_u32(&mut bytes, base + 4, symbol.name_offset)?;
            put_u32(&mut bytes, base + 8, symbol.data)?;
            put_u32(&mut bytes, base + 0x0C, symbol.name_hash)?;
            put_u32(&mut bytes, base + 0x10, symbol.native_call)?;
            write_string(
                &mut bytes,
                name_blob + symbol.name_offset as usize,
                &symbol.name,
            )?;
        }
        Ok(bytes)
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        self.encode()
    }
}

fn parse_spc_text_aligned(bytes: &[u8]) -> Result<(Vec<SpcInstruction>, usize)> {
    let whole_error = match parse_spc_text(bytes) {
        Ok(instructions) => return Ok((instructions, bytes.len())),
        Err(error) => error,
    };
    for padding_len in 1..=3.min(bytes.len()) {
        let text_len = bytes.len() - padding_len;
        if bytes[text_len..].iter().all(|byte| *byte == 0) {
            if let Ok(instructions) = parse_spc_text(&bytes[..text_len]) {
                return Ok((instructions, text_len));
            }
        }
    }
    Err(whole_error)
}

fn parse_spc_text(bytes: &[u8]) -> Result<Vec<SpcInstruction>> {
    let mut instructions = Vec::new();
    let mut offset = 0usize;
    while offset < bytes.len() {
        let opcode = bytes[offset];
        offset += 1;
        let one_u32 = |offset: &mut usize| -> Result<u32> {
            let value = be_u32(bytes, *offset, FORMAT)?;
            *offset += 4;
            Ok(value)
        };
        let instruction = match opcode {
            0 => SpcInstruction::Int(one_u32(&mut offset)? as i32),
            1 => SpcInstruction::Float(f32::from_bits(one_u32(&mut offset)?)),
            2 => SpcInstruction::String(one_u32(&mut offset)?),
            3 => SpcInstruction::Address(one_u32(&mut offset)?),
            4 => SpcInstruction::Variable {
                layer: one_u32(&mut offset)?,
                variable: one_u32(&mut offset)?,
            },
            5 => SpcInstruction::Nop,
            6 | 7 => {
                require_len(FORMAT, bytes, offset + 1)?;
                let reserved = bytes[offset];
                offset += 1;
                let layer = one_u32(&mut offset)?;
                let variable = one_u32(&mut offset)?;
                if opcode == 6 {
                    SpcInstruction::Increment {
                        reserved,
                        layer,
                        variable,
                    }
                } else {
                    SpcInstruction::Decrement {
                        reserved,
                        layer,
                        variable,
                    }
                }
            }
            8 => SpcInstruction::Add,
            9 => SpcInstruction::Subtract,
            10 => SpcInstruction::Multiply,
            11 => SpcInstruction::Divide,
            12 => SpcInstruction::Modulo,
            13 => {
                require_len(FORMAT, bytes, offset + 1)?;
                let reserved = bytes[offset];
                offset += 1;
                SpcInstruction::Assign {
                    reserved,
                    layer: one_u32(&mut offset)?,
                    variable: one_u32(&mut offset)?,
                }
            }
            14 => SpcInstruction::Equal,
            15 => SpcInstruction::NotEqual,
            16 => SpcInstruction::Greater,
            17 => SpcInstruction::Less,
            18 => SpcInstruction::GreaterEqual,
            19 => SpcInstruction::LessEqual,
            20 => SpcInstruction::Negate,
            21 => SpcInstruction::Not,
            22 => SpcInstruction::LogicalAnd,
            23 => SpcInstruction::LogicalOr,
            24 => SpcInstruction::BitAnd,
            25 => SpcInstruction::BitOr,
            26 => SpcInstruction::ShiftLeft,
            27 => SpcInstruction::ShiftRight,
            28 => SpcInstruction::Call {
                address: one_u32(&mut offset)?,
                argument_count: one_u32(&mut offset)? as i32,
            },
            29 => SpcInstruction::Builtin {
                symbol_index: one_u32(&mut offset)?,
                argument_count: one_u32(&mut offset)?,
            },
            30 => SpcInstruction::MakeFrame(one_u32(&mut offset)? as i32),
            31 => SpcInstruction::MakeDisplay(one_u32(&mut offset)? as i32),
            32 => SpcInstruction::Return,
            33 => SpcInstruction::ReturnZero,
            34 => SpcInstruction::JumpIfZero(one_u32(&mut offset)?),
            35 => SpcInstruction::Jump(one_u32(&mut offset)?),
            36 => SpcInstruction::Pop,
            37 => SpcInstruction::IntZero,
            38 => SpcInstruction::IntOne,
            39 => SpcInstruction::End,
            _ => return Err(unsupported(format!("unknown SPC opcode {opcode:#x}"))),
        };
        instructions.push(instruction);
    }
    Ok(instructions)
}

fn encode_spc_text(instructions: &[SpcInstruction]) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let put = |bytes: &mut Vec<u8>, value: u32| bytes.extend_from_slice(&value.to_be_bytes());
    for instruction in instructions {
        let opcode = match instruction {
            SpcInstruction::Int(_) => 0,
            SpcInstruction::Float(_) => 1,
            SpcInstruction::String(_) => 2,
            SpcInstruction::Address(_) => 3,
            SpcInstruction::Variable { .. } => 4,
            SpcInstruction::Nop => 5,
            SpcInstruction::Increment { .. } => 6,
            SpcInstruction::Decrement { .. } => 7,
            SpcInstruction::Add => 8,
            SpcInstruction::Subtract => 9,
            SpcInstruction::Multiply => 10,
            SpcInstruction::Divide => 11,
            SpcInstruction::Modulo => 12,
            SpcInstruction::Assign { .. } => 13,
            SpcInstruction::Equal => 14,
            SpcInstruction::NotEqual => 15,
            SpcInstruction::Greater => 16,
            SpcInstruction::Less => 17,
            SpcInstruction::GreaterEqual => 18,
            SpcInstruction::LessEqual => 19,
            SpcInstruction::Negate => 20,
            SpcInstruction::Not => 21,
            SpcInstruction::LogicalAnd => 22,
            SpcInstruction::LogicalOr => 23,
            SpcInstruction::BitAnd => 24,
            SpcInstruction::BitOr => 25,
            SpcInstruction::ShiftLeft => 26,
            SpcInstruction::ShiftRight => 27,
            SpcInstruction::Call { .. } => 28,
            SpcInstruction::Builtin { .. } => 29,
            SpcInstruction::MakeFrame(_) => 30,
            SpcInstruction::MakeDisplay(_) => 31,
            SpcInstruction::Return => 32,
            SpcInstruction::ReturnZero => 33,
            SpcInstruction::JumpIfZero(_) => 34,
            SpcInstruction::Jump(_) => 35,
            SpcInstruction::Pop => 36,
            SpcInstruction::IntZero => 37,
            SpcInstruction::IntOne => 38,
            SpcInstruction::End => 39,
        };
        bytes.push(opcode);
        match instruction {
            SpcInstruction::Int(value)
            | SpcInstruction::MakeFrame(value)
            | SpcInstruction::MakeDisplay(value) => put(&mut bytes, *value as u32),
            SpcInstruction::Float(value) => put(&mut bytes, value.to_bits()),
            SpcInstruction::String(value)
            | SpcInstruction::Address(value)
            | SpcInstruction::JumpIfZero(value)
            | SpcInstruction::Jump(value) => put(&mut bytes, *value),
            SpcInstruction::Variable { layer, variable } => {
                put(&mut bytes, *layer);
                put(&mut bytes, *variable);
            }
            SpcInstruction::Increment {
                reserved,
                layer,
                variable,
            }
            | SpcInstruction::Decrement {
                reserved,
                layer,
                variable,
            } => {
                bytes.push(*reserved);
                put(&mut bytes, *layer);
                put(&mut bytes, *variable);
            }
            SpcInstruction::Assign {
                reserved,
                layer,
                variable,
            } => {
                bytes.push(*reserved);
                put(&mut bytes, *layer);
                put(&mut bytes, *variable);
            }
            SpcInstruction::Call {
                address,
                argument_count,
            } => {
                put(&mut bytes, *address);
                put(&mut bytes, *argument_count as u32);
            }
            SpcInstruction::Builtin {
                symbol_index,
                argument_count,
            } => {
                put(&mut bytes, *symbol_index);
                put(&mut bytes, *argument_count);
            }
            _ => {}
        }
    }
    Ok(bytes)
}

struct Coverage {
    covered: Vec<bool>,
}

impl Coverage {
    fn new(len: usize) -> Self {
        Self {
            covered: vec![false; len],
        }
    }

    fn mark(&mut self, offset: usize, length: usize) -> Result<()> {
        let end = offset
            .checked_add(length)
            .ok_or_else(|| invalid(offset, self.covered.len()))?;
        if end > self.covered.len() {
            return Err(invalid(end, self.covered.len()));
        }
        self.covered[offset..end].fill(true);
        Ok(())
    }

    fn classify(&self, bytes: &[u8]) -> Result<Vec<StageMiscPaddingRegion>> {
        let mut regions = Vec::new();
        let mut offset = 0usize;
        while offset < self.covered.len() {
            if self.covered[offset] {
                offset += 1;
                continue;
            }
            let end = self.covered[offset..]
                .iter()
                .position(|covered| *covered)
                .map_or(self.covered.len(), |length| offset + length);
            let data = &bytes[offset..end];
            let style = if data.iter().all(|byte| *byte == 0) {
                StageMiscPaddingStyle::Zero
            } else if data.iter().all(|byte| *byte == 0xFF) {
                StageMiscPaddingStyle::Ff
            } else if data
                .iter()
                .enumerate()
                .all(|(index, byte)| *byte == RETAIL_PADDING[index % RETAIL_PADDING.len()])
            {
                StageMiscPaddingStyle::RetailPhrase
            } else {
                return Err(unsupported(format!(
                    "unmodelled non-padding bytes at {offset:#x}..{end:#x}"
                )));
            };
            regions.push(StageMiscPaddingRegion {
                offset: usize_u32(offset, "padding offset")?,
                length: usize_u32(end - offset, "padding length")?,
                style,
            });
            offset = end;
        }
        Ok(regions)
    }
}

fn read_hashed_sized_string(bytes: &[u8], offset: &mut usize) -> Result<String> {
    let hash = be_u16(bytes, *offset, FORMAT)?;
    let length = be_u16(bytes, *offset + 2, FORMAT)? as usize;
    *offset += 4;
    require_range(bytes, *offset, length)?;
    let encoded = &bytes[*offset..*offset + length];
    *offset += length;
    let expected_hash = encoded.iter().fold(0u16, |key, byte| {
        key.wrapping_mul(3).wrapping_add(*byte as u16)
    });
    if hash != expected_hash {
        return Err(unsupported(format!(
            "ReplayLink string hash is {hash:#06x}, expected {expected_hash:#06x}"
        )));
    }
    let (decoded, had_errors) = SHIFT_JIS.decode_without_bom_handling(encoded);
    if had_errors {
        return Err(unsupported("invalid ReplayLink Shift-JIS string"));
    }
    let value = decoded.into_owned();
    let (roundtrip, _, had_errors) = SHIFT_JIS.encode(&value);
    if had_errors || roundtrip.as_ref() != encoded {
        return Err(unsupported("noncanonical ReplayLink Shift-JIS string"));
    }
    Ok(value)
}

fn push_hashed_sized_string(bytes: &mut Vec<u8>, value: &str) -> Result<()> {
    let (encoded, _, had_errors) = SHIFT_JIS.encode(value);
    if had_errors {
        return Err(unsupported(
            "ReplayLink string cannot be encoded as Shift-JIS",
        ));
    }
    let length = usize_u16(encoded.len(), "ReplayLink string length")?;
    let hash = encoded.iter().fold(0u16, |key, byte| {
        key.wrapping_mul(3).wrapping_add(*byte as u16)
    });
    bytes.extend_from_slice(&hash.to_be_bytes());
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(encoded.as_ref());
    Ok(())
}

fn align_stage_misc(value: usize, alignment: usize) -> Result<usize> {
    debug_assert!(alignment.is_power_of_two());
    value
        .checked_add(alignment - 1)
        .map(|value| value & !(alignment - 1))
        .ok_or_else(|| resource("aligned stage misc bytes", usize::MAX, u32::MAX as usize))
}

fn unique_authored_graph_name(
    base: &str,
    used_names: &std::collections::BTreeSet<String>,
) -> Result<String> {
    for ordinal in 1..=u16::MAX {
        let suffix = if ordinal == 1 {
            "_authored".to_string()
        } else {
            format!("_authored_{ordinal}")
        };
        let candidate = format!("{base}{suffix}");
        if used_names.contains(&candidate) {
            continue;
        }
        let (_, _, had_errors) = SHIFT_JIS.encode(&candidate);
        if !had_errors && !candidate.contains('\0') {
            return Ok(candidate);
        }
    }
    Err(unsupported(format!(
        "could not allocate a unique authored name for RAL graph {base:?}"
    )))
}

fn read_string(bytes: &[u8], coverage: &mut Coverage, offset: usize) -> Result<String> {
    if offset >= bytes.len() {
        return Err(invalid(offset, bytes.len()));
    }
    let end = bytes[offset..]
        .iter()
        .position(|byte| *byte == 0)
        .map(|length| offset + length)
        .ok_or_else(|| invalid(offset, bytes.len()))?;
    coverage.mark(offset, end - offset + 1)?;
    let encoded = &bytes[offset..end];
    let (decoded, had_errors) = SHIFT_JIS.decode_without_bom_handling(encoded);
    if had_errors {
        return Err(unsupported("invalid Shift-JIS string"));
    }
    let value = decoded.into_owned();
    let (roundtrip, _, had_errors) = SHIFT_JIS.encode(&value);
    if had_errors || roundtrip.as_ref() != encoded {
        return Err(unsupported("noncanonical Shift-JIS string"));
    }
    Ok(value)
}

fn write_string(bytes: &mut [u8], offset: usize, value: &str) -> Result<()> {
    let (encoded, _, had_errors) = SHIFT_JIS.encode(value);
    if had_errors {
        return Err(unsupported("string cannot be encoded as Shift-JIS"));
    }
    put_bytes(bytes, offset, encoded.as_ref())?;
    put_byte(bytes, offset + encoded.len(), 0)
}

fn fill_padding(bytes: &mut [u8], regions: &[StageMiscPaddingRegion]) -> Result<()> {
    for region in regions {
        let start = region.offset as usize;
        let end = start
            .checked_add(region.length as usize)
            .ok_or_else(|| invalid(start, bytes.len()))?;
        if end > bytes.len() {
            return Err(invalid(end, bytes.len()));
        }
        match region.style {
            StageMiscPaddingStyle::Zero => bytes[start..end].fill(0),
            StageMiscPaddingStyle::Ff => bytes[start..end].fill(0xFF),
            StageMiscPaddingStyle::RetailPhrase => {
                for (index, byte) in bytes[start..end].iter_mut().enumerate() {
                    *byte = RETAIL_PADDING[index % RETAIL_PADDING.len()];
                }
            }
        }
    }
    Ok(())
}

fn require_range(bytes: &[u8], offset: usize, length: usize) -> Result<()> {
    let end = offset
        .checked_add(length)
        .ok_or_else(|| invalid(offset, bytes.len()))?;
    if end > bytes.len() {
        return Err(invalid(end, bytes.len()));
    }
    Ok(())
}

fn put_byte(bytes: &mut [u8], offset: usize, value: u8) -> Result<()> {
    if offset >= bytes.len() {
        return Err(invalid(offset, bytes.len()));
    }
    bytes[offset] = value;
    Ok(())
}

fn put_bytes(bytes: &mut [u8], offset: usize, value: &[u8]) -> Result<()> {
    require_range(bytes, offset, value.len())?;
    bytes[offset..offset + value.len()].copy_from_slice(value);
    Ok(())
}

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) -> Result<()> {
    put_bytes(bytes, offset, &value.to_be_bytes())
}

fn put_i16(bytes: &mut [u8], offset: usize, value: i16) -> Result<()> {
    put_bytes(bytes, offset, &value.to_be_bytes())
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) -> Result<()> {
    put_bytes(bytes, offset, &value.to_be_bytes())
}

fn usize_u16(value: usize, label: &'static str) -> Result<u16> {
    u16::try_from(value).map_err(|_| resource(label, value, u16::MAX as usize))
}

fn usize_u32(value: usize, label: &'static str) -> Result<u32> {
    u32::try_from(value).map_err(|_| resource(label, value, u32::MAX as usize))
}

fn resource(resource: &'static str, requested: usize, limit: usize) -> FormatError {
    FormatError::ResourceLimit {
        format: FORMAT,
        resource,
        requested,
        limit,
    }
}

fn invalid(offset: usize, len: usize) -> FormatError {
    FormatError::InvalidOffset {
        format: FORMAT,
        offset,
        len,
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
    use super::{
        MeDocument, RalDocument, RalGraph, RalNode, ReplayLinkDocument, SpcDocument, YmpDocument,
    };

    fn ral_node(x: i16) -> RalNode {
        RalNode {
            position: [x, 2, 3],
            connection_count: 0,
            flags: 0,
            pitch: 0,
            yaw: 0,
            roll: 0,
            speed: 0,
            connections: [0; 8],
            periods: [0.0; 8],
        }
    }

    fn ral_graph(name: &str, x: i16) -> RalGraph {
        RalGraph {
            name_offset: 0,
            nodes_offset: 0,
            name: name.to_string(),
            nodes: vec![ral_node(x)],
        }
    }

    #[test]
    fn canonical_ral_layout_uses_exact_eof_after_contiguous_nodes() {
        let mut document = RalDocument {
            graphs: vec![ral_graph("route_a", 1), ral_graph("route_b", 2)],
            file_size: 0,
            padding: Vec::new(),
        };
        document.canonicalize_layout().unwrap();
        // Three 12-byte descriptors, two 8-byte NUL-terminated names, then
        // two contiguous 0x44-byte nodes with no invented EOF alignment.
        assert_eq!(document.graphs[0].name_offset, 36);
        assert_eq!(document.graphs[1].name_offset, 44);
        assert_eq!(document.graphs[0].nodes_offset, 52);
        assert_eq!(document.graphs[1].nodes_offset, 120);
        assert_eq!(document.file_size, 188);
        assert!(document.padding.is_empty());
        let bytes = document.encode().unwrap();
        assert_eq!(bytes.len(), 188);
        assert_eq!(RalDocument::parse(&bytes).unwrap().encode().unwrap(), bytes);
    }

    #[test]
    fn named_ral_merge_reuses_equal_graphs_and_renames_conflicts() {
        let mut source = RalDocument {
            graphs: vec![ral_graph("route_a", 10), ral_graph("route_b", 20)],
            file_size: 0,
            padding: Vec::new(),
        };
        source.canonicalize_layout().unwrap();
        let mut target = RalDocument {
            graphs: vec![ral_graph("route_a", 99)],
            file_size: 0,
            padding: Vec::new(),
        };
        target.canonicalize_layout().unwrap();

        let outcomes = target
            .merge_named_graphs(&source, &["route_a".to_string(), "route_b".to_string()])
            .unwrap();
        assert_eq!(outcomes[0].source_name, "route_a");
        assert_eq!(outcomes[0].target_name, "route_a_authored");
        assert!(outcomes[0].inserted);
        assert_eq!(outcomes[1].target_name, "route_b");
        assert!(
            target
                .graphs
                .iter()
                .any(|graph| graph.name == "route_a_authored"
                    && graph.nodes == source.graphs[0].nodes)
        );
        assert!(target.graphs.iter().any(|graph| graph.name == "route_b"));

        let bytes = target.encode().unwrap();
        let repeated = target
            .merge_named_graphs(&source, &["route_a".to_string()])
            .unwrap();
        assert_eq!(repeated[0].target_name, "route_a_authored");
        assert!(!repeated[0].inserted);
        assert_eq!(target.encode().unwrap(), bytes);
        assert_eq!(RalDocument::parse(&bytes).unwrap().encode().unwrap(), bytes);

        let mut identical = source.clone();
        let identical_bytes = identical.encode().unwrap();
        let outcomes = identical
            .merge_named_graphs(&source, &["route_a".to_string()])
            .unwrap();
        assert!(!outcomes[0].inserted);
        assert_eq!(identical.encode().unwrap(), identical_bytes);
    }

    #[test]
    #[ignore = "requires SMS_BASE_ROOT with extracted retail stage archives"]
    fn source_free_rebuilds_stage_misc_corpus() {
        let root = std::env::var_os("SMS_BASE_ROOT")
            .map(std::path::PathBuf::from)
            .expect("set SMS_BASE_ROOT to an extracted retail game root");
        let archives = crate::discover_scene_archives(root).expect("discover stage archives");
        let mut counts = std::collections::BTreeMap::<String, usize>::new();
        let mut failures = Vec::new();
        for archive_info in archives {
            let source = std::fs::read(&archive_info.path).expect("read stage archive");
            let decoded = if source.starts_with(b"Yaz0") {
                crate::decode_yaz0(&source).expect("decode stage archive")
            } else {
                source
            };
            let archive = crate::RarcArchive::parse(decoded).expect("parse stage archive");
            for entry in archive.file_entries() {
                let path = entry.path.to_ascii_lowercase();
                let format = if path.ends_with("linkdata.bin") {
                    Some("replay_link")
                } else {
                    ["ral", "ymp", "me", "sb"]
                        .into_iter()
                        .find(|extension| path.ends_with(&format!(".{extension}")))
                };
                let Some(format) = format else {
                    continue;
                };
                let original = archive
                    .file_bytes_raw(&entry.raw_path)
                    .expect("read misc entry");
                let rebuilt = match format {
                    "ral" => RalDocument::parse(&original).and_then(|document| document.encode()),
                    "ymp" => YmpDocument::parse(&original).and_then(|document| document.encode()),
                    "me" => MeDocument::parse(&original).map(MeDocument::encode),
                    "sb" => SpcDocument::parse(&original).and_then(|document| document.encode()),
                    "replay_link" => {
                        ReplayLinkDocument::parse(&original).and_then(|document| document.encode())
                    }
                    _ => unreachable!(),
                };
                match rebuilt {
                    Ok(bytes) if bytes == original => {
                        *counts.entry(format.to_string()).or_default() += 1;
                    }
                    Ok(_) => failures.push(format!(
                        "{}!/{}: byte mismatch",
                        archive_info.path.display(),
                        entry.path
                    )),
                    Err(error) => failures.push(format!(
                        "{}!/{}: {error}",
                        archive_info.path.display(),
                        entry.path
                    )),
                }
            }
        }
        assert!(
            failures.is_empty(),
            "{} stage misc failure(s), rebuilt={counts:?}:\n{}",
            failures.len(),
            failures.into_iter().take(40).collect::<Vec<_>>().join("\n")
        );
        for (format, expected) in [
            ("ral", 107usize),
            ("ymp", 90),
            ("me", 492),
            ("sb", 150),
            ("replay_link", 20),
        ] {
            assert_eq!(
                counts.get(format).copied().unwrap_or_default(),
                expected,
                "retail {format} count drifted"
            );
        }
        eprintln!("source-free stage misc rebuild counts: {counts:?}");
    }

    #[test]
    #[ignore = "requires SMS_BASE_ROOT with extracted retail stage archives"]
    fn census_stage_misc_headers() {
        let root = std::env::var_os("SMS_BASE_ROOT")
            .map(std::path::PathBuf::from)
            .expect("set SMS_BASE_ROOT to an extracted retail game root");
        let archives = crate::discover_scene_archives(root).expect("discover stage archives");
        let mut counts = std::collections::BTreeMap::<String, usize>::new();
        let mut fingerprints = std::collections::BTreeMap::<String, usize>::new();
        let mut examples = std::collections::BTreeMap::<String, String>::new();
        for archive_info in archives {
            let source = std::fs::read(&archive_info.path).expect("read stage archive");
            let decoded = if source.starts_with(b"Yaz0") {
                crate::decode_yaz0(&source).expect("decode stage archive")
            } else {
                source
            };
            let archive = crate::RarcArchive::parse(decoded).expect("parse stage archive");
            for entry in archive.file_entries() {
                let path = entry.path.to_ascii_lowercase();
                let Some(extension) = ["ral", "ymp", "me", "sb"]
                    .into_iter()
                    .find(|extension| path.ends_with(&format!(".{extension}")))
                else {
                    continue;
                };
                let bytes = archive
                    .file_bytes_raw(&entry.raw_path)
                    .expect("read misc entry");
                *counts.entry(extension.to_string()).or_default() += 1;
                let header = bytes
                    .iter()
                    .take(32)
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>();
                let key = format!("{extension}:len={:#x}:head={header}", bytes.len());
                *fingerprints.entry(key.clone()).or_default() += 1;
                examples
                    .entry(key)
                    .or_insert_with(|| format!("{}!/{}", archive_info.path.display(), entry.path));
            }
        }
        eprintln!("misc extension counts: {counts:?}");
        for (fingerprint, count) in fingerprints {
            eprintln!(
                "count={count}: {fingerprint}: {}",
                examples.get(&fingerprint).expect("example")
            );
        }
    }
}
