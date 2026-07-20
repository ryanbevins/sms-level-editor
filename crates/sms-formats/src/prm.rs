use serde::{Deserialize, Serialize};

use crate::{FormatError, Result};

const FORMAT: &str = "SMS parameter file";
const MAX_ENTRIES: usize = 65_536;

/// A decoded SMS parameter value.
///
/// Floating-point values retain their IEEE-754 bit patterns so NaNs and signed
/// zero rebuild exactly without retaining their source bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrmValue {
    U8(u8),
    I16(i16),
    I32(i32),
    F32Bits(u32),
    Vec3F32Bits([u32; 3]),
    /// Four creator-authored on/off slots used by mBirthSwitch.
    U8Array4([u8; 4]),
    /// Four creator-authored floating-point slots used by mBirthRate.
    F32Array4Bits([u32; 4]),
    /// The creator-only `mBirthMax` marker: four required zero words.
    ///
    /// The game does not register this parameter and every retail instance is
    /// sixteen zero bytes. No arbitrary slot payload is retained or accepted.
    BirthMaxZeroSlots,
}

impl PrmValue {
    pub fn from_f32(value: f32) -> Self {
        Self::F32Bits(value.to_bits())
    }

    pub fn from_vec3_f32(value: [f32; 3]) -> Self {
        Self::Vec3F32Bits(value.map(f32::to_bits))
    }

    pub fn as_f32(&self) -> Option<f32> {
        match self {
            Self::F32Bits(bits) => Some(f32::from_bits(*bits)),
            _ => None,
        }
    }

    pub fn as_vec3_f32(&self) -> Option<[f32; 3]> {
        match self {
            Self::Vec3F32Bits(bits) => Some(bits.map(f32::from_bits)),
            _ => None,
        }
    }

    fn kind(&self) -> PrmValueKind {
        match self {
            Self::U8(_) => PrmValueKind::U8,
            Self::I16(_) => PrmValueKind::I16,
            Self::I32(_) => PrmValueKind::I32,
            Self::F32Bits(_) => PrmValueKind::F32,
            Self::Vec3F32Bits(_) => PrmValueKind::Vec3F32,
            Self::U8Array4(_) => PrmValueKind::U8Array4,
            Self::F32Array4Bits(_) => PrmValueKind::F32Array4,
            Self::BirthMaxZeroSlots => PrmValueKind::BirthMaxZeroSlots,
        }
    }

