//! Project-side dialogue routing and instance authoring.
//!
//! Sunshine does not serialize a message id in an NPC's JDrama record.  The
//! relationship is owned by SPC scripts which compare the runtime actor name
//! and eventually call `setTalkMsgID` (or the balloon equivalent).  This
//! module therefore keeps dialogue authoring separate from `SceneObject`
//! parameters and derives a non-serialized route index from the effective
//! typed resources.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use encoding_rs::SHIFT_JIS;
use serde::{Deserialize, Serialize};
use sms_formats::{
    discover_scene_archives, mount_scene_archive, read_stage_asset_bytes, BmgFile, BmgMessage,
    SmsBmgControl, SpcDocument, SpcInstruction, SpcProgramSymbol, SpcRelocatableProgram,
    SMS_BMG_RUNTIME_MESSAGE_LIMIT, SMS_TALK_SOUND_LIMIT,
};

use crate::{
    Result, SceneError, SceneObject, StageArchiveEdits, StageDocument, StageResourceDocument,
    ValidationIssue,
};

pub const DIALOGUE_AUTHORING_FORMAT_VERSION: u32 = 1;
pub const PROJECT_DIALOGUE_LIBRARY_FORMAT_VERSION: u32 = 1;
pub const PROJECT_DIALOGUE_LIBRARY_PATH: &str = "editor/dialogue.library.json";
pub const GENERATED_DIALOGUE_SCRIPT_PATH: &[u8] = b"map/sp/graffito_dialogue.sb";
pub const GENERATED_DIALOGUE_SCRIPT_MARKER: &str = "__graffito_dialogue_v1";
pub const GENERATED_DIALOGUE_RUNTIME_NAME_PREFIX: &str = "GraffitoDlg_";
pub const STAGE_DIALOGUE_MESSAGE_PATH: &[u8] = b"map/message.bmg";
pub const SYSTEM_DIALOGUE_MESSAGE_PATH: &[u8] = b"2d/sys_message.bmg";
pub const BALLOON_DIALOGUE_MESSAGE_PATH: &[u8] = b"2d/balloon.bmg";
/// Semantic source identity for the stock post-script happy/reward selection
/// in `TTalk2D2::setMessageID`.  This is intentionally not a regional DOL
/// address: the runtime patcher locates the same convergence independently.
const STOCK_HAPPY_DIALOGUE_SOURCE_PATH: &[u8] = b"sys/main.dol/TTalk2D2::setMessageID";

// Derived from `TMarNameRefGen::getNameRef_NPC` and the predicates used by
// `TTalk2D2::setMessageID` in the adjacent decompilation.
const NPC_ACTOR_TYPE_MONTE_M_FIRST: u32 = 0x0400_0001;
const NPC_ACTOR_TYPE_MONTE_M_LAST: u32 = 0x0400_0009;
const NPC_ACTOR_TYPE_MONTE_W_FIRST: u32 = 0x0400_000a;
const NPC_ACTOR_TYPE_MONTE_W_LAST: u32 = 0x0400_000d;
const NPC_ACTOR_TYPE_MARE_M_FIRST: u32 = 0x0400_000e;
const NPC_ACTOR_TYPE_MARE_M_LAST: u32 = 0x0400_0012;
const NPC_ACTOR_TYPE_MARE_W_FIRST: u32 = 0x0400_0013;
const NPC_ACTOR_TYPE_MARE_W_LAST: u32 = 0x0400_0015;
const NPC_ACTOR_TYPE_KINOPIO: u32 = 0x0400_0016;
const NPC_ACTOR_TYPE_MARE_MB_OVERRIDE: u32 = 0x0400_0010;
const NPC_ACTOR_TYPE_RACCOON_DOG: u32 = 0x0400_0019;
const NPC_ACTOR_TYPE_SUNFLOWER_SMALL: u32 = 0x0400_001b;
const NPC_ACTOR_TYPE_BOARD: u32 = 0x0400_001d;

// The two exact per-instance overrides initialized by TTalk2D2. These names
// are data from the decompilation, not stage-specific editor routing tables.
const STOCK_HAPPY_AIRPORT_MONTE_NAME: &str = "空港沈みモンテ";
const STOCK_HAPPY_MONTE_ONE_NAME: &str = "モンテ1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogueAuthoringDocument {
    #[serde(default = "dialogue_authoring_format_version")]
    pub format_version: u32,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub objects: BTreeMap<String, DialogueObjectAuthoring>,
}

impl Default for DialogueAuthoringDocument {
    fn default() -> Self {
        Self {
            format_version: DIALOGUE_AUTHORING_FORMAT_VERSION,
            objects: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogueObjectAuthoring {
    /// A duplicated actor initially consumes the same resolved routes as this
    /// object.  Its first instance edit becomes an independent override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inherited_from_object_id: Option<String>,
    /// Original placed-object name retained while an editor-owned generated
    /// route temporarily replaces it with a collision-resistant identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_runtime_name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overrides: Vec<DialogueVariantOverride>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stable_allocations: Vec<DialogueStableAllocation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogueVariantOverride {
    pub key: DialogueVariantKey,
    #[serde(default)]
    pub scope: DialogueEditScope,
    pub route_kind: DialogueRouteKind,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub condition_path: String,
    pub content: DialogueContent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogueStableAllocation {
    pub key: DialogueVariantKey,
    pub message_index: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectDialogueLibrary {
    #[serde(default = "project_dialogue_library_format_version")]
    pub format_version: u32,
    /// Shared semantic overrides for resources in `common.szs`.  These are
    /// deltas, not copied retail BMG documents.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub common_overrides: Vec<ProjectDialogueOverride>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stable_allocations: Vec<ProjectDialogueAllocation>,
}

impl Default for ProjectDialogueLibrary {
    fn default() -> Self {
        Self {
            format_version: PROJECT_DIALOGUE_LIBRARY_FORMAT_VERSION,
            common_overrides: Vec::new(),
            stable_allocations: Vec::new(),
        }
    }
}

impl ProjectDialogueLibrary {
    pub fn is_empty(&self) -> bool {
        self.common_overrides.is_empty() && self.stable_allocations.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectDialogueOverride {
    pub message: DialogueMessageRef,
    pub content: DialogueContent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectDialogueAllocation {
    pub stage_id: String,
    pub object_id: String,
    pub key: DialogueVariantKey,
    pub message_index: u16,
    /// Common BMG identity. Defaults preserve the first development version,
    /// in which project allocations were balloon-only.
    #[serde(default = "default_project_allocation_domain")]
    pub domain: DialogueDomain,
    #[serde(default = "default_project_allocation_path")]
    pub raw_resource_path: Vec<u8>,
    /// The materialized clone is project data, not a copied retail archive.
    /// Keeping it here lets any stage rebuild every other stage's common-BMG
    /// clones deterministically. `None` is a retained, non-reusable tombstone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<DialogueContent>,
}

fn default_project_allocation_domain() -> DialogueDomain {
    DialogueDomain::Balloon
}

fn default_project_allocation_path() -> Vec<u8> {
    BALLOON_DIALOGUE_MESSAGE_PATH.to_vec()
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DialogueVariantKey {
    pub source: DialogueSourceAnchor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_message: Option<DialogueMessageRef>,
}

impl DialogueVariantKey {
    pub fn generated_for_object(object_id: &str) -> Self {
        Self {
            source: DialogueSourceAnchor {
                raw_resource_path: GENERATED_DIALOGUE_SCRIPT_PATH.to_vec(),
                function_symbol: "graffito_dialogue".to_string(),
                normalized_fingerprint: stable_text_fingerprint(object_id),
                callsite_occurrence: 0,
                original_message_id: None,
            },
            original_message: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DialogueSourceAnchor {
    pub raw_resource_path: Vec<u8>,
    pub function_symbol: String,
    pub normalized_fingerprint: u64,
    pub callsite_occurrence: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_message_id: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DialogueMessageRef {
    pub domain: DialogueDomain,
    pub raw_resource_path: Vec<u8>,
    pub full_message_id: u32,
    pub entry_index: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DialogueDomain {
    Stage,
    System,
    Balloon,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DialogueEditScope {
    #[default]
    Instance,
    Shared,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DialogueRouteKind {
    Normal,
    Choice,
    Forced,
    HappyOverride,
    Balloon,
    BoardOrSign,
    Shop,
    Generated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DialogueContent {
    pub message: BmgMessage,
    /// Optional editor-level tokens. Unlike encoded BMG controls, these can
    /// retain temporarily invalid choice text for project save; export alone
    /// performs Shift-JIS/control encoding and blocks invalid drafts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authored_tokens: Option<Vec<DialogueAuthoringToken>>,
    /// Exact INF1 bytes.  Unknown presentation fields stay opaque and are
    /// always retained.  In SMS's 12-byte entry layout, byte four is the
    /// talk-sound ordinal exposed separately below.
    pub attributes: Vec<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_index: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum DialogueAuthoringToken {
    Text(String),
    Control(SmsBmgControl),
    /// Editor-level page boundary. `line_count` is derived from Sunshine's
    /// presentation path when the token is inserted: ordinary talk windows
    /// consume three lines, while `setupBoardTextBox` consumes six.
    PageBreak {
        line_count: u8,
    },
}

impl DialogueContent {
    pub fn from_bmg_entry(entry: &sms_formats::BmgEntry) -> Self {
        Self {
            message: entry.message.clone(),
            authored_tokens: None,
            attributes: entry.attributes.clone(),
            voice_index: entry.attributes.get(4).copied(),
        }
    }

    pub fn with_voice_index(mut self, voice_index: Option<u8>) -> Self {
        self.voice_index = voice_index;
        if let (Some(target), Some(value)) = (self.attributes.get_mut(4), voice_index) {
            *target = value;
        }
        self
    }

    pub fn with_authored_tokens(mut self, tokens: Vec<DialogueAuthoringToken>) -> Self {
        self.authored_tokens = Some(tokens);
        self
    }

    pub fn compiled_message(&self) -> sms_formats::Result<BmgMessage> {
        let Some(tokens) = self.authored_tokens.as_ref() else {
            return Ok(self.message.clone());
        };
        let mut compiled = Vec::with_capacity(tokens.len());
        let mut lines_since_page_break = 0usize;
        for token in tokens {
            match token {
                DialogueAuthoringToken::Text(text) => {
                    lines_since_page_break += text.matches('\n').count();
                    push_compiled_dialogue_text(&mut compiled, text);
                }
                DialogueAuthoringToken::Control(control) => compiled.push(
                    sms_formats::BmgMessageToken::from_sms_control(control.clone())?,
                ),
                DialogueAuthoringToken::PageBreak { line_count } => {
                    let page_lines = usize::from(*line_count);
                    if !matches!(page_lines, 3 | 6) {
                        return Err(sms_formats::FormatError::Unsupported {
                            format: "SMS dialogue authoring",
                            message: format!(
                                "page breaks require a decomp-confirmed 3- or 6-line presentation, got {page_lines}"
                            ),
                        });
                    }
                    let line_in_page = lines_since_page_break % page_lines;
                    push_compiled_dialogue_text(
                        &mut compiled,
                        &"\n".repeat(page_lines - line_in_page),
                    );
                    lines_since_page_break = 0;
                }
            }
        }
        Ok(BmgMessage { tokens: compiled })
    }
}

fn push_compiled_dialogue_text(tokens: &mut Vec<sms_formats::BmgMessageToken>, text: &str) {
    if let Some(sms_formats::BmgMessageToken::Text(previous)) = tokens.last_mut() {
        previous.push_str(text);
    } else {
        tokens.push(sms_formats::BmgMessageToken::Text(text.to_string()));
    }
}

/// A derived route.  This type deliberately has no serde implementation: the
/// script and BMG resources remain authoritative and the index must be rebuilt
/// after resource changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogueVariant {
    pub stage_id: String,
    pub object_id: String,
    pub runtime_name: String,
    pub key: DialogueVariantKey,
    pub route_kind: DialogueRouteKind,
    pub condition_path: String,
    pub message: Option<DialogueMessageRef>,
    pub content: DialogueContent,
    pub presentation_flags: Option<u32>,
    pub talk_flags: Option<u32>,
    pub shared_consumers: Vec<DialogueConsumer>,
    pub provenance: DialogueProvenance,
    compiler_location: DialogueCompilerLocation,
    message_storage_offset: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DialogueConsumer {
    pub stage_id: String,
    pub object_id: String,
    pub variant_key: DialogueVariantKey,
    /// Decomp-derived text-window capacity for this consumer.
    pub page_line_count: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogueProvenance {
    ScriptBuiltin { symbol: String },
    Generated,
    RuntimeOverride,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DialogueCompilerLocation {
    script_path: Vec<u8>,
    message_instruction_index: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DialogueRouteIndex {
    variants_by_object: BTreeMap<String, Vec<DialogueVariant>>,
    /// Message-setting callsites which are semantically understood but do not
    /// belong to a placed editor object. They stay out of the inspector while
    /// still participating in base-wide shared-edit impact reporting.
    detached_consumers: Vec<DetachedDialogueConsumer>,
    pub issues: Vec<DialogueResolutionIssue>,
    pub callsites: Vec<DialogueCallsiteClassification>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DetachedDialogueConsumer {
    message: DialogueMessageRef,
    message_storage_offset: u32,
    consumer: DialogueConsumer,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DialogueGameConsumerIndex {
    consumers_by_storage: BTreeMap<DialogueConsumerGroupKey, Vec<DialogueConsumer>>,
    storage_by_message: BTreeMap<DialogueConsumerLookupKey, DialogueConsumerGroupKey>,
    storage_by_route: BTreeMap<DialogueConsumerRouteLookupKey, DialogueConsumerGroupKey>,
    shared_rejoins_by_route: BTreeMap<DialogueConsumerRouteLookupKey, Vec<DialogueConsumer>>,
}

impl DialogueGameConsumerIndex {
    /// Returns every consumer sharing the selected entry's DAT1 payload.
    /// Stage BMG identities are scoped by `stage_id`; common/system/balloon
    /// resources are global and intentionally ignore it.
    pub fn consumers(&self, stage_id: &str, message: &DialogueMessageRef) -> &[DialogueConsumer] {
        let lookup = DialogueConsumerLookupKey::new(stage_id, message);
        self.storage_by_message
            .get(&lookup)
            .and_then(|storage| self.consumers_by_storage.get(storage))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// Returns the consumers of the selected route's effective message. This
    /// differs from [`Self::consumers`] after a persisted instance edit has
    /// redirected the route to a copy-on-write allocation.
    pub fn consumers_for_variant(&self, variant: &DialogueVariant) -> &[DialogueConsumer] {
        let lookup = DialogueConsumerRouteLookupKey::new(variant);
        self.storage_by_route
            .get(&lookup)
            .and_then(|storage| self.consumers_by_storage.get(storage))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// Returns the exact impact of converting this route to a shared edit.
    /// The operation mutates the retail entry, so it affects current consumers
    /// of that entry plus this actor and descendants that will stop following
    /// its copy-on-write allocation when the instance override is removed.
    pub fn shared_edit_consumers_for_variant(
        &self,
        variant: &DialogueVariant,
    ) -> Vec<DialogueConsumer> {
        let mut consumers = variant
            .message
            .as_ref()
            .map(|message| self.consumers(&variant.stage_id, message).to_vec())
            .unwrap_or_default();
        if let Some(rejoining) = self
            .shared_rejoins_by_route
            .get(&DialogueConsumerRouteLookupKey::new(variant))
        {
            consumers.extend(rejoining.iter().cloned());
        }
        consumers.sort();
        consumers.dedup();
        consumers
    }

    pub fn message_count(&self) -> usize {
        self.storage_by_message.len()
    }

    #[cfg(test)]
    fn add_variant(&mut self, variant: &DialogueVariant) -> Result<()> {
        self.add_variant_with_allocation(variant, None)
    }

    fn add_variant_with_allocation(
        &mut self,
        variant: &DialogueVariant,
        allocation: Option<&EffectiveDialogueAllocation>,
    ) -> Result<()> {
        let (Some(message), Some(storage_offset)) =
            (variant.message.as_ref(), variant.message_storage_offset)
        else {
            return Ok(());
        };
        let lookup = DialogueConsumerLookupKey::new(&variant.stage_id, message);
        let retail_storage =
            DialogueConsumerGroupKey::retail(&variant.stage_id, message, storage_offset);
        if let Some(previous) = self
            .storage_by_message
            .insert(lookup, retail_storage.clone())
        {
            if previous != retail_storage {
                return Err(SceneError::StageExport(format!(
                    "dialogue message {} in {} resolved to conflicting DAT1 storage offsets",
                    message.entry_index,
                    String::from_utf8_lossy(&message.raw_resource_path)
                )));
            }
        }
        let effective_storage = allocation
            .map(|allocation| DialogueConsumerGroupKey::allocated(&variant.stage_id, allocation))
            .unwrap_or(retail_storage);
        let route_lookup = DialogueConsumerRouteLookupKey::new(variant);
        if let Some(previous) = self
            .storage_by_route
            .insert(route_lookup, effective_storage.clone())
        {
            if previous != effective_storage {
                return Err(SceneError::StageExport(format!(
                    "dialogue route for object {:?} resolved to conflicting effective message storage",
                    variant.object_id
                )));
            }
        }
        self.consumers_by_storage
            .entry(effective_storage)
            .or_default()
            .push(DialogueConsumer {
                stage_id: variant.stage_id.clone(),
                object_id: variant.object_id.clone(),
                variant_key: variant.key.clone(),
                page_line_count: dialogue_route_page_line_count(variant.route_kind),
            });
        Ok(())
    }

    fn add_detached(&mut self, detached: &DetachedDialogueConsumer) -> Result<()> {
        let message = &detached.message;
        let consumer = &detached.consumer;
        let lookup = DialogueConsumerLookupKey::new(&consumer.stage_id, message);
        let storage = DialogueConsumerGroupKey::retail(
            &consumer.stage_id,
            message,
            detached.message_storage_offset,
        );
        if let Some(previous) = self.storage_by_message.insert(lookup, storage.clone()) {
            if previous != storage {
                return Err(SceneError::StageExport(format!(
                    "dialogue message {} in {} resolved to conflicting DAT1 storage offsets",
                    message.entry_index,
                    String::from_utf8_lossy(&message.raw_resource_path)
                )));
            }
        }
        self.consumers_by_storage
            .entry(storage)
            .or_default()
            .push(consumer.clone());
        Ok(())
    }

    fn add_shared_edit_rejoins(&mut self, document: &StageDocument, routes: &DialogueRouteIndex) {
        for selected in routes
            .all_variants()
            .filter(|variant| variant.message.is_some())
        {
            let rejoining = routes
                .all_variants()
                .filter(|candidate| {
                    candidate.key == selected.key
                        && dialogue_route_rejoins_with_shared_override(
                            document,
                            &candidate.object_id,
                            &selected.object_id,
                            &selected.key,
                        )
                })
                .map(|candidate| DialogueConsumer {
                    stage_id: candidate.stage_id.clone(),
                    object_id: candidate.object_id.clone(),
                    variant_key: candidate.key.clone(),
                    page_line_count: dialogue_route_page_line_count(candidate.route_kind),
                })
                .collect::<Vec<_>>();
            self.shared_rejoins_by_route
                .insert(DialogueConsumerRouteLookupKey::new(selected), rejoining);
        }
    }

    fn finish(&mut self) {
        for consumers in self.consumers_by_storage.values_mut() {
            consumers.sort();
            consumers.dedup();
        }
        for consumers in self.shared_rejoins_by_route.values_mut() {
            consumers.sort();
            consumers.dedup();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct DialogueConsumerRouteLookupKey {
    stage_id: String,
    object_id: String,
    variant_key: DialogueVariantKey,
}

impl DialogueConsumerRouteLookupKey {
    fn new(variant: &DialogueVariant) -> Self {
        Self {
            stage_id: variant.stage_id.clone(),
            object_id: variant.object_id.clone(),
            variant_key: variant.key.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct DialogueConsumerLookupKey {
    stage_scope: Option<String>,
    message: DialogueMessageRef,
}

impl DialogueConsumerLookupKey {
    fn new(stage_id: &str, message: &DialogueMessageRef) -> Self {
        Self {
            stage_scope: (message.domain == DialogueDomain::Stage).then(|| stage_id.to_string()),
            message: message.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct DialogueConsumerGroupKey {
    stage_scope: Option<String>,
    domain: DialogueDomain,
    raw_resource_path: Vec<u8>,
    storage: DialogueConsumerStorage,
}

impl DialogueConsumerGroupKey {
    fn retail(stage_id: &str, message: &DialogueMessageRef, message_storage_offset: u32) -> Self {
        Self {
            stage_scope: (message.domain == DialogueDomain::Stage).then(|| stage_id.to_string()),
            domain: message.domain,
            raw_resource_path: normalized_raw_path(&message.raw_resource_path),
            storage: DialogueConsumerStorage::RetailDat1Offset(message_storage_offset),
        }
    }

    fn allocated(stage_id: &str, allocation: &EffectiveDialogueAllocation) -> Self {
        Self {
            stage_scope: (allocation.domain == DialogueDomain::Stage).then(|| stage_id.to_string()),
            domain: allocation.domain,
            raw_resource_path: normalized_raw_path(&allocation.raw_resource_path),
            storage: DialogueConsumerStorage::AllocatedEntry(allocation.message_index),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum DialogueConsumerStorage {
    RetailDat1Offset(u32),
    AllocatedEntry(u16),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EffectiveDialogueAllocation {
    domain: DialogueDomain,
    raw_resource_path: Vec<u8>,
    message_index: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogueCallsiteClassification {
    pub script_path: Vec<u8>,
    pub function_symbol: String,
    pub setter_symbol: String,
    pub status: DialogueCallsiteStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogueCallsiteStatus {
    Resolved,
    DynamicHelper,
    GlobalOrUnplaced,
    Ambiguous,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogueResolutionIssue {
    pub severity: DialogueResolutionSeverity,
    pub code: &'static str,
    pub message: String,
    pub script_path: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogueResolutionSeverity {
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeDialogueOverride {
    pub area: u8,
    pub scenario: u8,
    /// `TLiveActor::mInstanceIndex`, counted within the actor's resolved live
    /// manager after final depth-first placement reconciliation.
    pub manager_instance_index: i16,
    pub original_message_id: u32,
    pub replacement_message_id: u32,
    pub guard: RuntimeDialogueGuard,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeDialogueGuard {
    pub actor_type: u32,
    /// Exact bytes compared by the runtime hook.  A Unicode string or hash is
    /// insufficient because JDrama names are Shift-JIS byte sequences.
    pub runtime_name_shift_jis: Vec<u8>,
    pub instance_index: i16,
    pub reset_position_bits: [u32; 3],
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CompiledDialogueEdits {
    /// Resource upserts for the selected stage archive only.
    pub stage_edits: StageArchiveEdits,
    /// Semantic BMG replacements to apply to the managed run-root copy of
    /// `common.szs`. The extracted base archive is never a write target.
    pub common_resources: Vec<CommonDialogueResourceEdit>,
    /// Requests whose final area/scenario/DFS ordinal and manager instance
    /// index must be resolved after placement reconciliation.
    pub runtime_override_requests: Vec<RuntimeDialogueOverrideRequest>,
}

impl CompiledDialogueEdits {
    pub fn is_noop(&self) -> bool {
        self.stage_edits == StageArchiveEdits::default()
            && self.common_resources.is_empty()
            && self.runtime_override_requests.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommonDialogueResourceEdit {
    pub raw_resource_path: Vec<u8>,
    pub document: BmgFile,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeDialogueOverrideRequest {
    pub stage_id: String,
    pub object_id: String,
    pub source: Option<sms_formats::SourceLocation>,
    pub placement: Option<crate::PlacementBinding>,
    pub factory_name: String,
    pub actor_type: u32,
    pub runtime_name: String,
    pub runtime_name_shift_jis: Vec<u8>,
    pub reset_position_bits: [u32; 3],
    pub route_kind: DialogueRouteKind,
    pub domain: DialogueDomain,
    pub original_message_id: Option<u32>,
    pub replacement_message_id: u32,
}

impl DialogueRouteIndex {
    pub fn variants_for_object(&self, object_id: &str) -> &[DialogueVariant] {
        self.variants_by_object
            .get(object_id)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn object_ids(&self) -> impl Iterator<Item = &str> {
        self.variants_by_object.keys().map(String::as_str)
    }

    pub fn find_variant(
        &self,
        object_id: &str,
        key: &DialogueVariantKey,
    ) -> Option<&DialogueVariant> {
        self.variants_for_object(object_id)
            .iter()
            .find(|variant| &variant.key == key)
    }

    pub fn all_variants(&self) -> impl Iterator<Item = &DialogueVariant> {
        self.variants_by_object.values().flatten()
    }

    pub fn has_errors(&self) -> bool {
        self.issues
            .iter()
            .any(|issue| issue.severity == DialogueResolutionSeverity::Error)
    }
}

fn dialogue_route_page_line_count(route_kind: DialogueRouteKind) -> u8 {
    if route_kind == DialogueRouteKind::BoardOrSign {
        6
    } else {
        3
    }
}

fn ensure_dialogue_consumer_index_not_cancelled(cancelled: &AtomicBool) -> Result<()> {
    if cancelled.load(Ordering::Relaxed) {
        Err(SceneError::StageExport(
            "dialogue consumer-index rebuild was superseded".to_string(),
        ))
    } else {
        Ok(())
    }
}

impl StageDocument {
    /// Returns the line capacity used by Sunshine's selected presentation.
    /// Board actors keep their six-line behavior even for editor-generated
    /// routes, whose route kind is necessarily `Generated`.
    pub fn dialogue_page_line_count(&self, object_id: &str, route_kind: DialogueRouteKind) -> u8 {
        let is_board_actor = self
            .objects
            .iter()
            .find(|object| object.id == object_id)
            .and_then(|object| {
                self.registry
                    .as_ref()
                    .and_then(|registry| registry.find_npc_actor_type(&object.factory_name))
            })
            == Some(NPC_ACTOR_TYPE_BOARD);
        if is_board_actor {
            6
        } else {
            dialogue_route_page_line_count(route_kind)
        }
    }

    pub fn requires_game_dialogue_consumer_validation(&self) -> bool {
        self.dialogue_library
            .common_overrides
            .iter()
            .any(|shared| !dialogue_page_break_line_counts(&shared.content).is_empty())
            || self.dialogue_authoring.as_ref().is_some_and(|authoring| {
                authoring.objects.values().any(|object| {
                    object.overrides.iter().any(|authored| {
                        authored.scope == DialogueEditScope::Shared
                            && authored
                                .key
                                .original_message
                                .as_ref()
                                .is_some_and(|message| message.domain == DialogueDomain::Stage)
                            && !dialogue_page_break_line_counts(&authored.content).is_empty()
                    })
                })
            })
    }

    /// Rejects a shared page break when any base-wide consumer uses a
    /// different Sunshine presentation height. One encoded BMG payload cannot
    /// simultaneously advance both a three-line talk window and a six-line
    /// board window correctly.
    pub fn validate_game_dialogue_consumer_presentations(
        &self,
        consumers: &DialogueGameConsumerIndex,
    ) -> Result<()> {
        let validate_shared = |label: &str,
                               message: &DialogueMessageRef,
                               content: &DialogueContent| {
            let authored_counts = dialogue_page_break_line_counts(content);
            if authored_counts.is_empty() {
                return Ok(());
            }
            let affected = consumers.consumers(&self.stage_id, message);
            if affected.is_empty() {
                return Err(SceneError::StageExport(format!(
                    "{label} has page breaks but no base-wide dialogue consumers"
                )));
            }
            for consumer in affected {
                for authored_count in &authored_counts {
                    if *authored_count != consumer.page_line_count {
                        return Err(SceneError::StageExport(format!(
                                "{label} has a {authored_count}-line page break, but consumer {:?}/{:?} uses a {}-line presentation",
                                consumer.stage_id, consumer.object_id, consumer.page_line_count
                            )));
                    }
                }
            }
            Ok(())
        };

        for shared in &self.dialogue_library.common_overrides {
            validate_shared(
                &format!(
                    "common dialogue message {} in {}",
                    shared.message.entry_index,
                    String::from_utf8_lossy(&shared.message.raw_resource_path)
                ),
                &shared.message,
                &shared.content,
            )?;
        }
        if let Some(authoring) = &self.dialogue_authoring {
            for (object_id, object) in &authoring.objects {
                for authored in object.overrides.iter().filter(|authored| {
                    authored.scope == DialogueEditScope::Shared
                        && authored
                            .key
                            .original_message
                            .as_ref()
                            .is_some_and(|message| message.domain == DialogueDomain::Stage)
                }) {
                    if let Some(message) = authored.key.original_message.as_ref() {
                        validate_shared(
                            &format!("shared dialogue override for object {object_id:?}"),
                            message,
                            &authored.content,
                        )?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Builds the complete derived index from effective stage resources and
    /// the extracted base's `common.szs`.  Callers should cache the result;
    /// this performs archive IO and symbolic script analysis.
    pub fn build_dialogue_route_index(&self) -> Result<DialogueRouteIndex> {
        let common = collect_common_dialogue_resources(&self.base_root)?;
        self.build_dialogue_route_index_with_common(&common)
    }

    fn build_dialogue_route_index_with_common(
        &self,
        common: &DialogueResourceSet,
    ) -> Result<DialogueRouteIndex> {
        let mut resources = collect_stage_dialogue_resources(self)?;
        resources.extend(common.clone());
        let mut index = resolve_dialogue_routes(
            &self.stage_id,
            &self.objects,
            self.registry.as_ref(),
            &resources,
        );
        // Source-derived empty fallbacks must exist before duplicate
        // inheritance is evaluated, including for originals whose only stock
        // route is forced/cutscene-only. Authored and inherited targets are
        // skipped here and materialized by the next pass.
        classify_unresolved_talk_capable_objects(self, &mut index);
        apply_authored_and_inherited_routes(self, &mut index);
        attach_sharing_metadata(&mut index);
        Ok(index)
    }

    /// Builds the game-wide impact list used by explicit shared edits. This
    /// fails instead of returning an incomplete count when any retail stage
    /// cannot be indexed. Callers should run and cache it off the UI thread.
    pub fn build_game_dialogue_consumer_index(&self) -> Result<DialogueGameConsumerIndex> {
        self.build_game_dialogue_consumer_index_with_cancel(&AtomicBool::new(false))
    }

    /// Cancel-aware form used by the editor's coalesced background worker.
    /// Cancellation is checked between stage snapshots so superseded workers
    /// stop reopening the live project while a newer edit or save is pending.
    pub fn build_game_dialogue_consumer_index_with_cancel(
        &self,
        cancelled: &AtomicBool,
    ) -> Result<DialogueGameConsumerIndex> {
        ensure_dialogue_consumer_index_not_cancelled(cancelled)?;
        let registry = self.registry.clone();
        let project_root = self
            .loaded_project
            .as_ref()
            .map(|loaded| loaded.project_root.clone());
        let retail_stage_ids = discover_scene_archives(&self.base_root)?
            .into_iter()
            .map(|stage| stage.stage_id)
            .collect::<BTreeSet<_>>();
        let mut stage_ids = retail_stage_ids.clone();
        stage_ids.insert(self.stage_id.clone());
        if let Some(project_root) = project_root.as_ref() {
            stage_ids.extend(crate::discover_authored_project_stage_ids(project_root)?);
        }
        ensure_dialogue_consumer_index_not_cancelled(cancelled)?;
        let common = collect_common_dialogue_resources(&self.base_root)?;
        let mut result = DialogueGameConsumerIndex::default();
        for stage_id in stage_ids {
            ensure_dialogue_consumer_index_not_cancelled(cancelled)?;
            let mut document = if stage_id == self.stage_id {
                self.clone()
            } else if retail_stage_ids.contains(&stage_id) {
                let mut document = StageDocument::open(&self.base_root, &stage_id)?;
                if let Some(project_root) = project_root.as_ref() {
                    document.load_project_folder(project_root)?;
                }
                document
            } else {
                let project_root = project_root.as_ref().ok_or_else(|| {
                    SceneError::StageExport(format!(
                        "dialogue consumer census cannot reopen authored stage {stage_id:?} without a loaded project"
                    ))
                })?;
                StageDocument::open_authored_project_stage(
                    &self.base_root,
                    &stage_id,
                    project_root,
                )?
            };
            // The current in-memory common library wins over the persisted
            // copy so unsaved Edit-all work is represented consistently.
            document.dialogue_library = self.dialogue_library.clone();
            if let Some(registry) = registry.clone() {
                document.registry = Some(registry);
            }
            let routes = document.build_dialogue_route_index_with_common(&common)?;
            ensure_dialogue_consumer_index_not_cancelled(cancelled)?;
            let blocking = routes
                .issues
                .iter()
                .filter(|issue| issue.severity == DialogueResolutionSeverity::Error)
                .map(|issue| format!("{}: {}", issue.code, issue.message))
                .collect::<Vec<_>>();
            if !blocking.is_empty() {
                return Err(SceneError::StageExport(format!(
                    "game-wide dialogue consumer census stopped at stage {:?}: {}",
                    document.stage_id,
                    blocking.join("; ")
                )));
            }
            for variant in routes.all_variants() {
                let allocation = effective_dialogue_consumer_allocation(&document, variant);
                result.add_variant_with_allocation(variant, allocation.as_ref())?;
            }
            result.add_shared_edit_rejoins(&document, &routes);
            for detached in &routes.detached_consumers {
                result.add_detached(detached)?;
            }
        }
        result.finish();
        Ok(result)
    }

    pub fn set_dialogue_override(
        &mut self,
        object_id: &str,
        key: DialogueVariantKey,
        scope: DialogueEditScope,
        route_kind: DialogueRouteKind,
        condition_path: impl Into<String>,
        content: DialogueContent,
    ) -> Result<()> {
        if !self.objects.iter().any(|object| object.id == object_id) {
            return Err(SceneError::StageExport(format!(
                "dialogue object {object_id:?} was not found"
            )));
        }
        let adopted_generated_identity =
            if scope == DialogueEditScope::Instance && key.original_message.is_none() {
                let generated_runtime_name =
                    self.available_generated_dialogue_runtime_name(object_id)?;
                let placed_object = self
                    .objects
                    .iter_mut()
                    .find(|object| object.id == object_id)
                    .expect("dialogue object existence was checked");
                let current_runtime_name = placed_object
                    .raw_param("name")
                    .unwrap_or(&placed_object.id)
                    .to_string();
                if current_runtime_name.starts_with(GENERATED_DIALOGUE_RUNTIME_NAME_PREFIX) {
                    None
                } else {
                    placed_object.set_raw_param("name", generated_runtime_name);
                    Some(current_runtime_name)
                }
            } else {
                None
            };
        if scope == DialogueEditScope::Shared
            && key
                .original_message
                .as_ref()
                .is_some_and(|message| message.domain != DialogueDomain::Stage)
        {
            let message = key.original_message.clone().expect("checked above");
            if let Some(existing) = self
                .dialogue_library
                .common_overrides
                .iter_mut()
                .find(|existing| existing.message == message)
            {
                existing.content = content.clone();
            } else {
                self.dialogue_library
                    .common_overrides
                    .push(ProjectDialogueOverride {
                        message,
                        content: content.clone(),
                    });
            }
            self.dialogue_library
                .common_overrides
                .sort_by(|left, right| left.message.cmp(&right.message));
        }

        let authoring = self
            .dialogue_authoring
            .get_or_insert_with(DialogueAuthoringDocument::default);
        let object = authoring.objects.entry(object_id.to_string()).or_default();
        if object.prior_runtime_name.is_none() {
            object.prior_runtime_name = adopted_generated_identity;
        }
        if let Some(existing) = object
            .overrides
            .iter_mut()
            .find(|existing| existing.key == key)
        {
            *existing = DialogueVariantOverride {
                key,
                scope,
                route_kind,
                condition_path: condition_path.into(),
                content,
            };
        } else {
            object.overrides.push(DialogueVariantOverride {
                key,
                scope,
                route_kind,
                condition_path: condition_path.into(),
                content,
            });
            object
                .overrides
                .sort_by(|left, right| left.key.cmp(&right.key));
        }
        Ok(())
    }

    pub fn remove_dialogue_override(
        &mut self,
        object_id: &str,
        key: &DialogueVariantKey,
        scope: DialogueEditScope,
    ) -> bool {
        if let Some(message) = (scope == DialogueEditScope::Shared)
            .then_some(key.original_message.as_ref())
            .flatten()
            .filter(|message| message.domain != DialogueDomain::Stage)
        {
            let before = self.dialogue_library.common_overrides.len();
            self.dialogue_library
                .common_overrides
                .retain(|candidate| &candidate.message != message);
            let removed_library = before != self.dialogue_library.common_overrides.len();
            let mut removed_markers = false;
            if let Some(authoring) = self.dialogue_authoring.as_mut() {
                for object in authoring.objects.values_mut() {
                    let before = object.overrides.len();
                    object.overrides.retain(|candidate| {
                        candidate.scope != DialogueEditScope::Shared
                            || candidate.key.original_message.as_ref() != Some(message)
                    });
                    removed_markers |= before != object.overrides.len();
                }
                authoring.objects.retain(|_, object| {
                    !object.overrides.is_empty()
                        || !object.stable_allocations.is_empty()
                        || object.inherited_from_object_id.is_some()
                        || object.prior_runtime_name.is_some()
                });
            }
            return removed_library || removed_markers;
        }
        let inherited_generated_route = self
            .dialogue_authoring
            .as_ref()
            .map(|authoring| {
                let mut visited = BTreeSet::new();
                let mut current = authoring
                    .objects
                    .get(object_id)
                    .and_then(|object| object.inherited_from_object_id.as_deref());
                while let Some(candidate) = current {
                    if !visited.insert(candidate) {
                        return false;
                    }
                    let Some(object) = authoring.objects.get(candidate) else {
                        return false;
                    };
                    if object
                        .overrides
                        .iter()
                        .any(|override_| override_.key.original_message.is_none())
                    {
                        return true;
                    }
                    current = object.inherited_from_object_id.as_deref();
                }
                false
            })
            .unwrap_or(false);
        let (removed, restored_runtime_name, remove_authoring_object, promoted_generated_override) = {
            let Some(authoring) = self.dialogue_authoring.as_mut() else {
                return false;
            };
            let Some(object) = authoring.objects.get_mut(object_id) else {
                return false;
            };
            let removed_override = object
                .overrides
                .iter()
                .find(|candidate| &candidate.key == key && candidate.scope == scope)
                .cloned();
            let before = object.overrides.len();
            object
                .overrides
                .retain(|candidate| &candidate.key != key || candidate.scope != scope);
            let removed = before != object.overrides.len();
            let removes_final_generated_override = removed
                && key.original_message.is_none()
                && !inherited_generated_route
                && !object
                    .overrides
                    .iter()
                    .any(|candidate| candidate.key.original_message.is_none());
            let restored_runtime_name = removes_final_generated_override
                .then(|| object.prior_runtime_name.take())
                .flatten();
            let remove_authoring_object = object.overrides.is_empty()
                && object.stable_allocations.is_empty()
                && object.inherited_from_object_id.is_none()
                && object.prior_runtime_name.is_none();
            (
                removed,
                restored_runtime_name,
                remove_authoring_object,
                removes_final_generated_override
                    .then_some(removed_override)
                    .flatten(),
            )
        };
        if let Some(promoted) = promoted_generated_override {
            let authoring = self
                .dialogue_authoring
                .as_mut()
                .expect("authoring existed above");
            for (dependent_id, dependent) in &mut authoring.objects {
                if dependent.inherited_from_object_id.as_deref() != Some(object_id)
                    || dependent
                        .overrides
                        .iter()
                        .any(|override_| override_.key.original_message.is_none())
                {
                    continue;
                }
                let mut independent = promoted.clone();
                independent.key = DialogueVariantKey::generated_for_object(dependent_id);
                dependent.overrides.push(independent);
                dependent
                    .overrides
                    .sort_by(|left, right| left.key.cmp(&right.key));
                dependent.inherited_from_object_id = None;
            }
        }
        if let Some(restored_runtime_name) = restored_runtime_name {
            if let Some(object) = self
                .objects
                .iter_mut()
                .find(|object| object.id == object_id)
            {
                object.set_raw_param("name", restored_runtime_name);
            }
        }
        if remove_authoring_object {
            self.dialogue_authoring
                .as_mut()
                .expect("authoring existed above")
                .objects
                .remove(object_id);
        }
        if removed {
            for allocation in &mut self.dialogue_library.stable_allocations {
                if allocation.stage_id == self.stage_id
                    && allocation.object_id == object_id
                    && allocation.key == *key
                {
                    // Keep the ordinal permanently reserved, but do not keep
                    // materializing an obsolete balloon/common clone after
                    // its instance override is reset.
                    allocation.content = None;
                }
            }
        }
        removed
    }

    pub fn effective_dialogue_content(
        &self,
        index: &DialogueRouteIndex,
        object_id: &str,
        key: &DialogueVariantKey,
    ) -> Option<DialogueContent> {
        self.effective_dialogue_content_inner(index, object_id, key, &mut BTreeSet::new())
    }

    fn effective_dialogue_content_inner(
        &self,
        index: &DialogueRouteIndex,
        object_id: &str,
        key: &DialogueVariantKey,
        visited: &mut BTreeSet<String>,
    ) -> Option<DialogueContent> {
        if !visited.insert(object_id.to_string()) {
            return None;
        }
        let direct_authored = self
            .dialogue_authoring
            .as_ref()
            .and_then(|authoring| authoring.objects.get(object_id))
            .and_then(|object| object.overrides.iter().find(|entry| &entry.key == key));
        let stops_inheritance = direct_authored.is_some_and(|authored| {
            authored.scope == DialogueEditScope::Shared
                && authored
                    .key
                    .original_message
                    .as_ref()
                    .is_some_and(|message| message.domain != DialogueDomain::Stage)
        });
        if let Some(authored) = direct_authored {
            let shared_common_marker = authored.scope == DialogueEditScope::Shared
                && authored
                    .key
                    .original_message
                    .as_ref()
                    .is_some_and(|message| message.domain != DialogueDomain::Stage);
            if !shared_common_marker {
                return Some(authored.content.clone());
            }
        }
        let variant = index.find_variant(object_id, key)?;
        if !stops_inheritance && variant.route_kind != DialogueRouteKind::Forced {
            if let Some(source_object_id) = self
                .dialogue_authoring
                .as_ref()
                .and_then(|authoring| authoring.objects.get(object_id))
                .and_then(|object| object.inherited_from_object_id.as_deref())
            {
                if let Some(content) =
                    self.effective_dialogue_content_inner(index, source_object_id, key, visited)
                {
                    return Some(content);
                }
            }
        }
        if variant
            .message
            .as_ref()
            .is_some_and(|message| message.domain != DialogueDomain::Stage)
        {
            if let Some(shared) = self
                .dialogue_library
                .common_overrides
                .iter()
                .find(|entry| Some(&entry.message) == variant.message.as_ref())
            {
                return Some(shared.content.clone());
            }
        }
        self.dialogue_authoring
            .as_ref()
            .into_iter()
            .flat_map(|authoring| authoring.objects.values())
            .flat_map(|object| &object.overrides)
            .find(|entry| {
                entry.scope == DialogueEditScope::Shared
                    && entry
                        .key
                        .original_message
                        .as_ref()
                        .is_some_and(|message| message.domain == DialogueDomain::Stage)
                    && entry.key.original_message.as_ref() == variant.message.as_ref()
            })
            .map(|entry| entry.content.clone())
            .or_else(|| Some(variant.content.clone()))
    }

    pub fn duplicate_dialogue_authoring(
        &mut self,
        source_object_id: &str,
        new_object_id: &str,
    ) -> Result<()> {
        let source = self
            .objects
            .iter()
            .find(|object| object.id == source_object_id)
            .ok_or_else(|| {
                SceneError::StageExport(format!(
                    "dialogue source object {source_object_id:?} was not found"
                ))
            })?;
        let duplicate = self
            .objects
            .iter()
            .find(|object| object.id == new_object_id)
            .ok_or_else(|| {
                SceneError::StageExport(format!(
                    "dialogue duplicate object {new_object_id:?} was not found"
                ))
            })?;
        if let Some(registry) = self.registry.as_ref() {
            for (label, object) in [("source", source), ("duplicate", duplicate)] {
                if !registry.is_dialogue_instance_eligible(&object.factory_name) {
                    return Err(SceneError::StageExport(format!(
                        "dialogue {label} object {:?} factory {:?} is not eligible for placed-instance dialogue",
                        object.id, object.factory_name
                    )));
                }
            }
        }
        let inherits_generated_route = self.object_inherits_generated_dialogue(source_object_id);
        let generated_runtime_name = inherits_generated_route
            .then(|| self.available_generated_dialogue_runtime_name(new_object_id))
            .transpose()?;
        let prior_runtime_name = generated_runtime_name.as_ref().and_then(|_| {
            self.objects
                .iter()
                .find(|object| object.id == new_object_id)
                .and_then(|object| object.raw_param("name"))
                .map(str::to_string)
        });
        let authoring = self
            .dialogue_authoring
            .get_or_insert_with(DialogueAuthoringDocument::default);
        authoring.objects.insert(
            new_object_id.to_string(),
            DialogueObjectAuthoring {
                inherited_from_object_id: Some(source_object_id.to_string()),
                prior_runtime_name,
                overrides: Vec::new(),
                stable_allocations: Vec::new(),
            },
        );
        if let Some(generated_runtime_name) = generated_runtime_name {
            self.objects
                .iter_mut()
                .find(|object| object.id == new_object_id)
                .expect("dialogue duplicate existence was checked")
                .set_raw_param("name", generated_runtime_name);
        }
        Ok(())
    }

    /// Seeds the one empty normal-talk route used for a genuinely new
    /// talk-capable actor. When decomp-derived registry data is available, the
    /// same eligibility rule used by the inspector is enforced here as well.
    pub fn initialize_dialogue_for_new_object(
        &mut self,
        object_id: &str,
    ) -> Result<DialogueVariantKey> {
        let object = self
            .objects
            .iter()
            .find(|object| object.id == object_id)
            .ok_or_else(|| {
                SceneError::StageExport(format!("dialogue object {object_id:?} was not found"))
            })?;
        if self
            .registry
            .as_ref()
            .is_some_and(|registry| !registry.is_dialogue_instance_eligible(&object.factory_name))
        {
            return Err(SceneError::StageExport(format!(
                "dialogue object {object_id:?} factory {:?} is not eligible for placed-instance dialogue",
                object.factory_name
            )));
        }
        let generated_runtime_name = self.available_generated_dialogue_runtime_name(object_id)?;
        let existing_prior_runtime_name = self
            .dialogue_authoring
            .as_ref()
            .and_then(|authoring| authoring.objects.get(object_id))
            .and_then(|object| object.prior_runtime_name.clone());
        let object = self
            .objects
            .iter_mut()
            .find(|object| object.id == object_id)
            .ok_or_else(|| {
                SceneError::StageExport(format!("dialogue object {object_id:?} was not found"))
            })?;
        let prior_runtime_name = existing_prior_runtime_name.or_else(|| {
            object
                .raw_param("name")
                .filter(|name| !name.starts_with(GENERATED_DIALOGUE_RUNTIME_NAME_PREFIX))
                .map(str::to_string)
        });
        object.set_raw_param("name", generated_runtime_name);
        self.dialogue_authoring
            .get_or_insert_with(DialogueAuthoringDocument::default)
            .objects
            .entry(object_id.to_string())
            .or_default()
            .prior_runtime_name = prior_runtime_name;
        let key = DialogueVariantKey::generated_for_object(object_id);
        self.set_dialogue_override(
            object_id,
            key.clone(),
            DialogueEditScope::Instance,
            DialogueRouteKind::Generated,
            "New normal conversation",
            DialogueContent {
                message: BmgMessage::default(),
                authored_tokens: None,
                attributes: vec![0; 8],
                voice_index: Some(0),
            },
        )?;
        Ok(key)
    }

    fn object_inherits_generated_dialogue(&self, object_id: &str) -> bool {
        let Some(authoring) = self.dialogue_authoring.as_ref() else {
            return false;
        };
        let mut visited = BTreeSet::new();
        let mut current = Some(object_id);
        while let Some(candidate) = current {
            if !visited.insert(candidate) {
                return false;
            }
            let Some(object) = authoring.objects.get(candidate) else {
                return false;
            };
            if object
                .overrides
                .iter()
                .any(|override_| override_.key.original_message.is_none())
            {
                return true;
            }
            current = object.inherited_from_object_id.as_deref();
        }
        false
    }

    pub(crate) fn owns_generated_dialogue_runtime_name(&self, object_id: &str) -> bool {
        let Some(object) = self.objects.iter().find(|object| object.id == object_id) else {
            return false;
        };
        let expected = format!(
            "{GENERATED_DIALOGUE_RUNTIME_NAME_PREFIX}{:016x}",
            stable_text_fingerprint(object_id)
        );
        if object.raw_param("name") != Some(expected.as_str()) {
            return false;
        }
        let Some(authoring) = self
            .dialogue_authoring
            .as_ref()
            .and_then(|authoring| authoring.objects.get(object_id))
        else {
            return false;
        };
        let owns_previous_identity =
            authoring.prior_runtime_name.is_some() || authoring.inherited_from_object_id.is_some();
        owns_previous_identity && self.object_inherits_generated_dialogue(object_id)
    }

    fn available_generated_dialogue_runtime_name(&self, object_id: &str) -> Result<String> {
        let generated_runtime_name = format!(
            "{GENERATED_DIALOGUE_RUNTIME_NAME_PREFIX}{:016x}",
            stable_text_fingerprint(object_id)
        );
        if self.objects.iter().any(|object| {
            object.id != object_id
                && object.raw_param("name") == Some(generated_runtime_name.as_str())
        }) {
            return Err(SceneError::StageExport(format!(
                "generated dialogue runtime name {generated_runtime_name:?} collides with another object"
            )));
        }
        Ok(generated_runtime_name)
    }

    fn uninitialized_generated_dialogue_objects(&self, index: &DialogueRouteIndex) -> Vec<String> {
        let Some(registry) = self.registry.as_ref() else {
            return Vec::new();
        };
        self.objects
            .iter()
            .filter(|object| {
                matches!(
                    object.placement.as_ref(),
                    Some(
                        crate::PlacementBinding::Authored(_) | crate::PlacementBinding::CloneOf(_)
                    )
                ) && registry.is_dialogue_instance_eligible(&object.factory_name)
                    && !self.object_inherits_generated_dialogue(&object.id)
                    && index.variants_for_object(&object.id).iter().any(|variant| {
                        variant.provenance == DialogueProvenance::Generated
                            && variant.message.is_none()
                    })
            })
            .map(|object| object.id.clone())
            .collect()
    }

    /// Returns true when an editor-placed talk actor still needs its own empty
    /// generated route. Retail placements remain untouched so a dialogue no-op
    /// still rebuilds byte-identically.
    pub fn has_uninitialized_generated_dialogue(&self, index: &DialogueRouteIndex) -> bool {
        !self
            .uninitialized_generated_dialogue_objects(index)
            .is_empty()
    }

    fn initialize_missing_generated_dialogue(&mut self, index: &DialogueRouteIndex) -> Result<()> {
        for object_id in self.uninitialized_generated_dialogue_objects(index) {
            self.initialize_dialogue_for_new_object(&object_id)?;
        }
        Ok(())
    }

    /// Compiles authored deltas without mutating the imported stage or the
    /// extracted base. Newly allocated message indexes are persisted back to
    /// the authoring document only after every resource edit succeeds.
    pub fn compile_dialogue_authoring(
        &mut self,
        index: &DialogueRouteIndex,
    ) -> Result<CompiledDialogueEdits> {
        // A newly placed actor with no retail normal route must still select a
        // message on every conversation. Otherwise TTalk2D2 retains whichever
        // message the previously spoken-to NPC selected, leaking dialogue
        // between unrelated instances.
        self.initialize_missing_generated_dialogue(index)?;
        let has_stage_authoring = self.dialogue_authoring.as_ref().is_some_and(|authoring| {
            authoring
                .objects
                .values()
                .any(|object| !object.overrides.is_empty() || !object.stable_allocations.is_empty())
        });
        if !has_stage_authoring
            && self.dialogue_library.common_overrides.is_empty()
            && self.dialogue_library.stable_allocations.is_empty()
        {
            return Ok(CompiledDialogueEdits::default());
        }
        self.validate_dialogue_for_export(index)?;

        let resources = collect_dialogue_resources(self)?;
        let mut authoring = self.dialogue_authoring.clone().unwrap_or_default();
        let mut dialogue_library = self.dialogue_library.clone();
        validate_authoring_versions(&authoring, &dialogue_library)?;
        let mut stage_messages = resources
            .messages
            .iter()
            .filter(|((domain, _), _)| *domain == DialogueDomain::Stage)
            .map(|((_, path), bmg)| (path.clone(), bmg.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut common_messages = resources
            .messages
            .iter()
            .filter(|((domain, _), _)| *domain != DialogueDomain::Stage)
            .map(|((domain, path), bmg)| ((*domain, path.clone()), bmg.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut changed_stage_messages = BTreeSet::<Vec<u8>>::new();
        let mut changed_common_messages = BTreeSet::<(DialogueDomain, Vec<u8>)>::new();
        let mut changed_scripts = BTreeMap::<Vec<u8>, SpcDocument>::new();
        let mut runtime_override_requests = Vec::new();
        let mut generated_routes = Vec::<(String, u32)>::new();
        let mut compiled_generated_messages = BTreeMap::<(String, DialogueVariantKey), u32>::new();
        let mut compiled_instance_messages = BTreeMap::<(String, DialogueVariantKey), u32>::new();

        materialize_stage_allocation_tombstones(
            &authoring,
            &mut stage_messages,
            &mut changed_stage_messages,
        )?;

        materialize_project_allocations(
            &dialogue_library,
            &mut common_messages,
            &mut changed_common_messages,
        )?;

        for shared in &dialogue_library.common_overrides {
            if shared.message.domain == DialogueDomain::Stage {
                return Err(SceneError::StageExport(
                    "project dialogue library contains a stage-local override".to_string(),
                ));
            }
            validate_dialogue_content(&shared.content)?;
            let key = (
                shared.message.domain,
                shared.message.raw_resource_path.clone(),
            );
            let bmg = common_messages.get_mut(&key).ok_or_else(|| {
                SceneError::StageExport(format!(
                    "common dialogue resource {} was not found",
                    String::from_utf8_lossy(&shared.message.raw_resource_path)
                ))
            })?;
            apply_shared_content(bmg, shared.message.entry_index as usize, &shared.content)?;
            bmg.validate_sms_dialogue()?;
            changed_common_messages.insert(key);
        }

        let mut object_ids = authoring.objects.keys().cloned().collect::<Vec<_>>();
        object_ids.sort();
        for object_id in object_ids {
            let overrides = authoring
                .objects
                .get(&object_id)
                .map(|object| object.overrides.clone())
                .unwrap_or_default();
            for authored in overrides {
                let matches = index
                    .variants_for_object(&object_id)
                    .iter()
                    .filter(|variant| variant.key == authored.key)
                    .collect::<Vec<_>>();
                if matches.len() != 1 {
                    return Err(SceneError::StageExport(format!(
                        "dialogue override for object {object_id:?} resolves to {} routes; expected exactly one",
                        matches.len()
                    )));
                }
                let variant = matches[0];
                let shared_common_marker = authored.scope == DialogueEditScope::Shared
                    && variant
                        .message
                        .as_ref()
                        .is_some_and(|message| message.domain != DialogueDomain::Stage);
                let validation_content = if shared_common_marker {
                    let message = variant.message.as_ref().expect("checked above");
                    &dialogue_library
                        .common_overrides
                        .iter()
                        .find(|shared| shared.message == *message)
                        .ok_or_else(|| {
                            SceneError::StageExport(format!(
                                "shared common dialogue marker for object {object_id:?} has no project-library override"
                            ))
                        })?
                        .content
                } else {
                    &authored.content
                };
                validate_dialogue_content(validation_content)?;
                validate_dialogue_page_break_presentation(
                    validation_content,
                    self.dialogue_page_line_count(&object_id, variant.route_kind),
                )?;
                if authored.scope == DialogueEditScope::Shared && !shared_common_marker {
                    for consumer in &variant.shared_consumers {
                        if let Some(shared_variant) =
                            index.find_variant(&consumer.object_id, &consumer.variant_key)
                        {
                            if effective_dialogue_consumer_allocation(self, shared_variant)
                                .is_some()
                            {
                                continue;
                            }
                            validate_dialogue_page_break_presentation(
                                validation_content,
                                self.dialogue_page_line_count(
                                    &consumer.object_id,
                                    shared_variant.route_kind,
                                ),
                            )?;
                        }
                    }
                }
                if shared_common_marker {
                    continue;
                }
                match variant.message.as_ref() {
                    Some(message) if message.domain == DialogueDomain::Stage => {
                        let (effective_message_id, requires_source_runtime_remap) =
                            compile_stage_message_override(
                                self,
                                &mut authoring,
                                &object_id,
                                &authored,
                                variant,
                                message,
                                &mut stage_messages,
                                &mut changed_stage_messages,
                                &mut changed_scripts,
                            )?;
                        if authored.scope == DialogueEditScope::Instance {
                            compiled_instance_messages.insert(
                                (object_id.clone(), authored.key.clone()),
                                effective_message_id,
                            );
                        }
                        if requires_source_runtime_remap {
                            runtime_override_requests.push(self.runtime_override_request(
                                &object_id,
                                authored.route_kind,
                                DialogueDomain::Stage,
                                Some(message.full_message_id),
                                effective_message_id,
                            )?);
                        }
                    }
                    Some(message) if authored.scope == DialogueEditScope::Shared => {
                        // The object override is a route-scope marker which
                        // stops inherited copy-on-write routing. Common BMG
                        // content has one project-wide source of truth and was
                        // applied from dialogue_library above; marker content
                        // may be an older snapshot from another stage.
                        if !dialogue_library
                            .common_overrides
                            .iter()
                            .any(|shared| shared.message == *message)
                        {
                            return Err(SceneError::StageExport(format!(
                                "shared common dialogue marker for object {object_id:?} has no project-library override"
                            )));
                        }
                    }
                    Some(message) if message.domain == DialogueDomain::Balloon => {
                        let replacement_message_id = compile_balloon_message_clone(
                            &self.stage_id,
                            &mut dialogue_library,
                            &object_id,
                            &authored,
                            message,
                            &mut common_messages,
                        )?;
                        changed_common_messages
                            .insert((message.domain, message.raw_resource_path.clone()));
                        runtime_override_requests.push(self.runtime_override_request(
                            &object_id,
                            authored.route_kind,
                            message.domain,
                            Some(message.full_message_id),
                            replacement_message_id,
                        )?);
                        compiled_instance_messages.insert(
                            (object_id.clone(), authored.key.clone()),
                            replacement_message_id,
                        );
                    }
                    Some(message) => {
                        let (replacement_message_id, stage_path) = compile_runtime_message_clone(
                            &mut authoring,
                            &object_id,
                            &authored,
                            message,
                            &mut stage_messages,
                        )?;
                        changed_stage_messages.insert(stage_path);
                        runtime_override_requests.push(self.runtime_override_request(
                            &object_id,
                            authored.route_kind,
                            message.domain,
                            Some(message.full_message_id),
                            replacement_message_id,
                        )?);
                        compiled_instance_messages.insert(
                            (object_id.clone(), authored.key.clone()),
                            replacement_message_id,
                        );
                    }
                    None => {
                        let (replacement_message_id, stage_path) = compile_generated_message(
                            &mut authoring,
                            &object_id,
                            &authored,
                            &mut stage_messages,
                        )?;
                        changed_stage_messages.insert(stage_path);
                        generated_routes.push((object_id.clone(), replacement_message_id));
                        compiled_generated_messages.insert(
                            (object_id.clone(), authored.key.clone()),
                            replacement_message_id,
                        );
                    }
                }
            }
        }

        // Untouched duplicates inherit the nearest player-initiated instance
        // override copy-on-write. Resource mutations at the original message
        // need no extra work, but cloned stage/system/balloon messages require
        // the duplicate's guarded runtime identity to select the same clone.
        let mut inherited_object_ids = authoring.objects.keys().cloned().collect::<Vec<_>>();
        inherited_object_ids.sort();
        for object_id in inherited_object_ids {
            let Some(object_authoring) = authoring.objects.get(&object_id) else {
                continue;
            };
            let Some(initial_source_id) = object_authoring.inherited_from_object_id.as_deref()
            else {
                continue;
            };
            for variant in index.variants_for_object(&object_id) {
                let Some(message) = variant.message.as_ref() else {
                    continue;
                };
                if object_authoring
                    .overrides
                    .iter()
                    .any(|override_| override_.key == variant.key)
                {
                    continue;
                }
                let mut visited = BTreeSet::new();
                let mut source_id = Some(initial_source_id);
                let mut replacement_message_id = None;
                while let Some(candidate_id) = source_id {
                    if !visited.insert(candidate_id) {
                        break;
                    }
                    let Some(candidate) = authoring.objects.get(candidate_id) else {
                        break;
                    };
                    if let Some(source_override) = candidate
                        .overrides
                        .iter()
                        .find(|override_| override_.key == variant.key)
                    {
                        if source_override.scope == DialogueEditScope::Instance {
                            replacement_message_id = compiled_instance_messages
                                .get(&(candidate_id.to_string(), variant.key.clone()))
                                .copied();
                        }
                        break;
                    }
                    source_id = candidate.inherited_from_object_id.as_deref();
                }
                if let Some(replacement_message_id) = replacement_message_id
                    .filter(|replacement| *replacement != message.full_message_id)
                {
                    runtime_override_requests.push(self.runtime_override_request(
                        &object_id,
                        variant.route_kind,
                        message.domain,
                        Some(message.full_message_id),
                        replacement_message_id,
                    )?);
                }
            }
        }

        // Generated duplicates need their own name-based script branch, but
        // initially consume the source allocation.  This keeps the inherited
        // content genuinely copy-on-write: the first target override is
        // compiled above into a fresh stable allocation, while an untouched
        // duplicate adds only a route to the source message.
        for object_id in authoring.objects.keys() {
            let Some(object_authoring) = authoring.objects.get(object_id) else {
                continue;
            };
            if object_authoring.inherited_from_object_id.is_none() {
                continue;
            }
            for variant in index
                .variants_for_object(object_id)
                .iter()
                .filter(|variant| {
                    variant.key.original_message.is_none()
                        && variant.message.is_none()
                        && !object_authoring
                            .overrides
                            .iter()
                            .any(|override_| override_.key == variant.key)
                })
            {
                let Some(replacement_message_id) = inherited_generated_message_id(
                    &authoring,
                    object_id,
                    &variant.key,
                    &compiled_generated_messages,
                ) else {
                    // A derived empty fallback is inspector-visible but emits
                    // no script or BMG entry until someone authors it. An
                    // untouched duplicate of that fallback therefore has no
                    // allocation to inherit yet.
                    continue;
                };
                generated_routes.push((object_id.clone(), replacement_message_id));
            }
        }

        if !generated_routes.is_empty() {
            let script = compile_generated_dialogue_script(self, &generated_routes)?;
            changed_scripts.insert(GENERATED_DIALOGUE_SCRIPT_PATH.to_vec(), script);
        }

        let mut stage_edits = StageArchiveEdits::default();
        for path in changed_stage_messages {
            let bmg = stage_messages.remove(&path).expect("changed BMG exists");
            bmg.validate_sms_dialogue()?;
            stage_edits.upsert_resource(path, StageResourceDocument::Message(bmg));
        }
        for (path, script) in changed_scripts {
            stage_edits.upsert_resource(path, StageResourceDocument::Script(script));
        }
        let mut common_resources = changed_common_messages
            .into_iter()
            .map(|key| CommonDialogueResourceEdit {
                raw_resource_path: key.1.clone(),
                document: common_messages.remove(&key).expect("changed BMG exists"),
            })
            .collect::<Vec<_>>();
        common_resources
            .sort_by(|left, right| left.raw_resource_path.cmp(&right.raw_resource_path));
        runtime_override_requests.sort_by(|left, right| {
            left.object_id
                .cmp(&right.object_id)
                .then_with(|| left.original_message_id.cmp(&right.original_message_id))
        });

        if has_stage_authoring {
            self.dialogue_authoring = Some(authoring);
        }
        self.dialogue_library = dialogue_library;
        Ok(CompiledDialogueEdits {
            stage_edits,
            common_resources,
            runtime_override_requests,
        })
    }

    /// Performs the route-dependent export gate against an asynchronously
    /// built (or freshly rebuilt) index. Ordinary `StageDocument::validate`
    /// intentionally remains IO-free for per-keystroke UI use.
    pub fn validate_dialogue_for_export(&self, index: &DialogueRouteIndex) -> Result<()> {
        let mut errors = index
            .issues
            .iter()
            .filter(|issue| issue.severity == DialogueResolutionSeverity::Error)
            .map(|issue| format!("{}: {}", issue.code, issue.message))
            .collect::<Vec<_>>();
        errors.extend(
            validate_dialogue_document(self)
                .into_iter()
                .filter(|issue| issue.severity == crate::ValidationSeverity::Error)
                .map(|issue| format!("{}: {}", issue.code, issue.message)),
        );
        if let Some(authoring) = &self.dialogue_authoring {
            for (object_id, object) in &authoring.objects {
                for authored in &object.overrides {
                    let matches = index
                        .variants_for_object(object_id)
                        .iter()
                        .filter(|variant| variant.key == authored.key)
                        .collect::<Vec<_>>();
                    if matches.len() != 1 {
                        errors.push(format!(
                            "dialogue-anchor-unresolved: override for object {object_id:?} resolves to {} routes",
                            matches.len()
                        ));
                        continue;
                    }
                    let variant = matches[0];
                    let shared_common_marker = authored.scope == DialogueEditScope::Shared
                        && variant
                            .message
                            .as_ref()
                            .is_some_and(|message| message.domain != DialogueDomain::Stage);
                    let validation_content = if shared_common_marker {
                        let message = variant.message.as_ref().expect("checked above");
                        let Some(shared) = self
                            .dialogue_library
                            .common_overrides
                            .iter()
                            .find(|shared| shared.message == *message)
                        else {
                            errors.push(format!(
                                "dialogue-common-shared-marker-orphaned: object {object_id:?} has no project-library override"
                            ));
                            continue;
                        };
                        &shared.content
                    } else {
                        &authored.content
                    };
                    if let Err(error) = validate_dialogue_page_break_presentation(
                        validation_content,
                        self.dialogue_page_line_count(object_id, variant.route_kind),
                    ) {
                        errors.push(format!(
                            "dialogue-page-presentation-mismatch: object {object_id:?}: {error}"
                        ));
                    }
                    if authored.scope == DialogueEditScope::Shared && !shared_common_marker {
                        for consumer in &variant.shared_consumers {
                            let Some(shared_variant) =
                                index.find_variant(&consumer.object_id, &consumer.variant_key)
                            else {
                                continue;
                            };
                            if effective_dialogue_consumer_allocation(self, shared_variant)
                                .is_some()
                            {
                                continue;
                            }
                            if let Err(error) = validate_dialogue_page_break_presentation(
                                validation_content,
                                self.dialogue_page_line_count(
                                    &consumer.object_id,
                                    shared_variant.route_kind,
                                ),
                            ) {
                                errors.push(format!(
                                    "dialogue-page-presentation-mismatch: shared consumer {:?}: {error}",
                                    consumer.object_id
                                ));
                            }
                        }
                    }
                }
            }
        }
        for shared in &self.dialogue_library.common_overrides {
            for variant in index.all_variants().filter(|variant| {
                variant.message.as_ref() == Some(&shared.message)
                    && effective_dialogue_consumer_allocation(self, variant).is_none()
            }) {
                if let Err(error) = validate_dialogue_page_break_presentation(
                    &shared.content,
                    self.dialogue_page_line_count(&variant.object_id, variant.route_kind),
                ) {
                    errors.push(format!(
                        "dialogue-page-presentation-mismatch: common message {} consumer {:?}: {error}",
                        shared.message.entry_index, variant.object_id
                    ));
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(SceneError::ValidationFailed(errors.join("; ")))
        }
    }

    fn runtime_override_request(
        &self,
        object_id: &str,
        route_kind: DialogueRouteKind,
        domain: DialogueDomain,
        original_message_id: Option<u32>,
        replacement_message_id: u32,
    ) -> Result<RuntimeDialogueOverrideRequest> {
        let object = self
            .objects
            .iter()
            .find(|object| object.id == object_id)
            .ok_or_else(|| {
                SceneError::StageExport(format!("dialogue object {object_id:?} was not found"))
            })?;
        let registry = self.registry.as_ref().ok_or_else(|| {
            SceneError::StageExport(format!(
                "dialogue runtime override for {object_id:?} requires a decomp-derived registry"
            ))
        })?;
        if !registry.is_dialogue_instance_eligible(&object.factory_name) {
            return Err(SceneError::StageExport(format!(
                "dialogue runtime override for {object_id:?} factory {:?} is not eligible for placed-instance dialogue",
                object.factory_name
            )));
        }
        let actor_type = registry
            .find_npc_actor_type(&object.factory_name)
            .ok_or_else(|| {
                SceneError::StageExport(format!(
                    "dialogue runtime override for {object_id:?} has no decomp-derived TBaseNPC actor type for factory {:?}",
                    object.factory_name
                ))
            })?;
        let runtime_name = object.raw_param("name").ok_or_else(|| {
            SceneError::StageExport(format!(
                "dialogue runtime override for {object_id:?} has no exact runtime name"
            ))
        })?;
        let (encoded_name, _, had_errors) = SHIFT_JIS.encode(runtime_name);
        if had_errors {
            return Err(SceneError::StageExport(format!(
                "dialogue runtime name {runtime_name:?} for {object_id:?} is not representable in Shift-JIS"
            )));
        }
        Ok(RuntimeDialogueOverrideRequest {
            stage_id: self.stage_id.clone(),
            object_id: object_id.to_string(),
            source: object.source.clone(),
            placement: object.placement.clone(),
            factory_name: object.factory_name.clone(),
            actor_type,
            runtime_name: runtime_name.to_string(),
            runtime_name_shift_jis: encoded_name.into_owned(),
            reset_position_bits: object.transform.translation.map(f32::to_bits),
            route_kind,
            domain,
            original_message_id,
            replacement_message_id,
        })
    }
}

fn validate_authoring_versions(
    authoring: &DialogueAuthoringDocument,
    library: &ProjectDialogueLibrary,
) -> Result<()> {
    if authoring.format_version != DIALOGUE_AUTHORING_FORMAT_VERSION {
        return Err(SceneError::StageExport(format!(
            "unsupported dialogue authoring format version {}; expected {}",
            authoring.format_version, DIALOGUE_AUTHORING_FORMAT_VERSION
        )));
    }
    if library.format_version != PROJECT_DIALOGUE_LIBRARY_FORMAT_VERSION {
        return Err(SceneError::StageExport(format!(
            "unsupported project dialogue library format version {}; expected {}",
            library.format_version, PROJECT_DIALOGUE_LIBRARY_FORMAT_VERSION
        )));
    }
    Ok(())
}

fn validate_dialogue_content(content: &DialogueContent) -> Result<()> {
    content.compiled_message()?.validate_sms_controls()?;
    if content.attributes.len() != 8 {
        return Err(SceneError::StageExport(format!(
            "SMS dialogue content has {} INF1 attribute bytes; expected 8",
            content.attributes.len()
        )));
    }
    if let Some(voice) = content.voice_index {
        if voice as usize >= SMS_TALK_SOUND_LIMIT {
            return Err(SceneError::StageExport(format!(
                "dialogue talk-sound index {voice} is outside 0..={} ",
                SMS_TALK_SOUND_LIMIT - 1
            )));
        }
    }
    Ok(())
}

fn dialogue_content_has_visible_text(content: &DialogueContent) -> bool {
    if let Some(tokens) = content.authored_tokens.as_ref() {
        return tokens.iter().any(|token| match token {
            DialogueAuthoringToken::Text(text) => !text.trim().is_empty(),
            DialogueAuthoringToken::Control(SmsBmgControl::Choice { text, .. }) => {
                !text.trim().is_empty()
            }
            DialogueAuthoringToken::Control(_) | DialogueAuthoringToken::PageBreak { .. } => false,
        });
    }
    content.message.tokens.iter().any(|token| match token {
        sms_formats::BmgMessageToken::Text(text) => !text.trim().is_empty(),
        sms_formats::BmgMessageToken::Control(raw) => matches!(
            SmsBmgControl::decode(raw),
            Ok(SmsBmgControl::Choice { text, .. }) if !text.trim().is_empty()
        ),
    })
}

fn validate_dialogue_page_break_presentation(
    content: &DialogueContent,
    expected_line_count: u8,
) -> Result<()> {
    let mismatched = content
        .authored_tokens
        .as_ref()
        .into_iter()
        .flatten()
        .find_map(|token| match token {
            DialogueAuthoringToken::PageBreak { line_count }
                if *line_count != expected_line_count =>
            {
                Some(*line_count)
            }
            _ => None,
        });
    if let Some(authored_line_count) = mismatched {
        return Err(SceneError::StageExport(format!(
            "dialogue page break was authored for a {authored_line_count}-line presentation, but the resolved route uses {expected_line_count} lines"
        )));
    }
    Ok(())
}

fn dialogue_page_break_line_counts(content: &DialogueContent) -> BTreeSet<u8> {
    content
        .authored_tokens
        .as_ref()
        .into_iter()
        .flatten()
        .filter_map(|token| match token {
            DialogueAuthoringToken::PageBreak { line_count } => Some(*line_count),
            _ => None,
        })
        .collect()
}

fn apply_entry_content(
    bmg: &mut BmgFile,
    entry_index: usize,
    content: &DialogueContent,
) -> Result<()> {
    let entry_count = bmg.entries.len();
    let entry = bmg.entries.get_mut(entry_index).ok_or_else(|| {
        SceneError::StageExport(format!(
            "dialogue message index {entry_index} is outside {entry_count} entries"
        ))
    })?;
    entry.attributes.clone_from(&content.attributes);
    if let Some(voice) = content.voice_index {
        entry.set_sms_voice_index(voice)?;
    }
    Ok(())
}

fn apply_unique_content(
    bmg: &mut BmgFile,
    entry_index: usize,
    content: &DialogueContent,
) -> Result<()> {
    bmg.replace_message_stable(entry_index, content.compiled_message()?)?;
    apply_entry_content(bmg, entry_index, content)
}

fn apply_shared_content(
    bmg: &mut BmgFile,
    entry_index: usize,
    content: &DialogueContent,
) -> Result<()> {
    let aliases = bmg.message_aliases(entry_index)?;
    let selected_voice = bmg
        .entries
        .get(entry_index)
        .and_then(|entry| entry.attributes.get(4))
        .copied();
    let edited_voice = content
        .voice_index
        .filter(|voice| Some(*voice) != selected_voice);
    bmg.replace_message_aliases_stable(entry_index, content.compiled_message()?)?;
    for alias in aliases {
        if alias == entry_index {
            apply_entry_content(bmg, alias, content)?;
            continue;
        }
        // DAT1 aliases share only message storage. Their INF1 attributes are
        // independent and may contain opaque, intentionally different retail
        // bytes. A shared text edit must not copy the selected entry's INF1
        // record over its aliases; propagate only the exposed voice ordinal.
        if let Some(voice) = edited_voice {
            let entry_count = bmg.entries.len();
            let entry = bmg.entries.get_mut(alias).ok_or_else(|| {
                SceneError::StageExport(format!(
                    "dialogue alias index {alias} is outside {entry_count} entries"
                ))
            })?;
            let attribute_count = entry.attributes.len();
            let voice_byte = entry.attributes.get_mut(4).ok_or_else(|| {
                SceneError::StageExport(format!(
                    "dialogue alias index {alias} has only {attribute_count} INF1 attribute bytes; voice byte 4 is absent"
                ))
            })?;
            *voice_byte = voice;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn compile_stage_message_override(
    document: &StageDocument,
    authoring: &mut DialogueAuthoringDocument,
    object_id: &str,
    authored: &DialogueVariantOverride,
    variant: &DialogueVariant,
    message: &DialogueMessageRef,
    stage_messages: &mut BTreeMap<Vec<u8>, BmgFile>,
    changed_stage_messages: &mut BTreeSet<Vec<u8>>,
    changed_scripts: &mut BTreeMap<Vec<u8>, SpcDocument>,
) -> Result<(u32, bool)> {
    let bmg = stage_messages
        .get_mut(&message.raw_resource_path)
        .ok_or_else(|| {
            SceneError::StageExport(format!(
                "stage dialogue resource {} was not found",
                String::from_utf8_lossy(&message.raw_resource_path)
            ))
        })?;
    let entry_index = message.entry_index as usize;
    let aliases = bmg.message_aliases(entry_index)?;
    if authored.scope == DialogueEditScope::Shared {
        apply_shared_content(bmg, entry_index, &authored.content)?;
    } else if aliases.len() > 1 || variant.shared_consumers.len() > 1 {
        let allocation = find_stable_allocation(authoring, object_id, &authored.key);
        let allocated_index = if let Some(allocation) = allocation {
            let allocated_index = allocation as usize;
            rematerialize_persisted_allocation(bmg, allocated_index, object_id, &authored.content)?;
            allocated_index
        } else {
            let edit =
                bmg.clone_entry_stable(entry_index, Some(authored.content.compiled_message()?))?;
            apply_entry_content(bmg, edit.entry_index, &authored.content)?;
            persist_stable_allocation(
                authoring,
                object_id,
                authored.key.clone(),
                edit.entry_index,
            )?;
            edit.entry_index
        };
        let replacement_id = (message.full_message_id & 0xffff_0000)
            | u32::try_from(allocated_index).map_err(|_| {
                SceneError::StageExport("dialogue message index overflowed u32".to_string())
            })?;
        let shared_same_anchor = variant
            .shared_consumers
            .iter()
            .any(|consumer| consumer.object_id != object_id && consumer.variant_key == variant.key);
        if !shared_same_anchor {
            rewrite_route_message_id(
                document,
                variant,
                message.full_message_id,
                replacement_id,
                changed_scripts,
            )?;
        }
        bmg.validate_sms_dialogue()?;
        changed_stage_messages.insert(message.raw_resource_path.clone());
        return Ok((replacement_id, shared_same_anchor));
    } else {
        apply_unique_content(bmg, entry_index, &authored.content)?;
    }
    bmg.validate_sms_dialogue()?;
    changed_stage_messages.insert(message.raw_resource_path.clone());
    Ok((message.full_message_id, false))
}

fn rewrite_route_message_id(
    document: &StageDocument,
    variant: &DialogueVariant,
    original_message_id: u32,
    replacement_message_id: u32,
    scripts: &mut BTreeMap<Vec<u8>, SpcDocument>,
) -> Result<()> {
    let script_path = &variant.compiler_location.script_path;
    if !scripts.contains_key(script_path) {
        let script = match document.effective_resource_clone(script_path)? {
            Some(StageResourceDocument::Script(script)) => script,
            Some(_) => {
                return Err(SceneError::StageExport(format!(
                    "dialogue route resource {} is not an SPC script",
                    String::from_utf8_lossy(script_path)
                )))
            }
            None => {
                return Err(SceneError::StageExport(format!(
                    "dialogue route script {} was not found",
                    String::from_utf8_lossy(script_path)
                )))
            }
        };
        scripts.insert(script_path.clone(), script);
    }
    let script = scripts.get_mut(script_path).expect("inserted above");
    let mut program = script.to_relocatable()?;
    let instruction_index = variant.compiler_location.message_instruction_index;
    let instruction_count = program.instructions.len();
    let instruction = program
        .instructions
        .get_mut(instruction_index)
        .ok_or_else(|| {
            SceneError::StageExport(format!(
            "dialogue semantic anchor resolved outside the {instruction_count}-instruction script"
        ))
        })?;
    match instruction {
        SpcInstruction::Int(value) if *value as u32 == original_message_id => {
            *value = replacement_message_id as i32;
        }
        SpcInstruction::Int(value) => {
            return Err(SceneError::StageExport(format!(
            "dialogue semantic anchor expected message {original_message_id:#010x}, found {:#010x}",
            *value as u32
        )))
        }
        _ => {
            return Err(SceneError::StageExport(
                "dialogue semantic anchor no longer points to an integer message operand"
                    .to_string(),
            ))
        }
    }
    *script = program.to_document()?;
    Ok(())
}

fn materialize_stage_allocation_tombstones(
    authoring: &DialogueAuthoringDocument,
    stage_messages: &mut BTreeMap<Vec<u8>, BmgFile>,
    changed_stage_messages: &mut BTreeSet<Vec<u8>>,
) -> Result<()> {
    let default_path = stage_message_path(stage_messages);
    let mut by_resource = BTreeMap::<Vec<u8>, Vec<(u16, Option<DialogueContent>)>>::new();
    for object in authoring.objects.values() {
        for allocation in &object.stable_allocations {
            let path = allocation
                .key
                .original_message
                .as_ref()
                .filter(|message| message.domain == DialogueDomain::Stage)
                .map(|message| message.raw_resource_path.clone())
                .unwrap_or_else(|| default_path.clone());
            by_resource.entry(path).or_default().push((
                allocation.message_index,
                object
                    .overrides
                    .iter()
                    .find(|override_| {
                        override_.key == allocation.key
                            && override_.scope == DialogueEditScope::Instance
                    })
                    .map(|override_| override_.content.clone()),
            ));
        }
    }
    for (path, mut allocations) in by_resource {
        let bmg = stage_messages
            .entry(path.clone())
            .or_insert_with(empty_sms_bmg);
        let retail_entry_count = bmg.entries.len();
        allocations.sort_by_key(|(message_index, _)| *message_index);
        allocations.dedup_by_key(|(message_index, _)| *message_index);
        let attribute_size = usize::from(bmg.entry_size).checked_sub(4).ok_or_else(|| {
            SceneError::StageExport("BMG entry size is smaller than 4".to_string())
        })?;
        for (message_index, content) in allocations {
            let index = usize::from(message_index);
            if index < retail_entry_count {
                return Err(SceneError::StageExport(format!(
                    "stage dialogue allocation {message_index} collides with a retail entry in {}",
                    String::from_utf8_lossy(&path)
                )));
            }
            while bmg.entries.len() < index {
                bmg.append_entry_stable(vec![0; attribute_size], BmgMessage::default())?;
            }
            if bmg.entries.len() == index {
                if let Some(content) = content.as_ref() {
                    bmg.append_entry_stable(
                        content.attributes.clone(),
                        content.compiled_message()?,
                    )?;
                    apply_entry_content(bmg, index, content)?;
                } else {
                    bmg.append_entry_stable(vec![0; attribute_size], BmgMessage::default())?;
                }
            }
        }
        bmg.validate_sms_dialogue()?;
        changed_stage_messages.insert(path);
    }
    Ok(())
}

fn compile_runtime_message_clone(
    authoring: &mut DialogueAuthoringDocument,
    object_id: &str,
    authored: &DialogueVariantOverride,
    _original: &DialogueMessageRef,
    stage_messages: &mut BTreeMap<Vec<u8>, BmgFile>,
) -> Result<(u32, Vec<u8>)> {
    let path = stage_message_path(stage_messages);
    let bmg = stage_messages
        .entry(path.clone())
        .or_insert_with(empty_sms_bmg);
    let entry_index = allocate_or_replace_authored_entry(authoring, object_id, authored, bmg)?;
    bmg.validate_sms_dialogue()?;
    Ok((0x0001_0000 | entry_index as u32, path))
}

fn compile_balloon_message_clone(
    stage_id: &str,
    library: &mut ProjectDialogueLibrary,
    object_id: &str,
    authored: &DialogueVariantOverride,
    original: &DialogueMessageRef,
    common_messages: &mut BTreeMap<(DialogueDomain, Vec<u8>), BmgFile>,
) -> Result<u32> {
    let resource_key = (original.domain, original.raw_resource_path.clone());
    let bmg = common_messages.get_mut(&resource_key).ok_or_else(|| {
        SceneError::StageExport(format!(
            "balloon dialogue resource {} was not found",
            String::from_utf8_lossy(&original.raw_resource_path)
        ))
    })?;

    let existing = library.stable_allocations.iter().position(|allocation| {
        allocation.stage_id == stage_id
            && allocation.object_id == object_id
            && allocation.key == authored.key
    });
    let entry_index = if let Some(existing_index) = existing {
        let allocation = &mut library.stable_allocations[existing_index];
        allocation.domain = original.domain;
        allocation.raw_resource_path = original.raw_resource_path.clone();
        allocation.content = Some(authored.content.clone());
        let entry_index = usize::from(allocation.message_index);
        apply_unique_content(bmg, entry_index, &authored.content)?;
        entry_index
    } else {
        let edit = bmg.append_entry_stable(
            authored.content.attributes.clone(),
            authored.content.compiled_message()?,
        )?;
        apply_entry_content(bmg, edit.entry_index, &authored.content)?;
        persist_project_allocation(
            library,
            stage_id,
            object_id,
            authored.key.clone(),
            original,
            authored.content.clone(),
            edit.entry_index,
        )?;
        edit.entry_index
    };
    bmg.validate_sms_dialogue()?;
    Ok((original.full_message_id & 0xffff_0000) | entry_index as u32)
}

fn materialize_project_allocations(
    library: &ProjectDialogueLibrary,
    common_messages: &mut BTreeMap<(DialogueDomain, Vec<u8>), BmgFile>,
    changed_common_messages: &mut BTreeSet<(DialogueDomain, Vec<u8>)>,
) -> Result<()> {
    let mut by_resource =
        BTreeMap::<(DialogueDomain, Vec<u8>), Vec<&ProjectDialogueAllocation>>::new();
    for allocation in &library.stable_allocations {
        if allocation.domain == DialogueDomain::Stage {
            return Err(SceneError::StageExport(
                "project dialogue allocation targets a stage-local BMG".to_string(),
            ));
        }
        by_resource
            .entry((allocation.domain, allocation.raw_resource_path.clone()))
            .or_default()
            .push(allocation);
    }
    for (resource_key, mut allocations) in by_resource {
        let bmg = common_messages.get_mut(&resource_key).ok_or_else(|| {
            SceneError::StageExport(format!(
                "project dialogue allocation resource {} was not found",
                String::from_utf8_lossy(&resource_key.1)
            ))
        })?;
        let retail_entry_count = bmg.entries.len();
        allocations.sort_by_key(|allocation| allocation.message_index);
        let attribute_size = usize::from(bmg.entry_size).checked_sub(4).ok_or_else(|| {
            SceneError::StageExport("BMG entry size is smaller than 4".to_string())
        })?;
        for allocation in allocations {
            let index = usize::from(allocation.message_index);
            if index < retail_entry_count {
                return Err(SceneError::StageExport(format!(
                    "project dialogue allocation {} collides with a retail entry in {}",
                    allocation.message_index,
                    String::from_utf8_lossy(&allocation.raw_resource_path)
                )));
            }
            while bmg.entries.len() < index {
                bmg.append_entry_stable(vec![0; attribute_size], BmgMessage::default())?;
            }
            if bmg.entries.len() == index {
                if let Some(content) = allocation.content.as_ref() {
                    validate_dialogue_content(content)?;
                    bmg.append_entry_stable(
                        content.attributes.clone(),
                        content.compiled_message()?,
                    )?;
                    apply_entry_content(bmg, index, content)?;
                } else {
                    bmg.append_entry_stable(vec![0; attribute_size], BmgMessage::default())?;
                }
            } else if let Some(content) = allocation.content.as_ref() {
                validate_dialogue_content(content)?;
                apply_unique_content(bmg, index, content)?;
            }
        }
        bmg.validate_sms_dialogue()?;
        changed_common_messages.insert(resource_key);
    }
    Ok(())
}

fn persist_project_allocation(
    library: &mut ProjectDialogueLibrary,
    stage_id: &str,
    object_id: &str,
    key: DialogueVariantKey,
    original: &DialogueMessageRef,
    content: DialogueContent,
    entry_index: usize,
) -> Result<()> {
    if entry_index >= SMS_BMG_RUNTIME_MESSAGE_LIMIT {
        return Err(SceneError::StageExport(format!(
            "project dialogue allocation {entry_index} exceeds Sunshine's {}-entry runtime limit",
            SMS_BMG_RUNTIME_MESSAGE_LIMIT
        )));
    }
    let message_index = u16::try_from(entry_index).map_err(|_| {
        SceneError::StageExport("project dialogue allocation overflowed u16".to_string())
    })?;
    library.stable_allocations.push(ProjectDialogueAllocation {
        stage_id: stage_id.to_string(),
        object_id: object_id.to_string(),
        key,
        message_index,
        domain: original.domain,
        raw_resource_path: original.raw_resource_path.clone(),
        content: Some(content),
    });
    library.stable_allocations.sort_by(|left, right| {
        left.stage_id
            .cmp(&right.stage_id)
            .then_with(|| left.object_id.cmp(&right.object_id))
            .then_with(|| left.key.cmp(&right.key))
    });
    Ok(())
}

fn inherited_generated_message_id(
    authoring: &DialogueAuthoringDocument,
    object_id: &str,
    key: &DialogueVariantKey,
    compiled_messages: &BTreeMap<(String, DialogueVariantKey), u32>,
) -> Option<u32> {
    let mut visited = BTreeSet::new();
    let mut current = authoring
        .objects
        .get(object_id)
        .and_then(|object| object.inherited_from_object_id.as_deref());
    while let Some(source_object_id) = current {
        if !visited.insert(source_object_id) {
            return None;
        }
        if let Some(message_id) = compiled_messages
            .get(&(source_object_id.to_string(), key.clone()))
            .copied()
        {
            return Some(message_id);
        }
        current = authoring
            .objects
            .get(source_object_id)
            .and_then(|object| object.inherited_from_object_id.as_deref());
    }
    None
}

fn compile_generated_message(
    authoring: &mut DialogueAuthoringDocument,
    object_id: &str,
    authored: &DialogueVariantOverride,
    stage_messages: &mut BTreeMap<Vec<u8>, BmgFile>,
) -> Result<(u32, Vec<u8>)> {
    let path = stage_message_path(stage_messages);
    let bmg = stage_messages
        .entry(path.clone())
        .or_insert_with(empty_sms_bmg);
    let entry_index = allocate_or_replace_authored_entry(authoring, object_id, authored, bmg)?;
    bmg.validate_sms_dialogue()?;
    Ok((0x0001_0000 | entry_index as u32, path))
}

fn compile_generated_dialogue_script(
    document: &StageDocument,
    routes: &[(String, u32)],
) -> Result<SpcDocument> {
    if let Some(existing) = document.effective_resource_clone(GENERATED_DIALOGUE_SCRIPT_PATH)? {
        let StageResourceDocument::Script(existing) = existing else {
            return Err(SceneError::StageExport(format!(
                "owned dialogue path {} contains a non-SPC resource",
                String::from_utf8_lossy(GENERATED_DIALOGUE_SCRIPT_PATH)
            )));
        };
        if !existing
            .symbols
            .iter()
            .any(|symbol| symbol.name == GENERATED_DIALOGUE_SCRIPT_MARKER)
        {
            return Err(SceneError::StageExport(format!(
                "refusing to replace unowned script {}",
                String::from_utf8_lossy(GENERATED_DIALOGUE_SCRIPT_PATH)
            )));
        }
    }

    let mut program = SpcRelocatableProgram::new(1);
    program.append_symbol(SpcProgramSymbol {
        symbol_type: 2,
        data: 0,
        native_call: 0,
        name: GENERATED_DIALOGUE_SCRIPT_MARKER.to_string(),
    })?;
    let is_talk_mode = append_builtin_symbol(&mut program, "isTalkModeNow")?;
    let get_talk_name = append_builtin_symbol(&mut program, "getTalkNPCName")?;
    let set_message = append_builtin_symbol(&mut program, "setTalkMsgID")?;
    let yield_builtin = append_builtin_symbol(&mut program, "yield")?;

    let loop_start = program.instructions.len();
    program.push_instruction(SpcInstruction::Builtin {
        symbol_index: is_talk_mode,
        argument_count: 0,
    });
    let no_talk_jump = program.push_instruction(SpcInstruction::JumpIfZero(0));
    let mut next_route_jumps = Vec::new();
    let mut matched_route_jumps = Vec::new();
    let mut sorted_routes = routes.to_vec();
    sorted_routes.sort_by(|left, right| left.0.cmp(&right.0));
    for (object_id, message_id) in sorted_routes {
        let runtime_name = document
            .objects
            .iter()
            .find(|object| object.id == object_id)
            .and_then(|object| object.raw_param("name"))
            .ok_or_else(|| {
                SceneError::StageExport(format!(
                    "generated dialogue object {object_id:?} has no runtime name"
                ))
            })?;
        if !runtime_name.starts_with(GENERATED_DIALOGUE_RUNTIME_NAME_PREFIX) {
            return Err(SceneError::StageExport(format!(
                "generated dialogue object {object_id:?} does not use an editor-owned internal runtime name"
            )));
        }
        let name_index = program.append_data(runtime_name.to_string())?;
        program.push_instruction(SpcInstruction::Builtin {
            symbol_index: get_talk_name,
            argument_count: 0,
        });
        program.push_instruction(SpcInstruction::String(name_index));
        program.push_instruction(SpcInstruction::Equal);
        let next_route_jump = program.push_instruction(SpcInstruction::JumpIfZero(0));
        program.push_instruction(SpcInstruction::Int(message_id as i32));
        // A generated normal route is a complete standalone conversation.
        // TTalk2D2 treats bit 0 as the terminal-message flag: without it the
        // window closes into mode 1 ("waiting for the next message") and the
        // director intentionally keeps Mario and the talk camera locked.
        program.push_instruction(SpcInstruction::Int(1));
        program.push_instruction(SpcInstruction::Builtin {
            symbol_index: set_message,
            argument_count: 2,
        });
        program.push_instruction(SpcInstruction::Pop);
        let matched_route_jump = program.push_instruction(SpcInstruction::Jump(0));
        next_route_jumps.push((next_route_jump, program.instructions.len()));
        matched_route_jumps.push(matched_route_jump);
    }
    let idle_yield_index = program.instructions.len();
    program.push_instruction(SpcInstruction::Builtin {
        symbol_index: yield_builtin,
        argument_count: 0,
    });
    program.push_instruction(SpcInstruction::Pop);
    let idle_loop_jump = program.push_instruction(SpcInstruction::Jump(0));

    // Once one generated route has selected its message, do not invoke
    // setTalkMsgID again during the same talk phase: doing so resets TTalk2D2's
    // page/control progress. Yield until retail leaves talk mode, then arm the
    // detector for the next conversation.
    let wait_for_talk_end = program.instructions.len();
    program.push_instruction(SpcInstruction::Builtin {
        symbol_index: yield_builtin,
        argument_count: 0,
    });
    program.push_instruction(SpcInstruction::Pop);
    program.push_instruction(SpcInstruction::Builtin {
        symbol_index: is_talk_mode,
        argument_count: 0,
    });
    let talk_ended_jump = program.push_instruction(SpcInstruction::JumpIfZero(0));
    let wait_loop_jump = program.push_instruction(SpcInstruction::Jump(0));
    let end_index = program.push_instruction(SpcInstruction::End);
    program.set_instruction_target(no_talk_jump, idle_yield_index)?;
    for (jump, target) in next_route_jumps {
        program.set_instruction_target(jump, target.min(idle_yield_index))?;
    }
    for jump in matched_route_jumps {
        program.set_instruction_target(jump, wait_for_talk_end)?;
    }
    program.set_instruction_target(idle_loop_jump, loop_start)?;
    program.set_instruction_target(talk_ended_jump, loop_start)?;
    program.set_instruction_target(wait_loop_jump, wait_for_talk_end)?;
    // Keep a structurally valid target for tools which inspect the terminal
    // End even though the generated loop never exits.
    if end_index >= program.instructions.len() {
        return Err(SceneError::StageExport(
            "generated dialogue script lost its terminal instruction".to_string(),
        ));
    }
    Ok(program.to_document()?)
}

fn append_builtin_symbol(program: &mut SpcRelocatableProgram, name: &str) -> Result<u32> {
    let data = u32::try_from(program.symbols.len()).map_err(|_| {
        SceneError::StageExport("generated dialogue symbol table overflowed u32".to_string())
    })?;
    Ok(program.append_symbol(SpcProgramSymbol {
        symbol_type: 0,
        data,
        native_call: 0,
        name: name.to_string(),
    })?)
}

fn allocate_or_replace_authored_entry(
    authoring: &mut DialogueAuthoringDocument,
    object_id: &str,
    authored: &DialogueVariantOverride,
    bmg: &mut BmgFile,
) -> Result<usize> {
    if let Some(index) = find_stable_allocation(authoring, object_id, &authored.key) {
        let index = index as usize;
        rematerialize_persisted_allocation(bmg, index, object_id, &authored.content)?;
        return Ok(index);
    }
    let edit = bmg.append_entry_stable(
        authored.content.attributes.clone(),
        authored.content.compiled_message()?,
    )?;
    apply_entry_content(bmg, edit.entry_index, &authored.content)?;
    persist_stable_allocation(authoring, object_id, authored.key.clone(), edit.entry_index)?;
    Ok(edit.entry_index)
}

fn rematerialize_persisted_allocation(
    bmg: &mut BmgFile,
    index: usize,
    object_id: &str,
    content: &DialogueContent,
) -> Result<()> {
    while bmg.entries.len() < index {
        // Stable allocations are never renumbered or reused. If an older
        // authored route was removed, retain its ordinal as an unreachable
        // empty tombstone so later allocations still rebuild identically
        // from retail resources plus authoring deltas alone.
        bmg.append_entry_stable(vec![0; 8], BmgMessage::default())?;
    }
    if bmg.entries.len() == index {
        let edit =
            bmg.append_entry_stable(content.attributes.clone(), content.compiled_message()?)?;
        if edit.entry_index != index {
            return Err(SceneError::StageExport(format!(
                "persisted dialogue allocation {index} for object {object_id:?} rebuilt at unexpected index {}",
                edit.entry_index
            )));
        }
    }
    apply_unique_content(bmg, index, content)
}

fn find_stable_allocation(
    authoring: &DialogueAuthoringDocument,
    object_id: &str,
    key: &DialogueVariantKey,
) -> Option<u16> {
    authoring
        .objects
        .get(object_id)
        .and_then(|object| {
            object
                .stable_allocations
                .iter()
                .find(|allocation| &allocation.key == key)
        })
        .map(|allocation| allocation.message_index)
}

fn persist_stable_allocation(
    authoring: &mut DialogueAuthoringDocument,
    object_id: &str,
    key: DialogueVariantKey,
    entry_index: usize,
) -> Result<()> {
    if entry_index >= SMS_BMG_RUNTIME_MESSAGE_LIMIT {
        return Err(SceneError::StageExport(format!(
            "dialogue allocation {entry_index} exceeds Sunshine's {}-entry runtime limit",
            SMS_BMG_RUNTIME_MESSAGE_LIMIT
        )));
    }
    let message_index = u16::try_from(entry_index)
        .map_err(|_| SceneError::StageExport("dialogue allocation overflowed u16".to_string()))?;
    let object = authoring.objects.entry(object_id.to_string()).or_default();
    if let Some(allocation) = object
        .stable_allocations
        .iter_mut()
        .find(|allocation| allocation.key == key)
    {
        allocation.message_index = message_index;
    } else {
        object
            .stable_allocations
            .push(DialogueStableAllocation { key, message_index });
        object
            .stable_allocations
            .sort_by(|left, right| left.key.cmp(&right.key));
    }
    Ok(())
}

fn stage_message_path(messages: &BTreeMap<Vec<u8>, BmgFile>) -> Vec<u8> {
    messages
        .keys()
        .find(|path| normalized_raw_path(path) == STAGE_DIALOGUE_MESSAGE_PATH)
        .cloned()
        .unwrap_or_else(|| STAGE_DIALOGUE_MESSAGE_PATH.to_vec())
}

/// Resolves a persisted copy-on-write allocation for one derived route. An
/// untouched duplicate follows the nearest inherited instance override, just
/// as dialogue compilation does when it emits the guarded runtime remap.
fn effective_dialogue_consumer_allocation(
    document: &StageDocument,
    variant: &DialogueVariant,
) -> Option<EffectiveDialogueAllocation> {
    let original = variant.message.as_ref()?;
    let authoring = document.dialogue_authoring.as_ref()?;
    let mut visited = BTreeSet::new();
    let mut current_object_id = Some(variant.object_id.as_str());
    while let Some(object_id) = current_object_id {
        if !visited.insert(object_id) {
            return None;
        }
        let object_authoring = authoring.objects.get(object_id)?;
        if let Some(authored) = object_authoring
            .overrides
            .iter()
            .find(|authored| authored.key == variant.key)
        {
            if authored.scope != DialogueEditScope::Instance {
                return None;
            }
            if let Some(allocation) =
                document
                    .dialogue_library
                    .stable_allocations
                    .iter()
                    .find(|allocation| {
                        allocation.stage_id == document.stage_id
                            && allocation.object_id == object_id
                            && allocation.key == variant.key
                    })
            {
                return Some(EffectiveDialogueAllocation {
                    domain: allocation.domain,
                    raw_resource_path: allocation.raw_resource_path.clone(),
                    message_index: allocation.message_index,
                });
            }
            let allocation = object_authoring
                .stable_allocations
                .iter()
                .find(|allocation| allocation.key == variant.key)?;
            let (domain, raw_resource_path) = if original.domain == DialogueDomain::Stage {
                (original.domain, original.raw_resource_path.clone())
            } else {
                (DialogueDomain::Stage, STAGE_DIALOGUE_MESSAGE_PATH.to_vec())
            };
            return Some(EffectiveDialogueAllocation {
                domain,
                raw_resource_path,
                message_index: allocation.message_index,
            });
        }
        current_object_id = object_authoring.inherited_from_object_id.as_deref();
    }
    None
}

fn dialogue_route_rejoins_with_shared_override(
    document: &StageDocument,
    candidate_object_id: &str,
    selected_object_id: &str,
    key: &DialogueVariantKey,
) -> bool {
    if candidate_object_id == selected_object_id {
        return true;
    }
    let Some(authoring) = document.dialogue_authoring.as_ref() else {
        return false;
    };
    let mut visited = BTreeSet::new();
    let mut current_object_id = candidate_object_id;
    loop {
        if !visited.insert(current_object_id) {
            return false;
        }
        let Some(object_authoring) = authoring.objects.get(current_object_id) else {
            return false;
        };
        if object_authoring
            .overrides
            .iter()
            .any(|authored| &authored.key == key)
        {
            return false;
        }
        let Some(source_object_id) = object_authoring.inherited_from_object_id.as_deref() else {
            return false;
        };
        if source_object_id == selected_object_id {
            return true;
        }
        current_object_id = source_object_id;
    }
}

fn empty_sms_bmg() -> BmgFile {
    BmgFile {
        header_reserved: [0; 16],
        info_section_size: 0x20,
        data_section_size: 0x20,
        entry_size: 12,
        group_id: 0,
        default_color: 0,
        info_reserved: 0,
        entries: Vec::new(),
    }
}

pub(crate) fn validate_dialogue_document(document: &StageDocument) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    let authoring = document.dialogue_authoring.as_ref();
    if let Some(authoring) = authoring {
        if authoring.format_version != DIALOGUE_AUTHORING_FORMAT_VERSION {
            issues.push(ValidationIssue::error(
                "dialogue-authoring-version-unsupported",
                format!(
                    "Dialogue authoring format version {} is unsupported; expected {}",
                    authoring.format_version, DIALOGUE_AUTHORING_FORMAT_VERSION
                ),
            ));
        }
        let object_ids = document
            .objects
            .iter()
            .map(|object| object.id.as_str())
            .collect::<BTreeSet<_>>();
        for cycle in dialogue_inheritance_cycles(authoring) {
            let mut path = cycle.clone();
            path.push(cycle[0].clone());
            issues.push(ValidationIssue::error(
                "dialogue-inheritance-cycle",
                format!("Dialogue inheritance cycle: {}", path.join(" -> ")),
            ));
        }
        let mut allocated = BTreeMap::<(Vec<u8>, u16), String>::new();
        let mut stage_shared = BTreeMap::<DialogueMessageRef, (DialogueContent, String)>::new();
        for (object_id, object) in &authoring.objects {
            let has_active_instance_authoring =
                !object.overrides.is_empty() || object.inherited_from_object_id.is_some();
            if !object_ids.contains(object_id.as_str()) && has_active_instance_authoring {
                issues.push(ValidationIssue::error(
                    "dialogue-object-missing",
                    format!("Dialogue authoring references missing object {object_id:?}"),
                ));
            }
            if has_active_instance_authoring {
                if let (Some(registry), Some(scene_object)) = (
                    document.registry.as_ref(),
                    document
                        .objects
                        .iter()
                        .find(|scene_object| scene_object.id == *object_id),
                ) {
                    if !registry.is_dialogue_instance_eligible(&scene_object.factory_name) {
                        issues.push(ValidationIssue::error(
                            "dialogue-object-ineligible",
                            format!(
                                "Dialogue object {object_id:?} factory {:?} is not eligible for placed-instance dialogue",
                                scene_object.factory_name
                            ),
                        ));
                    }
                }
            }
            if let Some(source) = object.inherited_from_object_id.as_deref() {
                if !object_ids.contains(source) {
                    issues.push(ValidationIssue::error(
                        "dialogue-inherited-source-missing",
                        format!(
                            "Dialogue object {object_id:?} inherits from missing object {source:?}"
                        ),
                    ));
                }
            }
            let mut keys = BTreeSet::new();
            for authored in &object.overrides {
                if !keys.insert(&authored.key) {
                    issues.push(ValidationIssue::error(
                        "dialogue-override-duplicate",
                        format!(
                            "Dialogue object {object_id:?} has duplicate overrides for one semantic route"
                        ),
                    ));
                }
                let shared_common_marker = authored.scope == DialogueEditScope::Shared
                    && authored
                        .key
                        .original_message
                        .as_ref()
                        .is_some_and(|message| message.domain != DialogueDomain::Stage);
                if shared_common_marker
                    && !document
                        .dialogue_library
                        .common_overrides
                        .iter()
                        .any(|shared| {
                            Some(&shared.message) == authored.key.original_message.as_ref()
                        })
                {
                    issues.push(ValidationIssue::error(
                        "dialogue-common-shared-marker-orphaned",
                        format!(
                            "Dialogue object {object_id:?} has a shared common route marker without a project-library override"
                        ),
                    ));
                }
                if !shared_common_marker {
                    match validate_dialogue_content(&authored.content) {
                        Ok(()) => {}
                        Err(error) => issues.push(ValidationIssue::error(
                            "dialogue-content-invalid",
                            format!("Dialogue object {object_id:?}: {error}"),
                        )),
                    }
                }
                if authored.scope == DialogueEditScope::Shared {
                    if let Some(message) = authored
                        .key
                        .original_message
                        .as_ref()
                        .filter(|message| message.domain == DialogueDomain::Stage)
                    {
                        if let Some((previous_content, previous_object)) = stage_shared.get(message)
                        {
                            if previous_content != &authored.content {
                                issues.push(ValidationIssue::error(
                                    "dialogue-stage-shared-override-conflict",
                                    format!(
                                        "Dialogue objects {previous_object:?} and {object_id:?} assign different shared content to stage message {} in {}",
                                        message.entry_index,
                                        String::from_utf8_lossy(&message.raw_resource_path)
                                    ),
                                ));
                            }
                        } else {
                            stage_shared.insert(
                                message.clone(),
                                (authored.content.clone(), object_id.clone()),
                            );
                        }
                    }
                }
                if !shared_common_marker && !dialogue_content_has_visible_text(&authored.content) {
                    issues.push(ValidationIssue::warning(
                        "dialogue-message-empty",
                        format!("Dialogue object {object_id:?} has an intentionally empty message"),
                    ));
                }
                if authored.key.original_message.is_none() {
                    let runtime_name = document
                        .objects
                        .iter()
                        .find(|scene_object| scene_object.id == *object_id)
                        .and_then(|scene_object| scene_object.raw_param("name"))
                        .unwrap_or(object_id);
                    let matching_objects = document
                        .objects
                        .iter()
                        .filter(|scene_object| {
                            scene_object.raw_param("name").unwrap_or(&scene_object.id)
                                == runtime_name
                        })
                        .count();
                    if matching_objects != 1 {
                        issues.push(ValidationIssue::error(
                            "dialogue-generated-name-ambiguous",
                            format!(
                                "Generated dialogue for object {object_id:?} targets runtime name {runtime_name:?}, which is shared by {matching_objects} placed objects"
                            ),
                        ));
                    }
                }
            }
            let mut allocation_keys = BTreeSet::new();
            for allocation in &object.stable_allocations {
                if !allocation_keys.insert(&allocation.key) {
                    issues.push(ValidationIssue::error(
                        "dialogue-allocation-duplicate-key",
                        format!(
                            "Dialogue object {object_id:?} has duplicate stable allocations for one route"
                        ),
                    ));
                }
                if allocation.message_index as usize >= SMS_BMG_RUNTIME_MESSAGE_LIMIT {
                    issues.push(ValidationIssue::error(
                        "dialogue-allocation-out-of-range",
                        format!(
                            "Dialogue object {object_id:?} allocation {} exceeds Sunshine's runtime message table",
                            allocation.message_index
                        ),
                    ));
                }
                let resource_path = allocation
                    .key
                    .original_message
                    .as_ref()
                    .filter(|message| message.domain == DialogueDomain::Stage)
                    .map(|message| message.raw_resource_path.clone())
                    .unwrap_or_else(|| STAGE_DIALOGUE_MESSAGE_PATH.to_vec());
                if let Some(previous) = allocated.insert(
                    (resource_path.clone(), allocation.message_index),
                    object_id.clone(),
                ) {
                    issues.push(ValidationIssue::error(
                        "dialogue-allocation-conflict",
                        format!(
                            "Dialogue allocation owners {previous:?} and {object_id:?} both reserve stable message index {} in {}",
                            allocation.message_index,
                            String::from_utf8_lossy(&resource_path)
                        ),
                    ));
                }
            }
        }
    }
    if document.dialogue_library.format_version != PROJECT_DIALOGUE_LIBRARY_FORMAT_VERSION {
        issues.push(ValidationIssue::error(
            "dialogue-library-version-unsupported",
            format!(
                "Project dialogue library format version {} is unsupported; expected {}",
                document.dialogue_library.format_version, PROJECT_DIALOGUE_LIBRARY_FORMAT_VERSION
            ),
        ));
    }
    let mut project_allocation_keys = BTreeSet::new();
    let mut project_allocation_indexes =
        BTreeMap::<(DialogueDomain, Vec<u8>, u16), (String, String)>::new();
    for allocation in &document.dialogue_library.stable_allocations {
        let allocation_key = (
            allocation.stage_id.as_str(),
            allocation.object_id.as_str(),
            &allocation.key,
        );
        if !project_allocation_keys.insert(allocation_key) {
            issues.push(ValidationIssue::error(
                "dialogue-project-allocation-duplicate-key",
                format!(
                    "Project dialogue allocation for stage {:?}, object {:?} repeats one route key",
                    allocation.stage_id, allocation.object_id
                ),
            ));
        }
        if allocation.message_index as usize >= SMS_BMG_RUNTIME_MESSAGE_LIMIT {
            issues.push(ValidationIssue::error(
                "dialogue-project-allocation-out-of-range",
                format!(
                    "Project dialogue allocation {} exceeds Sunshine's runtime message table",
                    allocation.message_index
                ),
            ));
        }
        if allocation.domain == DialogueDomain::Stage {
            issues.push(ValidationIssue::error(
                "dialogue-project-allocation-stage-local",
                "Project dialogue allocation targets a stage-local BMG",
            ));
        }
        if allocation.raw_resource_path.is_empty() {
            issues.push(ValidationIssue::error(
                "dialogue-project-allocation-resource-missing",
                "Project dialogue allocation has no common resource path",
            ));
        }
        if let Some(content) = allocation.content.as_ref() {
            if let Err(error) = validate_dialogue_content(content) {
                issues.push(ValidationIssue::error(
                    "dialogue-project-allocation-content-invalid",
                    error.to_string(),
                ));
            }
        }
        if let Some((previous_stage, previous_object)) = project_allocation_indexes.insert(
            (
                allocation.domain,
                allocation.raw_resource_path.clone(),
                allocation.message_index,
            ),
            (allocation.stage_id.clone(), allocation.object_id.clone()),
        ) {
            issues.push(ValidationIssue::error(
                "dialogue-project-allocation-conflict",
                format!(
                    "Project dialogue allocations {previous_stage:?}/{previous_object:?} and {:?}/{:?} both own common message index {}",
                    allocation.stage_id, allocation.object_id, allocation.message_index
                ),
            ));
        }
    }
    let mut common_refs = BTreeSet::new();
    for shared in &document.dialogue_library.common_overrides {
        if !common_refs.insert(&shared.message) {
            issues.push(ValidationIssue::error(
                "dialogue-common-override-conflict",
                format!(
                    "Project dialogue library has conflicting edits for message {} in {}",
                    shared.message.entry_index,
                    String::from_utf8_lossy(&shared.message.raw_resource_path)
                ),
            ));
        }
        if shared.message.domain == DialogueDomain::Stage {
            issues.push(ValidationIssue::error(
                "dialogue-common-override-stage-local",
                "Project dialogue library contains a stage-local message override",
            ));
        }
        if let Err(error) = validate_dialogue_content(&shared.content) {
            issues.push(ValidationIssue::error(
                "dialogue-common-content-invalid",
                error.to_string(),
            ));
        }
        if !dialogue_content_has_visible_text(&shared.content) {
            issues.push(ValidationIssue::warning(
                "dialogue-message-empty",
                format!(
                    "Common dialogue message {} in {} has intentionally empty authored text",
                    shared.message.entry_index,
                    String::from_utf8_lossy(&shared.message.raw_resource_path)
                ),
            ));
        }
    }

    issues
}

#[derive(Debug, Clone)]
struct DialogueResourceSet {
    scripts: Vec<DialogueScriptResource>,
    messages: BTreeMap<(DialogueDomain, Vec<u8>), BmgFile>,
    collection_issues: Vec<DialogueResolutionIssue>,
}

impl DialogueResourceSet {
    fn empty() -> Self {
        Self {
            scripts: Vec::new(),
            messages: BTreeMap::new(),
            collection_issues: Vec::new(),
        }
    }

    fn extend(&mut self, other: Self) {
        self.scripts.extend(other.scripts);
        self.messages.extend(other.messages);
        self.collection_issues.extend(other.collection_issues);
        self.scripts
            .sort_by(|left, right| left.raw_path.cmp(&right.raw_path));
    }
}

#[derive(Debug, Clone)]
struct DialogueScriptResource {
    raw_path: Vec<u8>,
    document: SpcDocument,
}

fn collect_dialogue_resources(document: &StageDocument) -> Result<DialogueResourceSet> {
    let mut resources = collect_stage_dialogue_resources(document)?;
    resources.extend(collect_common_dialogue_resources(&document.base_root)?);
    Ok(resources)
}

fn collect_stage_dialogue_resources(document: &StageDocument) -> Result<DialogueResourceSet> {
    let mut resources = DialogueResourceSet::empty();
    let mut stage_paths = BTreeSet::new();
    let mut mounted_fallbacks = BTreeMap::<Vec<u8>, PathBuf>::new();
    if let Some(archive) = &document.stage_archive {
        stage_paths.extend(
            archive
                .resources()
                .iter()
                .map(|resource| resource.raw_path.clone()),
        );
    }
    stage_paths.extend(
        document
            .archive_edits
            .resources
            .iter()
            .map(|edit| edit.raw_resource_path.clone()),
    );
    if document.stage_archive.is_none() {
        for asset in &document.assets {
            let Some(raw_path) = raw_path_from_mounted_asset(&asset.path) else {
                continue;
            };
            if is_dialogue_resource_path(&raw_path) {
                stage_paths.insert(raw_path.clone());
                mounted_fallbacks.insert(raw_path, asset.path.clone());
            }
        }
    }
    for raw_path in stage_paths {
        if !is_dialogue_resource_path(&raw_path) {
            continue;
        }
        let effective = document.effective_resource_clone(&raw_path)?;
        let effective = if effective.is_some() {
            effective
        } else if let Some(path) = mounted_fallbacks.get(&raw_path) {
            let bytes = read_stage_asset_bytes(path)?;
            match StageResourceDocument::parse_for_path(&raw_path, &bytes) {
                Ok(resource) => Some(resource),
                Err(error) => {
                    resources.collection_issues.push(DialogueResolutionIssue {
                        severity: DialogueResolutionSeverity::Error,
                        code: "dialogue-stage-resource-parse-failed",
                        message: format!(
                            "Could not parse stage dialogue resource {}: {error}",
                            String::from_utf8_lossy(&raw_path)
                        ),
                        script_path: Some(raw_path.clone()),
                    });
                    None
                }
            }
        } else {
            None
        };
        match effective {
            Some(StageResourceDocument::Script(script)) => {
                resources.scripts.push(DialogueScriptResource {
                    raw_path,
                    document: script,
                })
            }
            Some(StageResourceDocument::Message(message)) => {
                let domain = if normalized_raw_path(&raw_path).ends_with(b"balloon.bmg") {
                    DialogueDomain::Balloon
                } else if normalized_raw_path(&raw_path).ends_with(b"sys_message.bmg") {
                    DialogueDomain::System
                } else {
                    DialogueDomain::Stage
                };
                resources.messages.insert((domain, raw_path), message);
            }
            Some(_) | None => {}
        }
    }
    resources
        .scripts
        .sort_by(|left, right| left.raw_path.cmp(&right.raw_path));
    Ok(resources)
}

fn collect_common_dialogue_resources(base_root: &Path) -> Result<DialogueResourceSet> {
    let mut resources = DialogueResourceSet::empty();
    let common_archive = find_common_archive(base_root);
    if let Some(common_archive) = common_archive {
        let mounted = mount_scene_archive(&common_archive)?;
        for asset in mounted {
            let Some(raw_path) = raw_path_from_mounted_asset(&asset.path) else {
                continue;
            };
            if !is_dialogue_resource_path(&raw_path) {
                continue;
            }
            let bytes = read_stage_asset_bytes(&asset.path)?;
            match StageResourceDocument::parse_for_path(&raw_path, &bytes) {
                Ok(StageResourceDocument::Script(script)) => {
                    resources.scripts.push(DialogueScriptResource {
                        raw_path,
                        document: script,
                    });
                }
                Ok(StageResourceDocument::Message(message)) => {
                    let normalized = normalized_raw_path(&raw_path);
                    let domain = if normalized.ends_with(b"balloon.bmg") {
                        Some(DialogueDomain::Balloon)
                    } else if normalized.ends_with(b"sys_message.bmg") {
                        Some(DialogueDomain::System)
                    } else {
                        None
                    };
                    if let Some(domain) = domain {
                        resources.messages.insert((domain, raw_path), message);
                    }
                }
                Ok(_) => {}
                Err(error) => resources.collection_issues.push(DialogueResolutionIssue {
                    severity: DialogueResolutionSeverity::Error,
                    code: "dialogue-common-resource-parse-failed",
                    message: format!(
                        "Could not parse common dialogue resource {}: {error}",
                        String::from_utf8_lossy(&raw_path)
                    ),
                    script_path: Some(raw_path),
                }),
            }
        }
    } else {
        resources.collection_issues.push(DialogueResolutionIssue {
            severity: DialogueResolutionSeverity::Warning,
            code: "dialogue-common-archive-missing",
            message: "The extracted base has no files/data/common.szs; system and balloon dialogue could not be indexed."
                .to_string(),
            script_path: None,
        });
    }
    resources
        .scripts
        .sort_by(|left, right| left.raw_path.cmp(&right.raw_path));
    Ok(resources)
}

fn find_common_archive(base_root: &Path) -> Option<PathBuf> {
    let canonical = base_root.join("files").join("data").join("common.szs");
    canonical.is_file().then_some(canonical)
}

fn raw_path_from_mounted_asset(path: &Path) -> Option<Vec<u8>> {
    let display = path.to_string_lossy().replace('\\', "/");
    display
        .split_once("!/")
        .map(|(_, raw_path)| raw_path.as_bytes().to_vec())
}

fn is_dialogue_resource_path(raw_path: &[u8]) -> bool {
    let normalized = normalized_raw_path(raw_path);
    normalized.ends_with(b".sb") || normalized.ends_with(b".bmg")
}

fn normalized_raw_path(raw_path: &[u8]) -> Vec<u8> {
    raw_path
        .iter()
        .map(|byte| match byte {
            b'\\' => b'/',
            byte => byte.to_ascii_lowercase(),
        })
        .collect()
}

fn dialogue_authoring_format_version() -> u32 {
    DIALOGUE_AUTHORING_FORMAT_VERSION
}

fn project_dialogue_library_format_version() -> u32 {
    PROJECT_DIALOGUE_LIBRARY_FORMAT_VERSION
}

fn stable_text_fingerprint(value: &str) -> u64 {
    value
        .as_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
        })
}

fn resolve_dialogue_routes(
    stage_id: &str,
    objects: &[SceneObject],
    registry: Option<&sms_schema::ObjectRegistry>,
    resources: &DialogueResourceSet,
) -> DialogueRouteIndex {
    let mut index = DialogueRouteIndex {
        variants_by_object: BTreeMap::new(),
        detached_consumers: Vec::new(),
        issues: resources.collection_issues.clone(),
        callsites: Vec::new(),
    };
    let objects_by_name = objects_by_runtime_name(objects);
    for script in &resources.scripts {
        resolve_script_routes(
            stage_id,
            script,
            resources,
            &objects_by_name,
            registry,
            &mut index,
        );
    }
    synthesize_stock_happy_routes(stage_id, objects, registry, resources, &mut index);
    attach_sharing_metadata(&mut index);
    for variants in index.variants_by_object.values_mut() {
        variants.sort_by(|left, right| left.key.cmp(&right.key));
        for pair in variants.windows(2) {
            if pair[0].key == pair[1].key {
                index.issues.push(DialogueResolutionIssue {
                    severity: DialogueResolutionSeverity::Error,
                    code: "dialogue-route-anchor-ambiguous",
                    message: format!(
                        "Object {:?} has more than one route matching the same semantic anchor",
                        pair[0].object_id
                    ),
                    script_path: Some(pair[0].key.source.raw_resource_path.clone()),
                });
            }
        }
    }
    index
}

fn objects_by_runtime_name(objects: &[SceneObject]) -> BTreeMap<String, Vec<&SceneObject>> {
    let mut by_name = BTreeMap::<String, Vec<&SceneObject>>::new();
    for object in objects {
        if let Some(name) = object.raw_param("name").filter(|name| !name.is_empty()) {
            by_name.entry(name.to_string()).or_default().push(object);
        }
    }
    by_name
}

fn synthesize_stock_happy_routes(
    stage_id: &str,
    objects: &[SceneObject],
    registry: Option<&sms_schema::ObjectRegistry>,
    resources: &DialogueResourceSet,
    index: &mut DialogueRouteIndex,
) {
    let Some(registry) = registry else {
        return;
    };
    for object in objects {
        let Some(actor_type) = registry.find_npc_actor_type(&object.factory_name) else {
            continue;
        };
        let runtime_name = object.raw_param("name").unwrap_or(&object.id);
        for (message_id, condition) in stock_happy_selections(object, actor_type, runtime_name) {
            let Some((message_ref, entry, storage_offset)) =
                resolve_message_reference(message_id, "setTalkMsgID", resources)
            else {
                index.issues.push(DialogueResolutionIssue {
                    severity: DialogueResolutionSeverity::Error,
                    code: "dialogue-stock-happy-message-unresolved",
                    message: format!(
                        "Stock happy/reward selection for object {:?} resolves final message {message_id:#010x}, but the system BMG entry is unavailable",
                        object.id
                    ),
                    script_path: Some(STOCK_HAPPY_DIALOGUE_SOURCE_PATH.to_vec()),
                });
                continue;
            };
            let fingerprint = stable_text_fingerprint(&format!(
                "TTalk2D2::setMessageID:{actor_type:08x}:{message_id:08x}:{condition}"
            ));
            let source = DialogueSourceAnchor {
                raw_resource_path: STOCK_HAPPY_DIALOGUE_SOURCE_PATH.to_vec(),
                function_symbol: "TTalk2D2::setMessageID".to_string(),
                normalized_fingerprint: fingerprint,
                callsite_occurrence: 0,
                original_message_id: Some(message_id),
            };
            index
                .variants_by_object
                .entry(object.id.clone())
                .or_default()
                .push(DialogueVariant {
                    stage_id: stage_id.to_string(),
                    object_id: object.id.clone(),
                    runtime_name: runtime_name.to_string(),
                    key: DialogueVariantKey {
                        source,
                        original_message: Some(message_ref.clone()),
                    },
                    route_kind: DialogueRouteKind::HappyOverride,
                    condition_path: condition,
                    message: Some(message_ref),
                    content: DialogueContent::from_bmg_entry(entry),
                    presentation_flags: None,
                    talk_flags: Some(0x200),
                    shared_consumers: Vec::new(),
                    provenance: DialogueProvenance::RuntimeOverride,
                    compiler_location: DialogueCompilerLocation {
                        script_path: STOCK_HAPPY_DIALOGUE_SOURCE_PATH.to_vec(),
                        message_instruction_index: 0,
                    },
                    message_storage_offset: Some(storage_offset),
                });
        }
    }
}

fn stock_happy_selections(
    object: &SceneObject,
    actor_type: u32,
    runtime_name: &str,
) -> Vec<(u32, String)> {
    if runtime_name == STOCK_HAPPY_AIRPORT_MONTE_NAME {
        return vec![(
            0x26,
            "Happy/reward flag set / stock exact-name override: Airport sinking Monte".to_string(),
        )];
    }
    if runtime_name == STOCK_HAPPY_MONTE_ONE_NAME {
        return vec![(
            0x23,
            "Happy/reward flag set / stock exact-name override: Monte 1".to_string(),
        )];
    }

    let child = object
        .transform
        .scale
        .iter()
        .all(|component| *component < 0.7);
    let mut selections = match actor_type {
        NPC_ACTOR_TYPE_MONTE_M_FIRST..=NPC_ACTOR_TYPE_MONTE_M_LAST => vec![(
            if child { 0x29 } else { 0x23 },
            format!(
                "Happy/reward flag set / stock Monte male {} selection",
                if child { "child" } else { "adult" }
            ),
        )],
        NPC_ACTOR_TYPE_MONTE_W_FIRST..=NPC_ACTOR_TYPE_MONTE_W_LAST => vec![(
            if child { 0x2c } else { 0x27 },
            format!(
                "Happy/reward flag set / stock Monte female {} selection",
                if child { "child" } else { "adult" }
            ),
        )],
        NPC_ACTOR_TYPE_MARE_M_FIRST..=NPC_ACTOR_TYPE_MARE_M_LAST => vec![(
            if child { 0x2a } else { 0x24 },
            format!(
                "Happy/reward flag set / stock Noki male {} selection",
                if child { "child" } else { "adult" }
            ),
        )],
        NPC_ACTOR_TYPE_MARE_W_FIRST..=NPC_ACTOR_TYPE_MARE_W_LAST => vec![(
            if child { 0x2d } else { 0x28 },
            format!(
                "Happy/reward flag set / stock Noki female {} selection",
                if child { "child" } else { "adult" }
            ),
        )],
        NPC_ACTOR_TYPE_KINOPIO => vec![(
            0x25,
            "Happy/reward flag set / stock Toad selection".to_string(),
        )],
        _ => Vec::new(),
    };
    // The retail function also carries a dedicated actor-type fallback for
    // 0x04000010 -> 0x2b. Keep it as a distinct final-ID route so a build can
    // remap whichever stock branch the exact executable reaches.
    if actor_type == NPC_ACTOR_TYPE_MARE_MB_OVERRIDE {
        selections.push((
            0x2b,
            "Happy/reward flag set / stock actor-type 0x04000010 override".to_string(),
        ));
    }
    selections
}

// Resolver implementation continues below.  It is intentionally a small,
// conservative abstract interpreter rather than a filename-specific parser.

#[derive(Debug, Clone)]
struct FunctionRange {
    name: String,
    start_address: u32,
    start_index: usize,
    end_index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AbstractValue {
    Int {
        value: i32,
        instruction_index: usize,
        branch_scopes: Vec<BranchScope>,
    },
    /// Bounded reaching definitions for a variable assigned under control
    /// flow. `may_be_unknown` keeps the analysis conservative when one branch
    /// has no statically known integer value.
    FiniteInts {
        values: BTreeMap<i32, AbstractIntCandidate>,
        may_be_unknown: bool,
    },
    String(String),
    TalkNpcName,
    TalkSelectedValue,
    SystemFlag(u32),
    Predicate(String),
    NameHandle(String),
    NameEquals(String),
    NameNotEquals(String),
    SelectedEquals(i32),
    SelectedNotEquals(i32),
    SystemFlagEquals {
        flag: u32,
        value: i32,
    },
    SystemFlagNotEquals {
        flag: u32,
        value: i32,
    },
    NotPredicate(String),
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AbstractIntCandidate {
    instruction_index: usize,
    branch_scopes: Vec<BranchScope>,
}

const MAX_ABSTRACT_INT_VALUES: usize = 8;

fn merge_branch_assignment(previous: AbstractValue, assigned: AbstractValue) -> AbstractValue {
    let mut values = BTreeMap::<i32, AbstractIntCandidate>::new();
    let mut may_be_unknown = false;
    for value in [previous, assigned] {
        match value {
            AbstractValue::Int {
                value,
                instruction_index,
                branch_scopes,
            } => {
                values.entry(value).or_insert(AbstractIntCandidate {
                    instruction_index,
                    branch_scopes,
                });
            }
            AbstractValue::FiniteInts {
                values: candidates,
                may_be_unknown: candidate_unknown,
            } => {
                may_be_unknown |= candidate_unknown;
                for (value, instruction_index) in candidates {
                    values.entry(value).or_insert(instruction_index);
                }
            }
            _ => may_be_unknown = true,
        }
    }
    if values.len() > MAX_ABSTRACT_INT_VALUES {
        return AbstractValue::Unknown;
    }
    if values.len() == 1 && !may_be_unknown {
        let (value, candidate) = values.pop_first().expect("one integer candidate");
        AbstractValue::Int {
            value,
            instruction_index: candidate.instruction_index,
            branch_scopes: candidate.branch_scopes,
        }
    } else {
        AbstractValue::FiniteInts {
            values,
            may_be_unknown,
        }
    }
}

fn abstract_value_has_integer_candidate(value: &AbstractValue) -> bool {
    match value {
        AbstractValue::Int { .. } => true,
        AbstractValue::FiniteInts { values, .. } => !values.is_empty(),
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BranchScope {
    actor_name: Option<String>,
    label: String,
    end_address: u32,
}

#[derive(Debug, Clone)]
struct VariableBranchMerge {
    else_address: u32,
    end_address: u32,
    has_else: bool,
    entry_variables: BTreeMap<(u32, u32), AbstractValue>,
    then_variables: Option<BTreeMap<(u32, u32), AbstractValue>>,
}

fn merge_variable_paths(
    left: BTreeMap<(u32, u32), AbstractValue>,
    right: BTreeMap<(u32, u32), AbstractValue>,
) -> BTreeMap<(u32, u32), AbstractValue> {
    let keys = left
        .keys()
        .chain(right.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    keys.into_iter()
        .map(|key| {
            let left = left.get(&key).cloned().unwrap_or(AbstractValue::Unknown);
            let right = right.get(&key).cloned().unwrap_or(AbstractValue::Unknown);
            let merged = if left == right {
                left
            } else if abstract_value_has_integer_candidate(&left)
                || abstract_value_has_integer_candidate(&right)
            {
                merge_branch_assignment(left, right)
            } else {
                AbstractValue::Unknown
            };
            (key, merged)
        })
        .collect()
}

fn invert_abstract_condition(value: AbstractValue) -> AbstractValue {
    match value {
        AbstractValue::NameEquals(name) => AbstractValue::NameNotEquals(name),
        AbstractValue::NameNotEquals(name) => AbstractValue::NameEquals(name),
        AbstractValue::SelectedEquals(value) => AbstractValue::SelectedNotEquals(value),
        AbstractValue::SelectedNotEquals(value) => AbstractValue::SelectedEquals(value),
        AbstractValue::SystemFlagEquals { flag, value } => {
            AbstractValue::SystemFlagNotEquals { flag, value }
        }
        AbstractValue::SystemFlagNotEquals { flag, value } => {
            AbstractValue::SystemFlagEquals { flag, value }
        }
        AbstractValue::Predicate(label) => AbstractValue::NotPredicate(label),
        AbstractValue::NotPredicate(label) => AbstractValue::Predicate(label),
        _ => AbstractValue::Unknown,
    }
}

fn branch_scope_for_condition(value: &AbstractValue, end_address: u32) -> Option<BranchScope> {
    let (actor_name, label) = match value {
        AbstractValue::NameEquals(name) => (Some(name.clone()), format!("Runtime name = {name:?}")),
        AbstractValue::NameNotEquals(name) => (None, format!("Runtime name != {name:?}")),
        AbstractValue::SelectedEquals(value) => (None, format!("Selected choice = {value}")),
        AbstractValue::SelectedNotEquals(value) => (None, format!("Selected choice != {value}")),
        AbstractValue::SystemFlagEquals { flag, value } => {
            (None, format!("System flag {flag:#010x} = {value}"))
        }
        AbstractValue::SystemFlagNotEquals { flag, value } => {
            (None, format!("System flag {flag:#010x} != {value}"))
        }
        AbstractValue::Predicate(label) => (None, label.clone()),
        AbstractValue::NotPredicate(label) => (None, format!("not {label}")),
        _ => return None,
    };
    Some(BranchScope {
        actor_name,
        label,
        end_address,
    })
}

fn structured_else_end(
    instructions: &[SpcInstruction],
    instruction_addresses: &[u32],
    branch_index: usize,
    else_address: u32,
) -> Option<u32> {
    let else_index = instruction_addresses
        .iter()
        .position(|address| *address == else_address)?;
    instructions
        .get(branch_index + 1..else_index)?
        .iter()
        .rev()
        .find_map(|instruction| match instruction {
            SpcInstruction::Jump(end_address) if *end_address > else_address => Some(*end_address),
            _ => None,
        })
}

fn talk_selected_return_functions(
    functions: &[FunctionRange],
    instructions: &[SpcInstruction],
    builtin_names: &BTreeMap<u32, &str>,
) -> BTreeSet<u32> {
    // SPC emits a `ReturnZero` fallback after value-carrying `Return`s, and
    // retail selection helpers branch to it when talk is cancelled. It cannot
    // establish selected-value semantics by itself, but it also must not hide
    // the helper's selected result. Any competing ordinary `Return` whose
    // producer is not selected-derived still disqualifies the function.
    let mut selected_returns = BTreeSet::new();
    loop {
        let mut changed = false;
        for function in functions {
            if selected_returns.contains(&function.start_address) {
                continue;
            }
            let mut saw_selected_return = false;
            let returns_selected = instructions[function.start_index..function.end_index]
                .iter()
                .enumerate()
                .all(|(relative_index, instruction)| match instruction {
                    SpcInstruction::Return => {
                        let selected = instructions
                            [function.start_index..function.start_index + relative_index]
                            .iter()
                            .rev()
                            .find(|instruction| !matches!(instruction, SpcInstruction::Nop))
                            .is_some_and(|producer| match producer {
                                SpcInstruction::Builtin { symbol_index, .. } => builtin_names
                                    .get(symbol_index)
                                    .is_some_and(|name| *name == "getTalkSelectedValue"),
                                SpcInstruction::Call { address, .. } => {
                                    selected_returns.contains(address)
                                }
                                _ => false,
                            });
                        saw_selected_return |= selected;
                        selected
                    }
                    _ => true,
                });
            if saw_selected_return && returns_selected {
                selected_returns.insert(function.start_address);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    selected_returns
}

fn resolve_script_routes(
    stage_id: &str,
    script: &DialogueScriptResource,
    resources: &DialogueResourceSet,
    objects_by_name: &BTreeMap<String, Vec<&SceneObject>>,
    registry: Option<&sms_schema::ObjectRegistry>,
    index: &mut DialogueRouteIndex,
) {
    if script
        .document
        .symbols
        .iter()
        .any(|symbol| symbol.name == GENERATED_DIALOGUE_SCRIPT_MARKER)
    {
        return;
    }
    let instruction_addresses = instruction_addresses(&script.document.instructions);
    let functions = function_ranges(&script.document, &instruction_addresses);
    let builtin_names = script
        .document
        .symbols
        .iter()
        .enumerate()
        .map(|(index, symbol)| (index as u32, symbol.name.as_str()))
        .collect::<BTreeMap<_, _>>();
    // Retail scripts commonly hide `getTalkSelectedValue` behind one or more
    // small helpers. Derive those wrappers from their return behavior so the
    // caller's selected-choice branches survive without filename or symbol
    // name assumptions.
    let talk_selected_functions =
        talk_selected_return_functions(&functions, &script.document.instructions, &builtin_names);
    let mut message_functions = BTreeMap::<u32, BTreeSet<String>>::new();
    for function in &functions {
        let setters = function_setters(function, &script.document.instructions, &builtin_names);
        if !setters.is_empty() {
            message_functions.insert(function.start_address, setters);
        }
    }
    loop {
        let mut changed = false;
        for function in &functions {
            let mut setters = message_functions
                .get(&function.start_address)
                .cloned()
                .unwrap_or_default();
            for instruction in
                &script.document.instructions[function.start_index..function.end_index]
            {
                if let SpcInstruction::Call { address, .. } = instruction {
                    if let Some(called) = message_functions.get(address) {
                        setters.extend(called.iter().cloned());
                    }
                }
            }
            if !setters.is_empty()
                && message_functions.get(&function.start_address) != Some(&setters)
            {
                message_functions.insert(function.start_address, setters);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let mut occurrences = BTreeMap::<(String, u64, Option<u32>), u32>::new();
    let mut script_proxy_targets = BTreeSet::<String>::new();
    for function in &functions {
        interpret_function(
            script,
            stage_id,
            resources,
            objects_by_name,
            registry,
            index,
            function,
            &instruction_addresses,
            &builtin_names,
            &message_functions,
            &talk_selected_functions,
            &mut occurrences,
            &mut script_proxy_targets,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn interpret_function(
    script: &DialogueScriptResource,
    stage_id: &str,
    resources: &DialogueResourceSet,
    objects_by_name: &BTreeMap<String, Vec<&SceneObject>>,
    registry: Option<&sms_schema::ObjectRegistry>,
    index: &mut DialogueRouteIndex,
    function: &FunctionRange,
    instruction_addresses: &[u32],
    builtin_names: &BTreeMap<u32, &str>,
    message_functions: &BTreeMap<u32, BTreeSet<String>>,
    talk_selected_functions: &BTreeSet<u32>,
    occurrences: &mut BTreeMap<(String, u64, Option<u32>), u32>,
    script_proxy_targets: &mut BTreeSet<String>,
) {
    let mut stack = Vec::<AbstractValue>::new();
    let mut variables = BTreeMap::<(u32, u32), AbstractValue>::new();
    let mut branch_scopes = Vec::<BranchScope>::new();
    let mut pending_branch_scopes = Vec::<(u32, BranchScope)>::new();
    let mut variable_branch_merges = Vec::<VariableBranchMerge>::new();
    let mut actor_context = BTreeSet::<String>::new();

    for (instruction_index, &address) in instruction_addresses
        .iter()
        .enumerate()
        .take(function.end_index)
        .skip(function.start_index)
    {
        branch_scopes.retain(|scope| address < scope.end_address);
        let (mut ending_merges, remaining_merges): (Vec<_>, Vec<_>) = variable_branch_merges
            .drain(..)
            .partition(|merge| merge.end_address == address);
        // Nested branches may converge at the same address. Merge the
        // innermost (latest else/start) state first.
        ending_merges.sort_by_key(|merge| std::cmp::Reverse(merge.else_address));
        for mut merge in ending_merges {
            let alternate = if merge.has_else {
                merge
                    .then_variables
                    .take()
                    .unwrap_or_else(|| merge.entry_variables.clone())
            } else {
                merge.entry_variables
            };
            variables = merge_variable_paths(alternate, variables);
            stack.clear();
        }
        variable_branch_merges = remaining_merges;
        for merge in &mut variable_branch_merges {
            if merge.has_else && merge.else_address == address && merge.then_variables.is_none() {
                merge.then_variables = Some(variables.clone());
                variables.clone_from(&merge.entry_variables);
                stack.clear();
            }
        }
        let mut still_pending = Vec::new();
        for (start_address, scope) in pending_branch_scopes.drain(..) {
            if start_address <= address {
                branch_scopes.push(scope);
            } else {
                still_pending.push((start_address, scope));
            }
        }
        pending_branch_scopes = still_pending;
        let instruction = &script.document.instructions[instruction_index];
        match instruction {
            SpcInstruction::Int(value) => stack.push(AbstractValue::Int {
                value: *value,
                instruction_index,
                branch_scopes: branch_scopes.clone(),
            }),
            SpcInstruction::IntZero => stack.push(AbstractValue::Int {
                value: 0,
                instruction_index,
                branch_scopes: branch_scopes.clone(),
            }),
            SpcInstruction::IntOne => stack.push(AbstractValue::Int {
                value: 1,
                instruction_index,
                branch_scopes: branch_scopes.clone(),
            }),
            SpcInstruction::String(data_index) => stack.push(
                script
                    .document
                    .data
                    .get(*data_index as usize)
                    .map(|entry| AbstractValue::String(entry.value.clone()))
                    .unwrap_or(AbstractValue::Unknown),
            ),
            SpcInstruction::Variable { layer, variable } => stack.push(
                variables
                    .get(&(*layer, *variable))
                    .cloned()
                    .unwrap_or(AbstractValue::Unknown),
            ),
            SpcInstruction::Assign {
                layer, variable, ..
            } => {
                let value = stack.pop().unwrap_or(AbstractValue::Unknown);
                variables.insert((*layer, *variable), value);
            }
            SpcInstruction::Equal => {
                let right = stack.pop().unwrap_or(AbstractValue::Unknown);
                let left = stack.pop().unwrap_or(AbstractValue::Unknown);
                stack.push(match (left, right) {
                    (AbstractValue::TalkNpcName, AbstractValue::String(name))
                    | (AbstractValue::String(name), AbstractValue::TalkNpcName) => {
                        AbstractValue::NameEquals(name)
                    }
                    (AbstractValue::TalkSelectedValue, AbstractValue::Int { value, .. })
                    | (AbstractValue::Int { value, .. }, AbstractValue::TalkSelectedValue) => {
                        AbstractValue::SelectedEquals(value)
                    }
                    (AbstractValue::SystemFlag(flag), AbstractValue::Int { value, .. })
                    | (AbstractValue::Int { value, .. }, AbstractValue::SystemFlag(flag)) => {
                        AbstractValue::SystemFlagEquals { flag, value }
                    }
                    _ => AbstractValue::Unknown,
                });
            }
            SpcInstruction::NotEqual => {
                let right = stack.pop().unwrap_or(AbstractValue::Unknown);
                let left = stack.pop().unwrap_or(AbstractValue::Unknown);
                stack.push(match (left, right) {
                    (AbstractValue::TalkNpcName, AbstractValue::String(name))
                    | (AbstractValue::String(name), AbstractValue::TalkNpcName) => {
                        AbstractValue::NameNotEquals(name)
                    }
                    (AbstractValue::TalkSelectedValue, AbstractValue::Int { value, .. })
                    | (AbstractValue::Int { value, .. }, AbstractValue::TalkSelectedValue) => {
                        AbstractValue::SelectedNotEquals(value)
                    }
                    (AbstractValue::SystemFlag(flag), AbstractValue::Int { value, .. })
                    | (AbstractValue::Int { value, .. }, AbstractValue::SystemFlag(flag)) => {
                        AbstractValue::SystemFlagNotEquals { flag, value }
                    }
                    _ => AbstractValue::Unknown,
                });
            }
            SpcInstruction::Greater
            | SpcInstruction::Less
            | SpcInstruction::GreaterEqual
            | SpcInstruction::LessEqual
            | SpcInstruction::Add
            | SpcInstruction::Subtract
            | SpcInstruction::Multiply
            | SpcInstruction::Divide
            | SpcInstruction::Modulo
            | SpcInstruction::LogicalAnd
            | SpcInstruction::LogicalOr
            | SpcInstruction::BitAnd
            | SpcInstruction::BitOr
            | SpcInstruction::ShiftLeft
            | SpcInstruction::ShiftRight => {
                stack.pop();
                stack.pop();
                stack.push(AbstractValue::Unknown);
            }
            SpcInstruction::Negate => {
                if stack.pop().is_some() {
                    stack.push(AbstractValue::Unknown);
                }
            }
            SpcInstruction::Not => {
                let value = stack.pop().unwrap_or(AbstractValue::Unknown);
                stack.push(invert_abstract_condition(value));
            }
            SpcInstruction::JumpIfZero(target) => {
                let condition = stack.pop().unwrap_or(AbstractValue::Unknown);
                let scope = branch_scope_for_condition(&condition, *target);
                if *target > address {
                    let else_end = structured_else_end(
                        &script.document.instructions,
                        instruction_addresses,
                        instruction_index,
                        *target,
                    );
                    let dynamic_control = match &condition {
                        AbstractValue::Int { .. } => false,
                        AbstractValue::FiniteInts {
                            values,
                            may_be_unknown: false,
                        } if values.len() == 1 => false,
                        _ => true,
                    };
                    if dynamic_control {
                        variable_branch_merges.push(VariableBranchMerge {
                            else_address: *target,
                            end_address: else_end.unwrap_or(*target),
                            has_else: else_end.is_some(),
                            entry_variables: variables.clone(),
                            then_variables: None,
                        });
                    }
                    if let Some(scope) = scope {
                        branch_scopes.push(scope);
                    }
                    if let Some(else_end) = else_end {
                        if let Some(inverse) = branch_scope_for_condition(
                            &invert_abstract_condition(condition),
                            else_end,
                        ) {
                            pending_branch_scopes.push((*target, inverse));
                        }
                    }
                }
            }
            SpcInstruction::Builtin {
                symbol_index,
                argument_count,
            } => {
                let args = pop_arguments(&mut stack, *argument_count as usize);
                let symbol = builtin_names
                    .get(symbol_index)
                    .copied()
                    .unwrap_or("<unknown builtin>");
                if matches!(
                    symbol,
                    "__forceStartTalk"
                        | "__forceStartTalkExceptNpc"
                        | "connectDummyNpc"
                        | "setNpcTalkForbidCount"
                ) {
                    let targets = args
                        .iter()
                        .filter_map(|value| match value {
                            AbstractValue::NameHandle(name) | AbstractValue::String(name) => {
                                Some(name.clone())
                            }
                            _ => None,
                        })
                        .collect::<BTreeSet<_>>();
                    if !targets.is_empty() {
                        if symbol == "connectDummyNpc" {
                            script_proxy_targets.extend(targets.iter().cloned());
                        }
                        actor_context = targets;
                    }
                }
                if is_dialogue_setter(symbol) {
                    let mut routed_args = args.clone();
                    if !routed_args
                        .iter()
                        .any(|value| matches!(value, AbstractValue::NameHandle(_)))
                        && !branch_scopes.iter().any(|scope| scope.actor_name.is_some())
                    {
                        routed_args
                            .extend(actor_context.iter().cloned().map(AbstractValue::NameHandle));
                    }
                    let resolved = resolve_message_callsite(
                        script,
                        stage_id,
                        resources,
                        objects_by_name,
                        registry,
                        index,
                        function,
                        instruction_index,
                        symbol,
                        &routed_args,
                        &branch_scopes,
                        occurrences,
                        script_proxy_targets,
                    );
                    if !resolved && !routed_args.iter().any(abstract_value_has_integer_candidate) {
                        index.callsites.push(DialogueCallsiteClassification {
                            script_path: script.raw_path.clone(),
                            function_symbol: function.name.clone(),
                            setter_symbol: symbol.to_string(),
                            status: DialogueCallsiteStatus::DynamicHelper,
                        });
                    }
                }
                let result = match symbol {
                    "getTalkNPCName" => AbstractValue::TalkNpcName,
                    "getTalkSelectedValue" => AbstractValue::TalkSelectedValue,
                    "getSystemFlag" => args
                        .iter()
                        .find_map(|value| match value {
                            AbstractValue::Int { value, .. } => Some(*value as u32),
                            _ => None,
                        })
                        .map(AbstractValue::SystemFlag)
                        .unwrap_or(AbstractValue::Unknown),
                    "getNameRefHandle" | "getAddressFromViewObjName" => args
                        .iter()
                        .find_map(|value| match value {
                            AbstractValue::String(name) => Some(name.clone()),
                            _ => None,
                        })
                        .map(AbstractValue::NameHandle)
                        .unwrap_or(AbstractValue::Unknown),
                    _ if symbol.starts_with("is") || symbol.starts_with("check") => {
                        AbstractValue::Predicate(symbol.to_string())
                    }
                    _ => AbstractValue::Unknown,
                };
                if let AbstractValue::NameHandle(name) = &result {
                    actor_context.clear();
                    actor_context.insert(name.clone());
                }
                stack.push(result);
            }
            SpcInstruction::Call {
                address: target,
                argument_count,
            } => {
                let argument_count = usize::try_from((*argument_count).max(0)).unwrap_or(0);
                let args = pop_arguments(&mut stack, argument_count);
                let mut routed_args = args.clone();
                if !routed_args
                    .iter()
                    .any(|value| matches!(value, AbstractValue::NameHandle(_)))
                    && !branch_scopes.iter().any(|scope| scope.actor_name.is_some())
                {
                    routed_args
                        .extend(actor_context.iter().cloned().map(AbstractValue::NameHandle));
                }
                if let Some(setters) = message_functions.get(target) {
                    if setters.len() == 1 {
                        let _ = resolve_message_callsite(
                            script,
                            stage_id,
                            resources,
                            objects_by_name,
                            registry,
                            index,
                            function,
                            instruction_index,
                            setters.iter().next().expect("one setter"),
                            &routed_args,
                            &branch_scopes,
                            occurrences,
                            script_proxy_targets,
                        );
                    } else if routed_args.iter().any(abstract_value_has_integer_candidate) {
                        index.issues.push(DialogueResolutionIssue {
                            severity: DialogueResolutionSeverity::Error,
                            code: "dialogue-callsite-setter-ambiguous",
                            message: format!(
                                "Script {} function {:?} calls a helper which can reach multiple dialogue setters",
                                String::from_utf8_lossy(&script.raw_path),
                                function.name
                            ),
                            script_path: Some(script.raw_path.clone()),
                        });
                    }
                }
                stack.push(if talk_selected_functions.contains(target) {
                    AbstractValue::TalkSelectedValue
                } else {
                    AbstractValue::Unknown
                });
            }
            SpcInstruction::Pop => {
                stack.pop();
            }
            SpcInstruction::Return | SpcInstruction::ReturnZero | SpcInstruction::End => {
                stack.clear()
            }
            SpcInstruction::Address(_)
            | SpcInstruction::Float(_)
            | SpcInstruction::MakeFrame(_)
            | SpcInstruction::MakeDisplay(_) => stack.push(AbstractValue::Unknown),
            SpcInstruction::Increment { .. }
            | SpcInstruction::Decrement { .. }
            | SpcInstruction::Nop
            | SpcInstruction::Jump(_) => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn resolve_message_callsite(
    script: &DialogueScriptResource,
    stage_id: &str,
    resources: &DialogueResourceSet,
    objects_by_name: &BTreeMap<String, Vec<&SceneObject>>,
    registry: Option<&sms_schema::ObjectRegistry>,
    index: &mut DialogueRouteIndex,
    function: &FunctionRange,
    instruction_index: usize,
    setter: &str,
    arguments: &[AbstractValue],
    branch_scopes: &[BranchScope],
    occurrences: &mut BTreeMap<(String, u64, Option<u32>), u32>,
    script_proxy_targets: &BTreeSet<String>,
) -> bool {
    if let Some((
        finite_index,
        AbstractValue::FiniteInts {
            values,
            may_be_unknown,
        },
    )) = arguments
        .iter()
        .enumerate()
        .find(|(_, value)| abstract_value_has_integer_candidate(value))
    {
        let values = values.clone();
        let may_be_unknown = *may_be_unknown;
        let mut resolved_any = false;
        for (value, candidate) in values {
            let mut candidate_arguments = arguments.to_vec();
            candidate_arguments[finite_index] = AbstractValue::Int {
                value,
                instruction_index: candidate.instruction_index,
                branch_scopes: candidate.branch_scopes.clone(),
            };
            let mut candidate_scopes = branch_scopes.to_vec();
            for scope in candidate.branch_scopes {
                if !candidate_scopes.iter().any(|existing| {
                    existing.actor_name == scope.actor_name && existing.label == scope.label
                }) {
                    candidate_scopes.push(scope);
                }
            }
            resolved_any |= resolve_message_callsite(
                script,
                stage_id,
                resources,
                objects_by_name,
                registry,
                index,
                function,
                instruction_index,
                setter,
                &candidate_arguments,
                &candidate_scopes,
                occurrences,
                script_proxy_targets,
            );
        }
        if may_be_unknown {
            index.issues.push(DialogueResolutionIssue {
                severity: DialogueResolutionSeverity::Warning,
                code: "dialogue-message-id-unknown-path",
                message: format!(
                    "Script {} function {:?} reaches {setter} through an additional path whose message ID is not finite; known candidates were retained and the unknown path remains dynamic",
                    String::from_utf8_lossy(&script.raw_path),
                    function.name
                ),
                script_path: Some(script.raw_path.clone()),
            });
            index.callsites.push(DialogueCallsiteClassification {
                script_path: script.raw_path.clone(),
                function_symbol: function.name.clone(),
                setter_symbol: setter.to_string(),
                status: DialogueCallsiteStatus::DynamicHelper,
            });
        }
        return resolved_any;
    }
    let int_arguments = arguments
        .iter()
        .filter_map(|value| match value {
            AbstractValue::Int {
                value,
                instruction_index,
                ..
            } => Some((*value as u32, *instruction_index)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let Some((message_id, message_instruction_index)) = int_arguments.first().copied() else {
        // Dynamic helper body: its concrete callers are classified instead.
        return false;
    };
    let Some((message_ref, entry, storage_offset)) =
        resolve_message_reference(message_id, setter, resources)
    else {
        index.issues.push(DialogueResolutionIssue {
            severity: DialogueResolutionSeverity::Error,
            code: "dialogue-message-reference-unresolved",
            message: format!(
                "Script {} function {:?} selects message {message_id:#010x}, but no matching typed BMG entry exists",
                String::from_utf8_lossy(&script.raw_path),
                function.name
            ),
            script_path: Some(script.raw_path.clone()),
        });
        index.callsites.push(DialogueCallsiteClassification {
            script_path: script.raw_path.clone(),
            function_symbol: function.name.clone(),
            setter_symbol: setter.to_string(),
            status: DialogueCallsiteStatus::Ambiguous,
        });
        return false;
    };

    let fingerprint = normalized_instruction_fingerprint(
        &script.document.instructions,
        instruction_index,
        message_instruction_index,
        &script.document.symbols,
        &script.document.data,
    );
    let occurrence_key = (function.name.clone(), fingerprint, Some(message_id));
    let occurrence = occurrences.entry(occurrence_key).or_default();
    let source = DialogueSourceAnchor {
        raw_resource_path: script.raw_path.clone(),
        function_symbol: function.name.clone(),
        normalized_fingerprint: fingerprint,
        callsite_occurrence: *occurrence,
        original_message_id: Some(message_id),
    };
    *occurrence += 1;
    let key = DialogueVariantKey {
        source,
        original_message: Some(message_ref.clone()),
    };
    let base_route_kind = if branch_scopes
        .iter()
        .any(|scope| scope.label.starts_with("Selected choice"))
    {
        DialogueRouteKind::Choice
    } else {
        route_kind(setter, &function.name)
    };

    let handle_names = arguments.iter().filter_map(|value| match value {
        AbstractValue::NameHandle(name) => Some(name.clone()),
        _ => None,
    });
    let mut names = handle_names
        .chain(
            branch_scopes
                .iter()
                .filter_map(|scope| scope.actor_name.clone()),
        )
        .collect::<BTreeSet<_>>();
    let semantic_default_targets =
        if names.is_empty() && message_ref.domain == DialogueDomain::Stage {
            semantic_default_talk_targets(
                script,
                function,
                objects_by_name,
                registry,
                NPC_ACTOR_TYPE_SUNFLOWER_SMALL,
            )
        } else {
            Vec::new()
        };
    if names.len() != 1 && semantic_default_targets.is_empty() {
        let detached_id = if names.is_empty() {
            format!(
                "<global:{}:{}#{}>",
                String::from_utf8_lossy(&script.raw_path),
                function.name,
                key.source.callsite_occurrence
            )
        } else {
            format!(
                "<ambiguous:{}>",
                names.iter().cloned().collect::<Vec<_>>().join("|")
            )
        };
        record_detached_consumer(
            index,
            stage_id,
            &message_ref,
            storage_offset,
            key,
            base_route_kind,
            detached_id,
        );
        index.issues.push(DialogueResolutionIssue {
            severity: DialogueResolutionSeverity::Warning,
            code: if names.is_empty() {
                "dialogue-callsite-global-or-unbound"
            } else {
                "dialogue-callsite-name-ambiguous"
            },
            message: format!(
                "Script {} function {:?} selects message {message_id:#010x} with {} actor-name candidates {names:?}; it was classified but not attached to an instance",
                String::from_utf8_lossy(&script.raw_path),
                function.name,
                names.len()
            ),
            script_path: Some(script.raw_path.clone()),
        });
        index.callsites.push(DialogueCallsiteClassification {
            script_path: script.raw_path.clone(),
            function_symbol: function.name.clone(),
            setter_symbol: setter.to_string(),
            status: DialogueCallsiteStatus::GlobalOrUnplaced,
        });
        return false;
    }
    let runtime_name = names
        .pop_first()
        .unwrap_or_else(|| "<NPCSunflowerS default-talk proxy>".to_string());
    let condition_path = if !semantic_default_targets.is_empty() {
        "Stage default-talk route for each placed NPCSunflowerS".to_string()
    } else if branch_scopes.is_empty() {
        format!("Runtime name = {runtime_name:?}")
    } else {
        branch_scopes
            .iter()
            .map(|scope| scope.label.as_str())
            .collect::<Vec<_>>()
            .join(" / ")
    };
    let presentation_flags = int_arguments.get(1).map(|(value, _)| *value);
    let talk_flags = int_arguments.get(2).map(|(value, _)| *value);
    let mut targets = if !semantic_default_targets.is_empty() {
        semantic_default_targets
    } else if let Some(objects) = objects_by_name.get(&runtime_name) {
        objects.to_vec()
    } else {
        semantic_proxy_targets(
            script,
            objects_by_name,
            registry,
            index,
            NPC_ACTOR_TYPE_RACCOON_DOG,
            script_proxy_targets,
        )
    };
    if targets.is_empty() {
        record_detached_consumer(
            index,
            stage_id,
            &message_ref,
            storage_offset,
            key,
            base_route_kind,
            format!("<unplaced:{runtime_name}>"),
        );
        index.issues.push(DialogueResolutionIssue {
            severity: DialogueResolutionSeverity::Warning,
            code: "dialogue-callsite-unplaced-actor",
            message: format!(
                "Script {} selects message {message_id:#010x} for runtime name {runtime_name:?}, which has no placed editor object",
                String::from_utf8_lossy(&script.raw_path)
            ),
            script_path: Some(script.raw_path.clone()),
        });
        index.callsites.push(DialogueCallsiteClassification {
            script_path: script.raw_path.clone(),
            function_symbol: function.name.clone(),
            setter_symbol: setter.to_string(),
            status: DialogueCallsiteStatus::GlobalOrUnplaced,
        });
        return false;
    };

    if base_route_kind == DialogueRouteKind::Forced && targets.len() > 1 {
        targets.sort_by_key(|object| match &object.placement {
            Some(crate::PlacementBinding::Existing(_)) => 0,
            Some(crate::PlacementBinding::CloneOf(_)) => 1,
            Some(crate::PlacementBinding::Authored(_)) | None => 2,
        });
        let retained = targets[0];
        index.issues.push(DialogueResolutionIssue {
            severity: DialogueResolutionSeverity::Warning,
            code: "dialogue-forced-route-duplicate-name",
            message: format!(
                "Forced dialogue for runtime name {runtime_name:?} matches {} placed objects; it remains attached only to original object {:?}",
                targets.len(), retained.id
            ),
            script_path: Some(script.raw_path.clone()),
        });
        targets.truncate(1);
    }
    for object in targets {
        let object_runtime_name = object
            .raw_param("name")
            .filter(|name| !name.is_empty())
            .unwrap_or(&object.id)
            .to_string();
        let route_kind = if base_route_kind == DialogueRouteKind::Normal {
            actor_dialogue_route_kind(object, registry).unwrap_or(base_route_kind)
        } else {
            base_route_kind
        };
        index
            .variants_by_object
            .entry(object.id.clone())
            .or_default()
            .push(DialogueVariant {
                stage_id: stage_id.to_string(),
                object_id: object.id.clone(),
                runtime_name: object_runtime_name,
                key: key.clone(),
                route_kind,
                condition_path: condition_path.clone(),
                message: Some(message_ref.clone()),
                content: DialogueContent::from_bmg_entry(entry),
                presentation_flags,
                talk_flags,
                shared_consumers: Vec::new(),
                provenance: DialogueProvenance::ScriptBuiltin {
                    symbol: setter.to_string(),
                },
                compiler_location: DialogueCompilerLocation {
                    script_path: script.raw_path.clone(),
                    message_instruction_index,
                },
                message_storage_offset: Some(storage_offset),
            });
    }
    index.callsites.push(DialogueCallsiteClassification {
        script_path: script.raw_path.clone(),
        function_symbol: function.name.clone(),
        setter_symbol: setter.to_string(),
        status: DialogueCallsiteStatus::Resolved,
    });
    true
}

fn record_detached_consumer(
    index: &mut DialogueRouteIndex,
    stage_id: &str,
    message: &DialogueMessageRef,
    message_storage_offset: u32,
    variant_key: DialogueVariantKey,
    route_kind: DialogueRouteKind,
    object_id: String,
) {
    index.detached_consumers.push(DetachedDialogueConsumer {
        message: message.clone(),
        message_storage_offset,
        consumer: DialogueConsumer {
            stage_id: stage_id.to_string(),
            object_id,
            variant_key,
            page_line_count: dialogue_route_page_line_count(route_kind),
        },
    });
}

fn resolve_message_reference<'a>(
    message_id: u32,
    setter: &str,
    resources: &'a DialogueResourceSet,
) -> Option<(DialogueMessageRef, &'a sms_formats::BmgEntry, u32)> {
    let domain = if setter == "setNpcBalloonMessage" {
        DialogueDomain::Balloon
    } else if message_id >> 16 == 0 {
        DialogueDomain::System
    } else {
        DialogueDomain::Stage
    };
    let entry_index = message_id as u16;
    let mut candidates = resources
        .messages
        .iter()
        .filter(|((candidate_domain, raw_path), _)| {
            *candidate_domain == domain && preferred_message_path(domain, raw_path)
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        candidates = resources
            .messages
            .iter()
            .filter(|((candidate_domain, _), _)| *candidate_domain == domain)
            .collect();
    }
    if candidates.len() != 1 {
        return None;
    }
    let ((_, raw_path), bmg) = candidates.pop().expect("one candidate");
    let entry = bmg.entries.get(entry_index as usize)?;
    Some((
        DialogueMessageRef {
            domain,
            raw_resource_path: raw_path.clone(),
            full_message_id: message_id,
            entry_index,
        },
        entry,
        entry.message_offset,
    ))
}

fn preferred_message_path(domain: DialogueDomain, raw_path: &[u8]) -> bool {
    let normalized = normalized_raw_path(raw_path);
    match domain {
        DialogueDomain::Stage => normalized == STAGE_DIALOGUE_MESSAGE_PATH,
        DialogueDomain::System => normalized.ends_with(SYSTEM_DIALOGUE_MESSAGE_PATH),
        DialogueDomain::Balloon => normalized.ends_with(BALLOON_DIALOGUE_MESSAGE_PATH),
    }
}

fn route_kind(setter: &str, function_name: &str) -> DialogueRouteKind {
    let lower = function_name.to_ascii_lowercase();
    if setter == "setNpcBalloonMessage" {
        DialogueRouteKind::Balloon
    } else if lower.contains("force") {
        DialogueRouteKind::Forced
    } else if lower.contains("select") {
        DialogueRouteKind::Choice
    } else if lower.contains("happy") {
        DialogueRouteKind::HappyOverride
    } else if lower.contains("board") || lower.contains("sign") {
        DialogueRouteKind::BoardOrSign
    } else if lower.contains("shop") {
        DialogueRouteKind::Shop
    } else {
        DialogueRouteKind::Normal
    }
}

fn actor_dialogue_route_kind(
    object: &SceneObject,
    registry: Option<&sms_schema::ObjectRegistry>,
) -> Option<DialogueRouteKind> {
    let actor_type = registry?.find_npc_actor_type(&object.factory_name)?;
    match actor_type {
        NPC_ACTOR_TYPE_BOARD => Some(DialogueRouteKind::BoardOrSign),
        NPC_ACTOR_TYPE_RACCOON_DOG => Some(DialogueRouteKind::Shop),
        _ => None,
    }
}

fn objects_for_actor_type<'a>(
    objects_by_name: &BTreeMap<String, Vec<&'a SceneObject>>,
    registry: Option<&sms_schema::ObjectRegistry>,
    actor_type: u32,
) -> Vec<&'a SceneObject> {
    let Some(registry) = registry else {
        return Vec::new();
    };
    objects_by_name
        .values()
        .flatten()
        .copied()
        .filter(|object| registry.find_npc_actor_type(&object.factory_name) == Some(actor_type))
        .collect()
}

fn script_calls_builtin(script: &DialogueScriptResource, builtin_name: &str) -> bool {
    instructions_call_builtin(
        &script.document.instructions,
        &script.document.symbols,
        builtin_name,
    )
}

fn instructions_call_builtin(
    instructions: &[SpcInstruction],
    symbols: &[sms_formats::SpcSymbol],
    builtin_name: &str,
) -> bool {
    instructions.iter().any(|instruction| match instruction {
        SpcInstruction::Builtin { symbol_index, .. } => symbols
            .get(*symbol_index as usize)
            .is_some_and(|symbol| symbol.name == builtin_name),
        _ => false,
    })
}

fn semantic_default_talk_targets<'a>(
    script: &DialogueScriptResource,
    function: &FunctionRange,
    objects_by_name: &BTreeMap<String, Vec<&'a SceneObject>>,
    registry: Option<&sms_schema::ObjectRegistry>,
    actor_type: u32,
) -> Vec<&'a SceneObject> {
    // Sunshine's stage default-talk table is recognizable from behavior: its
    // entrypoint reads the active talk NPC name and routes finite message IDs
    // through a talk setter helper. The retail filename is not semantic and
    // may legitimately change in a rebuilt or modded archive.
    if function.name != "<entry>"
        || !instructions_call_builtin(
            &script.document.instructions[function.start_index..function.end_index],
            &script.document.symbols,
            "getTalkNPCName",
        )
    {
        return Vec::new();
    }
    objects_for_actor_type(objects_by_name, registry, actor_type)
}

fn semantic_proxy_targets<'a>(
    script: &DialogueScriptResource,
    objects_by_name: &BTreeMap<String, Vec<&'a SceneObject>>,
    registry: Option<&sms_schema::ObjectRegistry>,
    index: &DialogueRouteIndex,
    actor_type: u32,
    script_proxy_targets: &BTreeSet<String>,
) -> Vec<&'a SceneObject> {
    // Hidden proxy scripts declare their intent by connecting a dummy NPC or
    // by driving Sunshine's sunglasses-shop state. Resource paths are
    // authoring details and are deliberately ignored.
    if !script_calls_builtin(script, "connectDummyNpc")
        && !script_calls_builtin(script, "changeSunglass")
    {
        return Vec::new();
    }
    let candidates = objects_for_actor_type(objects_by_name, registry, actor_type);
    let exact_targets = candidates
        .iter()
        .copied()
        .filter(|object| {
            object
                .raw_param("name")
                .is_some_and(|name| script_proxy_targets.contains(name))
        })
        .collect::<Vec<_>>();
    if !exact_targets.is_empty() {
        return exact_targets;
    }
    let Some(minimum_external_routes) = candidates
        .iter()
        .map(|object| {
            index
                .variants_for_object(&object.id)
                .iter()
                .filter(|variant| variant.key.source.raw_resource_path != script.raw_path)
                .count()
        })
        .min()
    else {
        return Vec::new();
    };
    candidates
        .into_iter()
        .filter(|object| {
            index
                .variants_for_object(&object.id)
                .iter()
                .filter(|variant| variant.key.source.raw_resource_path != script.raw_path)
                .count()
                == minimum_external_routes
        })
        .collect()
}

fn is_dialogue_setter(symbol: &str) -> bool {
    matches!(symbol, "setTalkMsgID" | "setNpcBalloonMessage")
}

fn pop_arguments(stack: &mut Vec<AbstractValue>, argument_count: usize) -> Vec<AbstractValue> {
    let split = stack.len().saturating_sub(argument_count);
    stack.split_off(split)
}

fn function_setters(
    function: &FunctionRange,
    instructions: &[SpcInstruction],
    builtin_names: &BTreeMap<u32, &str>,
) -> BTreeSet<String> {
    instructions[function.start_index..function.end_index]
        .iter()
        .filter_map(|instruction| match instruction {
            SpcInstruction::Builtin { symbol_index, .. } => builtin_names
                .get(symbol_index)
                .copied()
                .filter(|name| is_dialogue_setter(name)),
            _ => None,
        })
        .map(str::to_string)
        .collect()
}

fn function_ranges(document: &SpcDocument, addresses: &[u32]) -> Vec<FunctionRange> {
    let address_to_index = addresses
        .iter()
        .enumerate()
        .map(|(index, address)| (*address, index))
        .collect::<BTreeMap<_, _>>();
    let mut starts = document
        .symbols
        .iter()
        .filter(|symbol| symbol.symbol_type == 1)
        .filter_map(|symbol| {
            address_to_index
                .get(&symbol.data)
                .copied()
                .map(|index| (symbol.data, index, symbol.name.clone()))
        })
        .collect::<Vec<_>>();
    if !starts.iter().any(|(address, _, _)| *address == 0) {
        starts.push((0, 0, "<entry>".to_string()));
    }
    starts.sort_by_key(|(address, _, _)| *address);
    starts.dedup_by_key(|(address, _, _)| *address);
    starts
        .iter()
        .enumerate()
        .map(|(ordinal, (address, index, name))| FunctionRange {
            name: name.clone(),
            start_address: *address,
            start_index: *index,
            end_index: starts
                .get(ordinal + 1)
                .map_or(document.instructions.len(), |(_, index, _)| *index),
        })
        .collect()
}

fn instruction_addresses(instructions: &[SpcInstruction]) -> Vec<u32> {
    let mut address = 0u32;
    instructions
        .iter()
        .map(|instruction| {
            let current = address;
            address = address.saturating_add(instruction_size(instruction));
            current
        })
        .collect()
}

fn instruction_size(instruction: &SpcInstruction) -> u32 {
    match instruction {
        SpcInstruction::Int(_)
        | SpcInstruction::Float(_)
        | SpcInstruction::String(_)
        | SpcInstruction::Address(_)
        | SpcInstruction::MakeFrame(_)
        | SpcInstruction::MakeDisplay(_)
        | SpcInstruction::JumpIfZero(_)
        | SpcInstruction::Jump(_) => 5,
        SpcInstruction::Variable { .. } => 9,
        SpcInstruction::Increment { .. }
        | SpcInstruction::Decrement { .. }
        | SpcInstruction::Assign { .. } => 10,
        SpcInstruction::Call { .. } | SpcInstruction::Builtin { .. } => 9,
        _ => 1,
    }
}

fn normalized_instruction_fingerprint(
    instructions: &[SpcInstruction],
    callsite_index: usize,
    message_instruction_index: usize,
    symbols: &[sms_formats::SpcSymbol],
    data: &[sms_formats::SpcDataEntry],
) -> u64 {
    let start = callsite_index.saturating_sub(5);
    let end = (callsite_index + 3).min(instructions.len());
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for (index, instruction) in instructions[start..end].iter().enumerate() {
        let absolute_index = start + index;
        let (tag, operand) = normalized_instruction(instruction, symbols, data);
        hash = fnv_mix(hash, tag);
        // The edited message value is deliberately normalized away.  The
        // surrounding semantic program shape remains the durable anchor.
        hash = fnv_mix(
            hash,
            if absolute_index == message_instruction_index {
                0
            } else {
                operand
            },
        );
    }
    hash
}

fn normalized_instruction(
    instruction: &SpcInstruction,
    symbols: &[sms_formats::SpcSymbol],
    data: &[sms_formats::SpcDataEntry],
) -> (u64, u64) {
    match instruction {
        SpcInstruction::Int(value) => (0, *value as u32 as u64),
        SpcInstruction::Float(value) => (1, value.to_bits() as u64),
        SpcInstruction::String(value) => (
            2,
            data.get(*value as usize)
                .map(|entry| stable_text_fingerprint(&entry.value))
                .unwrap_or(u64::from(*value)),
        ),
        SpcInstruction::Address(_) => (3, 0),
        SpcInstruction::Variable { layer, variable } => {
            (4, (u64::from(*layer) << 32) | u64::from(*variable))
        }
        SpcInstruction::Nop => (5, 0),
        SpcInstruction::Increment {
            layer, variable, ..
        } => (6, (u64::from(*layer) << 32) | u64::from(*variable)),
        SpcInstruction::Decrement {
            layer, variable, ..
        } => (7, (u64::from(*layer) << 32) | u64::from(*variable)),
        SpcInstruction::Add => (8, 0),
        SpcInstruction::Subtract => (9, 0),
        SpcInstruction::Multiply => (10, 0),
        SpcInstruction::Divide => (11, 0),
        SpcInstruction::Modulo => (12, 0),
        SpcInstruction::Assign {
            layer, variable, ..
        } => (13, (u64::from(*layer) << 32) | u64::from(*variable)),
        SpcInstruction::Equal => (14, 0),
        SpcInstruction::NotEqual => (15, 0),
        SpcInstruction::Greater => (16, 0),
        SpcInstruction::Less => (17, 0),
        SpcInstruction::GreaterEqual => (18, 0),
        SpcInstruction::LessEqual => (19, 0),
        SpcInstruction::Negate => (20, 0),
        SpcInstruction::Not => (21, 0),
        SpcInstruction::LogicalAnd => (22, 0),
        SpcInstruction::LogicalOr => (23, 0),
        SpcInstruction::BitAnd => (24, 0),
        SpcInstruction::BitOr => (25, 0),
        SpcInstruction::ShiftLeft => (26, 0),
        SpcInstruction::ShiftRight => (27, 0),
        SpcInstruction::Call {
            address,
            argument_count,
        } => {
            let target = symbols
                .iter()
                .filter(|symbol| symbol.symbol_type == 1 && symbol.data == *address)
                .map(|symbol| stable_text_fingerprint(&symbol.name))
                .min()
                .unwrap_or(0);
            (28, target ^ ((*argument_count as u32 as u64) << 32))
        }
        SpcInstruction::Builtin {
            symbol_index,
            argument_count,
        } => {
            let symbol_hash = symbols
                .get(*symbol_index as usize)
                .map(|symbol| stable_text_fingerprint(&symbol.name))
                .unwrap_or(u64::from(*symbol_index));
            (29, symbol_hash ^ (u64::from(*argument_count) << 32))
        }
        SpcInstruction::MakeFrame(value) => (30, *value as u32 as u64),
        SpcInstruction::MakeDisplay(value) => (31, *value as u32 as u64),
        SpcInstruction::Return => (32, 0),
        SpcInstruction::ReturnZero => (33, 0),
        SpcInstruction::JumpIfZero(_) => (34, 0),
        SpcInstruction::Jump(_) => (35, 0),
        SpcInstruction::Pop => (36, 0),
        SpcInstruction::IntZero => (37, 0),
        SpcInstruction::IntOne => (38, 0),
        SpcInstruction::End => (39, 0),
    }
}

fn fnv_mix(hash: u64, value: u64) -> u64 {
    value.to_le_bytes().iter().fold(hash, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
    })
}

fn attach_sharing_metadata(index: &mut DialogueRouteIndex) {
    let mut groups = BTreeMap::<(DialogueDomain, Vec<u8>, u32), Vec<DialogueConsumer>>::new();
    for variant in index.all_variants() {
        let (Some(message), Some(storage_offset)) =
            (variant.message.as_ref(), variant.message_storage_offset)
        else {
            continue;
        };
        groups
            .entry((
                message.domain,
                message.raw_resource_path.clone(),
                storage_offset,
            ))
            .or_default()
            .push(DialogueConsumer {
                stage_id: variant.stage_id.clone(),
                object_id: variant.object_id.clone(),
                variant_key: variant.key.clone(),
                page_line_count: dialogue_route_page_line_count(variant.route_kind),
            });
    }
    for variant in index.variants_by_object.values_mut().flatten() {
        let (Some(message), Some(storage_offset)) =
            (variant.message.as_ref(), variant.message_storage_offset)
        else {
            continue;
        };
        variant.shared_consumers = groups
            .get(&(
                message.domain,
                message.raw_resource_path.clone(),
                storage_offset,
            ))
            .cloned()
            .unwrap_or_default();
    }
}

fn dialogue_inheritance_cycles(authoring: &DialogueAuthoringDocument) -> Vec<Vec<String>> {
    let mut cycles = BTreeSet::new();
    for start in authoring.objects.keys() {
        let mut positions = BTreeMap::<String, usize>::new();
        let mut path = Vec::<String>::new();
        let mut current = Some(start.as_str());
        while let Some(object_id) = current {
            if let Some(position) = positions.get(object_id).copied() {
                let mut cycle = path[position..].to_vec();
                if let Some((rotation, _)) = cycle
                    .iter()
                    .enumerate()
                    .min_by(|left, right| left.1.cmp(right.1).then_with(|| left.0.cmp(&right.0)))
                {
                    cycle.rotate_left(rotation);
                }
                cycles.insert(cycle);
                break;
            }
            positions.insert(object_id.to_string(), path.len());
            path.push(object_id.to_string());
            current = authoring
                .objects
                .get(object_id)
                .and_then(|object| object.inherited_from_object_id.as_deref());
        }
    }
    cycles.into_iter().collect()
}

fn apply_authored_and_inherited_routes(document: &StageDocument, index: &mut DialogueRouteIndex) {
    let Some(authoring) = document.dialogue_authoring.as_ref() else {
        return;
    };
    // Generated variants are independent authoring roots. Materialize every
    // one before following inheritance so an A -> B -> C chain never depends
    // on BTreeMap iteration order.
    for (object_id, object_authoring) in &authoring.objects {
        let runtime_name = document
            .objects
            .iter()
            .find(|object| object.id == *object_id)
            .and_then(|object| object.raw_param("name"))
            .unwrap_or(object_id)
            .to_string();
        for authored in object_authoring
            .overrides
            .iter()
            .filter(|entry| entry.key.original_message.is_none())
        {
            let matching_objects = document
                .objects
                .iter()
                .filter(|object| object.raw_param("name").unwrap_or(&object.id) == runtime_name)
                .count();
            if matching_objects != 1 {
                index.issues.push(DialogueResolutionIssue {
                    severity: DialogueResolutionSeverity::Error,
                    code: "dialogue-generated-name-ambiguous",
                    message: format!(
                        "Generated dialogue for object {object_id:?} targets runtime name {runtime_name:?}, which is shared by {matching_objects} placed objects"
                    ),
                    script_path: Some(GENERATED_DIALOGUE_SCRIPT_PATH.to_vec()),
                });
            }
            let target = index
                .variants_by_object
                .entry(object_id.clone())
                .or_default();
            if target.iter().any(|variant| variant.key == authored.key) {
                continue;
            }
            target.push(DialogueVariant {
                stage_id: document.stage_id.clone(),
                object_id: object_id.clone(),
                runtime_name: runtime_name.clone(),
                key: authored.key.clone(),
                route_kind: authored.route_kind,
                condition_path: authored.condition_path.clone(),
                message: None,
                content: authored.content.clone(),
                presentation_flags: None,
                talk_flags: None,
                shared_consumers: Vec::new(),
                provenance: DialogueProvenance::Generated,
                compiler_location: DialogueCompilerLocation {
                    script_path: GENERATED_DIALOGUE_SCRIPT_PATH.to_vec(),
                    message_instruction_index: usize::MAX,
                },
                message_storage_offset: None,
            });
        }
    }

    let cycles = dialogue_inheritance_cycles(authoring);
    let mut failed = cycles.iter().flatten().cloned().collect::<BTreeSet<_>>();
    for cycle in cycles {
        let mut path = cycle.clone();
        path.push(cycle[0].clone());
        index.issues.push(DialogueResolutionIssue {
            severity: DialogueResolutionSeverity::Error,
            code: "dialogue-inheritance-cycle",
            message: format!("Dialogue inheritance cycle: {}", path.join(" -> ")),
            script_path: None,
        });
    }

    let mut pending = authoring
        .objects
        .iter()
        .filter_map(|(object_id, object)| {
            (object.inherited_from_object_id.is_some() && !failed.contains(object_id))
                .then_some(object_id.clone())
        })
        .collect::<BTreeSet<_>>();
    while !pending.is_empty() {
        let mut progressed = false;
        for object_id in pending.iter().cloned().collect::<Vec<_>>() {
            let source_id = authoring.objects[&object_id]
                .inherited_from_object_id
                .as_deref()
                .expect("pending inheritance has a source");
            if pending.contains(source_id) {
                continue;
            }
            progressed = true;
            pending.remove(&object_id);
            if failed.contains(source_id) {
                failed.insert(object_id.clone());
                index.issues.push(DialogueResolutionIssue {
                    severity: DialogueResolutionSeverity::Error,
                    code: "dialogue-inherited-source-cycle",
                    message: format!(
                        "Dialogue object {object_id:?} inherits through cyclic source {source_id:?}"
                    ),
                    script_path: None,
                });
                continue;
            }

            let inherited = index
                .variants_for_object(source_id)
                .iter()
                .filter(|variant| variant.route_kind != DialogueRouteKind::Forced)
                .cloned()
                .collect::<Vec<_>>();
            if inherited.is_empty() {
                failed.insert(object_id.clone());
                index.issues.push(DialogueResolutionIssue {
                    severity: DialogueResolutionSeverity::Error,
                    code: "dialogue-inherited-source-unresolved",
                    message: format!(
                        "Dialogue object {object_id:?} inherits from {source_id:?}, but that source has no player-initiated routes"
                    ),
                    script_path: None,
                });
                continue;
            }
            let runtime_name = document
                .objects
                .iter()
                .find(|object| object.id == object_id)
                .and_then(|object| object.raw_param("name"))
                .unwrap_or(&object_id)
                .to_string();
            let target = index
                .variants_by_object
                .entry(object_id.clone())
                .or_default();
            for mut variant in inherited {
                variant.object_id.clone_from(&object_id);
                variant.runtime_name.clone_from(&runtime_name);
                if !target.iter().any(|existing| existing.key == variant.key) {
                    target.push(variant);
                }
            }
        }
        if !progressed {
            // All genuine cycles were removed above. This is a conservative
            // guard against malformed future graph shapes.
            for object_id in std::mem::take(&mut pending) {
                index.issues.push(DialogueResolutionIssue {
                    severity: DialogueResolutionSeverity::Error,
                    code: "dialogue-inheritance-unresolved",
                    message: format!(
                        "Dialogue inheritance for object {object_id:?} could not be ordered"
                    ),
                    script_path: None,
                });
            }
        }
    }
    for variants in index.variants_by_object.values_mut() {
        variants.sort_by(|left, right| left.key.cmp(&right.key));
    }
}

fn classify_unresolved_talk_capable_objects(
    document: &StageDocument,
    index: &mut DialogueRouteIndex,
) {
    let Some(registry) = document.registry.as_ref() else {
        return;
    };
    for object in &document.objects {
        if registry.find_npc_actor_type(&object.factory_name).is_none() {
            continue;
        }
        if !registry.is_dialogue_instance_eligible(&object.factory_name) {
            index.issues.push(DialogueResolutionIssue {
                severity: DialogueResolutionSeverity::Warning,
                code: "dialogue-dummy-proxy-classified",
                message: format!(
                    "NPCDummy object {:?} is Sunshine's hidden talk proxy; its dialogue belongs to the placed actor connected through connectDummyNpc",
                    object.id
                ),
                script_path: None,
            });
            continue;
        }
        let pending_generated_or_inherited_route = document
            .dialogue_authoring
            .as_ref()
            .and_then(|authoring| authoring.objects.get(&object.id))
            .is_some_and(|authoring| {
                authoring.inherited_from_object_id.is_some()
                    || authoring
                        .overrides
                        .iter()
                        .any(|authored| authored.key.original_message.is_none())
            });
        if pending_generated_or_inherited_route {
            continue;
        }
        let existing_routes = index.variants_for_object(&object.id);
        let has_conversation_entry_route = existing_routes
            .iter()
            .any(|variant| dialogue_route_starts_conversation(variant.route_kind));
        if !has_conversation_entry_route {
            let forced_only = !existing_routes.is_empty()
                && existing_routes
                    .iter()
                    .all(|variant| variant.route_kind == DialogueRouteKind::Forced);
            let secondary_only = !existing_routes.is_empty() && !forced_only;
            index.issues.push(DialogueResolutionIssue {
                severity: DialogueResolutionSeverity::Warning,
                code: if secondary_only {
                    "dialogue-talk-capable-secondary-only"
                } else if forced_only {
                    "dialogue-talk-capable-forced-only"
                } else {
                    "dialogue-talk-capable-retail-unrouted"
                },
                message: format!(
                    "Talk-capable object {:?} ({}) with runtime name {:?} has no player-initiated retail talk-setting route in this scenario; it receives an empty, instance-editable generated fallback{}",
                    object.id,
                    object.factory_name,
                    object.raw_param("name").unwrap_or(&object.id),
                    if forced_only {
                        " while its forced/cutscene route remains original-only"
                    } else if secondary_only {
                        " while its conditional follow-up/override routes remain independently editable"
                    } else {
                        ""
                    }
                ),
                script_path: None,
            });
            let runtime_name = object
                .raw_param("name")
                .filter(|name| !name.is_empty())
                .unwrap_or(&object.id)
                .to_string();
            index
                .variants_by_object
                .entry(object.id.clone())
                .or_default()
                .push(DialogueVariant {
                    stage_id: document.stage_id.clone(),
                    object_id: object.id.clone(),
                    runtime_name,
                    key: DialogueVariantKey::generated_for_object(&object.id),
                    route_kind: actor_dialogue_route_kind(object, Some(registry))
                        .unwrap_or(DialogueRouteKind::Generated),
                    condition_path: "No retail route in this scenario".to_string(),
                    message: None,
                    content: DialogueContent {
                        message: BmgMessage::default(),
                        authored_tokens: None,
                        attributes: vec![0; 8],
                        voice_index: Some(0),
                    },
                    presentation_flags: None,
                    talk_flags: None,
                    shared_consumers: Vec::new(),
                    provenance: DialogueProvenance::Generated,
                    compiler_location: DialogueCompilerLocation {
                        script_path: GENERATED_DIALOGUE_SCRIPT_PATH.to_vec(),
                        message_instruction_index: 0,
                    },
                    message_storage_offset: None,
                });
        }
    }
}

fn dialogue_route_starts_conversation(route_kind: DialogueRouteKind) -> bool {
    matches!(
        route_kind,
        DialogueRouteKind::Normal
            | DialogueRouteKind::BoardOrSign
            | DialogueRouteKind::Shop
            | DialogueRouteKind::Generated
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sms_formats::{BmgEntry, SpcDataEntry, SpcSymbol};
    use sms_schema::{NpcFactoryActorTypeDefinition, ObjectRegistry};

    fn text(value: &str) -> BmgMessage {
        BmgMessage {
            tokens: vec![sms_formats::BmgMessageToken::Text(value.to_string())],
        }
    }

    fn stage_bmg(value: &str) -> BmgFile {
        let mut bmg = empty_sms_bmg();
        bmg.entries.push(BmgEntry {
            message_offset: 0,
            attributes: vec![0, 0, 0, 0, 2, 0, 0, 0],
            message: text(value),
        });
        bmg
    }

    fn indexed_bmg(entry_count: usize, prefix: &str) -> BmgFile {
        let mut bmg = empty_sms_bmg();
        for index in 0..entry_count {
            bmg.entries.push(BmgEntry {
                message_offset: 0,
                attributes: vec![0, 0, 0, 0, index as u8, 0, 0, 0],
                message: text(&format!("{prefix} {index}")),
            });
        }
        bmg.canonicalize_layout().unwrap();
        bmg
    }

    fn npc_registry(entries: &[(&str, u32)]) -> ObjectRegistry {
        ObjectRegistry {
            npc_factory_actor_types: entries
                .iter()
                .map(|(factory_name, actor_type)| NpcFactoryActorTypeDefinition {
                    factory_name: (*factory_name).to_string(),
                    actor_type: *actor_type,
                    source_file: "src/System/MarNameRefGen_NPC.cpp".to_string(),
                })
                .collect(),
            ..ObjectRegistry::default()
        }
    }

    fn handle_script(runtime_name: &str, message_id: u32) -> SpcDocument {
        let instructions = vec![
            SpcInstruction::String(0),
            SpcInstruction::Builtin {
                symbol_index: 0,
                argument_count: 1,
            },
            SpcInstruction::Int(message_id as i32),
            SpcInstruction::Builtin {
                symbol_index: 1,
                argument_count: 2,
            },
            SpcInstruction::Pop,
            SpcInstruction::End,
        ];
        SpcDocument {
            text_offset: 0x1c,
            text_length: instructions
                .iter()
                .map(SpcInstruction::encoded_len)
                .sum::<usize>() as u32,
            data_offset: 0x40,
            symbol_offset: 0x50,
            initial_storage_count: 1,
            instructions,
            data: vec![SpcDataEntry {
                offset: 0,
                value: runtime_name.to_string(),
            }],
            symbols: vec![
                SpcSymbol {
                    symbol_type: 0,
                    name_offset: 0,
                    data: 0,
                    name_hash: 0,
                    native_call: 0,
                    name: "getNameRefHandle".to_string(),
                },
                SpcSymbol {
                    symbol_type: 0,
                    name_offset: 17,
                    data: 1,
                    name_hash: 0,
                    native_call: 0,
                    name: "setTalkMsgID".to_string(),
                },
            ],
            file_size: 0x100,
            padding: Vec::new(),
        }
    }

    fn empty_document(stage_id: &str) -> StageDocument {
        StageDocument {
            stage_id: stage_id.to_string(),
            base_root: PathBuf::new(),
            assets: Vec::new(),
            objects: Vec::new(),
            changed_files: BTreeMap::new(),
            stage_archive: None,
            stage_archive_source_path: None,
            archive_edits: StageArchiveEdits::default(),
            registry: None,
            route_authoring: None,
            goop_authoring: None,
            dialogue_authoring: None,
            dialogue_library: ProjectDialogueLibrary::default(),
            load_issues: Vec::new(),
            lighting: Default::default(),
            actor_previews: BTreeMap::new(),
            loaded_project: None,
        }
    }

    fn authored_dialogue_object(id: &str, runtime_name: &str) -> SceneObject {
        let mut object = SceneObject::new(id, "NPCFixture");
        object.insert_source_raw_param("name", runtime_name);
        object.placement = Some(crate::PlacementBinding::Authored(
            crate::AuthoredPlacement {
                raw_resource_path: b"map/scene.bin".to_vec(),
                target_group_index: 0,
                prototype: sms_formats::JDramaRecord::new(
                    "NPCFixture",
                    runtime_name,
                    sms_formats::JDramaRecordPayload::Empty,
                )
                .unwrap(),
                dependencies: Vec::new(),
            },
        ));
        object
    }

    fn retail_stage_dialogue_index(
        base_root: &Path,
        archives: &[sms_formats::SceneArchiveInfo],
        registry: &ObjectRegistry,
        common: &DialogueResourceSet,
        stage_id: &str,
    ) -> DialogueRouteIndex {
        let archive = archives
            .iter()
            .find(|archive| archive.stage_id == stage_id)
            .unwrap_or_else(|| panic!("retail fixture has no {stage_id} stage archive"));
        let assets = mount_scene_archive(&archive.path)
            .unwrap_or_else(|error| panic!("mount retail {stage_id} archive: {error}"));
        let (objects, load_issues, lighting) = crate::load_scene_objects_from_assets(&assets);
        let mut document = empty_document(stage_id);
        document.base_root = base_root.to_path_buf();
        document.assets = assets;
        document.objects = objects;
        document.load_issues = load_issues;
        document.lighting = lighting;
        document.registry = Some(registry.clone());
        let index = document
            .build_dialogue_route_index_with_common(common)
            .unwrap_or_else(|error| panic!("resolve retail {stage_id} dialogue: {error}"));
        assert!(
            !index.has_errors(),
            "retail {stage_id} dialogue has errors: {:?}",
            index.issues
        );
        index
    }

    fn matching_retail_routes<'a>(
        index: &'a DialogueRouteIndex,
        runtime_name: Option<&str>,
        source_path: &[u8],
        message_id: u32,
    ) -> Vec<&'a DialogueVariant> {
        index
            .all_variants()
            .filter(|variant| {
                runtime_name.is_none_or(|name| variant.runtime_name == name)
                    && variant.key.source.raw_resource_path == source_path
                    && variant
                        .message
                        .as_ref()
                        .is_some_and(|message| message.full_message_id == message_id)
            })
            .collect()
    }

    fn assert_retail_route<'a>(
        index: &'a DialogueRouteIndex,
        runtime_name: Option<&str>,
        source_path: &[u8],
        message_id: u32,
        route_kind: DialogueRouteKind,
    ) -> Vec<&'a DialogueVariant> {
        let matches = matching_retail_routes(index, runtime_name, source_path, message_id);
        assert!(
            !matches.is_empty(),
            "missing retail route runtime={runtime_name:?} source={} message={message_id:#010x} kind={route_kind:?}",
            String::from_utf8_lossy(source_path)
        );
        assert!(
            matches
                .iter()
                .all(|variant| variant.route_kind == route_kind),
            "retail route runtime={runtime_name:?} source={} message={message_id:#010x} had kinds {:?}",
            String::from_utf8_lossy(source_path),
            matches
                .iter()
                .map(|variant| variant.route_kind)
                .collect::<Vec<_>>()
        );
        matches
    }

    fn balloon_document(stage_id: &str, object_id: &str, runtime_name: &str) -> StageDocument {
        let mut document = empty_document(stage_id);
        let mut object = SceneObject::new(object_id, "NPCBoard");
        object.insert_source_raw_param("name", runtime_name);
        document.objects.push(object);
        document.registry = Some(npc_registry(&[("NPCBoard", NPC_ACTOR_TYPE_BOARD)]));
        document.archive_edits.upsert_resource(
            BALLOON_DIALOGUE_MESSAGE_PATH.to_vec(),
            StageResourceDocument::Message(stage_bmg("Retail balloon")),
        );
        let mut script = handle_script(runtime_name, 0);
        script.symbols[1].name = "setNpcBalloonMessage".to_string();
        document.archive_edits.upsert_resource(
            b"map/sp/balloon_route.sb".to_vec(),
            StageResourceDocument::Script(script),
        );
        document
    }

    fn unique_project_paths(label: &str) -> (PathBuf, PathBuf) {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "graffito-dialogue-{label}-{}-{nonce}",
            std::process::id()
        ));
        (root.join("base"), root.join("project"))
    }

    fn consumer_variant(
        stage_id: &str,
        object_id: &str,
        entry_index: u16,
        storage_offset: u32,
    ) -> DialogueVariant {
        let message = DialogueMessageRef {
            domain: DialogueDomain::Stage,
            raw_resource_path: STAGE_DIALOGUE_MESSAGE_PATH.to_vec(),
            full_message_id: 0x0001_0000 | u32::from(entry_index),
            entry_index,
        };
        DialogueVariant {
            stage_id: stage_id.to_string(),
            object_id: object_id.to_string(),
            runtime_name: object_id.to_string(),
            key: DialogueVariantKey {
                source: DialogueVariantKey::generated_for_object(object_id).source,
                original_message: Some(message.clone()),
            },
            route_kind: DialogueRouteKind::Normal,
            condition_path: "Always".to_string(),
            message: Some(message),
            content: DialogueContent {
                message: text(object_id),
                authored_tokens: None,
                attributes: vec![0; 12],
                voice_index: Some(0),
            },
            presentation_flags: None,
            talk_flags: None,
            shared_consumers: Vec::new(),
            provenance: DialogueProvenance::ScriptBuiltin {
                symbol: "setTalkMsgID".to_string(),
            },
            compiler_location: DialogueCompilerLocation {
                script_path: b"map/sp/test.sb".to_vec(),
                message_instruction_index: 0,
            },
            message_storage_offset: Some(storage_offset),
        }
    }

    #[test]
    fn superseded_game_consumer_index_stops_before_project_or_base_io() {
        let document = empty_document("cancelled-consumer");
        let cancelled = AtomicBool::new(true);

        let error = document
            .build_game_dialogue_consumer_index_with_cancel(&cancelled)
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("consumer-index rebuild was superseded"),
            "{error}"
        );
    }

    #[test]
    fn semantic_handle_route_resolves_message_voice_and_sharing() {
        let mut first = SceneObject::new("first", "NPCBoard");
        first.insert_source_raw_param("name", "Board A");
        let mut second = SceneObject::new("second", "NPCBoard");
        second.insert_source_raw_param("name", "Board A");
        let resources = DialogueResourceSet {
            scripts: vec![DialogueScriptResource {
                raw_path: b"map/sp/talk.sb".to_vec(),
                document: handle_script("Board A", 0x0001_0000),
            }],
            messages: BTreeMap::from([(
                (DialogueDomain::Stage, STAGE_DIALOGUE_MESSAGE_PATH.to_vec()),
                stage_bmg("Retail line"),
            )]),
            collection_issues: Vec::new(),
        };

        let index = resolve_dialogue_routes("test", &[first, second], None, &resources);

        assert!(!index.has_errors(), "{:?}", index.issues);
        assert_eq!(index.variants_for_object("first").len(), 1);
        let variant = &index.variants_for_object("first")[0];
        assert_eq!(variant.content.voice_index, Some(2));
        assert_eq!(variant.shared_consumers.len(), 2);
        assert_eq!(variant.message.as_ref().unwrap().entry_index, 0);
        assert_eq!(index.callsites[0].status, DialogueCallsiteStatus::Resolved);
    }

    #[test]
    fn address_builtin_and_schema_identity_classify_boards_and_shops() {
        let mut board = SceneObject::new("board", "NPCBoard");
        board.insert_source_raw_param("name", "Board A");
        let mut shop = SceneObject::new("shop", "NPCRaccoonDog");
        shop.insert_source_raw_param("name", "Shop A");
        let mut board_script = handle_script("Board A", 0x0001_0000);
        board_script.symbols[0].name = "getAddressFromViewObjName".to_string();
        let mut shop_script = handle_script("Shop A", 0x0001_0000);
        shop_script.symbols[0].name = "getAddressFromViewObjName".to_string();
        let resources = DialogueResourceSet {
            scripts: vec![
                DialogueScriptResource {
                    raw_path: b"map/sp/board_route.sb".to_vec(),
                    document: board_script,
                },
                DialogueScriptResource {
                    raw_path: b"map/sp/shop_route.sb".to_vec(),
                    document: shop_script,
                },
            ],
            messages: BTreeMap::from([(
                (DialogueDomain::Stage, STAGE_DIALOGUE_MESSAGE_PATH.to_vec()),
                stage_bmg("Retail line"),
            )]),
            collection_issues: Vec::new(),
        };
        let registry = npc_registry(&[
            ("NPCBoard", NPC_ACTOR_TYPE_BOARD),
            ("NPCRaccoonDog", NPC_ACTOR_TYPE_RACCOON_DOG),
        ]);

        let index = resolve_dialogue_routes("test", &[board, shop], Some(&registry), &resources);

        assert_eq!(
            index.variants_for_object("board")[0].route_kind,
            DialogueRouteKind::BoardOrSign
        );
        assert_eq!(
            index.variants_for_object("shop")[0].route_kind,
            DialogueRouteKind::Shop
        );
    }

    #[test]
    fn structured_choice_else_branch_keeps_inverse_condition_and_actor_binding() {
        let mut program = SpcRelocatableProgram::new(1);
        let get_name = append_builtin_symbol(&mut program, "getTalkNPCName").unwrap();
        let get_selected = append_builtin_symbol(&mut program, "getTalkSelectedValue").unwrap();
        let set_message = append_builtin_symbol(&mut program, "setTalkMsgID").unwrap();
        let name = program.append_data("Choice NPC".to_string()).unwrap();
        program.push_instruction(SpcInstruction::Builtin {
            symbol_index: get_name,
            argument_count: 0,
        });
        program.push_instruction(SpcInstruction::String(name));
        program.push_instruction(SpcInstruction::Equal);
        let not_actor = program.push_instruction(SpcInstruction::JumpIfZero(0));
        program.push_instruction(SpcInstruction::Builtin {
            symbol_index: get_selected,
            argument_count: 0,
        });
        program.push_instruction(SpcInstruction::IntZero);
        program.push_instruction(SpcInstruction::Equal);
        let else_jump = program.push_instruction(SpcInstruction::JumpIfZero(0));
        program.push_instruction(SpcInstruction::Int(0x0001_0000));
        program.push_instruction(SpcInstruction::IntZero);
        program.push_instruction(SpcInstruction::Builtin {
            symbol_index: set_message,
            argument_count: 2,
        });
        program.push_instruction(SpcInstruction::Pop);
        let end_true = program.push_instruction(SpcInstruction::Jump(0));
        let else_start = program.instructions.len();
        program.push_instruction(SpcInstruction::Int(0x0001_0001));
        program.push_instruction(SpcInstruction::IntZero);
        program.push_instruction(SpcInstruction::Builtin {
            symbol_index: set_message,
            argument_count: 2,
        });
        program.push_instruction(SpcInstruction::Pop);
        let end = program.push_instruction(SpcInstruction::End);
        program.set_instruction_target(not_actor, end).unwrap();
        program
            .set_instruction_target(else_jump, else_start)
            .unwrap();
        program.set_instruction_target(end_true, end).unwrap();

        let mut object = SceneObject::new("choice", "NPCMonteM");
        object.insert_source_raw_param("name", "Choice NPC");
        let resources = DialogueResourceSet {
            scripts: vec![DialogueScriptResource {
                raw_path: b"map/sp/choice.sb".to_vec(),
                document: program.to_document().unwrap(),
            }],
            messages: BTreeMap::from([(
                (DialogueDomain::Stage, STAGE_DIALOGUE_MESSAGE_PATH.to_vec()),
                indexed_bmg(2, "Choice"),
            )]),
            collection_issues: Vec::new(),
        };

        let index = resolve_dialogue_routes("test", &[object], None, &resources);
        let variants = index.variants_for_object("choice");
        assert_eq!(variants.len(), 2, "{:?}", index.issues);
        assert!(variants
            .iter()
            .any(|variant| variant.condition_path.contains("Selected choice = 0")));
        assert!(variants
            .iter()
            .any(|variant| variant.condition_path.contains("Selected choice != 0")));
        assert!(variants
            .iter()
            .all(|variant| variant.route_kind == DialogueRouteKind::Choice));
    }

    #[test]
    fn selected_choice_return_survives_semantic_helper_chain() {
        let mut program = SpcRelocatableProgram::new(1);
        let get_name = append_builtin_symbol(&mut program, "getTalkNPCName").unwrap();
        let get_selected = append_builtin_symbol(&mut program, "getTalkSelectedValue").unwrap();
        let set_message = append_builtin_symbol(&mut program, "setTalkMsgID").unwrap();
        let wrapper_symbol = program
            .append_symbol(SpcProgramSymbol {
                symbol_type: 1,
                data: 0,
                native_call: 0,
                name: "fixture_selection_wrapper".to_string(),
            })
            .unwrap();
        let leaf_symbol = program
            .append_symbol(SpcProgramSymbol {
                symbol_type: 1,
                data: 0,
                native_call: 0,
                name: "fixture_selection_leaf".to_string(),
            })
            .unwrap();
        let name = program.append_data("Choice NPC".to_string()).unwrap();

        program.push_instruction(SpcInstruction::Builtin {
            symbol_index: get_name,
            argument_count: 0,
        });
        program.push_instruction(SpcInstruction::String(name));
        program.push_instruction(SpcInstruction::Equal);
        let not_actor = program.push_instruction(SpcInstruction::JumpIfZero(0));
        program.push_instruction(SpcInstruction::Int(0x0001_0000));
        program.push_instruction(SpcInstruction::IntZero);
        let call_wrapper = program.push_instruction(SpcInstruction::Call {
            address: 0,
            argument_count: 2,
        });
        program.push_instruction(SpcInstruction::Assign {
            reserved: 0,
            layer: 0,
            variable: 0,
        });
        program.push_instruction(SpcInstruction::Variable {
            layer: 0,
            variable: 0,
        });
        program.push_instruction(SpcInstruction::IntZero);
        program.push_instruction(SpcInstruction::Equal);
        let else_jump = program.push_instruction(SpcInstruction::JumpIfZero(0));
        program.push_instruction(SpcInstruction::Int(0x0001_0001));
        program.push_instruction(SpcInstruction::IntZero);
        program.push_instruction(SpcInstruction::Builtin {
            symbol_index: set_message,
            argument_count: 2,
        });
        program.push_instruction(SpcInstruction::Pop);
        let end_true = program.push_instruction(SpcInstruction::Jump(0));
        let else_start = program.instructions.len();
        program.push_instruction(SpcInstruction::Int(0x0001_0002));
        program.push_instruction(SpcInstruction::IntZero);
        program.push_instruction(SpcInstruction::Builtin {
            symbol_index: set_message,
            argument_count: 2,
        });
        program.push_instruction(SpcInstruction::Pop);
        let end = program.push_instruction(SpcInstruction::End);

        let wrapper_start = program.instructions.len();
        program.push_instruction(SpcInstruction::Variable {
            layer: 1,
            variable: 0,
        });
        program.push_instruction(SpcInstruction::Variable {
            layer: 1,
            variable: 1,
        });
        let call_leaf = program.push_instruction(SpcInstruction::Call {
            address: 0,
            argument_count: 2,
        });
        program.push_instruction(SpcInstruction::Return);

        let leaf_start = program.instructions.len();
        program.push_instruction(SpcInstruction::Variable {
            layer: 1,
            variable: 0,
        });
        program.push_instruction(SpcInstruction::Variable {
            layer: 1,
            variable: 1,
        });
        program.push_instruction(SpcInstruction::Builtin {
            symbol_index: set_message,
            argument_count: 2,
        });
        program.push_instruction(SpcInstruction::Pop);
        program.push_instruction(SpcInstruction::Builtin {
            symbol_index: get_selected,
            argument_count: 0,
        });
        program.push_instruction(SpcInstruction::Return);

        program.set_instruction_target(not_actor, end).unwrap();
        program
            .set_instruction_target(else_jump, else_start)
            .unwrap();
        program.set_instruction_target(end_true, end).unwrap();
        program
            .set_instruction_target(call_wrapper, wrapper_start)
            .unwrap();
        program
            .set_instruction_target(call_leaf, leaf_start)
            .unwrap();
        program
            .set_symbol_target(wrapper_symbol as usize, wrapper_start)
            .unwrap();
        program
            .set_symbol_target(leaf_symbol as usize, leaf_start)
            .unwrap();

        let mut object = SceneObject::new("choice", "NPCMonteM");
        object.insert_source_raw_param("name", "Choice NPC");
        let resources = DialogueResourceSet {
            scripts: vec![DialogueScriptResource {
                raw_path: b"map/sp/helper_choice.sb".to_vec(),
                document: program.to_document().unwrap(),
            }],
            messages: BTreeMap::from([(
                (DialogueDomain::Stage, STAGE_DIALOGUE_MESSAGE_PATH.to_vec()),
                indexed_bmg(3, "Choice"),
            )]),
            collection_issues: Vec::new(),
        };

        let index = resolve_dialogue_routes("test", &[object], None, &resources);
        let variants = index.variants_for_object("choice");
        assert_eq!(variants.len(), 3, "{:?}", index.issues);
        let find = |message_id| {
            variants
                .iter()
                .find(|variant| {
                    variant
                        .message
                        .as_ref()
                        .is_some_and(|message| message.full_message_id == message_id)
                })
                .unwrap_or_else(|| panic!("missing message {message_id:#010x}"))
        };
        assert_eq!(find(0x0001_0000).route_kind, DialogueRouteKind::Normal);
        let selected = find(0x0001_0001);
        assert_eq!(selected.route_kind, DialogueRouteKind::Choice);
        assert!(selected.condition_path.contains("Selected choice = 0"));
        let inverse = find(0x0001_0002);
        assert_eq!(inverse.route_kind, DialogueRouteKind::Choice);
        assert!(inverse.condition_path.contains("Selected choice != 0"));
    }

    #[test]
    fn selection_helper_analysis_rejects_mixed_value_returns() {
        let instructions = vec![
            SpcInstruction::Builtin {
                symbol_index: 0,
                argument_count: 0,
            },
            SpcInstruction::Return,
            SpcInstruction::IntOne,
            SpcInstruction::Return,
            SpcInstruction::ReturnZero,
        ];
        let functions = vec![FunctionRange {
            name: "mixed".to_string(),
            start_address: 0,
            start_index: 0,
            end_index: instructions.len(),
        }];
        let builtin_names = BTreeMap::from([(0, "getTalkSelectedValue")]);

        assert!(
            talk_selected_return_functions(&functions, &instructions, &builtin_names).is_empty()
        );

        let zero_only = vec![SpcInstruction::ReturnZero];
        let zero_function = vec![FunctionRange {
            name: "zero_only".to_string(),
            start_address: 0,
            start_index: 0,
            end_index: zero_only.len(),
        }];
        assert!(
            talk_selected_return_functions(&zero_function, &zero_only, &builtin_names).is_empty()
        );
    }

    #[test]
    fn branch_assigned_message_ids_emit_each_finite_route_instead_of_last_writer_wins() {
        let mut program = SpcRelocatableProgram::new(1);
        let get_name = append_builtin_symbol(&mut program, "getTalkNPCName").unwrap();
        let predicate = append_builtin_symbol(&mut program, "checkDialogueRoute").unwrap();
        let set_message = append_builtin_symbol(&mut program, "setTalkMsgID").unwrap();
        let name = program.append_data("Branch NPC".to_string()).unwrap();
        program.push_instruction(SpcInstruction::Builtin {
            symbol_index: get_name,
            argument_count: 0,
        });
        program.push_instruction(SpcInstruction::String(name));
        program.push_instruction(SpcInstruction::Equal);
        let not_actor = program.push_instruction(SpcInstruction::JumpIfZero(0));
        program.push_instruction(SpcInstruction::Int(0x0001_0000));
        program.push_instruction(SpcInstruction::Assign {
            reserved: 0,
            layer: 0,
            variable: 0,
        });
        program.push_instruction(SpcInstruction::Builtin {
            symbol_index: predicate,
            argument_count: 0,
        });
        let alternate = program.push_instruction(SpcInstruction::JumpIfZero(0));
        program.push_instruction(SpcInstruction::Int(0x0001_0000));
        program.push_instruction(SpcInstruction::Assign {
            reserved: 0,
            layer: 0,
            variable: 0,
        });
        let join = program.push_instruction(SpcInstruction::Jump(0));
        let alternate_start = program.instructions.len();
        program.push_instruction(SpcInstruction::Int(0x0001_0001));
        program.push_instruction(SpcInstruction::Assign {
            reserved: 0,
            layer: 0,
            variable: 0,
        });
        let join_target = program.instructions.len();
        program.push_instruction(SpcInstruction::Variable {
            layer: 0,
            variable: 0,
        });
        program.push_instruction(SpcInstruction::IntZero);
        program.push_instruction(SpcInstruction::Builtin {
            symbol_index: set_message,
            argument_count: 2,
        });
        program.push_instruction(SpcInstruction::Pop);
        let end = program.push_instruction(SpcInstruction::End);
        program.set_instruction_target(not_actor, end).unwrap();
        program
            .set_instruction_target(alternate, alternate_start)
            .unwrap();
        program.set_instruction_target(join, join_target).unwrap();

        let mut object = SceneObject::new("branch", "NPCMonteM");
        object.insert_source_raw_param("name", "Branch NPC");
        let resources = DialogueResourceSet {
            scripts: vec![DialogueScriptResource {
                raw_path: b"map/sp/branch_values.sb".to_vec(),
                document: program.to_document().unwrap(),
            }],
            messages: BTreeMap::from([(
                (DialogueDomain::Stage, STAGE_DIALOGUE_MESSAGE_PATH.to_vec()),
                indexed_bmg(2, "Branch"),
            )]),
            collection_issues: Vec::new(),
        };

        let index = resolve_dialogue_routes("test", &[object], None, &resources);
        let variants = index.variants_for_object("branch");
        assert_eq!(variants.len(), 2, "{:?}", index.issues);
        assert_eq!(
            variants
                .iter()
                .filter_map(|variant| variant.message.as_ref())
                .map(|message| message.full_message_id)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([0x0001_0000, 0x0001_0001])
        );
        assert!(variants
            .iter()
            .any(|variant| variant.condition_path.contains("checkDialogueRoute")));
        assert!(variants
            .iter()
            .any(|variant| variant.condition_path.contains("not checkDialogueRoute")));
        for variant in variants {
            let expected = variant.message.as_ref().unwrap().full_message_id as i32;
            assert_eq!(
                resources.scripts[0]
                    .document
                    .instructions
                    .get(variant.compiler_location.message_instruction_index),
                Some(&SpcInstruction::Int(expected))
            );
        }
        assert_eq!(index.callsites.len(), 2);
        assert!(index
            .callsites
            .iter()
            .all(|callsite| callsite.status == DialogueCallsiteStatus::Resolved));
    }

    #[test]
    fn semantic_default_and_proxy_routes_do_not_depend_on_retail_filenames() {
        let mut default_program = SpcRelocatableProgram::new(0);
        let get_name = append_builtin_symbol(&mut default_program, "getTalkNPCName").unwrap();
        let set_message = append_builtin_symbol(&mut default_program, "setTalkMsgID").unwrap();
        default_program.push_instruction(SpcInstruction::Builtin {
            symbol_index: get_name,
            argument_count: 0,
        });
        default_program.push_instruction(SpcInstruction::Pop);
        default_program.push_instruction(SpcInstruction::Int(0x0001_0000));
        default_program.push_instruction(SpcInstruction::IntZero);
        default_program.push_instruction(SpcInstruction::Builtin {
            symbol_index: set_message,
            argument_count: 2,
        });
        default_program.push_instruction(SpcInstruction::Pop);
        default_program.push_instruction(SpcInstruction::End);

        let mut proxy_program = SpcRelocatableProgram::new(0);
        let connect = append_builtin_symbol(&mut proxy_program, "connectDummyNpc").unwrap();
        let get_handle = append_builtin_symbol(&mut proxy_program, "getNameRefHandle").unwrap();
        let set_message = append_builtin_symbol(&mut proxy_program, "setTalkMsgID").unwrap();
        let shop_name = proxy_program.append_data("Shop A".to_string()).unwrap();
        let alias_name = proxy_program
            .append_data("Unplaced script alias".to_string())
            .unwrap();
        proxy_program.push_instruction(SpcInstruction::String(shop_name));
        proxy_program.push_instruction(SpcInstruction::Builtin {
            symbol_index: connect,
            argument_count: 1,
        });
        proxy_program.push_instruction(SpcInstruction::Pop);
        proxy_program.push_instruction(SpcInstruction::String(alias_name));
        proxy_program.push_instruction(SpcInstruction::Builtin {
            symbol_index: get_handle,
            argument_count: 1,
        });
        proxy_program.push_instruction(SpcInstruction::Int(0x0001_0000));
        proxy_program.push_instruction(SpcInstruction::IntZero);
        proxy_program.push_instruction(SpcInstruction::Builtin {
            symbol_index: set_message,
            argument_count: 2,
        });
        proxy_program.push_instruction(SpcInstruction::Pop);
        proxy_program.push_instruction(SpcInstruction::End);

        let mut sunflower = SceneObject::new("sunflower", "NPCSunflowerS");
        sunflower.insert_source_raw_param("name", "Sunflower");
        let mut shop = SceneObject::new("shop", "NPCRaccoonDog");
        shop.insert_source_raw_param("name", "Shop A");
        let registry = npc_registry(&[
            ("NPCSunflowerS", NPC_ACTOR_TYPE_SUNFLOWER_SMALL),
            ("NPCRaccoonDog", NPC_ACTOR_TYPE_RACCOON_DOG),
        ]);
        let resources = DialogueResourceSet {
            scripts: vec![
                DialogueScriptResource {
                    raw_path: b"map/sp/renamed_default_table.sb".to_vec(),
                    document: default_program.to_document().unwrap(),
                },
                DialogueScriptResource {
                    raw_path: b"map/sp/renamed_hidden_proxy.sb".to_vec(),
                    document: proxy_program.to_document().unwrap(),
                },
            ],
            messages: BTreeMap::from([(
                (DialogueDomain::Stage, STAGE_DIALOGUE_MESSAGE_PATH.to_vec()),
                stage_bmg("Semantic route"),
            )]),
            collection_issues: Vec::new(),
        };

        let index =
            resolve_dialogue_routes("test", &[sunflower, shop], Some(&registry), &resources);
        assert_eq!(index.variants_for_object("sunflower").len(), 1);
        assert_eq!(index.variants_for_object("shop").len(), 1);
        assert!(index
            .callsites
            .iter()
            .all(|callsite| { callsite.status == DialogueCallsiteStatus::Resolved }));
    }

    #[test]
    fn instruction_fingerprint_uses_string_content_instead_of_data_ordinal() {
        let instructions_a = vec![
            SpcInstruction::String(0),
            SpcInstruction::Int(0x0001_0000),
            SpcInstruction::Builtin {
                symbol_index: 0,
                argument_count: 2,
            },
        ];
        let instructions_b = vec![
            SpcInstruction::String(1),
            SpcInstruction::Int(0x0001_0000),
            SpcInstruction::Builtin {
                symbol_index: 0,
                argument_count: 2,
            },
        ];
        let symbols = vec![SpcSymbol {
            symbol_type: 0,
            name_offset: 0,
            data: 0,
            name_hash: 0,
            native_call: 0,
            name: "setTalkMsgID".to_string(),
        }];
        let data_a = vec![
            SpcDataEntry {
                offset: 0,
                value: "Semantic NPC".to_string(),
            },
            SpcDataEntry {
                offset: 0,
                value: "Other".to_string(),
            },
        ];
        let data_b = vec![data_a[1].clone(), data_a[0].clone()];

        let fingerprint_a =
            normalized_instruction_fingerprint(&instructions_a, 2, 1, &symbols, &data_a);
        let fingerprint_b =
            normalized_instruction_fingerprint(&instructions_b, 2, 1, &symbols, &data_b);
        assert_eq!(fingerprint_a, fingerprint_b);

        let mut changed_data = data_b;
        changed_data[1].value = "Different NPC".to_string();
        assert_ne!(
            fingerprint_a,
            normalized_instruction_fingerprint(&instructions_b, 2, 1, &symbols, &changed_data,)
        );

        let call_at = |address| {
            vec![SpcInstruction::Call {
                address,
                argument_count: 1,
            }]
        };
        let function_symbol = |name: &str, address| SpcSymbol {
            symbol_type: 1,
            name_offset: 0,
            data: address,
            name_hash: 0,
            native_call: 0,
            name: name.to_string(),
        };
        let helper_a = normalized_instruction_fingerprint(
            &call_at(100),
            0,
            usize::MAX,
            &[function_symbol("helperA", 100)],
            &[],
        );
        let relocated_helper_a = normalized_instruction_fingerprint(
            &call_at(400),
            0,
            usize::MAX,
            &[function_symbol("helperA", 400)],
            &[],
        );
        let helper_b = normalized_instruction_fingerprint(
            &call_at(100),
            0,
            usize::MAX,
            &[function_symbol("helperB", 100)],
            &[],
        );
        assert_eq!(helper_a, relocated_helper_a);
        assert_ne!(helper_a, helper_b);
    }

    #[test]
    fn detached_global_callsites_are_counted_as_shared_consumers() {
        let mut program = SpcRelocatableProgram::new(0);
        let set_message = append_builtin_symbol(&mut program, "setTalkMsgID").unwrap();
        program.push_instruction(SpcInstruction::IntZero);
        program.push_instruction(SpcInstruction::IntZero);
        program.push_instruction(SpcInstruction::Builtin {
            symbol_index: set_message,
            argument_count: 2,
        });
        program.push_instruction(SpcInstruction::Pop);
        program.push_instruction(SpcInstruction::End);
        let resources = DialogueResourceSet {
            scripts: vec![DialogueScriptResource {
                raw_path: b"common/sp/global_announcement.sb".to_vec(),
                document: program.to_document().unwrap(),
            }],
            messages: BTreeMap::from([(
                (
                    DialogueDomain::System,
                    SYSTEM_DIALOGUE_MESSAGE_PATH.to_vec(),
                ),
                stage_bmg("Global"),
            )]),
            collection_issues: Vec::new(),
        };

        let routes = resolve_dialogue_routes("stage-a", &[], None, &resources);
        assert_eq!(routes.detached_consumers.len(), 1);
        let message = routes.detached_consumers[0].message.clone();
        let mut consumers = DialogueGameConsumerIndex::default();
        consumers
            .add_detached(&routes.detached_consumers[0])
            .unwrap();
        consumers.finish();
        let affected = consumers.consumers("another-stage", &message);
        assert_eq!(affected.len(), 1);
        assert!(affected[0].object_id.starts_with("<global:"));
    }

    #[test]
    fn stock_happy_routes_cover_decomp_final_ids_and_child_selection() {
        let mut adult_monte = SceneObject::new("adult", "NPCMonteM");
        adult_monte.insert_source_raw_param("name", "Adult Monte");
        let mut child_monte = SceneObject::new("child", "NPCMonteM");
        child_monte.insert_source_raw_param("name", "Child Monte");
        child_monte.transform.scale = [0.5; 3];
        let mut child_mare_w = SceneObject::new("mare-child", "NPCMareW");
        child_mare_w.insert_source_raw_param("name", "Child Noki");
        child_mare_w.transform.scale = [0.6; 3];
        let mut kinopio = SceneObject::new("toad", "NPCKinopio");
        kinopio.insert_source_raw_param("name", "Toad");
        let mut mare_mb = SceneObject::new("mare-mb", "NPCMareMB");
        mare_mb.insert_source_raw_param("name", "Noki override");
        let mut named = SceneObject::new("airport", "NPCMonteM");
        named.insert_source_raw_param("name", STOCK_HAPPY_AIRPORT_MONTE_NAME);
        let objects = vec![
            adult_monte,
            child_monte,
            child_mare_w,
            kinopio,
            mare_mb,
            named,
        ];
        let registry = npc_registry(&[
            ("NPCMonteM", 0x0400_0001),
            ("NPCMareW", 0x0400_0013),
            ("NPCKinopio", NPC_ACTOR_TYPE_KINOPIO),
            ("NPCMareMB", NPC_ACTOR_TYPE_MARE_MB_OVERRIDE),
        ]);
        let resources = DialogueResourceSet {
            scripts: Vec::new(),
            messages: BTreeMap::from([(
                (
                    DialogueDomain::System,
                    SYSTEM_DIALOGUE_MESSAGE_PATH.to_vec(),
                ),
                indexed_bmg(0x2e, "System"),
            )]),
            collection_issues: Vec::new(),
        };

        let index = resolve_dialogue_routes("test", &objects, Some(&registry), &resources);
        let ids = |object_id: &str| {
            index
                .variants_for_object(object_id)
                .iter()
                .filter_map(|variant| {
                    variant
                        .message
                        .as_ref()
                        .map(|message| message.full_message_id)
                })
                .collect::<BTreeSet<_>>()
        };
        assert_eq!(ids("adult"), BTreeSet::from([0x23]));
        assert_eq!(ids("child"), BTreeSet::from([0x29]));
        assert_eq!(ids("mare-child"), BTreeSet::from([0x2d]));
        assert_eq!(ids("toad"), BTreeSet::from([0x25]));
        assert_eq!(ids("mare-mb"), BTreeSet::from([0x24, 0x2b]));
        assert_eq!(ids("airport"), BTreeSet::from([0x26]));
        assert!(index
            .variants_for_object("adult")
            .iter()
            .all(|variant| variant.provenance == DialogueProvenance::RuntimeOverride));
    }

    #[test]
    fn game_consumer_index_scopes_stage_messages_and_unions_dat1_aliases() {
        let stage_a = consumer_variant("stage-a", "a", 0, 24);
        let stage_b = consumer_variant("stage-b", "b", 0, 24);
        let alias = consumer_variant("stage-a", "alias", 1, 24);
        let stage_a_message = stage_a.message.clone().unwrap();
        let stage_b_message = stage_b.message.clone().unwrap();
        let alias_message = alias.message.clone().unwrap();
        let mut index = DialogueGameConsumerIndex::default();
        index.add_variant(&stage_a).unwrap();
        index.add_variant(&stage_b).unwrap();
        index.add_variant(&alias).unwrap();
        index.finish();

        let stage_a_consumers = index.consumers("stage-a", &stage_a_message);
        assert_eq!(stage_a_consumers.len(), 2);
        assert!(stage_a_consumers
            .iter()
            .any(|consumer| consumer.object_id == "a"));
        assert!(stage_a_consumers
            .iter()
            .any(|consumer| consumer.object_id == "alias"));
        assert_eq!(
            index.consumers("stage-a", &alias_message),
            stage_a_consumers
        );
        assert_eq!(index.consumers("stage-b", &stage_b_message).len(), 1);
        assert_eq!(
            index.consumers("stage-b", &stage_b_message)[0].object_id,
            "b"
        );
    }

    #[test]
    fn persisted_instance_allocation_splits_effective_consumers_and_inherited_duplicates() {
        let source = consumer_variant("stage-a", "source", 0, 24);
        let mut original_peer = consumer_variant("stage-a", "original-peer", 0, 24);
        original_peer.key = source.key.clone();
        let mut inherited = consumer_variant("stage-a", "inherited", 0, 24);
        inherited.key = source.key.clone();
        let original_message = source.message.clone().unwrap();

        let authored = DialogueVariantOverride {
            key: source.key.clone(),
            scope: DialogueEditScope::Instance,
            route_kind: DialogueRouteKind::Normal,
            condition_path: "Always".to_string(),
            content: source.content.clone(),
        };
        let mut document = empty_document("stage-a");
        document
            .objects
            .extend(["source", "original-peer", "inherited"].map(|object_id| {
                let mut object = SceneObject::new(object_id, "NPCMonteM");
                object.insert_source_raw_param("name", object_id);
                object
            }));
        document.dialogue_authoring = Some(DialogueAuthoringDocument {
            format_version: DIALOGUE_AUTHORING_FORMAT_VERSION,
            objects: BTreeMap::from([
                (
                    "source".to_string(),
                    DialogueObjectAuthoring {
                        overrides: vec![authored],
                        stable_allocations: vec![DialogueStableAllocation {
                            key: source.key.clone(),
                            message_index: 7,
                        }],
                        ..DialogueObjectAuthoring::default()
                    },
                ),
                (
                    "inherited".to_string(),
                    DialogueObjectAuthoring {
                        inherited_from_object_id: Some("source".to_string()),
                        ..DialogueObjectAuthoring::default()
                    },
                ),
            ]),
        });

        let source_allocation = effective_dialogue_consumer_allocation(&document, &source).unwrap();
        let inherited_allocation =
            effective_dialogue_consumer_allocation(&document, &inherited).unwrap();
        assert_eq!(source_allocation, inherited_allocation);
        assert!(effective_dialogue_consumer_allocation(&document, &original_peer).is_none());

        let mut index = DialogueGameConsumerIndex::default();
        index
            .add_variant_with_allocation(&source, Some(&source_allocation))
            .unwrap();
        index.add_variant(&original_peer).unwrap();
        index
            .add_variant_with_allocation(&inherited, Some(&inherited_allocation))
            .unwrap();
        let routes = DialogueRouteIndex {
            variants_by_object: BTreeMap::from([
                ("source".to_string(), vec![source.clone()]),
                ("original-peer".to_string(), vec![original_peer.clone()]),
                ("inherited".to_string(), vec![inherited.clone()]),
            ]),
            ..DialogueRouteIndex::default()
        };
        index.add_shared_edit_rejoins(&document, &routes);
        index.finish();

        let allocated_consumers = index.consumers_for_variant(&source);
        assert_eq!(allocated_consumers.len(), 2);
        assert!(allocated_consumers
            .iter()
            .any(|consumer| consumer.object_id == "source"));
        assert!(allocated_consumers
            .iter()
            .any(|consumer| consumer.object_id == "inherited"));
        assert_eq!(index.consumers_for_variant(&inherited), allocated_consumers);
        assert_eq!(index.consumers_for_variant(&original_peer).len(), 1);
        assert_eq!(
            index.consumers_for_variant(&original_peer)[0].object_id,
            "original-peer"
        );
        assert_eq!(index.consumers("stage-a", &original_message).len(), 1);
        assert_eq!(
            index.consumers("stage-a", &original_message)[0].object_id,
            "original-peer"
        );

        // Converting the source override to Shared mutates the retail entry.
        // The confirmation must therefore include the original peer plus the
        // source and its inheriting duplicate, which both rejoin that entry.
        let source_shared_impact = index.shared_edit_consumers_for_variant(&source);
        assert_eq!(source_shared_impact.len(), 3);
        assert!(source_shared_impact
            .iter()
            .any(|consumer| consumer.object_id == "source"));
        assert!(source_shared_impact
            .iter()
            .any(|consumer| consumer.object_id == "inherited"));
        assert!(source_shared_impact
            .iter()
            .any(|consumer| consumer.object_id == "original-peer"));

        let mut shared_document = document.clone();
        assert!(shared_document.remove_dialogue_override(
            "source",
            &source.key,
            DialogueEditScope::Instance,
        ));
        shared_document
            .set_dialogue_override(
                "source",
                source.key.clone(),
                DialogueEditScope::Shared,
                source.route_kind,
                source.condition_path.clone(),
                source.content.clone(),
            )
            .unwrap();
        let mut after_shared_edit = DialogueGameConsumerIndex::default();
        for variant in [&source, &original_peer, &inherited] {
            let allocation = effective_dialogue_consumer_allocation(&shared_document, variant);
            after_shared_edit
                .add_variant_with_allocation(variant, allocation.as_ref())
                .unwrap();
        }
        after_shared_edit.finish();
        assert_eq!(
            after_shared_edit.consumers_for_variant(&source),
            source_shared_impact
        );

        // Converting only the inheriting actor creates its own Shared override;
        // its source remains on the clone and is not part of the mutation.
        let inherited_shared_impact = index.shared_edit_consumers_for_variant(&inherited);
        assert_eq!(inherited_shared_impact.len(), 2);
        assert!(!inherited_shared_impact
            .iter()
            .any(|consumer| consumer.object_id == "source"));
    }

    #[test]
    fn inherited_common_route_shared_edit_stops_clone_inheritance_and_uses_library_content() {
        let mut source = consumer_variant("stage-a", "source", 0, 24);
        let system_message = DialogueMessageRef {
            domain: DialogueDomain::System,
            raw_resource_path: SYSTEM_DIALOGUE_MESSAGE_PATH.to_vec(),
            full_message_id: 0x23,
            entry_index: 0x23,
        };
        source.message = Some(system_message.clone());
        source.key.original_message = Some(system_message);
        source.content.message = text("Source instance clone");
        let mut inherited = source.clone();
        inherited.object_id = "inherited".to_string();
        inherited.runtime_name = "inherited".to_string();
        let mut original_peer = source.clone();
        original_peer.object_id = "original-peer".to_string();
        original_peer.runtime_name = "original-peer".to_string();
        original_peer.content.message = text("Retail system message");

        let mut document = empty_document("stage-a");
        document
            .objects
            .extend(["source", "inherited", "original-peer"].map(|object_id| {
                let mut object = SceneObject::new(object_id, "NPCMonteM");
                object.insert_source_raw_param("name", object_id);
                object
            }));
        document.dialogue_authoring = Some(DialogueAuthoringDocument {
            format_version: DIALOGUE_AUTHORING_FORMAT_VERSION,
            objects: BTreeMap::from([
                (
                    "source".to_string(),
                    DialogueObjectAuthoring {
                        overrides: vec![DialogueVariantOverride {
                            key: source.key.clone(),
                            scope: DialogueEditScope::Instance,
                            route_kind: source.route_kind,
                            condition_path: source.condition_path.clone(),
                            content: source.content.clone(),
                        }],
                        stable_allocations: vec![DialogueStableAllocation {
                            key: source.key.clone(),
                            message_index: 7,
                        }],
                        ..DialogueObjectAuthoring::default()
                    },
                ),
                (
                    "inherited".to_string(),
                    DialogueObjectAuthoring {
                        inherited_from_object_id: Some("source".to_string()),
                        ..DialogueObjectAuthoring::default()
                    },
                ),
            ]),
        });
        let routes = DialogueRouteIndex {
            variants_by_object: BTreeMap::from([
                ("source".to_string(), vec![source.clone()]),
                ("inherited".to_string(), vec![inherited.clone()]),
                ("original-peer".to_string(), vec![original_peer.clone()]),
            ]),
            ..DialogueRouteIndex::default()
        };
        let shared_content = DialogueContent {
            message: text("Project-wide shared system message"),
            ..original_peer.content.clone()
        };

        document
            .set_dialogue_override(
                "inherited",
                inherited.key.clone(),
                DialogueEditScope::Shared,
                inherited.route_kind,
                inherited.condition_path.clone(),
                shared_content.clone(),
            )
            .unwrap();

        assert!(
            document.dialogue_authoring.as_ref().unwrap().objects["inherited"]
                .overrides
                .iter()
                .any(|authored| {
                    authored.key == inherited.key && authored.scope == DialogueEditScope::Shared
                })
        );
        assert!(effective_dialogue_consumer_allocation(&document, &inherited).is_none());
        assert!(effective_dialogue_consumer_allocation(&document, &source).is_some());
        assert_eq!(
            document
                .effective_dialogue_content(&routes, "inherited", &inherited.key)
                .unwrap(),
            shared_content
        );
        assert_eq!(
            document
                .effective_dialogue_content(&routes, "original-peer", &original_peer.key)
                .unwrap(),
            shared_content
        );
        assert_eq!(
            document
                .effective_dialogue_content(&routes, "source", &source.key)
                .unwrap()
                .message,
            text("Source instance clone")
        );
    }

    #[test]
    fn base_wide_consumer_validation_rejects_mixed_shared_page_presentations() {
        let mut normal = consumer_variant("stage-a", "normal", 0, 24);
        let mut board = consumer_variant("stage-b", "board", 0, 24);
        for variant in [&mut normal, &mut board] {
            let message = variant.message.as_mut().unwrap();
            message.domain = DialogueDomain::System;
            message.raw_resource_path = SYSTEM_DIALOGUE_MESSAGE_PATH.to_vec();
        }
        board.route_kind = DialogueRouteKind::BoardOrSign;
        let shared_message = normal.message.clone().unwrap();
        let mut consumers = DialogueGameConsumerIndex::default();
        consumers.add_variant(&normal).unwrap();
        consumers.add_variant(&board).unwrap();
        consumers.finish();

        let mut document = empty_document("stage-a");
        document
            .dialogue_library
            .common_overrides
            .push(ProjectDialogueOverride {
                message: shared_message,
                content: DialogueContent {
                    message: BmgMessage::default(),
                    authored_tokens: Some(vec![DialogueAuthoringToken::PageBreak {
                        line_count: 3,
                    }]),
                    attributes: vec![0; 8],
                    voice_index: Some(0),
                },
            });

        assert!(document.requires_game_dialogue_consumer_validation());
        let error = document
            .validate_game_dialogue_consumer_presentations(&consumers)
            .unwrap_err();
        assert!(error.to_string().contains("stage-b"));
        assert!(error.to_string().contains("6-line presentation"));
    }

    #[test]
    fn shared_dat1_edit_preserves_each_alias_opaque_inf1_attributes() {
        let mut bmg = stage_bmg("Retail shared text");
        bmg.entries[0].attributes = vec![1, 2, 3, 4, 5, 6, 7, 8];
        bmg.entries.push(BmgEntry {
            message_offset: bmg.entries[0].message_offset,
            attributes: vec![8, 7, 6, 5, 4, 3, 2, 1],
            message: bmg.entries[0].message.clone(),
        });
        bmg.canonicalize_layout().unwrap();
        let text_only = DialogueContent {
            message: text("Text-only shared edit"),
            authored_tokens: None,
            attributes: bmg.entries[0].attributes.clone(),
            voice_index: Some(5),
        };
        apply_shared_content(&mut bmg, 0, &text_only).unwrap();
        assert_eq!(bmg.entries[1].attributes, vec![8, 7, 6, 5, 4, 3, 2, 1]);

        let content = DialogueContent {
            message: text("Edited shared text"),
            authored_tokens: None,
            attributes: vec![1, 2, 3, 4, 9, 6, 7, 8],
            voice_index: Some(9),
        };

        apply_shared_content(&mut bmg, 0, &content).unwrap();

        assert_eq!(bmg.entries[0].attributes, content.attributes);
        assert_eq!(bmg.entries[1].attributes, vec![8, 7, 6, 5, 9, 3, 2, 1]);
        assert_eq!(bmg.entries[0].message, text("Edited shared text"));
        assert_eq!(bmg.entries[1].message, text("Edited shared text"));
        assert_eq!(bmg.entries[0].message_offset, bmg.entries[1].message_offset);
    }

    #[test]
    fn authored_page_break_reaches_the_selected_presentation_boundary() {
        for (line_count, prefix, expected_newlines) in [
            (3, "", 3usize),
            (3, "line 1\n", 2),
            (3, "1\n2\n", 1),
            (6, "", 6),
            (6, "1\n2\n3\n4\n", 2),
        ] {
            let content = DialogueContent {
                message: BmgMessage::default(),
                authored_tokens: Some(vec![
                    DialogueAuthoringToken::Text(prefix.to_string()),
                    DialogueAuthoringToken::PageBreak { line_count },
                    DialogueAuthoringToken::Text("next page".to_string()),
                ]),
                attributes: vec![0; 8],
                voice_index: Some(0),
            };
            let compiled = content.compiled_message().unwrap();
            assert_eq!(compiled.tokens.len(), 1);
            assert_eq!(
                compiled.tokens[0],
                sms_formats::BmgMessageToken::Text(format!(
                    "{prefix}{}next page",
                    "\n".repeat(expected_newlines)
                ))
            );
        }
    }

    #[test]
    fn adjacent_authored_text_tokens_compile_to_the_canonical_bmg_shape() {
        let content = DialogueContent {
            message: BmgMessage::default(),
            authored_tokens: Some(vec![
                DialogueAuthoringToken::Text("first".to_string()),
                DialogueAuthoringToken::Text(" second".to_string()),
            ]),
            attributes: vec![0; 8],
            voice_index: Some(0),
        };

        assert_eq!(
            content.compiled_message().unwrap().tokens,
            [sms_formats::BmgMessageToken::Text(
                "first second".to_string()
            )]
        );
    }

    #[test]
    fn board_presentation_uses_six_lines_even_for_generated_routes() {
        let mut document = empty_document("board-page-lines");
        document.objects.push(SceneObject::new("board", "NPCBoard"));
        document.registry = Some(npc_registry(&[("NPCBoard", NPC_ACTOR_TYPE_BOARD)]));

        assert_eq!(
            document.dialogue_page_line_count("board", DialogueRouteKind::Generated),
            6
        );
        assert_eq!(
            document.dialogue_page_line_count("board", DialogueRouteKind::BoardOrSign),
            6
        );
    }

    #[test]
    fn export_rejects_stale_page_height_and_empty_page_only_text_warns() {
        let mut document = empty_document("board-page-stale");
        document.objects.push(SceneObject::new("board", "NPCBoard"));
        document.registry = Some(npc_registry(&[("NPCBoard", NPC_ACTOR_TYPE_BOARD)]));
        let key = DialogueVariantKey::generated_for_object("board");
        document
            .set_dialogue_override(
                "board",
                key,
                DialogueEditScope::Instance,
                DialogueRouteKind::Generated,
                "New normal conversation",
                DialogueContent {
                    message: BmgMessage::default(),
                    authored_tokens: Some(vec![DialogueAuthoringToken::PageBreak {
                        line_count: 3,
                    }]),
                    attributes: vec![0; 8],
                    voice_index: Some(0),
                },
            )
            .unwrap();
        let index = document
            .build_dialogue_route_index_with_common(&DialogueResourceSet::empty())
            .unwrap();

        let error = document.validate_dialogue_for_export(&index).unwrap_err();
        assert!(error.to_string().contains("page-presentation-mismatch"));
        assert!(validate_dialogue_document(&document)
            .iter()
            .any(|issue| issue.code == "dialogue-message-empty"));
    }

    #[test]
    fn stage_allocation_indexes_are_scoped_by_bmg_resource() {
        let mut document = empty_document("allocation-paths");
        for (object_id, path) in [
            ("first", b"map/first.bmg".as_slice()),
            ("second", b"map/second.bmg".as_slice()),
        ] {
            document
                .objects
                .push(SceneObject::new(object_id, "NPCBoard"));
            let message = DialogueMessageRef {
                domain: DialogueDomain::Stage,
                raw_resource_path: path.to_vec(),
                full_message_id: 0x0001_0000,
                entry_index: 0,
            };
            document
                .dialogue_authoring
                .get_or_insert_with(DialogueAuthoringDocument::default)
                .objects
                .insert(
                    object_id.to_string(),
                    DialogueObjectAuthoring {
                        inherited_from_object_id: None,
                        prior_runtime_name: None,
                        overrides: Vec::new(),
                        stable_allocations: vec![DialogueStableAllocation {
                            key: DialogueVariantKey {
                                source: DialogueVariantKey::generated_for_object(object_id).source,
                                original_message: Some(message),
                            },
                            message_index: 12,
                        }],
                    },
                );
        }

        assert!(!validate_dialogue_document(&document)
            .iter()
            .any(|issue| issue.code == "dialogue-allocation-conflict"));
    }

    #[test]
    fn export_rejects_same_object_route_allocations_colliding_in_one_bmg() {
        let mut document = empty_document("allocation-collision");
        document.objects.push(SceneObject::new("owner", "NPCBoard"));
        let message = DialogueMessageRef {
            domain: DialogueDomain::Stage,
            raw_resource_path: STAGE_DIALOGUE_MESSAGE_PATH.to_vec(),
            full_message_id: 0x0001_0000,
            entry_index: 0,
        };
        let mut first_key = DialogueVariantKey::generated_for_object("owner");
        first_key.original_message = Some(message.clone());
        let mut second_key = first_key.clone();
        second_key.source.callsite_occurrence = 1;
        document.dialogue_authoring = Some(DialogueAuthoringDocument {
            format_version: DIALOGUE_AUTHORING_FORMAT_VERSION,
            objects: BTreeMap::from([(
                "owner".to_string(),
                DialogueObjectAuthoring {
                    stable_allocations: vec![
                        DialogueStableAllocation {
                            key: first_key,
                            message_index: 12,
                        },
                        DialogueStableAllocation {
                            key: second_key,
                            message_index: 12,
                        },
                    ],
                    ..DialogueObjectAuthoring::default()
                },
            )]),
        });

        let error = document
            .validate_dialogue_for_export(&DialogueRouteIndex::default())
            .unwrap_err();
        assert!(error.to_string().contains("dialogue-allocation-conflict"));
    }

    #[test]
    fn conflicting_stage_shared_overrides_block_instead_of_last_writer_wins() {
        let mut document = empty_document("retail");
        document.objects.push(SceneObject::new("first", "NPCBoard"));
        document
            .objects
            .push(SceneObject::new("second", "NPCBoard"));
        let message = DialogueMessageRef {
            domain: DialogueDomain::Stage,
            raw_resource_path: STAGE_DIALOGUE_MESSAGE_PATH.to_vec(),
            full_message_id: 0x0001_0000,
            entry_index: 0,
        };
        for (object_id, value) in [("first", "First edit"), ("second", "Second edit")] {
            let mut key = DialogueVariantKey::generated_for_object(object_id);
            key.original_message = Some(message.clone());
            document
                .set_dialogue_override(
                    object_id,
                    key,
                    DialogueEditScope::Shared,
                    DialogueRouteKind::BoardOrSign,
                    "Always",
                    DialogueContent {
                        message: text(value),
                        authored_tokens: None,
                        attributes: vec![0; 8],
                        voice_index: Some(0),
                    },
                )
                .unwrap();
        }

        assert!(validate_dialogue_document(&document)
            .iter()
            .any(|issue| issue.code == "dialogue-stage-shared-override-conflict"));
    }

    #[test]
    fn balloon_clones_are_project_global_stable_and_rebuilt_across_stages() {
        let mut stage_a = balloon_document("stage-a", "npc-a", "Board A");
        let index_a = stage_a.build_dialogue_route_index().unwrap();
        let variant_a = &index_a.variants_for_object("npc-a")[0];
        let mut content_a = variant_a.content.clone();
        content_a.message = text("Stage A balloon");
        stage_a
            .set_dialogue_override(
                "npc-a",
                variant_a.key.clone(),
                DialogueEditScope::Instance,
                DialogueRouteKind::Balloon,
                variant_a.condition_path.clone(),
                content_a,
            )
            .unwrap();
        let compiled_a = stage_a.compile_dialogue_authoring(&index_a).unwrap();
        assert!(compiled_a.stage_edits.resources.is_empty());
        assert_eq!(
            stage_a.dialogue_library.stable_allocations[0].message_index,
            1
        );
        assert_eq!(
            compiled_a.common_resources[0].document.entries[1].message,
            text("Stage A balloon")
        );
        let compiled_a_again = stage_a.compile_dialogue_authoring(&index_a).unwrap();
        assert_eq!(compiled_a_again, compiled_a);

        let serialized_library = serde_json::to_vec(&stage_a.dialogue_library).unwrap();
        let mut stage_b = balloon_document("stage-b", "npc-b", "Board B");
        stage_b.dialogue_library = serde_json::from_slice(&serialized_library).unwrap();
        let index_b = stage_b.build_dialogue_route_index().unwrap();
        let variant_b = &index_b.variants_for_object("npc-b")[0];
        let mut content_b = variant_b.content.clone();
        content_b.message = text("Stage B balloon");
        stage_b
            .set_dialogue_override(
                "npc-b",
                variant_b.key.clone(),
                DialogueEditScope::Instance,
                DialogueRouteKind::Balloon,
                variant_b.condition_path.clone(),
                content_b,
            )
            .unwrap();
        let compiled_b = stage_b.compile_dialogue_authoring(&index_b).unwrap();
        let common_bmg = &compiled_b.common_resources[0].document;
        assert_eq!(common_bmg.entries.len(), 3);
        assert_eq!(common_bmg.entries[1].message, text("Stage A balloon"));
        assert_eq!(common_bmg.entries[2].message, text("Stage B balloon"));

        let mut rebuild = balloon_document("stage-c", "unused", "Unused");
        rebuild.objects.clear();
        rebuild.dialogue_library = stage_b.dialogue_library.clone();
        let rebuilt = rebuild
            .compile_dialogue_authoring(&DialogueRouteIndex::default())
            .unwrap();
        let rebuilt_bmg = &rebuilt.common_resources[0].document;
        assert_eq!(rebuilt_bmg.entries[1].message, text("Stage A balloon"));
        assert_eq!(rebuilt_bmg.entries[2].message, text("Stage B balloon"));
    }

    #[test]
    fn resetting_balloon_instance_keeps_only_a_non_reusable_allocation_tombstone() {
        let mut document = balloon_document("stage-a", "npc-a", "Board A");
        let index = document.build_dialogue_route_index().unwrap();
        let variant = index.variants_for_object("npc-a")[0].clone();
        let mut content = variant.content.clone();
        content.message = text("Temporary balloon clone");
        document
            .set_dialogue_override(
                "npc-a",
                variant.key.clone(),
                DialogueEditScope::Instance,
                DialogueRouteKind::Balloon,
                variant.condition_path,
                content,
            )
            .unwrap();
        document.compile_dialogue_authoring(&index).unwrap();
        assert!(document.dialogue_library.stable_allocations[0]
            .content
            .is_some());

        assert!(document.remove_dialogue_override(
            "npc-a",
            &variant.key,
            DialogueEditScope::Instance
        ));
        let allocation = &document.dialogue_library.stable_allocations[0];
        assert_eq!(allocation.message_index, 1);
        assert!(allocation.content.is_none());

        let rebuilt = document.compile_dialogue_authoring(&index).unwrap();
        let rebuilt_bmg = &rebuilt.common_resources[0].document;
        assert_eq!(rebuilt_bmg.entries.len(), 2);
        assert_eq!(rebuilt_bmg.entries[1].message, BmgMessage::default());
    }

    #[test]
    fn untouched_duplicate_inherits_balloon_clone_through_guarded_runtime_remap() {
        let mut document = balloon_document("stage-a", "source", "Board A");
        let mut duplicate = SceneObject::new("duplicate", "NPCBoard");
        duplicate.insert_source_raw_param("name", "Board B");
        document.objects.push(duplicate);
        document
            .duplicate_dialogue_authoring("source", "duplicate")
            .unwrap();
        let initial_index = document.build_dialogue_route_index().unwrap();
        let source_variant = initial_index.variants_for_object("source")[0].clone();
        let mut content = source_variant.content.clone();
        content.message = text("Inherited balloon clone");
        document
            .set_dialogue_override(
                "source",
                source_variant.key.clone(),
                DialogueEditScope::Instance,
                DialogueRouteKind::Balloon,
                source_variant.condition_path,
                content,
            )
            .unwrap();
        let index = document.build_dialogue_route_index().unwrap();

        let compiled = document.compile_dialogue_authoring(&index).unwrap();
        let mut requests = compiled.runtime_override_requests;
        requests.sort_by(|left, right| left.object_id.cmp(&right.object_id));
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].object_id, "duplicate");
        assert_eq!(requests[1].object_id, "source");
        assert_eq!(
            requests[0].replacement_message_id,
            requests[1].replacement_message_id
        );
        assert_eq!(requests[0].original_message_id, Some(0));
        assert_eq!(requests[1].original_message_id, Some(0));
    }

    #[test]
    fn persisted_stage_clone_allocation_rematerializes_on_every_compile() {
        let mut document = empty_document("retail");
        let mut object = SceneObject::new("board", "NPCBoard");
        object.insert_source_raw_param("name", "Board A");
        document.objects.push(object);

        let mut bmg = stage_bmg("Retail shared text");
        bmg.entries.push(BmgEntry {
            message_offset: bmg.entries[0].message_offset,
            attributes: vec![9, 8, 7, 6, 5, 4, 3, 2],
            message: bmg.entries[0].message.clone(),
        });
        bmg.canonicalize_layout().unwrap();
        document.archive_edits.upsert_resource(
            STAGE_DIALOGUE_MESSAGE_PATH.to_vec(),
            StageResourceDocument::Message(bmg),
        );
        document.archive_edits.upsert_resource(
            b"map/sp/talk.sb".to_vec(),
            StageResourceDocument::Script(handle_script("Board A", 0x0001_0000)),
        );
        let index = document.build_dialogue_route_index().unwrap();
        let variant = &index.variants_for_object("board")[0];
        let mut content = variant.content.clone();
        content.message = text("Instance-only edit");
        document
            .set_dialogue_override(
                "board",
                variant.key.clone(),
                DialogueEditScope::Instance,
                variant.route_kind,
                variant.condition_path.clone(),
                content,
            )
            .unwrap();

        let first = document.compile_dialogue_authoring(&index).unwrap();
        assert_eq!(
            document.dialogue_authoring.as_ref().unwrap().objects["board"].stable_allocations[0]
                .message_index,
            2
        );
        let second = document
            .compile_dialogue_authoring(&index)
            .expect("persisted stage clone allocation rematerializes from base plus deltas");
        assert_eq!(second, first);
    }

    #[test]
    fn newly_placed_retail_pianta_edit_appends_without_replacing_source_entry() {
        let mut document = empty_document("retail-pianta");
        let mut source = SceneObject::new("source-pianta", "NPCMonteM");
        source.insert_source_raw_param("name", "Pianta Guide");
        let mut placed = SceneObject::new("placed-pianta", "NPCMonteM");
        placed.insert_source_raw_param("name", "Pianta Guide");
        document.objects.extend([source, placed]);
        document.registry = Some(npc_registry(&[("NPCMonteM", NPC_ACTOR_TYPE_MONTE_M_FIRST)]));

        let mut retail_bmg = stage_bmg("Original retail Pianta line");
        retail_bmg.canonicalize_layout().unwrap();
        let retail_entry = retail_bmg.entries[0].clone();
        document.archive_edits.upsert_resource(
            STAGE_DIALOGUE_MESSAGE_PATH.to_vec(),
            StageResourceDocument::Message(retail_bmg),
        );
        document.archive_edits.upsert_resource(
            SYSTEM_DIALOGUE_MESSAGE_PATH.to_vec(),
            StageResourceDocument::Message(indexed_bmg(64, "System")),
        );
        document.archive_edits.upsert_resource(
            b"map/sp/pianta_talk.sb".to_vec(),
            StageResourceDocument::Script(handle_script("Pianta Guide", 0x0001_0000)),
        );
        document
            .duplicate_dialogue_authoring("source-pianta", "placed-pianta")
            .unwrap();

        let index = document.build_dialogue_route_index().unwrap();
        let placed_variant = index
            .variants_for_object("placed-pianta")
            .iter()
            .find(|variant| {
                variant
                    .message
                    .as_ref()
                    .is_some_and(|message| message.full_message_id == 0x0001_0000)
            })
            .expect("placed Pianta inherits the retail normal-talk route")
            .clone();
        assert_eq!(placed_variant.shared_consumers.len(), 2);
        let mut edited = placed_variant.content.clone();
        edited.message = text("Only the newly placed Pianta says this");
        document
            .set_dialogue_override(
                "placed-pianta",
                placed_variant.key.clone(),
                DialogueEditScope::Instance,
                placed_variant.route_kind,
                placed_variant.condition_path,
                edited.clone(),
            )
            .unwrap();

        let first = document.compile_dialogue_authoring(&index).unwrap();
        let compiled_bmg = first
            .stage_edits
            .resources
            .iter()
            .find_map(|edit| match &edit.document {
                StageResourceDocument::Message(bmg)
                    if edit.raw_resource_path == STAGE_DIALOGUE_MESSAGE_PATH =>
                {
                    Some(bmg)
                }
                _ => None,
            })
            .expect("compiled stage dialogue BMG");
        assert_eq!(compiled_bmg.entries.len(), 2);
        assert_eq!(compiled_bmg.entries[0], retail_entry);
        assert_eq!(compiled_bmg.entries[1].message, edited.message);
        assert_eq!(compiled_bmg.entries[1].attributes, edited.attributes);

        let allocation = &document.dialogue_authoring.as_ref().unwrap().objects["placed-pianta"]
            .stable_allocations;
        assert_eq!(allocation.len(), 1);
        assert_eq!(allocation[0].message_index, 1);
        assert_eq!(first.runtime_override_requests.len(), 1);
        let remap = &first.runtime_override_requests[0];
        assert_eq!(remap.object_id, "placed-pianta");
        assert_eq!(remap.original_message_id, Some(0x0001_0000));
        assert_eq!(remap.replacement_message_id, 0x0001_0001);

        let second = document
            .compile_dialogue_authoring(&index)
            .expect("persisted Pianta allocation recompiles stably");
        assert_eq!(second, first);
    }

    #[test]
    fn deleted_stage_allocation_tombstone_is_never_reused() {
        let mut document = empty_document("authored");
        document
            .objects
            .push(SceneObject::new("new-npc", "NPCBoard"));
        document.archive_edits.upsert_resource(
            STAGE_DIALOGUE_MESSAGE_PATH.to_vec(),
            StageResourceDocument::Message(stage_bmg("Retail")),
        );
        let deleted_key = DialogueVariantKey::generated_for_object("deleted-npc");
        let new_key = document
            .initialize_dialogue_for_new_object("new-npc")
            .unwrap();
        let authoring = document.dialogue_authoring.as_mut().unwrap();
        authoring.objects.insert(
            "deleted-npc".to_string(),
            DialogueObjectAuthoring {
                inherited_from_object_id: None,
                prior_runtime_name: None,
                overrides: Vec::new(),
                stable_allocations: vec![DialogueStableAllocation {
                    key: deleted_key,
                    message_index: 1,
                }],
            },
        );
        let new_override = authoring.objects["new-npc"].overrides[0].clone();
        let mut edited = new_override.content;
        edited.message = text("New allocation");
        authoring.objects.get_mut("new-npc").unwrap().overrides[0].content = edited;

        let index = document.build_dialogue_route_index().unwrap();
        let compiled = document.compile_dialogue_authoring(&index).unwrap();
        let new_allocation = document.dialogue_authoring.as_ref().unwrap().objects["new-npc"]
            .stable_allocations
            .iter()
            .find(|allocation| allocation.key == new_key)
            .unwrap();
        assert_eq!(new_allocation.message_index, 2);
        let bmg = compiled
            .stage_edits
            .resources
            .iter()
            .find_map(|edit| match &edit.document {
                StageResourceDocument::Message(bmg)
                    if edit.raw_resource_path == STAGE_DIALOGUE_MESSAGE_PATH =>
                {
                    Some(bmg)
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(bmg.entries.len(), 3);
        assert!(bmg.entries[1].message.tokens.is_empty());
        assert_eq!(bmg.entries[2].message, text("New allocation"));
    }

    #[test]
    fn authoring_documents_round_trip_and_old_defaults_are_supported() {
        let key = DialogueVariantKey::generated_for_object("npc");
        let mut authoring = DialogueAuthoringDocument::default();
        authoring.objects.insert(
            "npc".to_string(),
            DialogueObjectAuthoring {
                inherited_from_object_id: None,
                prior_runtime_name: None,
                overrides: vec![DialogueVariantOverride {
                    key: key.clone(),
                    scope: DialogueEditScope::Instance,
                    route_kind: DialogueRouteKind::Generated,
                    condition_path: "new".to_string(),
                    content: DialogueContent {
                        message: text("Hello"),
                        authored_tokens: None,
                        attributes: vec![0, 0, 0, 0, 3, 0, 0, 0],
                        voice_index: Some(3),
                    },
                }],
                stable_allocations: vec![DialogueStableAllocation {
                    key,
                    message_index: 7,
                }],
            },
        );
        let bytes = serde_json::to_vec(&authoring).unwrap();
        assert_eq!(
            serde_json::from_slice::<DialogueAuthoringDocument>(&bytes).unwrap(),
            authoring
        );
        let empty: DialogueAuthoringDocument = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, DialogueAuthoringDocument::default());
        let library: ProjectDialogueLibrary = serde_json::from_str("{}").unwrap();
        assert_eq!(library, ProjectDialogueLibrary::default());
    }

    #[test]
    fn invalid_shift_jis_choice_draft_saves_but_blocks_export_validation() {
        let mut document = empty_document("authored");
        document
            .objects
            .push(SceneObject::new("new-npc", "NPCBoard"));
        let key = document
            .initialize_dialogue_for_new_object("new-npc")
            .unwrap();
        document
            .set_dialogue_override(
                "new-npc",
                key,
                DialogueEditScope::Instance,
                DialogueRouteKind::Generated,
                "New normal conversation",
                DialogueContent {
                    message: text(""),
                    authored_tokens: Some(vec![DialogueAuthoringToken::Control(
                        SmsBmgControl::Choice {
                            slot: 0,
                            text: "😀".to_string(),
                        },
                    )]),
                    attributes: vec![0; 8],
                    voice_index: Some(0),
                },
            )
            .unwrap();

        serde_json::to_vec(document.dialogue_authoring.as_ref().unwrap())
            .expect("temporarily invalid authored choice remains project-serializable");
        let issues = validate_dialogue_document(&document);
        assert!(issues
            .iter()
            .any(|issue| issue.code == "dialogue-content-invalid"));
        assert!(!issues
            .iter()
            .any(|issue| issue.code == "dialogue-message-empty"));
    }

    #[test]
    fn stage_authoring_and_common_library_survive_project_reload() {
        let (base_root, project_root) = unique_project_paths("project-round-trip");
        std::fs::create_dir_all(&base_root).unwrap();
        let mut saved = empty_document("authored");
        saved.base_root.clone_from(&base_root);
        saved.objects.push(SceneObject::new("new-npc", "NPCBoard"));
        saved.initialize_dialogue_for_new_object("new-npc").unwrap();
        saved
            .dialogue_library
            .common_overrides
            .push(ProjectDialogueOverride {
                message: DialogueMessageRef {
                    domain: DialogueDomain::System,
                    raw_resource_path: SYSTEM_DIALOGUE_MESSAGE_PATH.to_vec(),
                    full_message_id: 0x0002_0000,
                    entry_index: 0,
                },
                content: DialogueContent {
                    message: text("Shared edit"),
                    authored_tokens: None,
                    attributes: vec![0; 8],
                    voice_index: Some(0),
                },
            });
        saved.save_project_folder(&project_root).unwrap();

        let mut reopened = empty_document("authored");
        reopened.base_root = base_root;
        assert!(reopened.load_project_folder(&project_root).unwrap());
        assert_eq!(reopened.dialogue_authoring, saved.dialogue_authoring);
        assert_eq!(reopened.dialogue_library, saved.dialogue_library);

        std::fs::remove_dir_all(project_root.parent().unwrap()).unwrap();
    }

    #[test]
    fn optional_dialogue_library_can_be_cleared_reloaded_and_readded() {
        let (base_root, project_root) = unique_project_paths("library-clear-readd");
        std::fs::create_dir_all(&base_root).unwrap();
        let library_path = project_root
            .join("files")
            .join(PROJECT_DIALOGUE_LIBRARY_PATH);
        let shared_override = ProjectDialogueOverride {
            message: DialogueMessageRef {
                domain: DialogueDomain::System,
                raw_resource_path: SYSTEM_DIALOGUE_MESSAGE_PATH.to_vec(),
                full_message_id: 0x0002_0000,
                entry_index: 0,
            },
            content: DialogueContent {
                message: text("Shared edit"),
                authored_tokens: None,
                attributes: vec![0; 8],
                voice_index: Some(0),
            },
        };

        let mut saved = empty_document("retail");
        saved.base_root.clone_from(&base_root);
        saved
            .dialogue_library
            .common_overrides
            .push(shared_override.clone());
        saved.save_project_folder(&project_root).unwrap();
        assert!(library_path.is_file());

        saved.dialogue_library = ProjectDialogueLibrary::default();
        saved.save_project_folder(&project_root).unwrap();
        assert!(
            !library_path.exists(),
            "cleared optional library must not be copied back as unmanaged"
        );

        let mut reopened = empty_document("retail");
        reopened.base_root.clone_from(&base_root);
        assert!(reopened.load_project_folder(&project_root).unwrap());
        assert!(reopened.dialogue_library.is_empty());
        reopened
            .dialogue_library
            .common_overrides
            .push(shared_override.clone());
        reopened.save_project_folder(&project_root).unwrap();
        assert!(library_path.is_file());

        let mut reloaded = empty_document("retail");
        reloaded.base_root = base_root;
        assert!(reloaded.load_project_folder(&project_root).unwrap());
        assert_eq!(
            reloaded.dialogue_library.common_overrides,
            vec![shared_override]
        );

        std::fs::remove_dir_all(project_root.parent().unwrap()).unwrap();
    }

    #[test]
    fn dialogue_compile_without_authoring_is_an_exact_noop() {
        let mut document = empty_document("retail");
        let compiled = document
            .compile_dialogue_authoring(&DialogueRouteIndex::default())
            .unwrap();
        assert!(compiled.is_noop());
        assert!(document.dialogue_authoring.is_none());
    }

    #[test]
    fn generated_route_compiles_owned_script_and_stable_message_allocation() {
        let mut document = empty_document("authored");
        document
            .objects
            .push(SceneObject::new("new-npc", "NPCBoard"));
        let key = document
            .initialize_dialogue_for_new_object("new-npc")
            .unwrap();
        document
            .set_dialogue_override(
                "new-npc",
                key,
                DialogueEditScope::Instance,
                DialogueRouteKind::Generated,
                "New normal conversation",
                DialogueContent {
                    message: text("Authored line"),
                    authored_tokens: None,
                    attributes: vec![0, 0, 0, 0, 4, 0, 0, 0],
                    voice_index: Some(4),
                },
            )
            .unwrap();
        let index = document.build_dialogue_route_index().unwrap();
        let compiled = document.compile_dialogue_authoring(&index).unwrap();

        assert!(compiled.runtime_override_requests.is_empty());
        let script = compiled
            .stage_edits
            .resources
            .iter()
            .find(|edit| edit.raw_resource_path == GENERATED_DIALOGUE_SCRIPT_PATH)
            .and_then(|edit| match &edit.document {
                StageResourceDocument::Script(script) => Some(script),
                _ => None,
            })
            .expect("generated SPC edit");
        assert!(script
            .symbols
            .iter()
            .any(|symbol| symbol.name == GENERATED_DIALOGUE_SCRIPT_MARKER));
        script.to_bytes().unwrap();
        let message = compiled
            .stage_edits
            .resources
            .iter()
            .find(|edit| edit.raw_resource_path == STAGE_DIALOGUE_MESSAGE_PATH)
            .and_then(|edit| match &edit.document {
                StageResourceDocument::Message(message) => Some(message),
                _ => None,
            })
            .expect("generated BMG edit");
        assert_eq!(message.entries.len(), 1);
        assert_eq!(message.entries[0].message, text("Authored line"));
        assert_eq!(
            document.dialogue_authoring.as_ref().unwrap().objects["new-npc"].stable_allocations[0]
                .message_index,
            0
        );
        let rebuilt = document
            .compile_dialogue_authoring(&index)
            .expect("persisted allocation recompiles without requiring prior build output");
        assert_eq!(rebuilt, compiled);
    }

    #[test]
    fn generated_route_reset_restores_each_objects_prior_runtime_name() {
        let mut document = empty_document("authored");
        let mut source = SceneObject::new("source", "NPCBoard");
        source.insert_source_raw_param("name", "Original source name");
        document.objects.push(source);

        let key = document
            .initialize_dialogue_for_new_object("source")
            .unwrap();
        assert!(document.objects[0]
            .raw_param("name")
            .unwrap()
            .starts_with(GENERATED_DIALOGUE_RUNTIME_NAME_PREFIX));
        assert_eq!(
            document.dialogue_authoring.as_ref().unwrap().objects["source"]
                .prior_runtime_name
                .as_deref(),
            Some("Original source name")
        );

        assert!(document.remove_dialogue_override("source", &key, DialogueEditScope::Instance));
        assert_eq!(
            document.objects[0].raw_param("name"),
            Some("Original source name")
        );
        assert!(!document
            .dialogue_authoring
            .as_ref()
            .is_some_and(|authoring| authoring.objects.contains_key("source")));

        let mut duplicate_document = empty_document("authored-duplicate");
        let mut duplicate_source = SceneObject::new("source", "NPCBoard");
        duplicate_source.insert_source_raw_param("name", "Source before generated");
        let mut duplicate = SceneObject::new("duplicate", "NPCBoard");
        duplicate.insert_source_raw_param("name", "Original duplicate name");
        duplicate_document
            .objects
            .extend([duplicate_source, duplicate]);
        let duplicate_key = duplicate_document
            .initialize_dialogue_for_new_object("source")
            .unwrap();
        duplicate_document
            .duplicate_dialogue_authoring("source", "duplicate")
            .unwrap();
        assert!(duplicate_document.objects[1]
            .raw_param("name")
            .unwrap()
            .starts_with(GENERATED_DIALOGUE_RUNTIME_NAME_PREFIX));
        assert_eq!(
            duplicate_document
                .dialogue_authoring
                .as_ref()
                .unwrap()
                .objects["duplicate"]
                .prior_runtime_name
                .as_deref(),
            Some("Original duplicate name")
        );
        let duplicate_generated_name = duplicate_document.objects[1]
            .raw_param("name")
            .unwrap()
            .to_string();
        duplicate_document
            .set_dialogue_override(
                "duplicate",
                duplicate_key.clone(),
                DialogueEditScope::Instance,
                DialogueRouteKind::Generated,
                "Duplicated normal conversation",
                DialogueContent {
                    message: text("Copy-on-write duplicate"),
                    authored_tokens: None,
                    attributes: vec![0; 8],
                    voice_index: Some(0),
                },
            )
            .unwrap();
        assert!(duplicate_document.remove_dialogue_override(
            "duplicate",
            &duplicate_key,
            DialogueEditScope::Instance
        ));
        assert_eq!(
            duplicate_document.objects[1].raw_param("name"),
            Some(duplicate_generated_name.as_str())
        );
        assert_eq!(
            duplicate_document
                .dialogue_authoring
                .as_ref()
                .unwrap()
                .objects["duplicate"]
                .prior_runtime_name
                .as_deref(),
            Some("Original duplicate name")
        );
    }

    #[test]
    fn resetting_generated_source_promotes_untouched_duplicate_route() {
        let mut document = empty_document("promote-generated-duplicate");
        for (object_id, runtime_name) in [
            ("source", "Source retail name"),
            ("duplicate", "Duplicate retail name"),
            ("nested", "Nested retail name"),
        ] {
            let mut object = SceneObject::new(object_id, "NPCBoard");
            object.insert_source_raw_param("name", runtime_name);
            document.objects.push(object);
        }
        document.registry = Some(npc_registry(&[("NPCBoard", NPC_ACTOR_TYPE_BOARD)]));
        let source_key = document
            .initialize_dialogue_for_new_object("source")
            .unwrap();
        document
            .set_dialogue_override(
                "source",
                source_key.clone(),
                DialogueEditScope::Instance,
                DialogueRouteKind::Generated,
                "Authored source dialogue",
                DialogueContent {
                    message: text("Keep this dialogue"),
                    authored_tokens: None,
                    attributes: vec![0; 8],
                    voice_index: Some(0),
                },
            )
            .unwrap();
        document
            .duplicate_dialogue_authoring("source", "duplicate")
            .unwrap();
        document
            .duplicate_dialogue_authoring("duplicate", "nested")
            .unwrap();

        assert!(document.remove_dialogue_override(
            "source",
            &source_key,
            DialogueEditScope::Instance
        ));
        assert_eq!(
            document.objects[0].raw_param("name"),
            Some("Source retail name")
        );
        let authoring = document.dialogue_authoring.as_ref().unwrap();
        let duplicate = &authoring.objects["duplicate"];
        assert!(duplicate.inherited_from_object_id.is_none());
        assert_eq!(duplicate.overrides.len(), 1);
        assert_eq!(
            duplicate.overrides[0].key,
            DialogueVariantKey::generated_for_object("duplicate")
        );
        assert_eq!(
            duplicate.overrides[0].content.message,
            text("Keep this dialogue")
        );
        assert_eq!(
            authoring.objects["nested"]
                .inherited_from_object_id
                .as_deref(),
            Some("duplicate")
        );

        let index = document.build_dialogue_route_index().unwrap();
        assert!(!index.has_errors(), "{:?}", index.issues);
        assert_eq!(
            index.variants_for_object("duplicate")[0].content.message,
            text("Keep this dialogue")
        );
        assert_eq!(
            index.variants_for_object("nested")[0].content.message,
            text("Keep this dialogue")
        );
    }

    #[test]
    fn editing_an_unrouted_existing_actor_adopts_owned_identity_and_compiles() {
        let mut document = empty_document("retail-unrouted");
        let mut board = SceneObject::new("dormant-board", "NPCBoard");
        board.insert_source_raw_param("name", "Retail board name");
        document.objects.push(board);
        document.registry = Some(npc_registry(&[("NPCBoard", NPC_ACTOR_TYPE_BOARD)]));
        document.archive_edits.upsert_resource(
            STAGE_DIALOGUE_MESSAGE_PATH.to_vec(),
            StageResourceDocument::Message(stage_bmg("Retail message")),
        );

        let initial_index = document.build_dialogue_route_index().unwrap();
        let fallback = initial_index.variants_for_object("dormant-board")[0].clone();
        assert!(fallback.message.is_none());
        let mut authored = fallback.content.clone();
        authored.message = text("New board dialogue");
        document
            .set_dialogue_override(
                "dormant-board",
                fallback.key.clone(),
                DialogueEditScope::Instance,
                fallback.route_kind,
                fallback.condition_path,
                authored,
            )
            .unwrap();
        assert!(document.objects[0]
            .raw_param("name")
            .unwrap()
            .starts_with(GENERATED_DIALOGUE_RUNTIME_NAME_PREFIX));
        assert_eq!(
            document.dialogue_authoring.as_ref().unwrap().objects["dormant-board"]
                .prior_runtime_name
                .as_deref(),
            Some("Retail board name")
        );

        let rebuilt_index = document.build_dialogue_route_index().unwrap();
        let compiled = document
            .compile_dialogue_authoring(&rebuilt_index)
            .expect("generated fallback becomes a valid owned route after its first edit");
        assert!(compiled
            .stage_edits
            .resources
            .iter()
            .any(|edit| edit.raw_resource_path == GENERATED_DIALOGUE_SCRIPT_PATH));

        let mut duplicate = SceneObject::new("dormant-board-copy", "NPCBoard");
        duplicate.insert_source_raw_param("name", "Retail duplicate name");
        document.objects.push(duplicate);
        document
            .duplicate_dialogue_authoring("dormant-board", "dormant-board-copy")
            .unwrap();
        let duplicate_index = document.build_dialogue_route_index().unwrap();
        document
            .compile_dialogue_authoring(&duplicate_index)
            .expect("a duplicate inherits and compiles a generated-key board route");
    }

    #[test]
    fn generated_script_sets_once_then_waits_for_talk_mode_to_end() {
        let mut document = empty_document("authored");
        document
            .objects
            .push(SceneObject::new("new-npc", "NPCBoard"));
        document
            .initialize_dialogue_for_new_object("new-npc")
            .unwrap();
        let script =
            compile_generated_dialogue_script(&document, &[("new-npc".to_string(), 0x0001_0000)])
                .unwrap();
        let program = script.to_relocatable().unwrap();
        let setter_symbol = program
            .symbols
            .iter()
            .position(|symbol| symbol.name == "setTalkMsgID")
            .unwrap() as u32;
        let setter_calls = program
            .instructions
            .iter()
            .enumerate()
            .filter_map(|(index, instruction)| match instruction {
                SpcInstruction::Builtin { symbol_index, .. } if *symbol_index == setter_symbol => {
                    Some(index)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(setter_calls.len(), 1);
        assert!(matches!(
            program.instructions.get(setter_calls[0].saturating_sub(1)),
            Some(SpcInstruction::Int(1))
        ));
        let matched_jump = setter_calls[0] + 2;
        assert!(matches!(
            program.instructions.get(matched_jump),
            Some(SpcInstruction::Jump(_))
        ));
        let wait_target = program
            .instruction_relocations
            .iter()
            .find(|relocation| relocation.instruction_index == matched_jump)
            .unwrap()
            .target_instruction_index;
        assert!(program.instructions[wait_target..]
            .iter()
            .all(|instruction| !matches!(
                instruction,
                SpcInstruction::Builtin { symbol_index, .. } if *symbol_index == setter_symbol
            )));
        assert!(program.instruction_relocations.iter().any(|relocation| {
            relocation.instruction_index > wait_target
                && relocation.target_instruction_index == wait_target
        }));
        assert!(program.instruction_relocations.iter().any(|relocation| {
            relocation.instruction_index > wait_target
                && relocation.target_instruction_index < wait_target
                && matches!(
                    program.instructions[relocation.instruction_index],
                    SpcInstruction::JumpIfZero(_)
                )
        }));
    }

    #[test]
    fn unassigned_editor_placed_actor_gets_its_own_empty_generated_route() {
        let mut document = empty_document("authored");
        document.registry = Some(npc_registry(&[("NPCFixture", NPC_ACTOR_TYPE_BOARD)]));
        document.objects = vec![
            authored_dialogue_object("edited", "Shared preset name"),
            authored_dialogue_object("unassigned", "Shared preset name"),
        ];
        let edited_key = document
            .initialize_dialogue_for_new_object("edited")
            .unwrap();
        document
            .dialogue_authoring
            .as_mut()
            .unwrap()
            .objects
            .get_mut("edited")
            .unwrap()
            .overrides
            .iter_mut()
            .find(|override_| override_.key == edited_key)
            .unwrap()
            .content
            .message = text("Only the edited actor");

        let index = document.build_dialogue_route_index().unwrap();
        assert!(document.has_uninitialized_generated_dialogue(&index));
        let compiled = document.compile_dialogue_authoring(&index).unwrap();
        assert!(!document.has_uninitialized_generated_dialogue(&index));

        let authoring = document.dialogue_authoring.as_ref().unwrap();
        let edited = &authoring.objects["edited"];
        let unassigned = &authoring.objects["unassigned"];
        assert_eq!(edited.stable_allocations.len(), 1);
        assert_eq!(unassigned.stable_allocations.len(), 1);
        assert_ne!(
            edited.stable_allocations[0].message_index,
            unassigned.stable_allocations[0].message_index
        );
        assert!(unassigned.overrides[0].content.message.tokens.is_empty());
        assert_ne!(
            document.objects[0].raw_param("name"),
            document.objects[1].raw_param("name")
        );

        let bmg = compiled
            .stage_edits
            .resources
            .iter()
            .find_map(|edit| match &edit.document {
                StageResourceDocument::Message(bmg)
                    if edit.raw_resource_path == STAGE_DIALOGUE_MESSAGE_PATH =>
                {
                    Some(bmg)
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(bmg.entries.len(), 2);

        let script = compiled
            .stage_edits
            .resources
            .iter()
            .find_map(|edit| match &edit.document {
                StageResourceDocument::Script(script)
                    if edit.raw_resource_path == GENERATED_DIALOGUE_SCRIPT_PATH =>
                {
                    Some(script)
                }
                _ => None,
            })
            .unwrap()
            .to_relocatable()
            .unwrap();
        let setter = script
            .symbols
            .iter()
            .position(|symbol| symbol.name == "setTalkMsgID")
            .unwrap() as u32;
        let selected_messages = script
            .instructions
            .iter()
            .enumerate()
            .filter_map(|(index, instruction)| match instruction {
                SpcInstruction::Builtin {
                    symbol_index,
                    argument_count: 2,
                } if *symbol_index == setter => match script.instructions.get(index - 2) {
                    Some(SpcInstruction::Int(message_id)) => Some(*message_id as u32),
                    _ => None,
                },
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(selected_messages.len(), 2);
    }

    #[test]
    fn generated_script_rejects_unowned_collision() {
        let mut document = empty_document("authored");
        document
            .objects
            .push(SceneObject::new("new-npc", "NPCBoard"));
        let mut unowned = SpcRelocatableProgram::new(0);
        unowned.push_instruction(SpcInstruction::End);
        document.archive_edits.upsert_resource(
            GENERATED_DIALOGUE_SCRIPT_PATH.to_vec(),
            StageResourceDocument::Script(unowned.to_document().unwrap()),
        );

        let error =
            compile_generated_dialogue_script(&document, &[("new-npc".to_string(), 0x0001_0000)])
                .unwrap_err();
        assert!(error.to_string().contains("unowned script"));
    }

    #[test]
    fn generated_route_rejects_a_duplicate_exact_runtime_name() {
        let mut document = empty_document("authored");
        document
            .objects
            .push(SceneObject::new("new-npc", "NPCBoard"));
        document
            .objects
            .push(SceneObject::new("other-npc", "NPCBoard"));
        document
            .initialize_dialogue_for_new_object("new-npc")
            .unwrap();
        let generated_name = document.objects[0].raw_param("name").unwrap().to_string();
        document.objects[1].set_raw_param("name", generated_name);

        let index = document.build_dialogue_route_index().unwrap();
        assert!(index.issues.iter().any(|issue| {
            issue.code == "dialogue-generated-name-ambiguous"
                && issue.severity == DialogueResolutionSeverity::Error
        }));
        let error = document.compile_dialogue_authoring(&index).unwrap_err();
        assert!(error.to_string().contains("shared by 2 placed objects"));
    }

    #[test]
    fn dialogue_inheritance_cycles_are_detected_as_complete_graphs() {
        let mut document = empty_document("authored");
        for object_id in ["a", "b", "c"] {
            document
                .objects
                .push(SceneObject::new(object_id, "NPCBoard"));
        }
        document.dialogue_authoring = Some(DialogueAuthoringDocument {
            format_version: DIALOGUE_AUTHORING_FORMAT_VERSION,
            objects: BTreeMap::from([
                (
                    "a".to_string(),
                    DialogueObjectAuthoring {
                        inherited_from_object_id: Some("b".to_string()),
                        ..DialogueObjectAuthoring::default()
                    },
                ),
                (
                    "b".to_string(),
                    DialogueObjectAuthoring {
                        inherited_from_object_id: Some("a".to_string()),
                        ..DialogueObjectAuthoring::default()
                    },
                ),
                (
                    "c".to_string(),
                    DialogueObjectAuthoring {
                        inherited_from_object_id: Some("a".to_string()),
                        ..DialogueObjectAuthoring::default()
                    },
                ),
            ]),
        });

        let structural = validate_dialogue_document(&document);
        assert_eq!(
            structural
                .iter()
                .filter(|issue| issue.code == "dialogue-inheritance-cycle")
                .count(),
            1
        );
        let index = document.build_dialogue_route_index().unwrap();
        assert!(index
            .issues
            .iter()
            .any(|issue| issue.code == "dialogue-inheritance-cycle"));
        assert!(index
            .issues
            .iter()
            .any(|issue| issue.code == "dialogue-inherited-source-cycle"));
    }

    #[test]
    fn forced_only_source_gets_fallback_before_duplicate_inheritance() {
        let mut document = empty_document("forced-only");
        let mut original = SceneObject::new("original", "NPCMonteM");
        original.insert_source_raw_param("name", "Original NPC");
        let mut duplicate = SceneObject::new("duplicate", "NPCMonteM");
        duplicate.insert_source_raw_param("name", "Duplicate NPC");
        document.objects = vec![original, duplicate];
        document.registry = Some(npc_registry(&[("NPCMonteM", 0x0400_0001)]));
        document.dialogue_authoring = Some(DialogueAuthoringDocument {
            format_version: DIALOGUE_AUTHORING_FORMAT_VERSION,
            objects: BTreeMap::from([(
                "duplicate".to_string(),
                DialogueObjectAuthoring {
                    inherited_from_object_id: Some("original".to_string()),
                    ..DialogueObjectAuthoring::default()
                },
            )]),
        });
        let mut forced = consumer_variant("forced-only", "original", 0, 24);
        forced.route_kind = DialogueRouteKind::Forced;
        let mut index = DialogueRouteIndex::default();
        index
            .variants_by_object
            .insert("original".to_string(), vec![forced]);

        classify_unresolved_talk_capable_objects(&document, &mut index);
        apply_authored_and_inherited_routes(&document, &mut index);

        assert!(!index.has_errors(), "{:?}", index.issues);
        assert!(index
            .variants_for_object("original")
            .iter()
            .any(|variant| variant.route_kind == DialogueRouteKind::Forced));
        assert!(index
            .variants_for_object("original")
            .iter()
            .any(|variant| variant.provenance == DialogueProvenance::Generated));
        let inherited = index.variants_for_object("duplicate");
        assert_eq!(inherited.len(), 1);
        assert_eq!(inherited[0].provenance, DialogueProvenance::Generated);
    }

    #[test]
    fn happy_override_does_not_suppress_the_instance_normal_talk_fallback() {
        let mut document = empty_document("happy-only");
        document
            .objects
            .push(SceneObject::new("pianta", "NPCMonteM"));
        document.registry = Some(npc_registry(&[("NPCMonteM", 0x0400_0001)]));
        let mut happy = consumer_variant("happy-only", "pianta", 35, 24);
        happy.route_kind = DialogueRouteKind::HappyOverride;
        document.dialogue_authoring = Some(DialogueAuthoringDocument {
            format_version: DIALOGUE_AUTHORING_FORMAT_VERSION,
            objects: BTreeMap::from([(
                "pianta".to_string(),
                DialogueObjectAuthoring {
                    overrides: vec![DialogueVariantOverride {
                        key: happy.key.clone(),
                        scope: DialogueEditScope::Instance,
                        route_kind: DialogueRouteKind::HappyOverride,
                        condition_path: "Happy/reward flag set".to_string(),
                        content: happy.content.clone(),
                    }],
                    ..DialogueObjectAuthoring::default()
                },
            )]),
        });
        let mut index = DialogueRouteIndex::default();
        index
            .variants_by_object
            .insert("pianta".to_string(), vec![happy]);

        classify_unresolved_talk_capable_objects(&document, &mut index);

        let routes = index.variants_for_object("pianta");
        assert_eq!(routes.len(), 2);
        assert!(routes
            .iter()
            .any(|variant| variant.route_kind == DialogueRouteKind::HappyOverride));
        assert!(routes.iter().any(|variant| {
            variant.provenance == DialogueProvenance::Generated && variant.message.is_none()
        }));
    }

    #[test]
    fn hidden_dummy_proxy_never_receives_a_generated_instance_fallback() {
        let mut document = empty_document("dummy-proxy");
        document.objects.push(SceneObject::new("dummy", "NPCDummy"));
        document
            .objects
            .push(SceneObject::new("normal", "NPCBoard"));
        document.registry = Some(npc_registry(&[
            ("NPCDummy", 0x0400_001c),
            ("NPCBoard", NPC_ACTOR_TYPE_BOARD),
        ]));
        let mut index = DialogueRouteIndex::default();

        classify_unresolved_talk_capable_objects(&document, &mut index);

        assert!(index.variants_for_object("dummy").is_empty());
        assert_eq!(index.variants_for_object("normal").len(), 1);
        assert_eq!(
            index.variants_for_object("normal")[0].provenance,
            DialogueProvenance::Generated
        );
        assert!(index.issues.iter().any(|issue| {
            issue.code == "dialogue-dummy-proxy-classified"
                && issue.message.contains("connectDummyNpc")
        }));
    }

    #[test]
    fn dummy_proxy_authoring_is_rejected_at_initialization_and_export() {
        let mut document = empty_document("dummy-proxy");
        document.objects.push(SceneObject::new("dummy", "NPCDummy"));
        document.registry = Some(npc_registry(&[("NPCDummy", 0x0400_001c)]));

        let error = document
            .initialize_dialogue_for_new_object("dummy")
            .unwrap_err();
        assert!(error.to_string().contains("not eligible"));
        assert!(document.dialogue_authoring.is_none());

        document.dialogue_authoring = Some(DialogueAuthoringDocument {
            format_version: DIALOGUE_AUTHORING_FORMAT_VERSION,
            objects: BTreeMap::from([(
                "dummy".to_string(),
                DialogueObjectAuthoring {
                    overrides: vec![DialogueVariantOverride {
                        key: DialogueVariantKey::generated_for_object("dummy"),
                        scope: DialogueEditScope::Instance,
                        route_kind: DialogueRouteKind::Generated,
                        condition_path: "Invalid manual authoring".to_string(),
                        content: DialogueContent {
                            message: text("Should never export"),
                            authored_tokens: None,
                            attributes: vec![0; 8],
                            voice_index: Some(0),
                        },
                    }],
                    ..DialogueObjectAuthoring::default()
                },
            )]),
        });
        let issues = validate_dialogue_document(&document);
        assert!(issues
            .iter()
            .any(|issue| issue.code == "dialogue-object-ineligible"));
        let index = document
            .build_dialogue_route_index_with_common(&DialogueResourceSet::empty())
            .unwrap();
        let error = document.validate_dialogue_for_export(&index).unwrap_err();
        assert!(error.to_string().contains("dialogue-object-ineligible"));
    }

    #[test]
    fn fallback_pass_does_not_shadow_pending_authored_generated_content() {
        let mut document = empty_document("authored-fallback");
        document
            .objects
            .push(SceneObject::new("authored", "NPCBoard"));
        document.registry = Some(npc_registry(&[("NPCBoard", NPC_ACTOR_TYPE_BOARD)]));
        let key = DialogueVariantKey::generated_for_object("authored");
        document.dialogue_authoring = Some(DialogueAuthoringDocument {
            format_version: DIALOGUE_AUTHORING_FORMAT_VERSION,
            objects: BTreeMap::from([(
                "authored".to_string(),
                DialogueObjectAuthoring {
                    overrides: vec![DialogueVariantOverride {
                        key,
                        scope: DialogueEditScope::Instance,
                        route_kind: DialogueRouteKind::Generated,
                        condition_path: "Authored".to_string(),
                        content: DialogueContent {
                            message: text("Authored content"),
                            authored_tokens: None,
                            attributes: vec![0; 8],
                            voice_index: Some(0),
                        },
                    }],
                    ..DialogueObjectAuthoring::default()
                },
            )]),
        });
        let mut index = DialogueRouteIndex::default();

        classify_unresolved_talk_capable_objects(&document, &mut index);
        assert!(index.variants_for_object("authored").is_empty());
        apply_authored_and_inherited_routes(&document, &mut index);

        let variants = index.variants_for_object("authored");
        assert_eq!(variants.len(), 1);
        assert_eq!(variants[0].content.message, text("Authored content"));
    }

    #[test]
    #[ignore = "requires SMS_BASE_ROOT with extracted JP or US retail stages and the neighboring SMS decomp"]
    fn retail_named_dialogue_routes_match_jp_and_us_semantics() {
        let base_root = std::env::var_os("SMS_BASE_ROOT")
            .map(PathBuf::from)
            .expect("set SMS_BASE_ROOT to an extracted JP or US game root");
        let decomp_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let registry = sms_schema::SchemaGenerator::new(&decomp_root)
            .generate()
            .unwrap_or_else(|error| {
                panic!(
                    "generate named retail dialogue registry from neighboring decomp {}: {error}",
                    decomp_root.display()
                )
            });
        let archives = discover_scene_archives(&base_root)
            .expect("discover extracted retail stage archives for named dialogue acceptance");
        let common = collect_common_dialogue_resources(&base_root)
            .expect("collect common dialogue resources for named dialogue acceptance");

        let peach = "\u{30d4}\u{30fc}\u{30c1}\u{59eb}";
        let monte_a = "\u{30e2}\u{30f3}\u{30c6}\u{ff21}";
        let monte_d = "\u{30e2}\u{30f3}\u{30c6}\u{ff24}";

        let airport =
            retail_stage_dialogue_index(&base_root, &archives, &registry, &common, "airport0");
        assert_retail_route(
            &airport,
            Some(peach),
            b"map/sp/peachtalk.sb",
            0x0002_0000,
            DialogueRouteKind::Normal,
        );
        let airport_before = assert_retail_route(
            &airport,
            Some(monte_d),
            b"map/sp/peachtalk.sb",
            0x0002_000a,
            DialogueRouteKind::Normal,
        );
        assert!(airport_before.iter().any(|variant| variant
            .condition_path
            .contains("System flag 0x00050000 = 0")));
        let airport_after = assert_retail_route(
            &airport,
            Some(monte_d),
            b"map/sp/peachtalk.sb",
            0x0002_0017,
            DialogueRouteKind::Normal,
        );
        assert!(airport_after.iter().any(|variant| variant
            .condition_path
            .contains("System flag 0x00050000 != 0")));
        let general_happy = assert_retail_route(
            &airport,
            Some(monte_d),
            STOCK_HAPPY_DIALOGUE_SOURCE_PATH,
            0x23,
            DialogueRouteKind::HappyOverride,
        );
        assert!(general_happy.iter().all(|variant| {
            variant
                .message
                .as_ref()
                .is_some_and(|message| message.domain == DialogueDomain::System)
        }));
        assert_retail_route(
            &airport,
            Some("\u{7a7a}\u{6e2f}\u{6c88}\u{307f}\u{30e2}\u{30f3}\u{30c6}"),
            STOCK_HAPPY_DIALOGUE_SOURCE_PATH,
            0x26,
            DialogueRouteKind::HappyOverride,
        );

        // The retail delfino0 archive contains these three placed Pianta
        // routes. It does not contain a Peach route; Airport's Peach/Pianta
        // flag split above is therefore kept as a separate acceptance case.
        let delfino =
            retail_stage_dialogue_index(&base_root, &archives, &registry, &common, "delfino0");
        for (runtime_name, message_id) in [
            (monte_a, 0x0052_0000),
            ("\u{30e2}\u{30f3}\u{30c6}\u{ff2a}", 0x0052_0001),
            ("\u{30e2}\u{30f3}\u{30c6}\u{ff2b}", 0x0052_0002),
        ] {
            assert_retail_route(
                &delfino,
                Some(runtime_name),
                b"map/sp/talkevent.sb",
                message_id,
                DialogueRouteKind::Normal,
            );
        }

        let monte =
            retail_stage_dialogue_index(&base_root, &archives, &registry, &common, "monte3");
        for (message_id, expected_runtime_names) in [
            (0x002a_0020, 10usize),
            (0x002a_0021, 13),
            (0x002a_0022, 2),
            (0x002a_0023, 3),
        ] {
            let routes = assert_retail_route(
                &monte,
                None,
                b"map/sp/rescuemonte.sb",
                message_id,
                DialogueRouteKind::Normal,
            );
            let runtime_names = routes
                .iter()
                .map(|variant| variant.runtime_name.as_str())
                .collect::<BTreeSet<_>>();
            assert_eq!(
                runtime_names.len(),
                expected_runtime_names,
                "Monte Village shared response {message_id:#010x} consumers"
            );
        }
        let monte_board = assert_retail_route(
            &monte,
            None,
            b"map/sp/rescuemonte.sb",
            0x002a_0024,
            DialogueRouteKind::BoardOrSign,
        );
        assert_eq!(monte_board.len(), 2);
        assert_eq!(
            monte_board
                .iter()
                .map(|variant| variant.runtime_name.as_str())
                .collect::<BTreeSet<_>>()
                .len(),
            1
        );

        let pinna_beach =
            retail_stage_dialogue_index(&base_root, &archives, &registry, &common, "pinnaBeach0");
        assert_retail_route(
            &pinna_beach,
            Some("\u{3072}\u{307e}\u{308f}\u{308a}4"),
            b"map/sp/defaulttalk.sb",
            0x002f_0003,
            DialogueRouteKind::Normal,
        );
        for suffix in 0..=3 {
            let runtime_name = format!("\u{3072}\u{307e}\u{308f}\u{308a}{suffix}");
            assert_retail_route(
                &pinna_beach,
                Some(&runtime_name),
                b"map/sp/defaulttalk.sb",
                0x002f_0004,
                DialogueRouteKind::Normal,
            );
        }

        let pinna_parco =
            retail_stage_dialogue_index(&base_root, &archives, &registry, &common, "pinnaParco5");
        let attendant = "\u{4fc2}\u{54e1}\u{30de}\u{30fc}\u{30ec}";
        assert_retail_route(
            &pinna_parco,
            Some(attendant),
            b"map/sp/kakaritalk.sb",
            0x0043_0009,
            DialogueRouteKind::Normal,
        );
        let choice_zero = assert_retail_route(
            &pinna_parco,
            Some(attendant),
            b"map/sp/kakaritalk.sb",
            0x0043_000d,
            DialogueRouteKind::Choice,
        );
        assert!(choice_zero
            .iter()
            .any(|variant| variant.condition_path.contains("Selected choice = 0")));
        let choice_other = assert_retail_route(
            &pinna_parco,
            Some(attendant),
            b"map/sp/kakaritalk.sb",
            0x0043_000b,
            DialogueRouteKind::Choice,
        );
        assert!(choice_other
            .iter()
            .any(|variant| variant.condition_path.contains("Selected choice != 0")));
        assert_retail_route(
            &pinna_parco,
            Some(attendant),
            b"map/sp/kakaritalk.sb",
            0x0043_000c,
            DialogueRouteKind::Normal,
        );

        let sirena =
            retail_stage_dialogue_index(&base_root, &archives, &registry, &common, "sirena5");
        let mut sirena_conditions = BTreeSet::new();
        for message_id in [0x004c_0000, 0x004c_0007, 0x004c_0008, 0x004c_0009] {
            for variant in assert_retail_route(
                &sirena,
                Some(monte_a),
                b"map/sp/cleanpollution.sb",
                message_id,
                DialogueRouteKind::Normal,
            ) {
                sirena_conditions.insert(variant.condition_path.as_str());
            }
        }
        assert!(
            sirena_conditions.len() >= 2,
            "Sirena before/after/success routes collapsed to {sirena_conditions:?}"
        );

        let bianco =
            retail_stage_dialogue_index(&base_root, &archives, &registry, &common, "bianco0");
        let board = assert_retail_route(
            &bianco,
            None,
            b"map/sp/talkevent.sb",
            0x0021_000d,
            DialogueRouteKind::BoardOrSign,
        );
        assert!(board.iter().all(|variant| !variant.runtime_name.is_empty()));

        let mare = retail_stage_dialogue_index(&base_root, &archives, &registry, &common, "mare0");
        for message_id in [0x005a_0000, 0x005a_0001] {
            let forced = assert_retail_route(
                &mare,
                None,
                b"map/sp/mare0.sb",
                message_id,
                DialogueRouteKind::Forced,
            );
            assert!(forced
                .iter()
                .all(|variant| !variant.runtime_name.is_empty()));
        }

        let dolpic =
            retail_stage_dialogue_index(&base_root, &archives, &registry, &common, "dolpic1");
        let balloon_routes = dolpic
            .all_variants()
            .filter(|variant| {
                variant.runtime_name == peach
                    && variant.key.source.raw_resource_path == b"map/sp/gateopen.sb"
                    && variant.route_kind == DialogueRouteKind::Balloon
                    && variant.message.as_ref().is_some_and(|message| {
                        message.domain == DialogueDomain::Balloon
                            && message.raw_resource_path == BALLOON_DIALOGUE_MESSAGE_PATH
                    })
            })
            .collect::<Vec<_>>();
        assert_eq!(balloon_routes.len(), 2);
        let balloon_ids = balloon_routes
            .iter()
            .filter_map(|variant| {
                variant
                    .message
                    .as_ref()
                    .map(|message| message.full_message_id)
            })
            .collect::<BTreeSet<_>>();
        assert!(
            balloon_ids == BTreeSet::from([0x000e_003b, 0x000e_0050])
                || balloon_ids == BTreeSet::from([0x0000_003e, 0x0000_0053]),
            "unexpected JP/US balloon routes {balloon_ids:?}"
        );
    }

    #[test]
    #[ignore = "requires SMS_BASE_ROOT with extracted retail stages and the neighboring SMS decomp"]
    fn retail_dialogue_route_census_has_no_ambiguous_or_error_sites() {
        let base_root = std::env::var_os("SMS_BASE_ROOT")
            .map(PathBuf::from)
            .expect("set SMS_BASE_ROOT to an extracted JP or US game root");
        let decomp_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let registry = sms_schema::SchemaGenerator::new(&decomp_root)
            .generate()
            .unwrap_or_else(|error| {
                panic!(
                    "generate dialogue census registry from neighboring decomp {}: {error}",
                    decomp_root.display()
                )
            });
        let archives = discover_scene_archives(&base_root)
            .expect("discover extracted retail stage archives for dialogue census");
        assert!(
            !archives.is_empty(),
            "retail dialogue census found no stages"
        );
        let common = collect_common_dialogue_resources(&base_root)
            .expect("collect common dialogue resources once for retail census");

        let mut stage_count = 0usize;
        let mut variant_count = 0usize;
        let mut callsite_count = 0usize;
        let mut resolved_count = 0usize;
        let mut dynamic_helper_count = 0usize;
        let mut global_or_unplaced_count = 0usize;
        let mut ambiguous_count = 0usize;
        let mut warning_count = 0usize;
        let mut failures = Vec::new();
        let stage_filter = std::env::var("SMS_DIALOGUE_CENSUS_STAGE").ok();
        let verbose = std::env::var_os("SMS_DIALOGUE_CENSUS_VERBOSE").is_some();

        for archive in archives {
            if stage_filter
                .as_ref()
                .is_some_and(|filter| archive.stage_id != *filter)
            {
                continue;
            }
            let assets = match mount_scene_archive(&archive.path) {
                Ok(assets) => assets,
                Err(error) => {
                    failures.push(format!(
                        "{}: archive mount failed: {error}",
                        archive.stage_id
                    ));
                    continue;
                }
            };
            let (objects, load_issues, lighting) = crate::load_scene_objects_from_assets(&assets);
            let mut document = empty_document(&archive.stage_id);
            document.base_root = base_root.clone();
            document.assets = assets;
            document.objects = objects;
            document.load_issues = load_issues;
            document.lighting = lighting;
            // The census needs registry identity only. Avoid building preview
            // geometry and importing a full semantic archive for every retail
            // stage merely to classify NPC routes.
            document.registry = Some(registry.clone());
            let index = match document.build_dialogue_route_index_with_common(&common) {
                Ok(index) => index,
                Err(error) => {
                    failures.push(format!("{}: route index failed: {error}", archive.stage_id));
                    continue;
                }
            };

            stage_count += 1;
            variant_count += index.all_variants().count();
            callsite_count += index.callsites.len();
            if verbose {
                for variant in index.all_variants() {
                    println!(
                        "{}: variant {:?} object={} runtime={:?} source={} message={:?}",
                        archive.stage_id,
                        variant.route_kind,
                        variant.object_id,
                        variant.runtime_name,
                        String::from_utf8_lossy(&variant.key.source.raw_resource_path),
                        variant
                            .message
                            .as_ref()
                            .map(|message| message.full_message_id)
                    );
                }
            }
            for callsite in &index.callsites {
                match callsite.status {
                    DialogueCallsiteStatus::Resolved => resolved_count += 1,
                    DialogueCallsiteStatus::DynamicHelper => dynamic_helper_count += 1,
                    DialogueCallsiteStatus::GlobalOrUnplaced => global_or_unplaced_count += 1,
                    DialogueCallsiteStatus::Ambiguous => {
                        ambiguous_count += 1;
                        failures.push(format!(
                            "{}: ambiguous {} call in {} function {}",
                            archive.stage_id,
                            callsite.setter_symbol,
                            String::from_utf8_lossy(&callsite.script_path),
                            callsite.function_symbol
                        ));
                    }
                }
                if verbose && callsite.status != DialogueCallsiteStatus::Resolved {
                    println!(
                        "{}: {:?} {} in {} function {}",
                        archive.stage_id,
                        callsite.status,
                        callsite.setter_symbol,
                        String::from_utf8_lossy(&callsite.script_path),
                        callsite.function_symbol
                    );
                }
            }
            for issue in &index.issues {
                match issue.severity {
                    DialogueResolutionSeverity::Warning => warning_count += 1,
                    DialogueResolutionSeverity::Error => failures.push(format!(
                        "{}: {}: {}",
                        archive.stage_id, issue.code, issue.message
                    )),
                }
                if verbose {
                    println!(
                        "{}: {:?} {}: {}",
                        archive.stage_id, issue.severity, issue.code, issue.message
                    );
                }
            }
        }

        println!(
            "dialogue route census root={} stages={} variants={} callsites={} resolved={} dynamic_helpers={} global_or_unplaced={} ambiguous={} warnings={} errors={}",
            base_root.display(),
            stage_count,
            variant_count,
            callsite_count,
            resolved_count,
            dynamic_helper_count,
            global_or_unplaced_count,
            ambiguous_count,
            warning_count,
            failures.len()
        );
        assert!(
            failures.is_empty(),
            "retail dialogue census found {} blocking failures:\n{}",
            failures.len(),
            failures.join("\n")
        );
        // The current corpora resolve 1,963 JP callsites and 1,968 US
        // callsites (US includes five finite branch definitions that a linear
        // walk used to collapse). Keep a regional floor so a control-flow
        // regression cannot relabel most known routes as merely global while
        // still reporting zero errors.
        if stage_filter.is_none() {
            assert!(
                resolved_count >= 1_900,
                "retail dialogue census precision collapsed to {resolved_count} resolved callsites"
            );
            assert!(
                variant_count >= 3_200,
                "retail dialogue census precision collapsed to {variant_count} placed variants"
            );
        }
    }
}