    fn decode(kind: PrmValueKind, bytes: &[u8], name: &str) -> Result<Self> {
        if bytes.len() != kind.encoded_len() {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: format!(
                    "parameter {name:?} is declared as {}, which requires {} byte(s), but the file contains {}",
                    kind.label(),
                    kind.encoded_len(),
                    bytes.len()
                ),
            });
        }

        Ok(match kind {
            PrmValueKind::U8 => Self::U8(bytes[0]),
            PrmValueKind::I16 => Self::I16(i16::from_be_bytes(
                bytes.try_into().expect("checked length"),
            )),
            PrmValueKind::I32 => Self::I32(i32::from_be_bytes(
                bytes.try_into().expect("checked length"),
            )),
            PrmValueKind::F32 => Self::F32Bits(u32::from_be_bytes(
                bytes.try_into().expect("checked length"),
            )),
            PrmValueKind::Vec3F32 => {
                Self::Vec3F32Bits(decode_u32_array(bytes).expect("checked length"))
            }
            PrmValueKind::U8Array4 => Self::U8Array4(bytes.try_into().expect("checked length")),
            PrmValueKind::F32Array4 => {
                Self::F32Array4Bits(decode_u32_array(bytes).expect("checked length"))
            }
            PrmValueKind::BirthMaxZeroSlots => {
                if bytes.iter().any(|byte| *byte != 0) {
                    return Err(FormatError::Unsupported {
                        format: FORMAT,
                        message: format!(
                            "parameter {name:?} is a creator-only marker and must contain sixteen zero bytes"
                        ),
                    });
                }
                Self::BirthMaxZeroSlots
            }
        })
    }

    fn append_encoded(&self, bytes: &mut Vec<u8>) {
        match self {
            Self::U8(value) => bytes.push(*value),
            Self::I16(value) => bytes.extend_from_slice(&value.to_be_bytes()),
            Self::I32(value) => bytes.extend_from_slice(&value.to_be_bytes()),
            Self::F32Bits(bits) => bytes.extend_from_slice(&bits.to_be_bytes()),
            Self::Vec3F32Bits(values) => {
                for bits in values {
                    bytes.extend_from_slice(&bits.to_be_bytes());
                }
            }
            Self::F32Array4Bits(values) => {
                for bits in values {
                    bytes.extend_from_slice(&bits.to_be_bytes());
                }
            }
            Self::U8Array4(values) => bytes.extend_from_slice(values),
            Self::BirthMaxZeroSlots => bytes.extend_from_slice(&[0; 16]),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrmEntry {
    pub name: String,
    pub value: PrmValue,
}

impl PrmEntry {
    pub fn new(name: impl Into<String>, value: PrmValue) -> Result<Self> {
        let entry = Self {
            name: name.into(),
            value,
        };
        entry.validate()?;
        Ok(entry)
    }

    /// Returns the deterministic JDrama key code derived from the name.
    pub fn key_code(&self) -> u16 {
        prm_key_code(&self.name)
    }

    fn validate(&self) -> Result<()> {
        let expected = prm_value_kind(&self.name).ok_or_else(|| FormatError::Unsupported {
            format: FORMAT,
            message: format!(
                "parameter {:?} has no decomp/corpus-backed semantic schema",
                self.name
            ),
        })?;
        let actual = self.value.kind();
        if actual != expected {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: format!(
                    "parameter {:?} requires {}, but the document contains {}",
                    self.name,
                    expected.label(),
                    actual.label()
                ),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrmFile {
    pub entries: Vec<PrmEntry>,
}

impl PrmFile {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 4 {
            return Err(FormatError::TooSmall {
                format: FORMAT,
                expected: 4,
                actual: bytes.len(),
            });
        }
        let entry_count = read_u32(bytes, 0)? as usize;
        if entry_count > MAX_ENTRIES {
            return Err(FormatError::ResourceLimit {
                format: FORMAT,
                resource: "entries",
                requested: entry_count,
                limit: MAX_ENTRIES,
            });
        }

        let mut offset = 4usize;
        let mut entries = Vec::with_capacity(entry_count);
        for _ in 0..entry_count {
            let stored_key_code = read_u16(bytes, offset)?;
            let name_len = read_u16(bytes, offset + 2)? as usize;
            offset = checked_add(offset, 4, bytes.len())?;
            let name_bytes = checked_slice(bytes, offset, name_len)?;
            let name = std::str::from_utf8(name_bytes)
                .map_err(|error| FormatError::Unsupported {
                    format: FORMAT,
                    message: format!("parameter name at {offset:#x} is not UTF-8: {error}"),
                })?
                .to_string();
            offset = checked_add(offset, name_len, bytes.len())?;

            let expected_key_code = prm_key_code(&name);
            if stored_key_code != expected_key_code {
                return Err(FormatError::Unsupported {
                    format: FORMAT,
                    message: format!(
                        "parameter {name:?} stores key code {stored_key_code:#06x}, but its derived JDrama key is {expected_key_code:#06x}"
                    ),
                });
            }

            let value_len = read_u32(bytes, offset)? as usize;
            offset = checked_add(offset, 4, bytes.len())?;
            let value_bytes = checked_slice(bytes, offset, value_len)?;
            let kind = prm_value_kind(&name).ok_or_else(|| FormatError::Unsupported {
                format: FORMAT,
                message: format!("parameter {name:?} has no decomp/corpus-backed semantic schema"),
            })?;
            let value = PrmValue::decode(kind, value_bytes, &name)?;
            offset = checked_add(offset, value_len, bytes.len())?;
            entries.push(PrmEntry { name, value });
        }

        if offset != bytes.len() {
            return Err(FormatError::Unsupported {
                format: FORMAT,
                message: format!(
                    "{} trailing byte(s) remain after the declared parameter entries",
                    bytes.len() - offset
                ),
            });
        }
        Ok(Self { entries })
    }

    pub fn value(&self, name: &str) -> Option<&PrmValue> {
        self.entries
            .iter()
            .find(|entry| entry.name == name)
            .map(|entry| &entry.value)
    }

    pub fn value_mut(&mut self, name: &str) -> Option<&mut PrmValue> {
        self.entries
            .iter_mut()
            .find(|entry| entry.name == name)
            .map(|entry| &mut entry.value)
    }

    pub fn f32(&self, name: &str) -> Option<f32> {
        self.value(name)?.as_f32()
    }

    /// Rebuilds the stream only from typed semantic entries.
    ///
    /// Key codes are recomputed from names. Source buffers, raw value payloads,
    /// and opaque trailing data are neither stored nor accepted.
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.entries.len() > MAX_ENTRIES {
            return Err(FormatError::ResourceLimit {
                format: FORMAT,
                resource: "entries",
                requested: self.entries.len(),
                limit: MAX_ENTRIES,
            });
        }
        let entry_count =
            u32::try_from(self.entries.len()).map_err(|_| FormatError::ResourceLimit {
                format: FORMAT,
                resource: "entries",
                requested: self.entries.len(),
                limit: u32::MAX as usize,
            })?;

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&entry_count.to_be_bytes());
        for entry in &self.entries {
            entry.validate()?;
            let name = entry.name.as_bytes();
            let name_len = u16::try_from(name.len()).map_err(|_| FormatError::ResourceLimit {
                format: FORMAT,
                resource: "parameter name bytes",
                requested: name.len(),
                limit: u16::MAX as usize,
            })?;
            let value_len = entry.value.kind().encoded_len() as u32;
            bytes.extend_from_slice(&prm_key_code(&entry.name).to_be_bytes());
            bytes.extend_from_slice(&name_len.to_be_bytes());
            bytes.extend_from_slice(name);
            bytes.extend_from_slice(&value_len.to_be_bytes());
            entry.value.append_encoded(&mut bytes);
        }
        Ok(bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrmValueKind {
    U8,
    I16,
    I32,
    F32,
    Vec3F32,
    U8Array4,
    F32Array4,
    BirthMaxZeroSlots,
}

impl PrmValueKind {
    const fn encoded_len(self) -> usize {
        match self {
            Self::U8 => 1,
            Self::I16 => 2,
            Self::I32 | Self::F32 | Self::U8Array4 => 4,
            Self::Vec3F32 => 12,
            Self::F32Array4 | Self::BirthMaxZeroSlots => 16,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::U8 => "u8",
            Self::I16 => "s16",
            Self::I32 => "s32",
            Self::F32 => "f32",
            Self::Vec3F32 => "TVec3<f32>",
            Self::U8Array4 => "u8[4]",
            Self::F32Array4 => "f32[4]",
            Self::BirthMaxZeroSlots => "zero-only mBirthMax creator marker",
        }
    }
}

// @generated from decomp TParam declarations and the complete Japanese retail
// stage corpus (159 files, 521 unique names). Regenerate instead of editing an
// entry by hand; the ignored corpus census below is the acceptance check.
/// Exact semantic schemas observed across all 159 Japanese retail stage PRMs.
///
/// Most types come directly from TParamRT declarations in the SMS decomp. The
/// remaining incomplete-decomp names were classified from their repeated
/// retail encodings and neighboring parameters. Unknown names are rejected.
const PRM_VALUE_SCHEMA: &[(&str, PrmValueKind)] = &[
    ("mAirFric", PrmValueKind::F32),
    ("mAlive", PrmValueKind::I16),
    ("mAppearDist", PrmValueKind::F32),
    ("mAroundDist", PrmValueKind::F32),
    ("mAroundSpeed", PrmValueKind::F32),
    ("mAroundTime", PrmValueKind::I32),
    ("mAttack", PrmValueKind::I16),
    ("mAttackSpeed", PrmValueKind::F32),
    ("mBirthMax", PrmValueKind::BirthMaxZeroSlots),
    ("mBirthRate", PrmValueKind::F32Array4),
    ("mBirthSwitch", PrmValueKind::U8Array4),
    ("mCarryFlag", PrmValueKind::U8),
    ("mCleanSize", PrmValueKind::I16),
    ("mDicideTiming", PrmValueKind::F32),
    ("mDir", PrmValueKind::Vec3F32),
    ("mDirTremble", PrmValueKind::F32),
    ("mDownTime", PrmValueKind::I16),
    ("mGA", PrmValueKind::F32),
    ("mGroundChangeY", PrmValueKind::F32),
    ("mHitHeight", PrmValueKind::F32),
    ("mHitRadius", PrmValueKind::F32),
    ("mInHouseMinFrame", PrmValueKind::U8),
    ("mInvincibleFlag", PrmValueKind::U8),
    ("mJumpFlag", PrmValueKind::U8),
    ("mLButtonRotateChase", PrmValueKind::I16),
    ("mLostOffsetYDown", PrmValueKind::F32),
    ("mLostOffsetYUp", PrmValueKind::F32),
    ("mNum", PrmValueKind::I32),
    ("mPanAfterMagnif", PrmValueKind::F32),
    ("mPanAfterMinHeight", PrmValueKind::F32),
    ("mPanWarpAngleX", PrmValueKind::I16),
    ("mPolluteFlag", PrmValueKind::U8),
    ("mPolluteSize", PrmValueKind::F32),
    ("mPos", PrmValueKind::Vec3F32),
    ("mPoseOmegaRate", PrmValueKind::F32),
    ("mPoseSpeed", PrmValueKind::F32),
    ("mPoseTime", PrmValueKind::I32),
    ("mPow", PrmValueKind::F32),
    ("mPowTremble", PrmValueKind::F32),
    ("mRandomFlag", PrmValueKind::U8),
    ("mRandomPow", PrmValueKind::F32),
    ("mReadFlag", PrmValueKind::U8),
    ("mResetTime", PrmValueKind::I32),
    ("mSearchDist", PrmValueKind::F32),
    ("mSearchHeight", PrmValueKind::F32),
    ("mSize", PrmValueKind::F32),
    ("mSizeTremble", PrmValueKind::F32),
    ("mSLAbilityBase", PrmValueKind::F32),
    ("mSLActionDelayInterval", PrmValueKind::I32),
    ("mSLActiveEnemyNum", PrmValueKind::U8),
    ("mSLAddAngleYSpeed", PrmValueKind::I16),
    ("mSLAddPosBase", PrmValueKind::F32),
    ("mSLAliveTime", PrmValueKind::I32),
    ("mSLAmputeeTime", PrmValueKind::I32),
    ("mSLAmputeeWait", PrmValueKind::I32),
    ("mSLAnmBlendTime0", PrmValueKind::I32),
    ("mSLAppearDist", PrmValueKind::F32),
    ("mSLAppearTime", PrmValueKind::I32),
    ("mSLAtChaseRateXZ", PrmValueKind::F32),
    ("mSLAtChaseRateY", PrmValueKind::F32),
    ("mSLAtJumpOffsetSpeed", PrmValueKind::F32),
    ("mSLAtOffsetY", PrmValueKind::F32),
    ("mSLAttackDist", PrmValueKind::F32),
    ("mSLAttackGravity", PrmValueKind::F32),
    ("mSLAttackHeight", PrmValueKind::I32),
    ("mSLAttackingTime", PrmValueKind::I32),
    ("mSLAttackInterval", PrmValueKind::I32),
    ("mSLAttackJumpSp", PrmValueKind::F32),
    ("mSLAttackJumpVy", PrmValueKind::F32),
    ("mSLAttackRadius", PrmValueKind::I32),
    ("mSLAttackWait", PrmValueKind::I32),
    ("mSLAutoChaseCompleteFrame", PrmValueKind::I32),
    ("mSLAutoChaseStartFrame", PrmValueKind::I32),
    ("mSLBackThrowVal", PrmValueKind::F32),
    ("mSLBankLimit", PrmValueKind::F32),
    ("mSLBankProp", PrmValueKind::F32),
    ("mSLBckMoveSpeed", PrmValueKind::F32),
    ("mSLBeakDamageHeight", PrmValueKind::F32),
    ("mSLBeakDamageRadius", PrmValueKind::F32),
    ("mSLBeakHoming", PrmValueKind::I32),
    ("mSLBeakLengthDamage", PrmValueKind::F32),
    ("mSLBeakLengthLimit", PrmValueKind::F32),
    ("mSLBeakLengthPollute", PrmValueKind::F32),
    ("mSLBeakStretch", PrmValueKind::F32),
    ("mSLBloomTimer", PrmValueKind::I32),
    ("mSLBlurJoint", PrmValueKind::I32),
    ("mSLBlurScale", PrmValueKind::F32),
    ("mSLBodyAngMax", PrmValueKind::F32),
    ("mSLBodyHitH", PrmValueKind::F32),
    ("mSLBodyHitH0", PrmValueKind::F32),
    ("mSLBodyHitR", PrmValueKind::F32),
    ("mSLBodyHitR0", PrmValueKind::F32),
    ("mSLBodyRadius", PrmValueKind::F32),
    ("mSLBodyScale", PrmValueKind::F32),
    ("mSLBodyScaleHigh", PrmValueKind::F32),
    ("mSLBodyScaleLow", PrmValueKind::F32),
    ("mSLBodyWallRadius", PrmValueKind::F32),
    ("mSLBombDist", PrmValueKind::F32),
    ("mSLBombHeiGenerateRate", PrmValueKind::F32),
    ("mSLBombInterval", PrmValueKind::I32),
    ("mSLBombRange", PrmValueKind::F32),
    ("mSLBombTime", PrmValueKind::I32),
    ("mSLBossAppear", PrmValueKind::F32),
    ("mSLBossFirstAppear", PrmValueKind::F32),
    ("mSLBossFirstAppearTimeMax", PrmValueKind::I32),
    ("mSLBossGravity", PrmValueKind::F32),
    ("mSLBossHideTimeMax", PrmValueKind::I32),
    ("mSLBoundNum", PrmValueKind::I32),
    ("mSLBoundVYMax", PrmValueKind::F32),
    ("mSLBudDist", PrmValueKind::F32),
    ("mSLCanSearchDist", PrmValueKind::F32),
    ("mSLCarapaceGravity", PrmValueKind::F32),
    ("mSLCarapaceSpeed", PrmValueKind::F32),
    ("mSLCarapaceSpinSpeed", PrmValueKind::F32),
    ("mSLCarapaceTurnSpeed", PrmValueKind::F32),
    ("mSLChangeTime", PrmValueKind::I32),
    ("mSLChorobeiAttackHeight", PrmValueKind::F32),
    ("mSLChorobeiAttackRadius", PrmValueKind::F32),
    ("mSLChorobeiDamageHeight", PrmValueKind::F32),
    ("mSLChorobeiDamageRadius", PrmValueKind::F32),
    ("mSLClipDistance", PrmValueKind::F32),
    ("mSLClipRadius", PrmValueKind::F32),
    ("mSLClipRectHeight", PrmValueKind::F32),
    ("mSLClipRectWidth", PrmValueKind::F32),
    ("mSLCoinCircleR", PrmValueKind::F32),
    ("mSLCoinVelocityXZ", PrmValueKind::F32),
    ("mSLCoinVelocityY", PrmValueKind::F32),
    ("mSLCollapseSize", PrmValueKind::F32),
    ("mSLCollapseStart", PrmValueKind::F32),
    ("mSLColorChangeRate", PrmValueKind::I32),
    ("mSLColumnScale", PrmValueKind::F32),
    ("mSLCombineInterval", PrmValueKind::I32),
    ("mSLCombineIntervalLimit", PrmValueKind::I32),
    ("mSLCrashBonusNum", PrmValueKind::I32),
    ("mSLCushionMax", PrmValueKind::F32),
    ("mSLCushionMin", PrmValueKind::F32),
    ("mSLDamageAngle", PrmValueKind::F32),
    ("mSLDamageHeadScale", PrmValueKind::F32),
    ("mSLDamageHeight", PrmValueKind::I32),
    ("mSLDamageRadius", PrmValueKind::I32),
    ("mSLDamageTimer", PrmValueKind::I32),
    ("mSLDashSpeed", PrmValueKind::F32),
    ("mSLDeadHeight", PrmValueKind::F32),
    ("mSLDistMax", PrmValueKind::F32),
    ("mSLDistMin", PrmValueKind::F32),
    ("mSLDoubleAttackLen", PrmValueKind::F32),
    ("mSLDoubleAttackSpeed", PrmValueKind::F32),
    ("mSLDropGravityY", PrmValueKind::F32),
    ("mSLDryTimer", PrmValueKind::I32),
    ("mSLEnemyGenerateInterval", PrmValueKind::I32),
    ("mSLEnemyGenerateNum", PrmValueKind::I32),
    ("mSLExpandMax", PrmValueKind::F32),
    ("mSLExpandRate", PrmValueKind::F32),
    ("mSLExplosionEmitTime", PrmValueKind::I32),
    ("mSLEyeDamageHeight", PrmValueKind::F32),
    ("mSLEyeDamageRadius", PrmValueKind::F32),
    ("mSLFadeInTime", PrmValueKind::I32),
    ("mSLFadeOutTime", PrmValueKind::I32),
    ("mSLFarClip", PrmValueKind::F32),
    ("mSLFirstDelayInterval", PrmValueKind::I32),
    ("mSLFirstKickVelocityY", PrmValueKind::F32),
    ("mSLFirstVelocityY", PrmValueKind::F32),
    ("mSLFlatScaleY", PrmValueKind::F32),
    ("mSLFlyAmplitudeMax", PrmValueKind::F32),
    ("mSLFlyAmplitudeMin", PrmValueKind::F32),
    ("mSLFlyBaseAmplitude", PrmValueKind::F32),
    ("mSLFlyBaseFrequency", PrmValueKind::F32),
    ("mSLFlyBaseHeight", PrmValueKind::F32),
    ("mSLFlyFrequencyMax", PrmValueKind::F32),
    ("mSLFlyFrequencyMin", PrmValueKind::F32),
    ("mSLFlyGravity", PrmValueKind::F32),
    ("mSLFlyHeightMax", PrmValueKind::F32),
    ("mSLFlyHeightMin", PrmValueKind::F32),
    ("mSLFlyLimitTime", PrmValueKind::I32),
    ("mSLFlySpeed", PrmValueKind::F32),
    ("mSLFlyTimer", PrmValueKind::I32),
    ("mSLFollowSpeedXmax", PrmValueKind::F32),
    ("mSLFollowSpeedXmin", PrmValueKind::F32),
    ("mSLForceUnisonLen", PrmValueKind::F32),
    ("mSLFovy", PrmValueKind::F32),
    ("mSLFreezeTimerLv0", PrmValueKind::I32),
    ("mSLFreezeWait", PrmValueKind::I32),
    ("mSLGA", PrmValueKind::F32),
    ("mSLGatherMax", PrmValueKind::I32),
    ("mSLGenAttackerTime", PrmValueKind::I32),
    ("mSLGenEggRate", PrmValueKind::F32),
    ("mSLGenerateInterval", PrmValueKind::I32),
    ("mSLGenerateOnlyDead", PrmValueKind::U8),
    ("mSLGenerateRootTime", PrmValueKind::I32),
    ("mSLGenerateSeedTime", PrmValueKind::I32),
    ("mSLGenItemRate", PrmValueKind::F32),
    ("mSLGeroLength", PrmValueKind::F32),
    ("mSLGiveUpHeight", PrmValueKind::F32),
    ("mSLGiveUpLength", PrmValueKind::F32),
    ("mSLGravityY", PrmValueKind::F32),
    ("mSLGroundHeightNormal", PrmValueKind::F32),
    ("mSLGroundHeightReadyGun", PrmValueKind::F32),
    ("mSLGroundOffsetY", PrmValueKind::F32),
    ("mSLGuardLen", PrmValueKind::F32),
    ("mSLHeadAttackHeight", PrmValueKind::F32),
    ("mSLHeadAttackRadius", PrmValueKind::F32),
    ("mSLHeadDamageHeight", PrmValueKind::F32),
    ("mSLHeadDamageRadius", PrmValueKind::F32),
    ("mSLHeadHeight", PrmValueKind::F32),
    ("mSLHeadHitH", PrmValueKind::F32),
    ("mSLHeadHitR", PrmValueKind::F32),
    ("mSLHeadHomingLimit", PrmValueKind::F32),
    ("mSLHeightPanChaseRateY", PrmValueKind::F32),
    ("mSLHideDist", PrmValueKind::F32),
    ("mSLHitJumpGravity", PrmValueKind::F32),
    ("mSLHitJumpSpRateXZ", PrmValueKind::F32),
    ("mSLHitJumpSpRateY", PrmValueKind::F32),
    ("mSLHitJumpSpXZ", PrmValueKind::F32),
    ("mSLHitJumpSpY", PrmValueKind::F32),
    ("mSLHitPointMax", PrmValueKind::U8),
    ("mSLHitPointMaxLv0", PrmValueKind::U8),
    ("mSLHitPointMaxLv1", PrmValueKind::U8),
    ("mSLHitPointMaxLv2", PrmValueKind::U8),
    ("mSLHitWaterSpXZ", PrmValueKind::F32),
    ("mSLHitWaterSpY", PrmValueKind::F32),
    ("mSLHoldAddDistXZMax", PrmValueKind::F32),
    ("mSLHoldAddDistXZMin", PrmValueKind::F32),
    ("mSLHoldAngleXChase", PrmValueKind::I16),
    ("mSLHoldDistChase", PrmValueKind::F32),
    ("mSLHoldOffsetAngleXMax", PrmValueKind::I16),
    ("mSLHoldOffsetAngleXMin", PrmValueKind::I16),
    ("mSLHoverTimer", PrmValueKind::I32),
    ("mSLInbetBossGeso", PrmValueKind::I16),
    ("mSLInbetCancan", PrmValueKind::I16),
    ("mSLInbetClimb", PrmValueKind::I16),
    ("mSLInbetClimbJump", PrmValueKind::I16),
    ("mSLInbetDefiniteA", PrmValueKind::I16),
    ("mSLInbetDefiniteB", PrmValueKind::I16),
    ("mSLInbetDefiniteC", PrmValueKind::I16),
    ("mSLInbetDefiniteD", PrmValueKind::I16),
    ("mSLInbetDefiniteD2", PrmValueKind::I16),
    ("mSLInbetDefiniteE", PrmValueKind::I16),
    ("mSLInbetDefiniteF", PrmValueKind::I16),
    ("mSLInbetDefiniteG", PrmValueKind::I16),
    ("mSLInbetDefiniteH", PrmValueKind::I16),
    ("mSLInbetDefiniteI", PrmValueKind::I16),
    ("mSLInbetDelfino", PrmValueKind::I16),
    ("mSLInbetDelfinoAttic", PrmValueKind::I16),
    ("mSLInbetDelfinoB", PrmValueKind::I16),
    ("mSLInbetDiving", PrmValueKind::I16),
    ("mSLInbetExMap0", PrmValueKind::I16),
    ("mSLInbetFence", PrmValueKind::I16),
    ("mSLInbetFixA", PrmValueKind::I16),
    ("mSLInbetFixB", PrmValueKind::I16),
    ("mSLInbetFixC", PrmValueKind::I16),
    ("mSLInbetFixD", PrmValueKind::I16),
    ("mSLInbetFixE", PrmValueKind::I16),
    ("mSLInbetFixF", PrmValueKind::I16),
    ("mSLInbetFixG", PrmValueKind::I16),
    ("mSLInbetFixH", PrmValueKind::I16),
    ("mSLInbetFixI", PrmValueKind::I16),
    ("mSLInbetFollow", PrmValueKind::I16),
    ("mSLInbetFollowB", PrmValueKind::I16),
    ("mSLInbetFollowC", PrmValueKind::I16),
    ("mSLInbetHang", PrmValueKind::I16),
    ("mSLInbetHipAttack", PrmValueKind::I16),
    ("mSLInbetHovering", PrmValueKind::I16),
    ("mSLInbetIndoor", PrmValueKind::I16),
    ("mSLInbetJetCoaster", PrmValueKind::I16),
    ("mSLInbetJumpCode", PrmValueKind::I16),
    ("mSLInbetKoopa", PrmValueKind::I16),
    ("mSLInbetLBack", PrmValueKind::I16),
    ("mSLInbetLeanMirror", PrmValueKind::I16),
    ("mSLInbetLNormal", PrmValueKind::I16),
    ("mSLInbetLookDown", PrmValueKind::I16),
    ("mSLInbetMareUnderGround", PrmValueKind::I16),
    ("mSLInbetMonteFence", PrmValueKind::I16),
    ("mSLInbetMonteHang", PrmValueKind::I16),
    ("mSLInbetMultiPlayer", PrmValueKind::I16),
    ("mSLInbetParallel", PrmValueKind::I16),
    ("mSLInbetParallelB", PrmValueKind::I16),
    ("mSLInbetRailFence", PrmValueKind::I16),
    ("mSLInbetRocketJump", PrmValueKind::I16),
    ("mSLInbetSlider", PrmValueKind::I16),
    ("mSLInbetSurfing", PrmValueKind::I16),
    ("mSLInbetSwimming", PrmValueKind::I16),
    ("mSLInbetTalkA", PrmValueKind::I16),
    ("mSLInbetTalkB", PrmValueKind::I16),
    ("mSLInbetTalkC", PrmValueKind::I16),
    ("mSLInbetTalkD", PrmValueKind::I16),
    ("mSLInbetTalkE", PrmValueKind::I16),
    ("mSLInbetTowerA", PrmValueKind::I16),
    ("mSLInbetTowerB", PrmValueKind::I16),
    ("mSLInbetTowerC", PrmValueKind::I16),
    ("mSLInbetTowerD", PrmValueKind::I16),
    ("mSLInbetTurbo", PrmValueKind::I16),
    ("mSLInbetUnderGround", PrmValueKind::I16),
    ("mSLInbetWallJump", PrmValueKind::I16),
    ("mSLInbetWire", PrmValueKind::I16),
    ("mSLInbetWireHang", PrmValueKind::I16),
    ("mSLInHouseMaginfXmax", PrmValueKind::F32),
    ("mSLInHouseMaginfXmin", PrmValueKind::F32),
    ("mSLInstanceNum", PrmValueKind::U8),
    ("mSLInvincibleTimer", PrmValueKind::I32),
    ("mSLJitabataTimer", PrmValueKind::I32),
    ("mSLJumpAngY", PrmValueKind::F32),
    ("mSLJumpAttackAngle", PrmValueKind::F32),
    ("mSLJumpAttackGravity", PrmValueKind::F32),
    ("mSLJumpAttackRadius", PrmValueKind::F32),
    ("mSLJumpAttackSp", PrmValueKind::F32),
    ("mSLJumpAttackTurnSp", PrmValueKind::F32),
    ("mSLJumpFollowSpeedXmax", PrmValueKind::F32),
    ("mSLJumpFollowSpeedXmin", PrmValueKind::F32),
    ("mSLJumpForce", PrmValueKind::F32),
    ("mSLJumpMaxAngle", PrmValueKind::F32),
    ("mSLJumpMaxFrame", PrmValueKind::F32),
    ("mSLJumpMaxScale", PrmValueKind::F32),
    ("mSLJumpMinimum", PrmValueKind::F32),
    ("mSLJumpPrepareFrame", PrmValueKind::I32),
    ("mSLJumpPrepareTime", PrmValueKind::I32),
    ("mSLJumpQuakeLen", PrmValueKind::F32),
    ("mSLJumpShake", PrmValueKind::F32),
    ("mSLJumpSp", PrmValueKind::F32),
    ("mSLKillerDist", PrmValueKind::F32),
    ("mSLKillerInterval", PrmValueKind::I32),
    ("mSLKillerTransYOffset", PrmValueKind::F32),
    ("mSLKyoroTimer", PrmValueKind::I32),
    ("mSLLandHeight", PrmValueKind::F32),
    ("mSLLaunchPeriod", PrmValueKind::I32),
    ("mSLLeafGravity", PrmValueKind::F32),
    ("mSLLeafVelocityXZ", PrmValueKind::F32),
    ("mSLLeafVelocityY", PrmValueKind::F32),
    ("mSLLeave", PrmValueKind::F32),
    ("mSLLevelLimit", PrmValueKind::F32),
    ("mSLLFollowMaginfXmax", PrmValueKind::F32),
    ("mSLLFollowMaginfXmin", PrmValueKind::F32),
    ("mSLLimitMaxAngleX", PrmValueKind::I16),
    ("mSLLimitMinAngleX", PrmValueKind::I16),
    ("mSLLimitMove", PrmValueKind::F32),
    ("mSLLiveTime", PrmValueKind::I32),
    ("mSLLoopAppearTime", PrmValueKind::I32),
    ("mSLLoopHideTime", PrmValueKind::I32),
    ("mSLMarchSpeedHigh", PrmValueKind::F32),
    ("mSLMarchSpeedLow", PrmValueKind::F32),
    ("mSLMarchSpeedLv0", PrmValueKind::F32),
    ("mSLMarchSpeedLv1", PrmValueKind::F32),
    ("mSLMarchSpeedLv2", PrmValueKind::F32),
    ("mSLMarioCircle", PrmValueKind::F32),
    ("mSLMaxAddAngleY", PrmValueKind::I16),
    ("mSLMaxHp", PrmValueKind::F32),
    ("mSLMaxScale", PrmValueKind::F32),
    ("mSLMinCushionXZ", PrmValueKind::F32),
    ("mSLMoveDist", PrmValueKind::F32),
    ("mSLMoveGravity", PrmValueKind::F32),
    ("mSLMoveInterval", PrmValueKind::I32),
    ("mSLMoveJumpSp", PrmValueKind::F32),
    ("mSLMoveSpeed", PrmValueKind::F32),
    ("mSLNearClip", PrmValueKind::F32),
    ("mSLNormalJumpVy", PrmValueKind::F32),
    ("mSLNumArray", PrmValueKind::I32),
    ("mSLNumDivision", PrmValueKind::I32),
    ("mSLNumFreezeWater", PrmValueKind::I32),
    ("mSLNumMajor", PrmValueKind::U8),
    ("mSLNumMinor", PrmValueKind::U8),
    ("mSLObstructMaginfXmax", PrmValueKind::F32),
    ("mSLObstructMaginfXmin", PrmValueKind::F32),
    ("mSLOffsetAngleX", PrmValueKind::I16),
    ("mSLOffsetAngleY", PrmValueKind::I16),
    ("mSLOffsetLookatXZ", PrmValueKind::F32),
    ("mSLPickUpTime", PrmValueKind::I32),
    ("mSLPivotSpeed", PrmValueKind::F32),
    ("mSLPivotSpeedAware", PrmValueKind::F32),
    ("mSLPolIntervalMax", PrmValueKind::I32),
    ("mSLPolIntervalMin", PrmValueKind::I32),
    ("mSLPollBallFront", PrmValueKind::F32),
    ("mSLPollBallRange", PrmValueKind::F32),
    ("mSLPollBallSpeed", PrmValueKind::F32),
    ("mSLPollBallStampScale", PrmValueKind::F32),
    ("mSLPolluteCycle", PrmValueKind::I32),
    ("mSLPolluteInterval", PrmValueKind::I32),
    ("mSLPolluteModelScale", PrmValueKind::F32),
    ("mSLPolluteObjGravity", PrmValueKind::F32),
    ("mSLPolluteObjLinerG", PrmValueKind::F32),
    ("mSLPolluteObjLinerSp", PrmValueKind::F32),
    ("mSLPolluteObjSpeed", PrmValueKind::F32),
    ("mSLPolluteRange", PrmValueKind::U8),
    ("mSLPolluteRMax", PrmValueKind::I32),
    ("mSLPolluteRMin", PrmValueKind::I32),
    ("mSLPollutionInterval", PrmValueKind::I32),
    ("mSLPollutionLength", PrmValueKind::F32),
    ("mSLPolWaitCount", PrmValueKind::I32),
    ("mSLPosChaseRateXZ", PrmValueKind::F32),
    ("mSLPosChaseRateXZ_C", PrmValueKind::F32),
    ("mSLPosChaseRateY", PrmValueKind::F32),
    ("mSLPosChaseRateY_C", PrmValueKind::F32),
    ("mSLPrePolWait", PrmValueKind::I32),
    ("mSLPumpRate", PrmValueKind::F32),
    ("mSLRandomRangeMax", PrmValueKind::F32),
    ("mSLRandomRangeMin", PrmValueKind::F32),
    ("mSLRateExpand", PrmValueKind::F32),
    ("mSLReadyTime", PrmValueKind::I32),
    ("mSLRecoverTime", PrmValueKind::I32),
    ("mSLRecoverTimer", PrmValueKind::I32),
    ("mSLRegenFoot", PrmValueKind::I32),
    ("mSLReleaseSpeed", PrmValueKind::F32),
    ("mSLReproduceDemoNearClip", PrmValueKind::F32),
    ("mSLRestTime", PrmValueKind::I32),
    ("mSLRollingMax", PrmValueKind::F32),
    ("mSLRollSpeed", PrmValueKind::F32),
    ("mSLRoofChangeY", PrmValueKind::F32),
    ("mSLRoofHeight", PrmValueKind::F32),
    ("mSLRootCircle", PrmValueKind::F32),
    ("mSLRotSpeed", PrmValueKind::F32),
    ("mSLScaleRate", PrmValueKind::F32),
    ("mSLScaleY", PrmValueKind::F32),
    ("mSLSearchActorTimer", PrmValueKind::I32),
    ("mSLSearchAngle", PrmValueKind::F32),
    ("mSLSearchAngleOnObj", PrmValueKind::F32),
    ("mSLSearchAware", PrmValueKind::F32),
    ("mSLSearchAwareOnObj", PrmValueKind::F32),
    ("mSLSearchHeight", PrmValueKind::F32),
    ("mSLSearchLength", PrmValueKind::F32),
    ("mSLSearchLengthOnObj", PrmValueKind::F32),
    ("mSLSecureViewChase", PrmValueKind::F32),
    ("mSLSecureViewDistXMax", PrmValueKind::F32),
    ("mSLSecureViewDistXMin", PrmValueKind::F32),
    ("mSLSecureViewDistZMax", PrmValueKind::F32),
    ("mSLSecureViewDistZMin", PrmValueKind::F32),
    ("mSLSeedGravity", PrmValueKind::F32),
    ("mSLSeedGravityC", PrmValueKind::F32),
    ("mSLSeedGravityS", PrmValueKind::F32),
    ("mSLSeedShootInterval", PrmValueKind::I32),
    ("mSLSeedShootRange", PrmValueKind::F32),
    ("mSLSeedSpeedC", PrmValueKind::F32),
    ("mSLSeedSpeedS", PrmValueKind::F32),
    ("mSLSeedSpeedXZ", PrmValueKind::F32),
    ("mSLSeedSpeedY", PrmValueKind::F32),
    ("mSLSerialCrashFrame", PrmValueKind::I32),
    ("mSLShootInterval", PrmValueKind::I32),
    ("mSLShootRadius", PrmValueKind::F32),
    ("mSLShootRange", PrmValueKind::F32),
    ("mSLShootVelocity", PrmValueKind::F32),
    ("mSLSightAngle", PrmValueKind::F32),
    ("mSLSingleAttackLen", PrmValueKind::F32),
    ("mSLSingleHoming", PrmValueKind::I32),
    ("mSLSize", PrmValueKind::F32),
    ("mSLSizeBase", PrmValueKind::F32),
    ("mSLSkipRopeAttackSpeed", PrmValueKind::F32),
    ("mSLSleepFrame", PrmValueKind::I32),
    ("mSLSlopeForwardDistXZ", PrmValueKind::F32),
    ("mSLSlopeMaxAngleX", PrmValueKind::I16),
    ("mSLSlopeSpeedAngleX", PrmValueKind::I16),
    ("mSLStampCount", PrmValueKind::I32),
    ("mSLStampProb", PrmValueKind::F32),
    ("mSLStampQuakeLen", PrmValueKind::F32),
    ("mSLStampRange", PrmValueKind::U8),
    ("mSLStrongReduce", PrmValueKind::F32),
    ("mSLStunTime", PrmValueKind::I32),
    ("mSLSwingLength", PrmValueKind::F32),
    ("mSLTargetAtJumpOffsetY", PrmValueKind::F32),
    ("mSLTelesaAttackGravityY", PrmValueKind::F32),
    ("mSLTelesaGravityY", PrmValueKind::F32),
    ("mSLTelesaPowerByWater", PrmValueKind::F32),
    ("mSLTentacleStretch", PrmValueKind::F32),
    ("mSLThrownGravity", PrmValueKind::F32),
    ("mSLThrownGravityY", PrmValueKind::F32),
    ("mSLThrownRateXZ", PrmValueKind::F32),
    ("mSLThrownVY", PrmValueKind::F32),
    ("mSLThrowSpeed", PrmValueKind::F32),
    ("mSLThrowXZSpeed", PrmValueKind::F32),
    ("mSLTornadoMoveInc", PrmValueKind::F32),
    ("mSLTornadoMoveInit", PrmValueKind::F32),
    ("mSLTornadoMoveLimit", PrmValueKind::F32),
    ("mSLTornadoProp", PrmValueKind::F32),
    ("mSLTornadoRollSpeed", PrmValueKind::F32),
    ("mSLTornadoSpeed", PrmValueKind::F32),
    ("mSLTrampleBonusNum", PrmValueKind::I32),
    ("mSLTrapJumpGravity", PrmValueKind::F32),
    ("mSLTrapJumpMaxSpXZ", PrmValueKind::F32),
    ("mSLTrapJumpMaxSpY", PrmValueKind::F32),
    ("mSLTrapJumpMinSpXZ", PrmValueKind::F32),
    ("mSLTrapJumpMinSpY", PrmValueKind::F32),
    ("mSLTumbleTime", PrmValueKind::I32),
    ("mSLTurnLength", PrmValueKind::F32),
    ("mSLTurnSpeedHigh", PrmValueKind::F32),
    ("mSLTurnSpeedLow", PrmValueKind::F32),
    ("mSLTurnSpeedLv0", PrmValueKind::F32),
    ("mSLTurnSpeedLv1", PrmValueKind::F32),
    ("mSLTurnSpeedLv2", PrmValueKind::F32),
    ("mSLUnisonAttackLen", PrmValueKind::F32),
    ("mSLUnisonAttackSpeed", PrmValueKind::F32),
    ("mSLUnisonHoming", PrmValueKind::I32),
    ("mSLUnisonInter", PrmValueKind::I32),
    ("mSLVanishSpeed", PrmValueKind::F32),
    ("mSLVelocityRate", PrmValueKind::F32),
    ("mSLViewShakeDist", PrmValueKind::F32),
    ("mSLVomitAnmRate", PrmValueKind::F32),
    ("mSLWaitFrameStg0", PrmValueKind::I32),
    ("mSLWaitTime", PrmValueKind::I32),
    ("mSLWakeFrame", PrmValueKind::I32),
    ("mSLWalkShake", PrmValueKind::F32),
    ("mSLWalkSpeedRateLv0", PrmValueKind::F32),
    ("mSLWallCheckRadius", PrmValueKind::F32),
    ("mSLWallRadius", PrmValueKind::F32),
    ("mSLWaterAttackCoeff", PrmValueKind::F32),
    ("mSLWaterCoeff", PrmValueKind::F32),
    ("mSLWaterEmitPos", PrmValueKind::F32),
    ("mSLWaterHitTimer", PrmValueKind::I32),
    ("mSLWaterMarkLimit", PrmValueKind::I32),
    ("mSLWaterScaleMax", PrmValueKind::F32),
    ("mSLWaterVanishSpeed", PrmValueKind::F32),
    ("mSLXAngleMax", PrmValueKind::I16),
    ("mSLXAngleMin", PrmValueKind::I16),
    ("mSLXRotRatioAtOffsetY", PrmValueKind::F32),
    ("mSLXRotRatioManualSpeed", PrmValueKind::F32),
    ("mSLYAngleManualSpeedXMax", PrmValueKind::I16),
    ("mSLYAngleManualSpeedXMin", PrmValueKind::I16),
    ("mSLZigzagAngle", PrmValueKind::F32),
    ("mSLZigzagCycle", PrmValueKind::F32),
    ("mStampFlag", PrmValueKind::U8),
    ("mStopFlag", PrmValueKind::U8),
    ("mTurnOffsetY", PrmValueKind::F32),
    ("mV", PrmValueKind::Vec3F32),
    ("mWaterCtMax", PrmValueKind::I16),
    ("mXRotStart", PrmValueKind::F32),
    ("mYButtonRotateChase", PrmValueKind::I16),
];

fn prm_value_kind(name: &str) -> Option<PrmValueKind> {
    PRM_VALUE_SCHEMA
        .iter()
        .find_map(|(candidate, kind)| (*candidate == name).then_some(*kind))
}

fn prm_key_code(name: &str) -> u16 {
    name.as_bytes().iter().fold(0u32, |hash, byte| {
        hash.wrapping_mul(3).wrapping_add(u32::from(*byte))
    }) as u16
}

fn decode_u32_array<const N: usize>(bytes: &[u8]) -> Option<[u32; N]> {
    if bytes.len() != N * 4 {
        return None;
    }
    let mut values = [0; N];
    for (value, chunk) in values.iter_mut().zip(bytes.chunks_exact(4)) {
        *value = u32::from_be_bytes(chunk.try_into().expect("four-byte chunk"));
    }
    Some(values)
}

fn checked_add(offset: usize, len: usize, source_len: usize) -> Result<usize> {
    offset.checked_add(len).ok_or(FormatError::InvalidOffset {
        format: FORMAT,
        offset,
        len: source_len,
    })
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16> {
    let value: [u8; 2] = checked_slice(bytes, offset, 2)?
        .try_into()
        .expect("checked two-byte slice");
    Ok(u16::from_be_bytes(value))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32> {
    let value: [u8; 4] = checked_slice(bytes, offset, 4)?
        .try_into()
        .expect("checked four-byte slice");
    Ok(u32::from_be_bytes(value))
}

fn checked_slice(bytes: &[u8], offset: usize, len: usize) -> Result<&[u8]> {
    let end = checked_add(offset, len, bytes.len())?;
    bytes.get(offset..end).ok_or(FormatError::InvalidOffset {
        format: FORMAT,
        offset,
        len: bytes.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_raw_entry(bytes: &mut Vec<u8>, name: &str, value: &[u8]) {
        bytes.extend_from_slice(&prm_key_code(name).to_be_bytes());
        bytes.extend_from_slice(&(name.len() as u16).to_be_bytes());
        bytes.extend_from_slice(name.as_bytes());
        bytes.extend_from_slice(&(value.len() as u32).to_be_bytes());
        bytes.extend_from_slice(value);
    }

    #[test]
    fn reads_named_big_endian_float_parameters_losslessly() {
        let mut bytes = 2u32.to_be_bytes().to_vec();
        push_raw_entry(
            &mut bytes,
            "mSLBodyScaleLow",
            &1.0_f32.to_bits().to_be_bytes(),
        );
        push_raw_entry(
            &mut bytes,
            "mSLBodyScaleHigh",
            &1.25_f32.to_bits().to_be_bytes(),
        );

        let file = PrmFile::parse(&bytes).expect("parse parameter fixture");
        assert_eq!(file.f32("mSLBodyScaleLow"), Some(1.0));
        assert_eq!(file.f32("mSLBodyScaleHigh"), Some(1.25));
        assert_eq!(file.encode().unwrap(), bytes);
    }

    #[test]
    fn rebuild_does_not_depend_on_source_and_typed_mutation_survives_reparse() {
        let mut source = 1u32.to_be_bytes().to_vec();
        push_raw_entry(
            &mut source,
            "mSLBodyScaleLow",
            &1.0_f32.to_bits().to_be_bytes(),
        );
        let expected = source.clone();
        let mut file = PrmFile::parse(&source).expect("parse fixture");
        source.fill(0xcc);
        assert_eq!(file.encode().unwrap(), expected);

        *file.value_mut("mSLBodyScaleLow").unwrap() = PrmValue::from_f32(2.5);
        let rebuilt = file.encode().expect("encode mutation");
        let reparsed = PrmFile::parse(&rebuilt).expect("reparse mutation");
        assert_eq!(reparsed.f32("mSLBodyScaleLow"), Some(2.5));
        assert_ne!(rebuilt, expected);
    }

    #[test]
    fn parses_creator_only_array_parameters_as_typed_slots() {
        let mut bytes = 3u32.to_be_bytes().to_vec();
        push_raw_entry(&mut bytes, "mBirthSwitch", &[0, 1, 1, 0]);
        push_raw_entry(
            &mut bytes,
            "mBirthRate",
            &[0, 0, 0, 0, 0x42, 0xc8, 0, 0, 0x42, 0xc8, 0, 0, 0, 0, 0, 0],
        );
        push_raw_entry(&mut bytes, "mBirthMax", &[0; 16]);

        let file = PrmFile::parse(&bytes).expect("parse creator array fixture");
        assert_eq!(
            file.value("mBirthSwitch"),
            Some(&PrmValue::U8Array4([0, 1, 1, 0]))
        );
        assert_eq!(
            file.value("mBirthRate"),
            Some(&PrmValue::F32Array4Bits([
                0,
                100.0_f32.to_bits(),
                100.0_f32.to_bits(),
                0,
            ]))
        );
        assert_eq!(file.value("mBirthMax"), Some(&PrmValue::BirthMaxZeroSlots));
        assert_eq!(file.encode().unwrap(), bytes);
    }

    #[test]
    fn rejects_nonzero_creator_only_birth_max_slots() {
        let mut bytes = 1u32.to_be_bytes().to_vec();
        let mut nonzero = [0; 16];
        nonzero[15] = 1;
        push_raw_entry(&mut bytes, "mBirthMax", &nonzero);

        let error = PrmFile::parse(&bytes).unwrap_err().to_string();
        assert!(error.contains("must contain sixteen zero bytes"), "{error}");
    }

    #[test]
    fn encoder_cannot_mutate_birth_max_into_arbitrary_words() {
        let file = PrmFile {
            entries: vec![PrmEntry {
                name: "mBirthMax".to_string(),
                value: PrmValue::F32Array4Bits([0, 0, 0, 1]),
            }],
        };

        let error = file.encode().unwrap_err().to_string();
        assert!(
            error.contains("zero-only mBirthMax creator marker"),
            "{error}"
        );
    }

    #[test]
    fn rejects_stored_key_code_that_does_not_match_name() {
        let mut bytes = 1u32.to_be_bytes().to_vec();
        push_raw_entry(
            &mut bytes,
            "mSLBodyScaleLow",
            &1.0_f32.to_bits().to_be_bytes(),
        );
        bytes[4..6].copy_from_slice(&0u16.to_be_bytes());
        assert!(matches!(
            PrmFile::parse(&bytes),
            Err(FormatError::Unsupported { .. })
        ));
    }

    #[test]
    fn rejects_unmodeled_parameter_names_instead_of_treating_values_as_bytes() {
        let mut bytes = 1u32.to_be_bytes().to_vec();
        push_raw_entry(&mut bytes, "unmodeled", &[0, 0, 0, 1]);
        let error = PrmFile::parse(&bytes).unwrap_err().to_string();
        assert!(error.contains("no decomp/corpus-backed semantic schema"));
    }

    #[test]
    fn rejects_value_length_that_disagrees_with_schema() {
        let mut bytes = 1u32.to_be_bytes().to_vec();
        push_raw_entry(&mut bytes, "mSLBodyScaleLow", &[0, 1]);
        let error = PrmFile::parse(&bytes).unwrap_err().to_string();
        assert!(error.contains("requires 4 byte"));
    }

    #[test]
    fn encoder_rejects_value_variant_that_disagrees_with_schema() {
        let file = PrmFile {
            entries: vec![PrmEntry {
                name: "mSLBodyScaleLow".to_string(),
                value: PrmValue::I32(1),
            }],
        };
        let error = file.encode().unwrap_err().to_string();
        assert!(error.contains("requires f32"));
    }

    #[test]
    fn rejects_truncated_parameter_values() {
        let name = "mSLBodyScaleLow";
        let mut bytes = 1u32.to_be_bytes().to_vec();
        bytes.extend_from_slice(&prm_key_code(name).to_be_bytes());
        bytes.extend_from_slice(&(name.len() as u16).to_be_bytes());
        bytes.extend_from_slice(name.as_bytes());
        bytes.extend_from_slice(&4u32.to_be_bytes());
        bytes.extend_from_slice(&[0, 0]);
        assert!(matches!(
            PrmFile::parse(&bytes),
            Err(FormatError::InvalidOffset { .. })
        ));
    }

    #[test]
    fn schema_has_unique_names_and_correct_fixed_widths() {
        let mut names = std::collections::BTreeSet::new();
        for (name, kind) in PRM_VALUE_SCHEMA {
            assert!(names.insert(*name), "duplicate schema name {name}");
            assert!(matches!(kind.encoded_len(), 1 | 2 | 4 | 12 | 16));
        }
        assert_eq!(names.len(), 521);
    }

    #[test]
    #[ignore = "requires SMS_BASE_ROOT with extracted retail stage archives"]
    fn source_free_rebuilds_every_retail_stage_parameter_file() {
        let base_root = std::env::var_os("SMS_BASE_ROOT")
            .map(std::path::PathBuf::from)
            .expect("set SMS_BASE_ROOT to an extracted retail game root");
        let archives =
            crate::discover_scene_archives(&base_root).expect("discover retail scene archives");
        let mut rebuilt = 0usize;
        for archive in archives {
            for asset in crate::mount_scene_archive(&archive.path)
                .unwrap_or_else(|error| panic!("mount {}: {error}", archive.path.display()))
            {
                if !asset
                    .path
                    .to_string_lossy()
                    .to_ascii_lowercase()
                    .ends_with(".prm")
                {
                    continue;
                }
                let mut source = crate::read_stage_asset_bytes(&asset.path)
                    .unwrap_or_else(|error| panic!("read {}: {error}", asset.path.display()));
                let expected = source.clone();
                let file = PrmFile::parse(&source)
                    .unwrap_or_else(|error| panic!("parse {}: {error}", asset.path.display()));
                source.fill(0xa5);
                assert_eq!(
                    file.encode().expect("encode typed semantic PRM"),
                    expected,
                    "source-free PRM rebuild differs for {}",
                    asset.path.display()
                );
                rebuilt += 1;
            }
        }
        assert_eq!(rebuilt, 159, "unexpected retail stage PRM census");
    }
}
