use std::collections::HashMap;
use std::fs;
use std::num::NonZeroU16;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, TryRecvError};
use std::sync::Arc;
use std::thread;

use rodio::buffer::SamplesBuffer;
use rodio::{DeviceSinkBuilder, MixerDeviceSink, Player};
use sms_formats::{decode_yaz0, RarcArchive};

use crate::SmsEditorApp;

// Sunshine's JAudio/DSP pipeline mixes at the GameCube's nominal 32 kHz rate.
const OUTPUT_RATE: u32 = 32_000;
const PREVIEW_SECONDS: f32 = 45.0;
const SOUND_PREVIEW_SECONDS: f32 = 8.0;
const MAX_TRACKS: usize = 64;
const MAX_COMMANDS_PER_TICK: usize = 50_000;

pub(super) struct AudioPreviewPlayback {
    target: AudioPreviewTarget,
    _device: MixerDeviceSink,
    player: Player,
}

impl AudioPreviewPlayback {
    fn is_playing(&self, bgm_id: u32) -> bool {
        self.target == AudioPreviewTarget::Music(bgm_id) && !self.player.empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AudioPreviewTarget {
    Music(u32),
    Sound(u32),
}

pub(super) struct AudioPreviewRenderResult {
    generation: u64,
    target: AudioPreviewTarget,
    result: Result<Vec<f32>, String>,
}

impl SmsEditorApp {
    pub(super) fn audio_preview_is_active(&self) -> bool {
        self.audio_preview_playback
            .as_ref()
            .is_some_and(|playback| !playback.player.empty())
    }

    pub(super) fn bgm_preview_is_active(&self, bgm_id: u32) -> bool {
        self.audio_preview_playback
            .as_ref()
            .is_some_and(|playback| playback.is_playing(bgm_id))
    }

    pub(super) fn sound_preview_is_active(&self, sound_id: u32) -> bool {
        self.audio_preview_playback
            .as_ref()
            .is_some_and(|playback| {
                playback.target == AudioPreviewTarget::Sound(sound_id) && !playback.player.empty()
            })
    }

    pub(super) fn audio_preview_is_loading(&self) -> bool {
        self.audio_preview_receiver.is_some()
    }

    pub(super) fn audio_preview_target_is_loading(&self, target: AudioPreviewTarget) -> bool {
        self.audio_preview_receiver.is_some() && self.audio_preview_loading_target == Some(target)
    }

    pub(super) fn preview_bgm_now(&mut self, bgm_id: u32) {
        if let Err(error) = self.start_audio_preview_render(AudioPreviewTarget::Music(bgm_id)) {
            self.log
                .push(format!("Could not preview Sunshine BGM: {error}"));
        }
    }

    pub(super) fn preview_sound_now(&mut self, sound_id: u32) {
        if let Err(error) = self.start_audio_preview_render(AudioPreviewTarget::Sound(sound_id)) {
            self.log
                .push(format!("Could not preview Sunshine sound: {error}"));
        }
    }

    pub(super) fn bgm_preview_transport(&mut self, ui: &mut egui::Ui, bgm_id: u32) {
        let is_playing = self
            .audio_preview_playback
            .as_ref()
            .is_some_and(|playback| playback.is_playing(bgm_id));
        let is_loading = self.audio_preview_target_is_loading(AudioPreviewTarget::Music(bgm_id));
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !is_playing && !is_loading,
                    egui::Button::new(if is_loading { "Loading..." } else { "Preview" }),
                )
                .on_hover_text("Render this track from Sunshine's sequence, instrument-bank, and AW sample data")
                .clicked()
            {
                if let Err(error) =
                    self.start_audio_preview_render(AudioPreviewTarget::Music(bgm_id))
                {
                    self.log.push(format!("Could not preview Sunshine BGM: {error}"));
                }
            }
            if ui
                .add_enabled(is_playing || is_loading, egui::Button::new("Stop"))
                .clicked()
            {
                self.stop_audio_preview();
            }
        });
        ui.small("Preview uses the actual Sunshine sequence, banks, and AW samples from this project's base game.");
    }

    pub(super) fn se_preview_notice(&self, ui: &mut egui::Ui) {
        ui.small("Preview runs the selected retail sound through Sunshine's JAudio SE dispatcher, banks, and AW samples.");
    }

    fn start_audio_preview_render(&mut self, target: AudioPreviewTarget) -> Result<(), String> {
        self.stop_audio_preview();
        let base_root = self
            .current_project
            .as_ref()
            .map(|project| project.descriptor.base_game_root.clone())
            .or_else(|| {
                let root = self.base_root.trim();
                (!root.is_empty()).then(|| PathBuf::from(root))
            })
            .ok_or_else(|| "select a Sunshine base game first".to_string())?;
        self.audio_preview_generation = self.audio_preview_generation.wrapping_add(1);
        let generation = self.audio_preview_generation;
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = match target {
                AudioPreviewTarget::Music(bgm_id) => {
                    render_bgm_preview(&base_root, bgm_id, PREVIEW_SECONDS)
                }
                AudioPreviewTarget::Sound(sound_id) => {
                    render_sound_preview(&base_root, sound_id, SOUND_PREVIEW_SECONDS)
                }
            };
            let _ = sender.send(AudioPreviewRenderResult {
                generation,
                target,
                result,
            });
        });
        self.audio_preview_receiver = Some(receiver);
        self.audio_preview_loading_target = Some(target);
        Ok(())
    }

    pub(super) fn poll_audio_preview(&mut self, ctx: &egui::Context) {
        let Some(receiver) = self.audio_preview_receiver.take() else {
            return;
        };
        match receiver.try_recv() {
            Ok(message) => {
                self.audio_preview_loading_target = None;
                if message.generation != self.audio_preview_generation {
                    return;
                }
                match message.result {
                    Ok(samples) => {
                        if let Err(error) = self.start_rendered_audio(message.target, samples) {
                            self.log
                                .push(format!("Could not start audio preview: {error}"));
                        }
                    }
                    Err(error) => self
                        .log
                        .push(format!("Could not render audio preview: {error}")),
                }
                ctx.request_repaint();
            }
            Err(TryRecvError::Empty) => {
                self.audio_preview_receiver = Some(receiver);
                ctx.request_repaint_after(std::time::Duration::from_millis(33));
            }
            Err(TryRecvError::Disconnected) => {
                self.audio_preview_loading_target = None;
                self.log
                    .push("The audio preview worker stopped unexpectedly.".to_string());
            }
        }
    }

    fn start_rendered_audio(
        &mut self,
        target: AudioPreviewTarget,
        samples: Vec<f32>,
    ) -> Result<(), String> {
        let device = DeviceSinkBuilder::open_default_sink()
            .map_err(|error| format!("open the default audio output: {error}"))?;
        let player = Player::connect_new(device.mixer());
        player.append(SamplesBuffer::new(
            NonZeroU16::new(2).expect("stereo is nonzero"),
            NonZeroU32::new(OUTPUT_RATE).expect("output rate is nonzero"),
            samples,
        ));
        self.audio_preview_playback = Some(AudioPreviewPlayback {
            target,
            _device: device,
            player,
        });
        Ok(())
    }

    pub(super) fn stop_audio_preview(&mut self) {
        self.audio_preview_generation = self.audio_preview_generation.wrapping_add(1);
        self.audio_preview_receiver = None;
        self.audio_preview_loading_target = None;
        if let Some(playback) = self.audio_preview_playback.take() {
            playback.player.stop();
        }
    }
}

#[derive(Clone)]
struct BankRecord {
    bytes: Arc<Vec<u8>>,
    wave_bank: usize,
}

#[derive(Clone)]
struct WaveRecord {
    archive: String,
    format: u8,
    root_key: u8,
    sample_rate: f32,
    offset: usize,
    byte_len: usize,
    looped: bool,
    loop_start: usize,
    loop_end: usize,
    sample_count: usize,
    _loop_history: [i16; 2],
}

struct AudioAssets {
    audio_res: PathBuf,
    sequence: Vec<u8>,
    sequence_header: Vec<u8>,
    sound_table: Vec<u8>,
    banks: HashMap<u8, BankRecord>,
    waves: Vec<HashMap<u16, WaveRecord>>,
    wave_archives: HashMap<String, Arc<Vec<u8>>>,
    decoded_waves: HashMap<(usize, u16), Arc<DecodedWave>>,
    fx_lines: [Option<FxLineConfig>; 4],
    instrument_rng: u32,
}

#[derive(Clone, Copy)]
struct SoundPreviewTrigger {
    id: u32,
    volume: f32,
    pitch: f32,
    fxmix: f32,
}

#[derive(Clone, Copy, Debug)]
struct FxDestination {
    buffer_id: u16,
    volume: f32,
}

#[derive(Clone, Debug)]
struct FxLineConfig {
    enabled: u8,
    circular_buffer_blocks: usize,
    destinations: [FxDestination; 2],
    filter_coefficients: [f32; 8],
}

#[derive(Debug)]
struct DecodedWave {
    samples: Vec<f32>,
    root_key: u8,
    sample_rate: f32,
    loop_range: Option<(usize, usize)>,
}

#[derive(Clone, Copy)]
struct ReleaseSpec {
    duration_units: u32,
    curve: u8,
}

impl ReleaseSpec {
    fn direct(encoded: u16) -> Self {
        let encoded = if encoded == 0 { 0x10 } else { encoded };
        Self {
            duration_units: u32::from(encoded & 0x3fff),
            curve: (encoded >> 14) as u8,
        }
    }

    fn frames(self) -> usize {
        ((self.duration_units as usize * OUTPUT_RATE as usize) / 600).max(1)
    }
}

#[derive(Clone, Copy, Debug)]
struct EnvelopeCommand {
    mode: i16,
    time: u16,
    value: i16,
}

#[derive(Clone, Debug)]
struct OscillatorSpec {
    kind: u8,
    rate: f32,
    attack: Option<Arc<Vec<EnvelopeCommand>>>,
    release: Option<Arc<Vec<EnvelopeCommand>>>,
    width: f32,
    vertex: f32,
}

#[derive(Clone)]
struct InstrumentRegion {
    wave_id: u16,
    volume: f32,
    pitch: f32,
    pan: f32,
    fxmix: f32,
    oscillators: Vec<OscillatorSpec>,
    direct_release: u16,
}

struct RegionParameters {
    volume: f32,
    pitch: f32,
    pan: f32,
    fxmix: f32,
    oscillators: Vec<OscillatorSpec>,
    direct_release: u16,
}

impl AudioAssets {
    fn load(base_root: &Path) -> Result<Self, String> {
        let audio_res = find_audio_res(base_root)?;
        let sequence = fs::read(audio_res.join("Seqs/sequence.arc"))
            .map_err(|error| format!("read sequence.arc: {error}"))?;
        let aaf = load_msound_aaf(base_root, &audio_res)?;
        let (sequence_header, sound_table, bank_entries, wave_entries, fx_lines) =
            parse_aaf_tables(&aaf)?;
        let mut banks = HashMap::new();
        for (offset, size, wave_bank) in bank_entries {
            let bytes = checked_slice(&aaf, offset, size, "AAF bank")?.to_vec();
            let virtual_bank = be_u32(&bytes, 8, "bank number")? as u8;
            banks.insert(
                virtual_bank,
                BankRecord {
                    bytes: Arc::new(bytes),
                    wave_bank,
                },
            );
        }
        let mut waves = Vec::new();
        for (offset, size, kind) in wave_entries {
            let bytes = checked_slice(&aaf, offset, size, "AAF wave system")?;
            waves.push(parse_wave_system(bytes, kind)?);
        }
        Ok(Self {
            audio_res,
            sequence,
            sequence_header,
            sound_table,
            banks,
            waves,
            wave_archives: HashMap::new(),
            decoded_waves: HashMap::new(),
            fx_lines,
            instrument_rng: 0,
        })
    }

    fn sequence_for_bgm(&self, bgm_id: u32) -> Result<&[u8], String> {
        if bgm_id & 0xffff_0000 != 0x8001_0000 {
            return Err(format!("0x{bgm_id:08X} is not a sequence BGM"));
        }
        self.sequence_for_entry((bgm_id & 0x3ff) as usize)
    }

    fn sequence_for_entry(&self, mut entry: usize) -> Result<&[u8], String> {
        for _ in 0..16 {
            let at = (entry + 1)
                .checked_mul(0x20)
                .ok_or_else(|| "sequence entry overflow".to_string())?;
            checked_slice(&self.sequence_header, at, 0x20, "sequence entry")?;
            let alias = be_u16(&self.sequence_header, at + 0x0e, "sequence alias")?;
            if alias != 0xffff {
                entry = alias as usize;
                continue;
            }
            let offset = be_u32(&self.sequence_header, at + 0x18, "sequence offset")? as usize;
            let size = be_u32(&self.sequence_header, at + 0x1c, "sequence size")? as usize;
            return checked_slice(&self.sequence, offset, size, "sequence data");
        }
        Err("sequence alias chain is too deep".to_string())
    }

    fn sound_preview_trigger(&self, sound_id: u32) -> Result<SoundPreviewTrigger, String> {
        if sound_id & 0xc000_0000 != 0 {
            return Err(format!("0x{sound_id:08X} is not a sound-effect ID"));
        }
        let category = ((sound_id >> 12) & 0xff) as usize;
        if category >= 16 {
            return Err(format!(
                "sound category {category} is outside Sunshine's SE table"
            ));
        }
        checked_slice(&self.sound_table, 0, 0x50, "sound table header")?;
        let count = be_u16(&self.sound_table, 6 + category * 4, "sound category count")? as usize;
        let first = be_u16(&self.sound_table, 8 + category * 4, "sound category start")? as usize;
        let index = (sound_id & 0x3ff) as usize;
        if index >= count {
            return Err(format!(
                "sound 0x{sound_id:08X} is outside category {category}'s {count} entries"
            ));
        }
        let record = 0x50 + (first + index) * 0x10;
        checked_slice(&self.sound_table, record, 0x10, "sound info record")?;
        let format = self.sound_table[0];
        let id = if format & 1 == 0 {
            sound_id
        } else {
            let alias = be_u16(&self.sound_table, record + 6, "sound alias")?;
            (sound_id & !0x3ff) | u32::from(alias)
        };
        Ok(SoundPreviewTrigger {
            id,
            pitch: if format & 2 != 0 {
                be_f32(&self.sound_table, record + 8, "sound pitch")?
            } else {
                1.0
            },
            volume: if format & 4 != 0 {
                f32::from(self.sound_table[record + 0x0c]) / 127.0
            } else {
                1.0
            },
            fxmix: if format & 8 != 0 {
                f32::from(self.sound_table[record + 0x0d]) / 127.0
            } else {
                0.0
            },
        })
    }

    fn instrument_region(
        &mut self,
        virtual_bank: u8,
        program: u8,
        key: u8,
        velocity: u8,
    ) -> Result<(usize, InstrumentRegion), String> {
        let (bank_bytes, wave_bank) = self
            .banks
            .get(&virtual_bank)
            .map(|bank| (Arc::clone(&bank.bytes), bank.wave_bank))
            .ok_or_else(|| format!("sequence requested missing virtual bank {virtual_bank}"))?;
        let bytes = bank_bytes.as_slice();
        let region = if program < 0x80 {
            let inst_offset =
                be_u32(bytes, 0x24 + program as usize * 4, "instrument offset")? as usize;
            if inst_offset == 0 {
                return Err(format!("bank {virtual_bank} has no program {program}"));
            }
            let inst_volume = be_f32(bytes, inst_offset + 8, "instrument volume")?;
            let inst_pitch = be_f32(bytes, inst_offset + 0x0c, "instrument pitch")?;
            let key_count = be_u32(bytes, inst_offset + 0x28, "key-region count")? as usize;
            let keymap =
                choose_offset_region(bytes, inst_offset + 0x2c, key_count, key, "key region")?;
            let velocity_count = be_u32(bytes, keymap + 4, "velocity-region count")? as usize;
            let vmap = choose_offset_region(
                bytes,
                keymap + 8,
                velocity_count,
                velocity,
                "velocity region",
            )?;
            let oscillators = instrument_oscillators(bytes, inst_offset)?;
            let effects = instrument_effects(
                bytes,
                [inst_offset + 0x18, inst_offset + 0x20],
                key,
                velocity,
                &mut self.instrument_rng,
            )?;
            parse_vmap(
                bytes,
                vmap,
                RegionParameters {
                    volume: inst_volume * effects.volume,
                    pitch: inst_pitch * effects.pitch,
                    pan: effects.pan.unwrap_or(0.5),
                    fxmix: effects.fxmix.unwrap_or(0.0),
                    oscillators,
                    direct_release: 0,
                },
            )?
        } else if (0xe4..=0xef).contains(&program) {
            let perc_offset = be_u32(
                bytes,
                0x3b4 + (program as usize - 0xe4) * 4,
                "percussion offset",
            )? as usize;
            if perc_offset == 0 {
                return Err(format!(
                    "bank {virtual_bank} has no percussion program {program}"
                ));
            }
            let pmap = be_u32(
                bytes,
                perc_offset + 0x88 + key as usize * 4,
                "percussion key offset",
            )? as usize;
            if pmap == 0 {
                return Err(format!("percussion program {program} has no key {key}"));
            }
            let volume = be_f32(bytes, pmap, "percussion volume")?;
            let pitch = be_f32(bytes, pmap + 4, "percussion pitch")?;
            let velocity_count = be_u32(bytes, pmap + 0x10, "percussion velocity count")? as usize;
            let vmap = choose_offset_region(
                bytes,
                pmap + 0x14,
                velocity_count,
                velocity,
                "percussion velocity region",
            )?;
            let direct_release = if be_u32(bytes, perc_offset, "percussion magic")? == 0x5045_5232 {
                be_u16(
                    bytes,
                    perc_offset + 0x308 + key as usize * 2,
                    "percussion release",
                )?
            } else {
                // TDrumSet::TPerc initializes this to 1000 when the older PERC
                // bank layout has no per-key release table.
                1000
            };
            let pan = if be_u32(bytes, perc_offset, "percussion magic")? == 0x5045_5232 {
                bytes[perc_offset + 0x288 + key as usize] as i8 as f32 / 127.0
            } else {
                0.5
            };
            let effects = instrument_effects(
                bytes,
                [pmap + 8, usize::MAX],
                key,
                velocity,
                &mut self.instrument_rng,
            )?;
            parse_vmap(
                bytes,
                vmap,
                RegionParameters {
                    volume: volume * effects.volume,
                    pitch: pitch * effects.pitch,
                    pan: effects.pan.unwrap_or(pan),
                    fxmix: effects.fxmix.unwrap_or(0.0),
                    oscillators: Vec::new(),
                    direct_release,
                },
            )?
        } else {
            return Err(format!("unsupported bank program 0x{program:02X}"));
        };
        Ok((wave_bank, region))
    }

    fn decoded_wave(&mut self, wave_bank: usize, wave_id: u16) -> Result<Arc<DecodedWave>, String> {
        if let Some(wave) = self.decoded_waves.get(&(wave_bank, wave_id)) {
            return Ok(Arc::clone(wave));
        }
        let record = self
            .waves
            .get(wave_bank)
            .and_then(|bank| bank.get(&wave_id))
            .cloned()
            .ok_or_else(|| format!("wave bank {wave_bank} has no wave 0x{wave_id:04X}"))?;
        if !self.wave_archives.contains_key(&record.archive) {
            let path = self.audio_res.join("Banks").join(&record.archive);
            let bytes = fs::read(&path)
                .map_err(|error| format!("read wave archive '{}': {error}", path.display()))?;
            self.wave_archives
                .insert(record.archive.clone(), Arc::new(bytes));
        }
        let archive = self
            .wave_archives
            .get(&record.archive)
            .expect("inserted above");
        let encoded = checked_slice(archive, record.offset, record.byte_len, "wave sample")?;
        let samples = match record.format {
            0 => decode_afc(encoded, record.sample_count, AfcQuality::High),
            1 => decode_afc(encoded, record.sample_count, AfcQuality::Low),
            2 => encoded
                .iter()
                .take(record.sample_count)
                .map(|sample| (*sample as i8 as f32) / 128.0)
                .collect(),
            3 => encoded
                .chunks_exact(2)
                .take(record.sample_count)
                .map(|sample| i16::from_be_bytes([sample[0], sample[1]]) as f32 / 32768.0)
                .collect(),
            format => return Err(format!("unsupported JAudio wave format {format}")),
        };
        let loop_range = if record.looped && !samples.is_empty() {
            let start = record.loop_start.min(samples.len().saturating_sub(1));
            let end = record.loop_end.min(samples.len()).max(start + 1);
            Some((start, end))
        } else {
            None
        };
        let wave = Arc::new(DecodedWave {
            samples,
            root_key: record.root_key,
            sample_rate: record.sample_rate,
            loop_range,
        });
        self.decoded_waves
            .insert((wave_bank, wave_id), Arc::clone(&wave));
        Ok(wave)
    }
}

fn find_audio_res(base_root: &Path) -> Result<PathBuf, String> {
    [base_root.join("files/AudioRes"), base_root.join("AudioRes")]
        .into_iter()
        .find(|path| path.join("Seqs/sequence.arc").is_file())
        .ok_or_else(|| {
            format!(
                "could not find AudioRes/Seqs/sequence.arc under '{}'",
                base_root.display()
            )
        })
}

fn load_msound_aaf(base_root: &Path, audio_res: &Path) -> Result<Vec<u8>, String> {
    for path in [
        audio_res.join("msound.aaf"),
        base_root.join("files/data/msound.aaf"),
        base_root.join("data/msound.aaf"),
    ] {
        if path.is_file() {
            return fs::read(&path).map_err(|error| format!("read '{}': {error}", path.display()));
        }
    }
    let archive_path = [
        base_root.join("files/data/nintendo.szs"),
        base_root.join("data/nintendo.szs"),
    ]
    .into_iter()
    .find(|path| path.is_file())
    .ok_or_else(|| "could not find data/nintendo.szs containing audi/msound.aaf".to_string())?;
    let encoded = fs::read(&archive_path)
        .map_err(|error| format!("read '{}': {error}", archive_path.display()))?;
    let decoded = if encoded.starts_with(b"Yaz0") {
        decode_yaz0(&encoded)
            .map_err(|error| format!("decode '{}': {error}", archive_path.display()))?
    } else {
        encoded
    };
    let archive = RarcArchive::parse(&decoded)
        .map_err(|error| format!("parse '{}': {error}", archive_path.display()))?;
    archive.file_bytes("audi/msound.aaf").map_err(|error| {
        format!(
            "read audi/msound.aaf from '{}': {error}",
            archive_path.display()
        )
    })
}

type BankEntry = (usize, usize, usize);
type WaveEntry = (usize, usize, u32);

type AafTables = (
    Vec<u8>,
    Vec<u8>,
    Vec<BankEntry>,
    Vec<WaveEntry>,
    [Option<FxLineConfig>; 4],
);

fn parse_aaf_tables(aaf: &[u8]) -> Result<AafTables, String> {
    let mut cursor = 0usize;
    let mut sequence_header = None;
    let mut sound_table = None;
    let mut banks = Vec::new();
    let mut waves = Vec::new();
    let mut fx_resource = None;
    for _ in 0..64 {
        let command = be_u32(aaf, cursor, "AAF command")?;
        cursor += 4;
        match command {
            0 => break,
            1 => {
                let offset = be_u32(aaf, cursor, "sound table offset")? as usize;
                let size = be_u32(aaf, cursor + 4, "sound table size")? as usize;
                let split = be_u32(aaf, cursor + 8, "sound table split")?;
                cursor += if split == 0 { 12 } else { 24 };
                checked_slice(aaf, offset, size, "sound table")?;
                sound_table = Some(checked_slice(aaf, offset, size, "sound table")?.to_vec());
            }
            2 | 3 => loop {
                let offset = be_u32(aaf, cursor, "AAF table offset")? as usize;
                cursor += 4;
                if offset == 0 {
                    break;
                }
                let size = be_u32(aaf, cursor, "AAF table size")? as usize;
                let extra = be_u32(aaf, cursor + 4, "AAF table metadata")? as usize;
                cursor += 8;
                checked_slice(aaf, offset, size, "AAF table")?;
                if command == 2 {
                    banks.push((offset, size, extra));
                } else {
                    waves.push((offset, size, extra as u32));
                }
            },
            4..=8 => {
                let offset = be_u32(aaf, cursor, "AAF resource offset")? as usize;
                let size = be_u32(aaf, cursor + 4, "AAF resource size")? as usize;
                cursor += 12;
                let resource = checked_slice(aaf, offset, size, "AAF resource")?;
                if command == 4 {
                    sequence_header = Some(resource.to_vec());
                } else if command == 7 {
                    fx_resource = Some(resource.to_vec());
                }
            }
            other => return Err(format!("unsupported msound.aaf command {other}")),
        }
    }
    Ok((
        sequence_header.ok_or_else(|| "msound.aaf has no sequence archive header".to_string())?,
        sound_table.ok_or_else(|| "msound.aaf has no sound-effect table".to_string())?,
        banks,
        waves,
        fx_resource
            .as_deref()
            .map(parse_fx_lines)
            .transpose()?
            .unwrap_or([None, None, None, None]),
    ))
}

fn parse_fx_lines(bytes: &[u8]) -> Result<[Option<FxLineConfig>; 4], String> {
    const SEND_TABLE: [u16; 12] = [
        0x0d00, 0x0d60, 0x0dc0, 0x0e20, 0x0e80, 0x0ee0, 0x0ca0, 0x0f40, 0x0fa0, 0x0b00, 0x09a0,
        0x0000,
    ];
    let preset_count = be_u32(bytes, 0, "FX preset count")? as usize;
    if preset_count == 0 {
        return Ok([None, None, None, None]);
    }
    let preset_offset = be_u32(bytes, 20, "FX preset offset")? as usize;
    let mut lines = [None, None, None, None];
    for (index, line) in lines.iter_mut().enumerate() {
        let offset = preset_offset + index * 0x20;
        checked_slice(bytes, offset, 0x20, "FX line config")?;
        let destination = |selector: u16, volume: i16| -> Result<FxDestination, String> {
            let buffer_id = SEND_TABLE
                .get(selector as usize)
                .copied()
                .ok_or_else(|| format!("invalid FX destination selector {selector}"))?;
            Ok(FxDestination {
                buffer_id,
                volume: volume as f32 / 32768.0,
            })
        };
        let mut filter_coefficients = [0.0; 8];
        for (coefficient, value) in filter_coefficients.iter_mut().enumerate() {
            *value = be_i16(
                bytes,
                offset + 0x10 + coefficient * 2,
                "FX filter coefficient",
            )? as f32
                / 32768.0;
        }
        *line = Some(FxLineConfig {
            enabled: bytes[offset],
            circular_buffer_blocks: be_u32(bytes, offset + 0x0c, "FX buffer blocks")? as usize,
            destinations: [
                destination(
                    be_u16(bytes, offset + 2, "FX destination 0")?,
                    be_i16(bytes, offset + 4, "FX volume 0")?,
                )?,
                destination(
                    be_u16(bytes, offset + 6, "FX destination 1")?,
                    be_i16(bytes, offset + 8, "FX volume 1")?,
                )?,
            ],
            filter_coefficients,
        });
    }
    Ok(lines)
}

fn parse_wave_system(bytes: &[u8], kind: u32) -> Result<HashMap<u16, WaveRecord>, String> {
    if kind != 2 {
        return Err(format!("unsupported JAudio wave-bank kind {kind}"));
    }
    let archive_bank = be_u32(bytes, 0x10, "wave archive bank")? as usize;
    let control_group = be_u32(bytes, 0x14, "wave control group")? as usize;
    let group_count = be_u32(bytes, control_group + 8, "wave group count")? as usize;
    let mut result = HashMap::new();
    for group in 0..group_count {
        let scene = be_u32(bytes, control_group + 0x0c + group * 4, "wave scene")? as usize;
        let control = be_u32(bytes, scene + 0x0c, "wave control")? as usize;
        let archive = be_u32(bytes, archive_bank + 8 + group * 4, "wave archive")? as usize;
        let wave_count = be_u32(bytes, control + 4, "wave count")? as usize;
        let archive_name = c_string(checked_slice(bytes, archive, 0x74, "wave archive name")?);
        for index in 0..wave_count {
            let ctrl_wave = be_u32(bytes, control + 8 + index * 4, "control wave")? as usize;
            let wave_id = be_u32(bytes, ctrl_wave, "virtual wave id")? as u16;
            let wave = be_u32(bytes, archive + 0x74 + index * 4, "wave record")? as usize;
            checked_slice(bytes, wave, 0x2c, "wave record")?;
            result.insert(
                wave_id,
                WaveRecord {
                    archive: archive_name.clone(),
                    format: bytes[wave + 1],
                    root_key: bytes[wave + 2],
                    sample_rate: be_f32(bytes, wave + 4, "wave sample rate")?,
                    offset: be_u32(bytes, wave + 8, "wave offset")? as usize,
                    byte_len: be_u32(bytes, wave + 0x0c, "wave byte length")? as usize,
                    looped: be_u32(bytes, wave + 0x10, "wave loop flag")? != 0,
                    loop_start: be_u32(bytes, wave + 0x14, "wave loop start")? as usize,
                    loop_end: be_u32(bytes, wave + 0x18, "wave loop end")? as usize,
                    sample_count: be_u32(bytes, wave + 0x1c, "wave sample count")? as usize,
                    _loop_history: [
                        be_i16(bytes, wave + 0x20, "wave history 1")?,
                        be_i16(bytes, wave + 0x22, "wave history 2")?,
                    ],
                },
            );
        }
    }
    Ok(result)
}

fn choose_offset_region(
    bytes: &[u8],
    table: usize,
    count: usize,
    value: u8,
    label: &str,
) -> Result<usize, String> {
    if count == 0 || count > 256 {
        return Err(format!("invalid {label} count {count}"));
    }
    let mut last = None;
    for index in 0..count {
        let offset = be_u32(bytes, table + index * 4, label)? as usize;
        checked_slice(bytes, offset, 4, label)?;
        last = Some(offset);
        if value <= bytes[offset] {
            return Ok(offset);
        }
    }
    last.ok_or_else(|| format!("empty {label}"))
}

struct InstrumentEffects {
    volume: f32,
    pitch: f32,
    pan: Option<f32>,
    fxmix: Option<f32>,
}

fn instrument_effects(
    bytes: &[u8],
    tables: [usize; 2],
    key: u8,
    velocity: u8,
    rng: &mut u32,
) -> Result<InstrumentEffects, String> {
    let mut effects = InstrumentEffects {
        volume: 1.0,
        pitch: 1.0,
        pan: None,
        fxmix: None,
    };
    if tables[0] != usize::MAX {
        for index in 0..2 {
            let offset = be_u32(bytes, tables[0] + index * 4, "random effect offset")? as usize;
            if offset == 0 {
                continue;
            }
            checked_slice(bytes, offset, 0x0c, "random instrument effect")?;
            let random = jaudio_random(rng) * 2.0 - 0.999_999_9;
            let value = be_f32(bytes, offset + 4, "random effect center")?
                + random * be_f32(bytes, offset + 8, "random effect width")?;
            apply_instrument_effect(&mut effects, bytes[offset], value);
        }
    }
    if tables[1] != usize::MAX {
        for index in 0..2 {
            let offset = be_u32(bytes, tables[1] + index * 4, "sense effect offset")? as usize;
            if offset == 0 {
                continue;
            }
            checked_slice(bytes, offset, 0x0c, "sense instrument effect")?;
            let input = match bytes[offset + 1] {
                1 => velocity,
                2 => key,
                _ => 0,
            };
            let pivot = bytes[offset + 2];
            let low = be_f32(bytes, offset + 4, "sense effect low")?;
            let high = be_f32(bytes, offset + 8, "sense effect high")?;
            let value = if pivot == 0 || pivot == 0x7f {
                low + input as f32 * (high - low) / 127.0
            } else if input < pivot {
                low + (1.0 - low) * input as f32 / pivot as f32
            } else {
                1.0 + (high - 1.0) * (input - pivot) as f32 / (0x7f - pivot) as f32
            };
            apply_instrument_effect(&mut effects, bytes[offset], value);
        }
    }
    Ok(effects)
}

fn apply_instrument_effect(effects: &mut InstrumentEffects, target: u8, value: f32) {
    match target {
        0 => effects.volume *= value,
        1 => effects.pitch *= value,
        2 => effects.pan = Some(value),
        3 => effects.fxmix = Some(value),
        _ => {}
    }
}

fn jaudio_random(state: &mut u32) -> f32 {
    *state = state.wrapping_mul(0x0019_660d).wrapping_add(0x3c6e_f35f);
    f32::from_bits((*state >> 9) | 0x3f80_0000) - 1.0
}

fn parse_vmap(
    bytes: &[u8],
    offset: usize,
    parameters: RegionParameters,
) -> Result<InstrumentRegion, String> {
    Ok(InstrumentRegion {
        wave_id: be_u32(bytes, offset + 4, "wave id")? as u16,
        volume: parameters.volume * be_f32(bytes, offset + 8, "region volume")?,
        pitch: parameters.pitch * be_f32(bytes, offset + 0x0c, "region pitch")?,
        pan: parameters.pan,
        fxmix: parameters.fxmix,
        oscillators: parameters.oscillators,
        direct_release: parameters.direct_release,
    })
}

fn instrument_oscillators(bytes: &[u8], inst_offset: usize) -> Result<Vec<OscillatorSpec>, String> {
    let mut oscillators = Vec::new();
    for index in 0..2 {
        let osc_offset = be_u32(
            bytes,
            inst_offset + 0x10 + index * 4,
            "instrument oscillator offset",
        )? as usize;
        if osc_offset == 0 {
            continue;
        }
        checked_slice(bytes, osc_offset, 0x18, "instrument oscillator")?;
        let attack_offset = be_u32(bytes, osc_offset + 8, "attack table offset")? as usize;
        let release_offset = be_u32(bytes, osc_offset + 0x0c, "release table offset")? as usize;
        oscillators.push(OscillatorSpec {
            kind: bytes[osc_offset],
            rate: be_f32(bytes, osc_offset + 4, "oscillator rate")?.max(f32::EPSILON),
            attack: (attack_offset != 0)
                .then(|| parse_envelope_table(bytes, attack_offset))
                .transpose()?
                .map(Arc::new),
            release: (release_offset != 0)
                .then(|| parse_envelope_table(bytes, release_offset))
                .transpose()?
                .map(Arc::new),
            width: be_f32(bytes, osc_offset + 0x10, "oscillator width")?,
            vertex: be_f32(bytes, osc_offset + 0x14, "oscillator vertex")?,
        });
    }
    Ok(oscillators)
}

fn parse_envelope_table(bytes: &[u8], mut offset: usize) -> Result<Vec<EnvelopeCommand>, String> {
    let mut commands = Vec::new();
    for _ in 0..256 {
        let command = EnvelopeCommand {
            mode: be_i16(bytes, offset, "envelope mode")?,
            time: be_i16(bytes, offset + 2, "envelope time")?.max(0) as u16,
            value: be_i16(bytes, offset + 4, "envelope value")?,
        };
        offset += 6;
        commands.push(command);
        if command.mode > 10 {
            return Ok(commands);
        }
    }
    Err("envelope table has no terminator".to_string())
}

// Fixed AFC predictor table uploaded by Sunshine's JAudio driver to the DSP.
const AFC_FILTERS: [(i32, i32); 16] = [
    (0x0000, 0x0000),
    (0x0800, 0x0000),
    (0x0000, 0x0800),
    (0x0400, 0x0400),
    (0x1000, -0x0800),
    (0x0e00, -0x0600),
    (0x0c00, -0x0400),
    (0x1200, -0x0a00),
    (0x1068, -0x08c8),
    (0x12c0, -0x08fc),
    (0x1400, -0x0c00),
    (0x0800, -0x0800),
    (0x0400, -0x0400),
    (-0x0400, 0x0400),
    (-0x0400, 0x0000),
    (-0x0800, 0x0000),
];

#[derive(Clone, Copy)]
enum AfcQuality {
    High,
    Low,
}

fn decode_afc(encoded: &[u8], sample_count: usize, quality: AfcQuality) -> Vec<f32> {
    let mut output = Vec::with_capacity(sample_count);
    let mut hist1 = 0i32;
    let mut hist2 = 0i32;
    let block_bytes = match quality {
        AfcQuality::High => 9,
        AfcQuality::Low => 5,
    };
    for frame in encoded.chunks(block_bytes) {
        if frame.len() < block_bytes || output.len() >= sample_count {
            break;
        }
        let delta = 1i32 << ((frame[0] >> 4) & 0x0f);
        let (coef1, coef2) = AFC_FILTERS[(frame[0] & 0x0f) as usize];
        let mut residuals = [0i32; 16];
        match quality {
            AfcQuality::High => {
                for (index, packed) in frame[1..].iter().enumerate() {
                    for (half, nibble) in [packed >> 4, packed & 0x0f].into_iter().enumerate() {
                        residuals[index * 2 + half] = if nibble >= 8 {
                            nibble as i32 - 16
                        } else {
                            nibble as i32
                        };
                    }
                }
            }
            AfcQuality::Low => {
                for (index, packed) in frame[1..].iter().enumerate() {
                    for quarter in 0..4 {
                        let bits = (packed >> (6 - quarter * 2)) & 3;
                        residuals[index * 4 + quarter] = (if bits >= 2 {
                            bits as i32 - 4
                        } else {
                            bits as i32
                        }) * 4;
                    }
                }
            }
        }
        for residual in residuals {
            if output.len() >= sample_count {
                break;
            }
            let sample = ((delta * residual * 2048 + coef1 * hist1 + coef2 * hist2) >> 11)
                .clamp(i16::MIN as i32, i16::MAX as i32);
            hist2 = hist1;
            hist1 = sample;
            output.push(sample as f32 / 32768.0);
        }
    }
    output
}

// First 0x100 signed coefficients from JAudio's DSPRES_FILTER. Sunshine's DSP
// selects one four-tap row using the upper six bits of the 12-bit phase.
const RESAMPLE_FILTER: [i16; 256] = [
    3129, 26285, 3398, -33, 2873, 26262, 3679, -40, 2628, 26217, 3971, -48, 2394, 26150, 4276, -56,
    2173, 26061, 4592, -65, 1963, 25950, 4920, -74, 1764, 25817, 5260, -84, 1576, 25663, 5611, -95,
    1399, 25487, 5974, -106, 1233, 25291, 6347, -118, 1077, 25075, 6732, -130, 932, 24838, 7127,
    -143, 796, 24583, 7532, -156, 671, 24309, 7947, -170, 554, 24016, 8371, -184, 446, 23706, 8804,
    -198, 347, 23379, 9246, -212, 257, 23036, 9696, -226, 174, 22678, 10153, -240, 99, 22304,
    10618, -254, 31, 21917, 11088, -268, -30, 21517, 11564, -280, -84, 21104, 12045, -293, -132,
    20679, 12531, -304, -173, 20244, 13020, -314, -210, 19799, 13512, -323, -241, 19345, 14006,
    -330, -267, 18882, 14501, -336, -289, 18413, 14997, -340, -306, 17937, 15493, -341, -320,
    17456, 15988, -340, -330, 16970, 16480, -337, -337, 16480, 16970, -330, -340, 15988, 17456,
    -320, -341, 15493, 17937, -306, -340, 14997, 18413, -289, -336, 14501, 18882, -267, -330,
    14006, 19345, -241, -323, 13512, 19799, -210, -314, 13020, 20244, -173, -304, 12531, 20679,
    -132, -293, 12045, 21104, -84, -280, 11564, 21517, -30, -268, 11088, 21917, 31, -254, 10618,
    22304, 99, -240, 10153, 22678, 174, -226, 9696, 23036, 257, -212, 9246, 23379, 347, -198, 8804,
    23706, 446, -184, 8371, 24016, 554, -170, 7947, 24309, 671, -156, 7532, 24583, 796, -143, 7127,
    24838, 932, -130, 6732, 25075, 1077, -118, 6347, 25291, 1233, -106, 5974, 25487, 1399, -95,
    5611, 25663, 1576, -84, 5260, 25817, 1764, -74, 4920, 25950, 1963, -65, 4592, 26061, 2173, -56,
    4276, 26150, 2394, -48, 3971, 26217, 2628, -40, 3679, 26262, 2873, -33, 3398, 26285, 3129,
];

#[derive(Clone)]
struct Track {
    pc: usize,
    wait: u32,
    finished: bool,
    transpose: i16,
    registers: [u16; 64],
    volume: f32,
    pitch: f32,
    pan: f32,
    fxmix: f32,
    external_volume: f32,
    external_pitch: f32,
    external_fxmix: f32,
    tempo: u16,
    timebase: u16,
    call_stack: Vec<usize>,
    loop_stack: Vec<(usize, u16)>,
    slots: [Option<u64>; 8],
    parent: Option<usize>,
    children: [Option<usize>; 16],
    local_tempo: bool,
    tick_fraction: f64,
    inherit_parent_mix: bool,
    self_oscillators: [VoiceOscillator; 2],
    self_osc_routes: [u8; 2],
    // JASystem::TTrackPort stores one value array shared by both directions.
    // Import/export flags only describe who most recently has unread data;
    // writing an export must overwrite an earlier imported value on that port.
    port_values: [u16; 16],
    import_flags: u16,
    export_flags: u16,
    interrupt_offsets: [Option<usize>; 8],
    interrupt_stack: Vec<(usize, u32)>,
    pending_interrupts: u16,
    interrupts_enabled: bool,
    interrupt_timer_repeats: u8,
    interrupt_timer_interval: u32,
    interrupt_timer_remaining: u32,
    timed_moves: [Option<TimedMove>; 12],
    pending_closes: Vec<usize>,
}

#[derive(Clone, Copy)]
struct TimedMove {
    amount: f32,
    remaining: u32,
}

impl Track {
    fn new(pc: usize) -> Self {
        let mut registers = [0; 64];
        registers[6] = 0x00f0;
        registers[14] = 12;
        Self {
            pc,
            wait: 0,
            finished: false,
            transpose: 0,
            registers,
            volume: 1.0,
            pitch: 0.0,
            pan: 0.5,
            fxmix: 0.0,
            external_volume: 1.0,
            external_pitch: 1.0,
            external_fxmix: 0.0,
            tempo: 120,
            timebase: 48,
            call_stack: Vec::new(),
            loop_stack: Vec::new(),
            slots: [None; 8],
            parent: None,
            children: [None; 16],
            local_tempo: false,
            tick_fraction: 0.0,
            inherit_parent_mix: false,
            self_oscillators: [default_track_envelope(), default_track_vibrato()],
            // JASTrack::initTrack starts oscillator 1 on the track-local route.
            self_osc_routes: [0x0f, 0x0e],
            port_values: [0; 16],
            import_flags: 0,
            export_flags: 0,
            interrupt_offsets: [None; 8],
            interrupt_stack: Vec::new(),
            pending_interrupts: 0,
            interrupts_enabled: true,
            interrupt_timer_repeats: 0,
            interrupt_timer_interval: 0,
            interrupt_timer_remaining: 0,
            timed_moves: [None; 12],
            pending_closes: Vec::new(),
        }
    }

    fn new_child(pc: usize, parent_index: usize, parent: &Track, mode: u8) -> Self {
        let mut child = Self::new(pc);
        child.parent = Some(parent_index);
        child.tempo = parent.tempo;
        child.timebase = parent.timebase;
        child.inherit_parent_mix = mode & 1 == 0;
        if mode & 2 == 0 {
            child.registers = parent.registers;
        }
        child
    }

    fn reg(&self, register: u8) -> u16 {
        match register {
            0x20 => self.registers[6] >> 8,
            0x21 => self.registers[6] & 0xff,
            0x22 => (self.registers[0] << 8) | (self.registers[1] & 0xff),
            0x2c => self
                .children
                .iter()
                .enumerate()
                .fold(0u16, |mask, (index, child)| {
                    mask | (u16::from(child.is_some()) << index)
                }),
            _ => self.registers.get(register as usize).copied().unwrap_or(0),
        }
    }

    fn set_reg(&mut self, register: u8, value: u16) {
        match register {
            0..=2 => self.registers[register as usize] = sign_extend_8(value as u8),
            0x20 => self.registers[6] = (value << 8) | (self.registers[6] & 0xff),
            0x21 => self.registers[6] = (self.registers[6] & 0xff00) | (value & 0xff),
            0x22 => {
                self.registers[0] = sign_extend_8((value >> 8) as u8);
                self.registers[1] = sign_extend_8(value as u8);
            }
            _ if (register as usize) < self.registers.len() => {
                self.registers[register as usize] = value
            }
            _ => {}
        }
        self.registers[3] = value;
    }

    fn set_simple_oscillator(&mut self, kind: u8) {
        let (index, oscillator) = match kind {
            0 => (1, default_track_vibrato()),
            1 => (0, default_track_tremolo()),
            2 => (1, default_track_tremolo()),
            _ => return,
        };
        self.self_oscillators[index] = oscillator;
    }

    fn set_simple_envelope(
        &mut self,
        sequence: &[u8],
        kind: u8,
        offset: usize,
    ) -> Result<(), String> {
        let commands = Arc::new(parse_envelope_table(sequence, offset)?);
        match kind {
            0 => {
                let release = self.self_oscillators[0].spec.release.clone();
                self.self_oscillators[0] = VoiceOscillator::new(OscillatorSpec {
                    kind: 0,
                    rate: 1.0,
                    attack: Some(commands),
                    release,
                    width: 1.0,
                    vertex: 0.0,
                });
            }
            1 => {
                self.self_oscillators[0].spec.release = Some(commands);
            }
            _ => {}
        }
        Ok(())
    }

    fn set_simple_adsr(&mut self, args: &[u32]) {
        let attack = Arc::new(vec![
            EnvelopeCommand {
                mode: 0,
                time: args[0] as u16,
                value: 0x7fff,
            },
            EnvelopeCommand {
                mode: 0,
                time: args[1] as u16,
                value: 0x7fff,
            },
            EnvelopeCommand {
                mode: 0,
                time: args[2] as u16,
                value: args[3] as u16 as i16,
            },
            EnvelopeCommand {
                mode: 14,
                time: 0,
                value: 0,
            },
        ]);
        let release = Arc::new(vec![
            EnvelopeCommand {
                mode: 0,
                time: args[4] as u16,
                value: 0,
            },
            EnvelopeCommand {
                mode: 15,
                time: 1,
                value: 0,
            },
        ]);
        self.self_oscillators[0] = VoiceOscillator::new(OscillatorSpec {
            kind: 0,
            rate: 1.0,
            attack: Some(attack),
            release: Some(release),
            width: 1.0,
            vertex: 0.0,
        });
    }

    fn set_oscillator_parameter(&mut self, target: u8, value: f32) {
        let Some(index) = target.checked_sub(6).map(|value| value as usize / 3) else {
            return;
        };
        let Some(oscillator) = self.self_oscillators.get_mut(index) else {
            return;
        };
        match (target - 6) % 3 {
            0 => oscillator.spec.width = value,
            1 => oscillator.spec.rate = value.max(f32::EPSILON),
            2 => oscillator.spec.vertex = value,
            _ => unreachable!(),
        }
    }

    fn set_oscillator_route(&mut self, encoded: u8) {
        let index = (encoded >> 4) as usize;
        let Some(route) = self.self_osc_routes.get_mut(index) else {
            return;
        };
        let new_route = encoded & 0x0f;
        if new_route == 0x0e && *route != 0x0e {
            let spec = self.self_oscillators[index].spec.clone();
            self.self_oscillators[index] = VoiceOscillator::new(spec);
        }
        *route = new_route;
    }

    fn apply_voice_oscillator_routes(&self, oscillators: &mut Vec<VoiceOscillator>) {
        for source in 0..self.self_osc_routes.len() {
            let route = self.self_osc_routes[source];
            if route >= 0x0e {
                continue;
            }
            let (destination, inherit_attack) = match route {
                0..=3 => (route as usize, false),
                4..=7 => ((route - 4) as usize, true),
                // Routes 8..=11 copy the channel oscillator back into the
                // track before overwriting it. They do not alter a newly
                // created voice's effective oscillator definition.
                _ => continue,
            };
            while oscillators.len() <= destination {
                oscillators.push(default_volume_oscillator());
            }
            let mut spec = self.self_oscillators[source].spec.clone();
            if inherit_attack {
                spec.attack = oscillators[destination].spec.attack.clone();
            }
            oscillators[destination] = VoiceOscillator::new(spec);
        }
    }

    fn set_interrupt_timer(&mut self, repeats: u8, interval: u32) {
        self.interrupt_timer_repeats = repeats;
        self.interrupt_timer_interval = interval;
        self.interrupt_timer_remaining = interval;
    }

    fn advance_interrupts(&mut self) {
        if self.interrupt_offsets[7].is_some() {
            self.pending_interrupts |= 1 << 7;
        }
        if self.interrupt_timer_remaining == 0 {
            return;
        }
        self.interrupt_timer_remaining -= 1;
        if self.interrupt_timer_remaining != 0 {
            return;
        }
        if self.interrupt_offsets[6].is_some() {
            self.pending_interrupts |= 1 << 6;
        }
        if self.interrupt_timer_repeats == 0 {
            self.interrupt_timer_remaining = self.interrupt_timer_interval;
        } else {
            self.interrupt_timer_repeats -= 1;
            if self.interrupt_timer_repeats != 0 {
                self.interrupt_timer_remaining = self.interrupt_timer_interval;
            }
        }
    }

    fn time_parameter(&self, target: u8) -> f32 {
        match target {
            0 => self.volume,
            1 => self.pitch,
            2 => self.fxmix,
            3 => self.pan,
            6..=11 => {
                let index = (target - 6) as usize / 3;
                let oscillator = &self.self_oscillators[index].spec;
                match (target - 6) % 3 {
                    0 => oscillator.width,
                    1 => oscillator.rate,
                    _ => oscillator.vertex,
                }
            }
            _ => 0.0,
        }
    }

    fn set_time_parameter(&mut self, target: u8, value: f32) {
        match target {
            0 => self.volume = value,
            1 => self.pitch = value,
            2 => self.fxmix = value,
            3 => self.pan = value,
            6..=11 => self.set_oscillator_parameter(target, value),
            _ => {}
        }
    }

    fn move_time_parameter(&mut self, target: u8, value: f32, duration: u32) {
        let current = self.time_parameter(target);
        let Some(slot) = self.timed_moves.get_mut(target as usize) else {
            return;
        };
        if duration == 0 {
            *slot = None;
            self.set_time_parameter(target, value);
            return;
        }
        *slot = Some(TimedMove {
            amount: (value - current) / duration as f32,
            remaining: duration,
        });
    }

    fn advance_timed_parameters(&mut self) {
        for target in 0..self.timed_moves.len() {
            let Some(mut movement) = self.timed_moves[target] else {
                continue;
            };
            self.set_time_parameter(
                target as u8,
                self.time_parameter(target as u8) + movement.amount,
            );
            movement.remaining -= 1;
            self.timed_moves[target] = (movement.remaining != 0).then_some(movement);
        }
    }

    fn next_self_modulation(&mut self) -> TrackModulation {
        let mut modulation = TrackModulation::default();
        for index in 0..2 {
            if self.self_osc_routes[index] != 0x0e {
                continue;
            }
            let kind = self.self_oscillators[index].spec.kind;
            let (value, stopped) = self.self_oscillators[index].next_value();
            if stopped {
                continue;
            }
            match kind {
                0 => modulation.volume *= value,
                1 => modulation.pitch *= value,
                2 => modulation.pan *= value,
                3 => modulation.fxmix *= value,
                _ => {}
            }
        }
        modulation
    }

    fn write_import_port(&mut self, port: usize, value: u16) {
        if port >= self.port_values.len() {
            return;
        }
        self.port_values[port] = value;
        self.import_flags |= 1 << port;
        if port <= 1 {
            self.pending_interrupts |= 1 << if port == 0 { 3 } else { 4 };
        }
    }

    fn read_import_port(&mut self, port: usize) -> u16 {
        self.import_flags &= !(1 << port);
        self.port_values[port]
    }

    fn write_export_port(&mut self, port: usize, value: u16) {
        self.port_values[port] = value;
        self.export_flags |= 1 << port;
    }

    fn begin_pending_interrupt(&mut self) -> bool {
        if !self.interrupts_enabled || !self.interrupt_stack.is_empty() {
            return false;
        }
        let Some(kind) = (0..self.interrupt_offsets.len())
            .find(|kind| self.pending_interrupts & (1 << kind) != 0)
        else {
            return false;
        };
        self.pending_interrupts &= !(1 << kind);
        let Some(offset) = self.interrupt_offsets[kind] else {
            return false;
        };
        self.interrupt_stack.push((self.pc, self.wait));
        self.wait = 0;
        self.pc = offset;
        true
    }
}

#[derive(Clone, Copy)]
struct EnvelopeSegment {
    start: f32,
    target: f32,
    elapsed: usize,
    frames: usize,
    curve: u8,
}

#[derive(Clone)]
enum OscillatorState {
    Table {
        commands: Arc<Vec<EnvelopeCommand>>,
        index: usize,
        segment: Option<EnvelopeSegment>,
    },
    Sustain,
    Direct(EnvelopeSegment),
    Stopped,
}

#[derive(Clone)]
struct VoiceOscillator {
    spec: OscillatorSpec,
    state: OscillatorState,
    phase: f32,
}

impl VoiceOscillator {
    fn new(spec: OscillatorSpec) -> Self {
        let state = if let Some(commands) = &spec.attack {
            OscillatorState::Table {
                commands: Arc::clone(commands),
                index: 0,
                segment: None,
            }
        } else {
            OscillatorState::Sustain
        };
        Self {
            phase: if spec.attack.is_some() { 0.0 } else { 1.0 },
            spec,
            state,
        }
    }

    fn begin_release(&mut self, direct: Option<u16>) {
        if matches!(
            self.state,
            OscillatorState::Stopped | OscillatorState::Direct(_)
        ) {
            return;
        }
        if let Some(encoded) = direct.filter(|value| *value != 0) {
            self.start_direct_release(encoded);
        } else if let Some(commands) = &self.spec.release {
            self.state = OscillatorState::Table {
                commands: Arc::clone(commands),
                index: 0,
                segment: None,
            };
        } else {
            self.start_direct_release(0x10);
        }
    }

    fn force_stop(&mut self) {
        self.start_direct_release(15);
    }

    fn start_direct_release(&mut self, encoded: u16) {
        let release = ReleaseSpec::direct(encoded);
        self.state = OscillatorState::Direct(EnvelopeSegment {
            start: self.phase,
            target: 0.0,
            elapsed: 0,
            frames: release.frames(),
            curve: release.curve,
        });
    }

    fn next_value(&mut self) -> (f32, bool) {
        loop {
            match &mut self.state {
                OscillatorState::Table {
                    commands,
                    index,
                    segment,
                } => {
                    if let Some(active) = segment {
                        self.phase = envelope_segment_value(*active);
                        active.elapsed += 1;
                        if active.elapsed >= active.frames {
                            self.phase = active.target;
                            *segment = None;
                        }
                        return (self.output(), false);
                    }
                    let Some(command) = commands.get(*index).copied() else {
                        self.state = OscillatorState::Stopped;
                        continue;
                    };
                    match command.mode {
                        0..=3 => {
                            *index += 1;
                            let target = command.value as f32 / 32768.0;
                            if command.time == 0 {
                                self.phase = target;
                                continue;
                            }
                            let frames = ((command.time as f32 * OUTPUT_RATE as f32)
                                / (600.0 * self.spec.rate))
                                .round()
                                .max(1.0) as usize;
                            *segment = Some(EnvelopeSegment {
                                start: self.phase,
                                target,
                                elapsed: 0,
                                frames,
                                curve: command.mode as u8,
                            });
                        }
                        13 => {
                            *index = (command.value.max(0) as usize).min(commands.len());
                        }
                        14 => {
                            self.state = OscillatorState::Sustain;
                        }
                        15 => {
                            self.state = OscillatorState::Stopped;
                        }
                        mode => {
                            let _ = mode;
                            self.state = OscillatorState::Stopped;
                        }
                    }
                }
                OscillatorState::Sustain => return (self.output(), false),
                OscillatorState::Direct(segment) => {
                    self.phase = envelope_segment_value(*segment);
                    segment.elapsed += 1;
                    if segment.elapsed >= segment.frames {
                        self.phase = 0.0;
                        self.state = OscillatorState::Stopped;
                    }
                    return (self.output(), false);
                }
                OscillatorState::Stopped => return (0.0, true),
            }
        }
    }

    fn output(&self) -> f32 {
        self.spec.vertex + self.phase * self.spec.width
    }
}

fn default_volume_oscillator() -> VoiceOscillator {
    VoiceOscillator::new(OscillatorSpec {
        kind: 0,
        rate: 1.0,
        attack: None,
        release: None,
        width: 1.0,
        vertex: 0.0,
    })
}

fn default_track_envelope() -> VoiceOscillator {
    VoiceOscillator::new(OscillatorSpec {
        kind: 0,
        rate: 1.0,
        attack: None,
        release: Some(Arc::new(vec![
            EnvelopeCommand {
                mode: 0,
                time: 10,
                value: 0,
            },
            EnvelopeCommand {
                mode: 15,
                time: 1,
                value: 0,
            },
        ])),
        width: 1.0,
        vertex: 0.0,
    })
}

fn default_track_vibrato() -> VoiceOscillator {
    VoiceOscillator::new(OscillatorSpec {
        kind: 1,
        rate: 0.5,
        attack: Some(Arc::new(vec![
            EnvelopeCommand {
                mode: 0,
                time: 0,
                value: 0,
            },
            EnvelopeCommand {
                mode: 0,
                time: 12,
                value: 0x7fff,
            },
            EnvelopeCommand {
                mode: 0,
                time: 12,
                value: 0,
            },
            EnvelopeCommand {
                mode: 0,
                time: 12,
                value: -0x4000,
            },
            EnvelopeCommand {
                mode: 0,
                time: 12,
                value: 0,
            },
            EnvelopeCommand {
                mode: 13,
                time: 0,
                value: 1,
            },
        ])),
        release: None,
        width: 0.0,
        vertex: 1.0,
    })
}

fn default_track_tremolo() -> VoiceOscillator {
    VoiceOscillator::new(OscillatorSpec {
        kind: 0,
        rate: 0.5,
        attack: Some(Arc::new(vec![
            EnvelopeCommand {
                mode: 0,
                time: 0,
                value: 0x7fff,
            },
            EnvelopeCommand {
                mode: 0,
                time: 20,
                value: 0,
            },
            EnvelopeCommand {
                mode: 0,
                time: 20,
                value: i16::MIN + 1,
            },
            EnvelopeCommand {
                mode: 0,
                time: 20,
                value: 0,
            },
            EnvelopeCommand {
                mode: 0,
                time: 20,
                value: 0x7fff,
            },
            EnvelopeCommand {
                mode: 13,
                time: 0,
                value: 1,
            },
        ])),
        release: None,
        width: 0.0,
        vertex: 1.0,
    })
}

#[derive(Clone, Copy)]
struct TrackModulation {
    volume: f32,
    pitch: f32,
    pan: f32,
    fxmix: f32,
}

impl Default for TrackModulation {
    fn default() -> Self {
        Self {
            volume: 1.0,
            pitch: 1.0,
            pan: 1.0,
            fxmix: 1.0,
        }
    }
}

fn envelope_segment_value(segment: EnvelopeSegment) -> f32 {
    let progress = segment.elapsed as f32 / segment.frames.max(1) as f32;
    envelope_curve(segment.curve, segment.start, segment.target, progress)
}

struct Voice {
    id: u64,
    wave: Arc<DecodedWave>,
    position: f64,
    base_step: f64,
    base_volume: f32,
    instrument_pan: f32,
    instrument_fxmix: f32,
    oscillators: Vec<VoiceOscillator>,
    direct_release: u16,
    releasing: bool,
    ticks_remaining: Option<u32>,
    owner_track: usize,
}

#[derive(Clone, Copy)]
enum TrackRelation {
    Child,
    Sibling,
}

#[derive(Clone, Copy)]
struct PendingTrack {
    offset: usize,
    slot: u8,
    mode: u8,
    relation: TrackRelation,
}

fn render_bgm_preview(base_root: &Path, bgm_id: u32, seconds: f32) -> Result<Vec<f32>, String> {
    let mut assets = AudioAssets::load(base_root)?;
    let sequence = assets.sequence_for_bgm(bgm_id)?.to_vec();
    render_sequence_preview(&mut assets, sequence, seconds, None)
}

fn render_sound_preview(base_root: &Path, sound_id: u32, seconds: f32) -> Result<Vec<f32>, String> {
    let mut assets = AudioAssets::load(base_root)?;
    let sound_trigger = assets.sound_preview_trigger(sound_id)?;
    // JAIBasic starts archive entry zero (ID 0x80000800) as Sunshine's
    // persistent sound-effect dispatcher. Individual SE IDs are delivered to
    // its category tracks through imported ports and interrupt 3.
    let sequence = assets.sequence_for_entry(0)?.to_vec();
    render_sequence_preview(&mut assets, sequence, seconds, Some(sound_trigger))
}

fn render_sequence_preview(
    assets: &mut AudioAssets,
    sequence: Vec<u8>,
    seconds: f32,
    sound_trigger: Option<SoundPreviewTrigger>,
) -> Result<Vec<f32>, String> {
    let mut tracks = vec![Track::new(0)];
    let mut voices = Vec::<Voice>::new();
    let mut next_voice_id = 1u64;
    let max_frames = (OUTPUT_RATE as f32 * seconds) as usize;
    let mut output = Vec::with_capacity(max_frames * 2);
    let mut reverb_send = Vec::with_capacity(max_frames * 2);
    let mut commands = 0usize;
    let mut sequence_frame_clock = 0.0f64;
    let mut mixed_sequence_frames = 0usize;
    let mut sound_triggered = sound_trigger.is_none();
    let mut voice_errors = Vec::new();

    while output.len() / 2 < max_frames && tracks.iter().any(|track| !track.finished) {
        let mut track_index = 0usize;
        let mut advanced = Vec::with_capacity(tracks.len());
        while track_index < tracks.len() {
            advanced.resize(tracks.len(), false);
            if tracks[track_index].finished || !track_tick_due(&mut tracks, track_index, &advanced)
            {
                track_index += 1;
                continue;
            }
            advanced[track_index] = true;
            tracks[track_index].advance_interrupts();
            let mut children = Vec::new();
            let interrupted = tracks[track_index].begin_pending_interrupt();
            if interrupted || sequence_tick_ready(&mut tracks[track_index]) {
                process_track(
                    &sequence,
                    &mut tracks[track_index],
                    track_index,
                    assets,
                    &mut voices,
                    &mut children,
                    &mut next_voice_id,
                    &mut commands,
                    &mut voice_errors,
                )
                .map_err(|error| {
                    format!(
                        "{error} on track {track_index} at sequence offset 0x{:X} near {}",
                        tracks[track_index].pc,
                        hex_preview(&sequence, tracks[track_index].pc.saturating_sub(32), 64)
                    )
                })?;
            }
            let pending_closes = std::mem::take(&mut tracks[track_index].pending_closes);
            for child in pending_closes {
                close_track_tree(&mut tracks, &mut voices, child);
            }
            tracks[track_index].advance_timed_parameters();
            if commands > MAX_COMMANDS_PER_TICK * (output.len() / 2 + 1) {
                return Err("sequence exceeded the preview command safety limit".to_string());
            }
            for pending in children {
                let parent_index = match pending.relation {
                    TrackRelation::Child => Some(track_index),
                    TrackRelation::Sibling => tracks[track_index].parent,
                };
                let Some(parent_index) = parent_index else {
                    continue;
                };
                let previous = tracks[parent_index].children[pending.slot as usize];
                if let Some(previous) = previous {
                    close_track_tree(&mut tracks, &mut voices, previous);
                }
                // JASTrack::mainProc visits a newly opened child later in the
                // same sequence update, so it must not lose its first tick.
                let child = Track::new_child(
                    pending.offset,
                    parent_index,
                    &tracks[parent_index],
                    pending.mode,
                );
                let child_index = if let Some(previous) = previous {
                    tracks[previous] = child;
                    previous
                } else if let Some(reusable) = tracks
                    .iter()
                    .enumerate()
                    .skip(1)
                    .find_map(|(index, track)| track.finished.then_some(index))
                {
                    tracks[reusable] = child;
                    reusable
                } else {
                    if tracks.len() >= MAX_TRACKS {
                        return Err("sequence opened too many tracks".to_string());
                    }
                    let child_index = tracks.len();
                    tracks.push(child);
                    child_index
                };
                tracks[parent_index].children[pending.slot as usize] = Some(child_index);
            }
            track_index += 1;
        }
        if !sound_triggered {
            let trigger = sound_trigger.expect("checked above");
            let category = ((trigger.id >> 12) & 0x0f) as u16;
            let candidate = sound_dispatcher_track(&tracks, category);
            if let Some(index) = candidate {
                tracks[index].external_volume = trigger.volume;
                tracks[index].external_pitch = trigger.pitch;
                tracks[index].external_fxmix = trigger.fxmix;
                // JAIBasic sends these route parameters before the ID/start
                // ports even when their values are zero. Their import flags
                // are observable to the dispatcher sequence.
                tracks[index].write_import_port(3, 0);
                tracks[index].write_import_port(6, 0);
                tracks[index].write_import_port(4, (trigger.id & 0x3ff) as u16);
                tracks[index].write_import_port(0, 1);
                sound_triggered = true;
            }
        }
        let root = &tracks[0];
        sequence_frame_clock +=
            (OUTPUT_RATE as f64 * 60.0) / (root.tempo.max(1) as f64 * root.timebase.max(1) as f64);
        let target_frames = sequence_frame_clock.round().max(1.0) as usize;
        let tick_frames = target_frames.saturating_sub(mixed_sequence_frames);
        mixed_sequence_frames = target_frames;
        mix_voices(
            &mut output,
            &mut reverb_send,
            &mut voices,
            &mut tracks,
            tick_frames,
            max_frames,
        );
    }
    if output.is_empty() {
        return Err("the selected sequence produced no preview audio".to_string());
    }
    if sound_trigger.is_some() && !output.iter().any(|sample| sample.abs() > 0.0001) {
        let detail = if voice_errors.is_empty() {
            "the retail event did not emit a playable note without its in-game scene or actor parameters"
                .to_string()
        } else {
            voice_errors.join("; ")
        };
        return Err(format!(
            "the sound-effect dispatcher produced silence: {detail}"
        ));
    }
    apply_jaudio_reverb(&mut output, &reverb_send, &assets.fx_lines);
    if sound_trigger.is_some() {
        apply_preview_peak_guard(&mut output);
    }
    for sample in &mut output {
        *sample = sample.clamp(-1.0, 32767.0 / 32768.0);
    }
    Ok(output)
}

fn sound_dispatcher_track(tracks: &[Track], category: u16) -> Option<usize> {
    tracks
        .iter()
        .enumerate()
        .skip(1)
        .find_map(|(index, track)| {
            (track.export_flags & (1 << 9) != 0 && track.port_values[9] == category)
                .then_some(index)
        })
}

fn hex_preview(bytes: &[u8], offset: usize, size: usize) -> String {
    bytes
        .get(offset..offset.saturating_add(size).min(bytes.len()))
        .unwrap_or_default()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join("")
}

struct FxLineState {
    delay: Vec<f32>,
    block: usize,
    last_eight: [f32; 8],
}

fn apply_jaudio_reverb(
    output: &mut [f32],
    reverb_send: &[f32],
    configs: &[Option<FxLineConfig>; 4],
) {
    const DSP_FRAME: usize = 0x50;
    let mut states: [Option<FxLineState>; 4] = std::array::from_fn(|index| {
        let config = configs[index].as_ref()?;
        (config.enabled != 0 && config.circular_buffer_blocks != 0).then(|| FxLineState {
            delay: vec![0.0; config.circular_buffer_blocks * DSP_FRAME],
            block: 0,
            last_eight: [0.0; 8],
        })
    });
    if states.iter().all(Option::is_none) {
        return;
    }

    let frame_count = output.len() / 2;
    for frame_start in (0..frame_count).step_by(DSP_FRAME) {
        let valid_frames = DSP_FRAME.min(frame_count - frame_start);
        let mut reverb_buffers = [[0.0f32; DSP_FRAME]; 4];

        for line_index in 0..4 {
            let (Some(config), Some(state)) =
                (configs[line_index].as_ref(), states[line_index].as_mut())
            else {
                continue;
            };
            let delay_start = state.block * DSP_FRAME;
            let mut filtered = [0.0f32; DSP_FRAME + 8];
            filtered[..8].copy_from_slice(&state.last_eight);
            filtered[8..].copy_from_slice(&state.delay[delay_start..delay_start + DSP_FRAME]);
            state
                .last_eight
                .copy_from_slice(&filtered[DSP_FRAME..DSP_FRAME + 8]);

            if config.enabled & 1 != 0 {
                filter_reverb_block(&mut filtered, &config.filter_coefficients);
            }
            for destination in config.destinations {
                route_reverb_block(
                    output,
                    frame_start,
                    valid_frames,
                    &mut reverb_buffers,
                    &filtered[..DSP_FRAME],
                    destination,
                );
            }
            if config.enabled & 1 == 0 && config.enabled & 2 != 0 {
                filter_reverb_block(&mut filtered, &config.filter_coefficients);
            }
            reverb_buffers[line_index].copy_from_slice(&filtered[..DSP_FRAME]);
        }

        for frame in 0..valid_frames {
            reverb_buffers[0][frame] += reverb_send[(frame_start + frame) * 2];
            reverb_buffers[1][frame] += reverb_send[(frame_start + frame) * 2 + 1];
        }

        for line_index in 0..4 {
            let (Some(config), Some(state)) =
                (configs[line_index].as_ref(), states[line_index].as_mut())
            else {
                continue;
            };
            let delay_start = state.block * DSP_FRAME;
            state.delay[delay_start..delay_start + DSP_FRAME]
                .copy_from_slice(&reverb_buffers[line_index]);
            state.block = (state.block + 1) % config.circular_buffer_blocks;
        }
    }
}

fn apply_preview_peak_guard(samples: &mut [f32]) {
    const TARGET_PEAK: f32 = 0.95;
    let peak = samples
        .iter()
        .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    if peak <= TARGET_PEAK {
        return;
    }
    let gain = TARGET_PEAK / peak;
    for sample in samples {
        *sample *= gain;
    }
}

fn filter_reverb_block(samples: &mut [f32; 0x58], coefficients: &[f32; 8]) {
    for index in 0..0x50 {
        samples[index] = coefficients
            .iter()
            .enumerate()
            .map(|(tap, coefficient)| samples[index + tap] * coefficient)
            .sum::<f32>()
            .clamp(-1.0, 32767.0 / 32768.0);
    }
}

fn route_reverb_block(
    output: &mut [f32],
    frame_start: usize,
    valid_frames: usize,
    reverb_buffers: &mut [[f32; 0x50]; 4],
    source: &[f32],
    destination: FxDestination,
) {
    let reverb_index = match destination.buffer_id {
        0x0dc0 => Some(0),
        0x0e20 => Some(1),
        0x0e80 => Some(2),
        0x0ee0 => Some(3),
        _ => None,
    };
    if let Some(index) = reverb_index {
        for frame in 0..0x50 {
            reverb_buffers[index][frame] += source[frame] * destination.volume;
        }
        return;
    }
    let channel = match destination.buffer_id {
        0x0d00 => Some(0),
        0x0d60 => Some(1),
        _ => None,
    };
    if let Some(channel) = channel {
        for frame in 0..valid_frames {
            output[(frame_start + frame) * 2 + channel] += source[frame] * destination.volume;
        }
    }
}

fn track_tick_due(tracks: &mut [Track], index: usize, advanced: &[bool]) -> bool {
    let Some(parent) = tracks[index].parent else {
        return true;
    };
    if !advanced.get(parent).copied().unwrap_or(false) {
        return false;
    }
    if !tracks[index].local_tempo {
        return true;
    }
    let parent_tempo = f64::from(tracks[parent].tempo.max(1));
    let ratio = (f64::from(tracks[index].tempo.max(1)) / parent_tempo).min(1.0);
    tracks[index].tick_fraction += ratio;
    if tracks[index].tick_fraction < 1.0 {
        return false;
    }
    tracks[index].tick_fraction -= 1.0;
    true
}

fn close_track_tree(tracks: &mut [Track], voices: &mut [Voice], index: usize) {
    if tracks.get(index).is_none_or(|track| track.finished) {
        return;
    }
    let children = tracks[index].children;
    tracks[index].finished = true;
    tracks[index].slots = [None; 8];
    for voice in voices.iter_mut().filter(|voice| voice.owner_track == index) {
        begin_release(voice, None);
    }
    for child in children.into_iter().flatten() {
        close_track_tree(tracks, voices, child);
    }
}

fn sequence_tick_ready(track: &mut Track) -> bool {
    if track.wait > 0 {
        track.wait -= 1;
    }
    track.wait == 0
}

#[allow(clippy::too_many_arguments)]
fn process_track(
    sequence: &[u8],
    track: &mut Track,
    track_index: usize,
    assets: &mut AudioAssets,
    voices: &mut Vec<Voice>,
    children: &mut Vec<PendingTrack>,
    next_voice_id: &mut u64,
    commands: &mut usize,
    voice_errors: &mut Vec<String>,
) -> Result<(), String> {
    let mut trace = [0usize; 16];
    for _ in 0..MAX_COMMANDS_PER_TICK {
        *commands += 1;
        trace.rotate_left(1);
        trace[15] = track.pc;
        let flag = read_u8(sequence, &mut track.pc, "sequence command")?;
        if flag < 0x80 {
            let mode = read_u8(sequence, &mut track.pc, "note mode")?;
            let mut note = flag.wrapping_add(track.transpose as u8);
            if mode & 0x80 != 0 {
                note = track.reg(note).wrapping_add(track.transpose as u16) as u8;
            }
            let velocity_raw = read_u8(sequence, &mut track.pc, "note velocity")?;
            let velocity = if velocity_raw >= 0x80 {
                track.reg(velocity_raw - 0x80) as u8
            } else {
                velocity_raw
            };
            let (slot, duration, gate) = if mode & 7 == 0 {
                let gate_raw = read_u8(sequence, &mut track.pc, "note gate")?;
                let gate = if gate_raw >= 0x80 {
                    track.reg(gate_raw - 0x80) as u8
                } else {
                    gate_raw
                };
                let width = ((mode >> 3) & 3) as usize;
                let mut duration = 0u32;
                for _ in 0..width {
                    duration =
                        (duration << 8) | read_u8(sequence, &mut track.pc, "note duration")? as u32;
                }
                if width == 1 && duration >= 0x80 {
                    duration = u32::from(track.reg(duration as u8 - 0x80));
                }
                (0usize, Some(duration), gate)
            } else {
                let mut slot = (mode & 7) as usize;
                if (mode >> 3) & 3 != 0 {
                    slot = track.reg((slot - 1) as u8) as usize;
                }
                (slot.min(7), None, 100)
            };
            let bank = (track.registers[6] >> 8) as u8;
            let program = track.registers[6] as u8;
            let gate_ticks =
                duration.map(|duration| (duration as u64 * gate as u64 / 100).max(1) as u32);
            match assets.instrument_region(bank, program, note, velocity) {
                Ok((wave_bank, region)) => match assets.decoded_wave(wave_bank, region.wave_id) {
                    Ok(wave) => {
                        let semitones = note as f32 - wave.root_key as f32;
                        let base_step = wave.sample_rate as f64 / OUTPUT_RATE as f64
                            * 2.0f64.powf(semitones as f64 / 12.0)
                            * region.pitch as f64;
                        let id = *next_voice_id;
                        *next_voice_id += 1;
                        if let Some(previous) = track.slots[slot].take() {
                            if let Some(voice) =
                                voices.iter_mut().find(|voice| voice.id == previous)
                            {
                                begin_release(voice, None);
                            }
                        }
                        let mut oscillators: Vec<_> = region
                            .oscillators
                            .into_iter()
                            .map(VoiceOscillator::new)
                            .collect();
                        if oscillators.is_empty() {
                            oscillators.push(default_volume_oscillator());
                        }
                        track.apply_voice_oscillator_routes(&mut oscillators);
                        voices.push(Voice {
                            id,
                            wave,
                            position: 0.0,
                            base_step,
                            base_volume: (velocity as f32 / 127.0).powi(2) * region.volume,
                            instrument_pan: region.pan,
                            instrument_fxmix: region.fxmix,
                            oscillators,
                            direct_release: region.direct_release,
                            releasing: false,
                            ticks_remaining: gate_ticks,
                            owner_track: track_index,
                        });
                        track.slots[slot] = Some(id);
                    }
                    Err(error) => push_voice_error(voice_errors, error),
                },
                Err(error) => push_voice_error(voice_errors, error),
            }
            if let Some(duration) = duration {
                track.wait = duration.max(1);
                return Ok(());
            }
        } else if flag & 0xf0 == 0x80 && flag & 7 == 0 {
            track.wait = if flag == 0x80 {
                read_u8(sequence, &mut track.pc, "wait")? as u32
            } else {
                read_be_value(sequence, &mut track.pc, 2, "wait")?
            };
            if track.wait > 0 {
                return Ok(());
            }
        } else if flag & 0xf0 == 0x80 || flag == 0xf9 {
            let mut slot = (flag & 0x0f) as usize;
            let mut direct_release = None;
            if flag == 0xf9 {
                let encoded = read_u8(sequence, &mut track.pc, "registered note off")?;
                slot = track.reg(encoded & 7) as usize;
                if encoded & 0x80 != 0 {
                    direct_release = Some(read_u8(sequence, &mut track.pc, "note release")?);
                }
            } else if flag & 8 != 0 {
                slot -= 8;
                direct_release = Some(read_u8(sequence, &mut track.pc, "note release")?);
            }
            if let Some(id) = track.slots[slot.min(7)].take() {
                if let Some(voice) = voices.iter_mut().find(|voice| voice.id == id) {
                    let direct_release = direct_release.map(decode_note_release);
                    begin_release(voice, direct_release.filter(|release| *release != 0));
                }
            }
        } else if flag & 0xf0 == 0x90 {
            parse_time_param(sequence, track, flag & 0x0f)?;
        } else if flag & 0xf0 == 0xa0 {
            parse_register_param(sequence, track, flag & 0x0f)?;
        } else if flag & 0xf0 == 0xb0 {
            let command = read_u8(sequence, &mut track.pc, "registered command")?;
            let command = if flag & 8 != 0 {
                track.reg(command) as u8
            } else {
                command
            };
            let mut override_types = 0u16;
            if flag & 8 == 0 || flag & 7 != 0 {
                let mask = read_u8(sequence, &mut track.pc, "registered argument mask")?;
                for index in 0..=(flag & 7) {
                    if mask & (0x80 >> index) != 0 {
                        override_types |= 3 << (index * 2);
                    }
                }
            }
            if process_command(
                sequence,
                track,
                track_index,
                command,
                override_types,
                children,
                voices,
            )? {
                return Ok(());
            }
        } else if process_command(sequence, track, track_index, flag, 0, children, voices)? {
            return Ok(());
        }
        if track.finished {
            return Ok(());
        }
    }
    Err(format!(
        "sequence command loop did not yield; recent offsets: {trace:02X?}"
    ))
}

fn push_voice_error(errors: &mut Vec<String>, error: String) {
    if errors.len() < 8 && !errors.contains(&error) {
        errors.push(error);
    }
}

const COMMAND_ARGS: [(u16, u16); 64] = [
    (0, 0),
    (2, 0x0008),
    (2, 0x0008),
    (1, 0x0002),
    (0, 0),
    (0, 0),
    (1, 0),
    (1, 0x0002),
    (0, 0),
    (1, 0x0001),
    (0, 0),
    (2, 0),
    (2, 0x000c),
    (1, 0),
    (1, 0),
    (1, 0x0003),
    (2, 0x0005),
    (2, 0x000c),
    (2, 0x000c),
    (0, 0),
    (1, 0),
    (1, 0),
    (1, 0),
    (2, 0x0008),
    (5, 0x0155),
    (1, 0),
    (1, 0),
    (1, 0),
    (1, 0x0001),
    (2, 0x0004),
    (1, 0),
    (2, 0x0008),
    (1, 0),
    (0, 0),
    (0, 0),
    (0, 0),
    (2, 0x0004),
    (0, 0),
    (0, 0),
    (1, 0x0001),
    (0, 0),
    (0, 0),
    (1, 0x0002),
    (5, 0),
    (4, 0x0055),
    (1, 0x0002),
    (1, 0x0002),
    (3, 0),
    (1, 0),
    (1, 0),
    (3, 0x0028),
    (1, 0),
    (0, 0),
    (0, 0),
    (0, 0),
    (0, 0),
    (0, 0),
    (0, 0),
    (1, 0x0001),
    (0, 0),
    (0, 0),
    (1, 0x0001),
    (1, 0x0001),
    (0, 0),
];

fn process_command(
    sequence: &[u8],
    track: &mut Track,
    track_index: usize,
    command: u8,
    override_types: u16,
    children: &mut Vec<PendingTrack>,
    voices: &mut [Voice],
) -> Result<bool, String> {
    if command == 0xc4 || command == 0xc8 {
        let condition = read_u8(sequence, &mut track.pc, "branch condition")?;
        let offset = if condition & 0x80 != 0 {
            let register = read_u8(sequence, &mut track.pc, "branch register")?;
            let index = track.reg(register) as usize;
            if condition & 0x40 != 0 {
                let table = if condition & 0x20 != 0 {
                    let table_register = read_u8(sequence, &mut track.pc, "branch table register")?;
                    track.reg(table_register) as usize
                } else {
                    read_be_value(sequence, &mut track.pc, 3, "branch table")? as usize
                };
                let entry = table
                    .checked_add(index.saturating_mul(3))
                    .ok_or_else(|| "branch table offset overflow".to_string())?;
                let mut cursor = entry;
                read_be_value(sequence, &mut cursor, 3, "branch table entry")? as usize
            } else {
                index
            }
        } else {
            read_be_value(sequence, &mut track.pc, 3, "branch offset")? as usize
        };
        if condition_matches(track.reg(3), condition) {
            if command == 0xc4 {
                track.call_stack.push(track.pc);
            }
            track.pc = offset;
        }
        return Ok(false);
    }
    let index = command
        .checked_sub(0xc0)
        .ok_or_else(|| format!("invalid sequence command 0x{command:02X}"))?
        as usize;
    let (count, mut types) = COMMAND_ARGS[index];
    types |= override_types;
    let mut args = [0u32; 8];
    for value in args.iter_mut().take(count as usize) {
        *value = match types & 3 {
            0 => read_u8(sequence, &mut track.pc, "command argument")? as u32,
            1 => read_be_value(sequence, &mut track.pc, 2, "command argument")?,
            2 => read_be_value(sequence, &mut track.pc, 3, "command argument")?,
            _ => {
                let register = read_u8(sequence, &mut track.pc, "command register")?;
                track.reg(register) as u32
            }
        };
        types >>= 2;
    }
    match command {
        0xc1 | 0xc2 => {
            let encoded = args[0] as u8;
            let mode = if encoded & 0x20 != 0 {
                4
            } else {
                (encoded >> 6) & 3
            };
            children.push(PendingTrack {
                offset: args[1] as usize,
                slot: encoded & 0x0f,
                mode,
                relation: if command == 0xc1 {
                    TrackRelation::Child
                } else {
                    TrackRelation::Sibling
                },
            });
            // Opening is immediate in JASTrack. Yield so the owning track list
            // can install the child before this sequence observes register 0x2c.
            return Ok(true);
        }
        0xc6 if condition_matches(track.reg(3), args[0] as u8) => {
            if let Some(pc) = track.call_stack.pop() {
                track.pc = pc;
            } else {
                track.finished = true;
            }
        }
        0xc9 => track.loop_stack.push((track.pc, args[0] as u16)),
        0xca => {
            if let Some((pc, remaining)) = track.loop_stack.last_mut() {
                if *remaining == 0 || *remaining > 1 {
                    if *remaining > 1 {
                        *remaining -= 1;
                    }
                    track.pc = *pc;
                } else {
                    track.loop_stack.pop();
                }
            }
        }
        0xcb => {
            let port = (args[0] as usize).min(15);
            let value = track.read_import_port(port);
            track.set_reg(args[1] as u8, value);
        }
        0xcc => {
            let port = (args[0] as usize).min(15);
            track.write_export_port(port, args[1] as u16);
        }
        0xcd => track.set_reg(
            3,
            u16::from(track.import_flags & (1 << args[0].min(15)) != 0),
        ),
        0xce => track.set_reg(
            3,
            u16::from(track.export_flags & (1 << args[0].min(15)) != 0),
        ),
        0xcf | 0xea => {
            track.wait = args[0];
            return Ok(track.wait > 0);
        }
        0xd6 => track.set_simple_oscillator(args[0] as u8),
        0xd7 => track.set_simple_envelope(sequence, args[0] as u8, args[1] as usize)?,
        0xd8 => track.set_simple_adsr(&args[..5]),
        0xd9 => track.transpose = args[0] as u8 as i8 as i16,
        0xda => {
            let slot = (args[0] as usize).min(15);
            if let Some(child) = track.children[slot].take() {
                track.pending_closes.push(child);
            }
        }
        0xdf => {
            let kind = args[0] as usize;
            if let Some(slot) = track.interrupt_offsets.get_mut(kind) {
                *slot = Some(args[1] as usize);
            }
        }
        0xe0 => {
            let kind = args[0] as usize;
            if let Some(slot) = track.interrupt_offsets.get_mut(kind) {
                *slot = None;
            }
        }
        0xe1 => track.interrupts_enabled = true,
        0xe2 => track.interrupts_enabled = false,
        0xe3 => {
            track.interrupts_enabled = true;
            if let Some((pc, wait)) = track.interrupt_stack.pop() {
                track.pc = pc;
                track.wait = wait;
            }
            return Ok(true);
        }
        0xe4 => track.set_interrupt_timer(args[0] as u8, args[1]),
        0xe7 => track.set_reg(3, 0),
        0xe8 => {
            for voice in voices
                .iter_mut()
                .filter(|voice| voice.owner_track == track_index)
            {
                begin_release(voice, None);
            }
            track.slots = [None; 8];
        }
        0xe9 => {
            for voice in voices
                .iter_mut()
                .filter(|voice| voice.owner_track == track_index && voice.releasing)
            {
                for oscillator in &mut voice.oscillators {
                    oscillator.force_stop();
                }
            }
        }
        0xf0 => track.set_oscillator_route(args[0] as u8),
        0xfb => skip_printf(sequence, track)?,
        0xfd => {
            track.tempo = args[0].max(1) as u16;
            if track.parent.is_some() {
                track.local_tempo = true;
            }
        }
        0xfe => track.timebase = args[0].max(1) as u16,
        0xff => track.finished = true,
        _ => {}
    }
    Ok(track.finished)
}

fn parse_time_param(sequence: &[u8], track: &mut Track, mode: u8) -> Result<(), String> {
    let target = read_u8(sequence, &mut track.pc, "time parameter")?;
    let raw = match mode & 0x0c {
        0 => {
            let register = read_u8(sequence, &mut track.pc, "time parameter register")?;
            track.reg(register) as i16
        }
        4 => read_u8(sequence, &mut track.pc, "time parameter value")? as i16,
        8 => {
            let byte = read_u8(sequence, &mut track.pc, "time parameter value")? as u16;
            if byte & 0x80 != 0 {
                (byte << 8) as i16
            } else {
                ((byte << 8) | (byte << 1)) as i16
            }
        }
        _ => read_be_value(sequence, &mut track.pc, 2, "time parameter value")? as u16 as i16,
    };
    let duration = match mode & 3 {
        1 => {
            let register = read_u8(sequence, &mut track.pc, "time duration register")?;
            u32::from(track.reg(register))
        }
        2 => u32::from(read_u8(sequence, &mut track.pc, "time duration")?),
        3 => read_be_value(sequence, &mut track.pc, 2, "time duration")?,
        _ => 0,
    };
    let value = raw as f32 / 32768.0;
    track.move_time_parameter(target, value, duration);
    Ok(())
}

fn decode_note_release(encoded: u8) -> u16 {
    if encoded > 100 {
        u16::from(encoded - 98) * 20
    } else {
        u16::from(encoded)
    }
}

fn parse_register_param(sequence: &[u8], track: &mut Track, mut mode: u8) -> Result<(), String> {
    let mut operation = mode & 3;
    let mut source_kind = mode & 0x0c;
    if mode == 0x0b {
        operation = 0x0b;
        source_kind = 0;
    }
    if mode == 0x09 {
        mode = read_u8(sequence, &mut track.pc, "extended register operation")?;
        source_kind = mode & 0x0c;
        operation = mode & 0xf0;
    }
    let destination = read_u8(sequence, &mut track.pc, "register destination")?;
    let source = match source_kind {
        0 => {
            let reg = read_u8(sequence, &mut track.pc, "register source")?;
            track.reg(reg) as i16 as i32
        }
        4 => read_u8(sequence, &mut track.pc, "register immediate")? as i32,
        8 => {
            let byte = read_u8(sequence, &mut track.pc, "register immediate")? as u16;
            if byte & 0x80 != 0 {
                (byte << 8) as i16 as i32
            } else {
                ((byte << 8) | (byte << 1)) as i16 as i32
            }
        }
        _ => read_be_value(sequence, &mut track.pc, 2, "register immediate")? as u16 as i16 as i32,
    };
    let current = track.reg(destination) as i16 as i32;
    if operation == 2 {
        let product = current.wrapping_mul(source);
        track.set_reg(4, ((product as u32) >> 16) as u16);
        track.set_reg(5, product as u16);
        return Ok(());
    }
    if operation == 3 {
        track.registers[3] = current.wrapping_sub(source) as u16;
        return Ok(());
    }
    let result = match operation {
        0 => source,
        1 => current.wrapping_add(source),
        0x0b => current.wrapping_sub(source),
        0x10 => {
            if source < 0 {
                ((current as u16) >> (-source).min(15)) as i32
            } else {
                ((current as u16) << source.min(15)) as i32
            }
        }
        0x20 => {
            if source < 0 {
                current >> (-source).min(15)
            } else {
                current << source.min(15)
            }
        }
        0x30 => current & source,
        0x40 => current | source,
        0x50 => current ^ source,
        0x60 => -current,
        _ => source,
    };
    track.set_reg(destination, result as u16);
    Ok(())
}

fn skip_printf(sequence: &[u8], track: &mut Track) -> Result<(), String> {
    let mut substitutions = 0usize;
    loop {
        let byte = read_u8(sequence, &mut track.pc, "printf string")?;
        if byte == 0 {
            break;
        }
        if byte == b'\\' {
            let _ = read_u8(sequence, &mut track.pc, "printf escape")?;
        }
        if byte == b'%' {
            let _ = read_u8(sequence, &mut track.pc, "printf format")?;
            substitutions += 1;
        }
    }
    for _ in 0..substitutions {
        let _ = read_u8(sequence, &mut track.pc, "printf register")?;
    }
    Ok(())
}

fn condition_matches(value: u16, condition: u8) -> bool {
    match condition & 0x0f {
        0 => true,
        1 => value == 0,
        2 => value != 0,
        3 => value == 1,
        4 => value >= 0x8000,
        5 => value < 0x8000,
        _ => false,
    }
}

fn mix_voices(
    output: &mut Vec<f32>,
    reverb_send: &mut Vec<f32>,
    voices: &mut Vec<Voice>,
    tracks: &mut [Track],
    frames: usize,
    max_frames: usize,
) {
    let frames = frames.min(max_frames.saturating_sub(output.len() / 2));
    let mut track_modulations = vec![TrackModulation::default(); tracks.len()];
    for _ in 0..frames {
        for (modulation, track) in track_modulations.iter_mut().zip(tracks.iter_mut()) {
            *modulation = track.next_self_modulation();
        }
        let mut left = 0.0f32;
        let mut right = 0.0f32;
        let mut reverb_left = 0.0f32;
        let mut reverb_right = 0.0f32;
        for voice in voices.iter_mut() {
            let index = voice.position as usize;
            if voice.wave.loop_range.is_none() && index >= voice.wave.samples.len() {
                for oscillator in &mut voice.oscillators {
                    oscillator.state = OscillatorState::Stopped;
                }
                continue;
            }
            let mut osc_volume = 1.0f32;
            let mut osc_pitch = 1.0f32;
            let mut osc_pan = voice.instrument_pan;
            let mut osc_fxmix = voice.instrument_fxmix;
            let mut primary_stopped = false;
            for (index, oscillator) in voice.oscillators.iter_mut().enumerate() {
                let kind = oscillator.spec.kind;
                let (value, stopped) = oscillator.next_value();
                if index == 0 {
                    primary_stopped = stopped;
                }
                match kind {
                    0 => osc_volume *= value,
                    1 => osc_pitch *= value,
                    2 => osc_pan = value,
                    3 => osc_fxmix = value,
                    _ => {}
                }
            }
            if primary_stopped {
                continue;
            }
            let (track_volume, track_pitch) =
                effective_track_gain_and_pitch(tracks, &track_modulations, voice.owner_track);
            let track = &tracks[voice.owner_track];
            let modulation = track_modulations[voice.owner_track];
            let pan = (track.pan * modulation.pan + osc_pan - 0.5).clamp(0.0, 1.0);
            let fxmix = (track.fxmix * modulation.fxmix
                + osc_fxmix
                + effective_track_external_fxmix(tracks, voice.owner_track))
            .clamp(0.0, 1.0);
            let sample =
                resample_voice(voice) * voice.base_volume * track_volume * osc_volume.max(0.0);
            let left_gain = ((1.0 - pan) * std::f32::consts::FRAC_PI_2).sin();
            let right_gain = (pan * std::f32::consts::FRAC_PI_2).sin();
            left += sample * left_gain;
            right += sample * right_gain;
            let send_gain = (fxmix * std::f32::consts::FRAC_PI_2).sin();
            reverb_left += sample * left_gain * send_gain;
            reverb_right += sample * right_gain * send_gain;
            let pitch = track_pitch as f64 * osc_pitch.max(0.0) as f64;
            voice.position += voice.base_step * pitch;
        }
        output.push(left);
        output.push(right);
        reverb_send.push(reverb_left);
        reverb_send.push(reverb_right);
        voices.retain(|voice| {
            !matches!(
                voice
                    .oscillators
                    .first()
                    .map(|oscillator| &oscillator.state),
                Some(OscillatorState::Stopped)
            )
        });
    }
    for voice in voices.iter_mut().filter(|voice| !voice.releasing) {
        if let Some(remaining) = &mut voice.ticks_remaining {
            *remaining = remaining.saturating_sub(1);
            if *remaining == 0 {
                begin_release(voice, None);
            }
        }
    }
}

fn begin_release(voice: &mut Voice, direct: Option<u16>) {
    if voice.releasing {
        return;
    }
    voice.releasing = true;
    voice.ticks_remaining = None;
    let direct = direct.or((voice.direct_release != 0).then_some(voice.direct_release));
    for (index, oscillator) in voice.oscillators.iter_mut().enumerate() {
        oscillator.begin_release(if index == 0 { direct } else { None });
    }
}

fn effective_track_gain_and_pitch(
    tracks: &[Track],
    modulations: &[TrackModulation],
    mut index: usize,
) -> (f32, f32) {
    let mut volume = 1.0f32;
    let mut pitch = 1.0f32;
    loop {
        let track = &tracks[index];
        let modulation = modulations[index];
        volume *= track.volume.max(0.0).powi(2)
            * modulation.volume.max(0.0)
            * track.external_volume.max(0.0);
        pitch *= 2.0f32.powf(track.pitch * 4.0)
            * modulation.pitch.max(0.0)
            * track.external_pitch.max(0.0);
        if !track.inherit_parent_mix {
            break;
        }
        let Some(parent) = track.parent else {
            break;
        };
        index = parent;
    }
    (volume, pitch)
}

fn effective_track_external_fxmix(tracks: &[Track], mut index: usize) -> f32 {
    let mut fxmix = 0.0f32;
    loop {
        let track = &tracks[index];
        fxmix += track.external_fxmix;
        if !track.inherit_parent_mix {
            break;
        }
        let Some(parent) = track.parent else {
            break;
        };
        index = parent;
    }
    fxmix.clamp(0.0, 1.0)
}

fn envelope_curve(curve: u8, start: f32, target: f32, progress: f32) -> f32 {
    const SQUARE: [f32; 17] = [
        1.0,
        0.968_246,
        0.935414,
        0.901388,
        0.866025,
        0.829156,
        0.790569,
        0.75,
        std::f32::consts::FRAC_1_SQRT_2,
        0.661438,
        0.612372,
        0.559017,
        0.5,
        0.433013,
        0.353553,
        0.25,
        0.0,
    ];
    const SQUARE_ROOT: [f32; 17] = [
        1.0, 0.878906, 0.765625, 0.660156, 0.5625, 0.472656, 0.390625, 0.316406, 0.25, 0.191406,
        0.140625, 0.0976562, 0.0625, 0.0351562, 0.015625, 0.00390625, 0.0,
    ];
    const SAMPLE_CELL: [f32; 17] = [
        1.0, 0.970489, 0.781274, 0.546281, 0.399792, 0.289315, 0.212104, 0.157476, 0.112613,
        0.0817896, 0.0579852, 0.0436415, 0.0308237, 0.0237129, 0.0152593, 0.00915555, 0.0,
    ];
    let progress = progress.clamp(0.0, 1.0);
    if curve == 0 {
        return start + (target - start) * progress;
    }
    let table = match curve {
        1 => &SQUARE,
        2 => &SQUARE_ROOT,
        3 => &SAMPLE_CELL,
        _ => return start + (target - start) * progress,
    };
    if target < start {
        target + (start - target) * sample_envelope_table(table, progress)
    } else {
        start + (target - start) * sample_envelope_table(table, 1.0 - progress)
    }
}

fn sample_envelope_table(table: &[f32; 17], progress: f32) -> f32 {
    let position = progress.clamp(0.0, 1.0) * 16.0;
    let index = (position as usize).min(15);
    let fraction = position - index as f32;
    table[index] + fraction * (table[index + 1] - table[index])
}

fn resample_voice(voice: &mut Voice) -> f32 {
    if let Some((start, end)) = voice.wave.loop_range {
        let index = voice.position as usize;
        if index >= end {
            let length = end.saturating_sub(start).max(1);
            voice.position =
                (start + index.saturating_sub(start) % length) as f64 + voice.position.fract();
        }
    }
    let base = voice.position as usize;
    let phase = ((voice.position.fract() * 4096.0) as usize >> 6).min(63);
    let coefficients = &RESAMPLE_FILTER[phase * 4..phase * 4 + 4];
    let mut result = 0.0f32;
    for (tap, coefficient) in coefficients.iter().enumerate() {
        result += wave_sample(&voice.wave, base + tap) * (2.0 * f32::from(*coefficient) / 65536.0);
    }
    result.clamp(-1.0, 32767.0 / 32768.0)
}

fn wave_sample(wave: &DecodedWave, mut index: usize) -> f32 {
    if let Some((start, end)) = wave.loop_range {
        if index >= end {
            let length = end.saturating_sub(start).max(1);
            index = start + index.saturating_sub(start) % length;
        }
    }
    wave.samples.get(index).copied().unwrap_or(0.0)
}
fn sign_extend_8(value: u8) -> u16 {
    value as i8 as i16 as u16
}

fn read_u8(bytes: &[u8], cursor: &mut usize, label: &str) -> Result<u8, String> {
    let value = *bytes
        .get(*cursor)
        .ok_or_else(|| format!("{label} reads past end"))?;
    *cursor += 1;
    Ok(value)
}

fn read_be_value(
    bytes: &[u8],
    cursor: &mut usize,
    width: usize,
    label: &str,
) -> Result<u32, String> {
    let slice = checked_slice(bytes, *cursor, width, label)?;
    *cursor += width;
    Ok(slice
        .iter()
        .fold(0, |value, byte| (value << 8) | *byte as u32))
}

fn checked_slice<'a>(
    bytes: &'a [u8],
    offset: usize,
    size: usize,
    label: &str,
) -> Result<&'a [u8], String> {
    let end = offset
        .checked_add(size)
        .ok_or_else(|| format!("{label} range overflow"))?;
    bytes.get(offset..end).ok_or_else(|| {
        format!(
            "{label} range 0x{offset:X}..0x{end:X} exceeds 0x{:X}",
            bytes.len()
        )
    })
}

fn be_u16(bytes: &[u8], offset: usize, label: &str) -> Result<u16, String> {
    let bytes = checked_slice(bytes, offset, 2, label)?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn be_i16(bytes: &[u8], offset: usize, label: &str) -> Result<i16, String> {
    Ok(be_u16(bytes, offset, label)? as i16)
}

fn be_u32(bytes: &[u8], offset: usize, label: &str) -> Result<u32, String> {
    let bytes = checked_slice(bytes, offset, 4, label)?;
    Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn be_f32(bytes: &[u8], offset: usize, label: &str) -> Result<f32, String> {
    Ok(f32::from_bits(be_u32(bytes, offset, label)?))
}

fn c_string(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn afc_zero_blocks_decode_to_silence() {
        assert_eq!(decode_afc(&[0; 9], 16, AfcQuality::High), vec![0.0; 16]);
        assert_eq!(decode_afc(&[0; 5], 16, AfcQuality::Low), vec![0.0; 16]);
        let mut high = [0u8; 9];
        high[0] = 0x10;
        high[1] = 0x10;
        assert_eq!(decode_afc(&high, 1, AfcQuality::High), vec![2.0 / 32768.0]);
        let mut low = [0u8; 5];
        low[0] = 0x10;
        low[1] = 0x40;
        assert_eq!(decode_afc(&low, 1, AfcQuality::Low), vec![8.0 / 32768.0]);
    }

    #[test]
    fn condition_codes_match_jas_sequence_parser() {
        assert!(condition_matches(0, 1));
        assert!(condition_matches(2, 2));
        assert!(condition_matches(1, 3));
        assert!(condition_matches(0x8000, 4));
        assert!(condition_matches(0x7fff, 5));
    }

    #[test]
    fn wait_expiration_resumes_on_the_same_sequence_tick() {
        let mut track = Track::new(0);
        track.wait = 2;
        assert!(!sequence_tick_ready(&mut track));
        assert_eq!(track.wait, 1);
        assert!(sequence_tick_ready(&mut track));
        assert_eq!(track.wait, 0);
    }

    #[test]
    fn sound_dispatch_never_falls_back_to_the_wrong_category() {
        let root = Track::new(0);
        let mut category_zero = Track::new(0);
        category_zero.export_flags = 1 << 9;
        category_zero.port_values[9] = 0;
        let mut category_three = Track::new(0);
        category_three.export_flags = 1 << 9;
        category_three.port_values[9] = 3;
        let tracks = [root, category_zero, category_three];
        assert_eq!(sound_dispatcher_track(&tracks, 3), Some(2));
        assert_eq!(sound_dispatcher_track(&tracks, 5), None);
    }

    #[test]
    fn track_ports_share_values_between_imports_and_exports() {
        let mut track = Track::new(0);
        track.write_import_port(0, 1);

        // The dispatcher acknowledges the request by exporting status on the
        // same port. TTrackPort uses one value array for both directions.
        track.write_export_port(0, 2);

        assert_eq!(track.read_import_port(0), 2);
        assert_eq!(track.import_flags & 1, 0);
        assert_eq!(track.export_flags & 1, 1);
    }

    #[test]
    fn child_local_tempo_uses_jaudio_parent_ratio() {
        let root = Track::new(0);
        let mut child = Track::new_child(1, 0, &root, 0);
        child.tempo = 60;
        child.local_tempo = true;
        let mut tracks = vec![root, child];
        let advanced = [true, false];
        assert!(!track_tick_due(&mut tracks, 1, &advanced));
        assert!(track_tick_due(&mut tracks, 1, &advanced));
    }

    #[test]
    fn jas_envelope_lengths_rates_and_curves_are_preserved() {
        assert_eq!(ReleaseSpec::direct(0).duration_units, 0x10);
        assert_eq!(ReleaseSpec::direct(1000).frames(), 53_333);
        assert_eq!(ReleaseSpec::direct(0xc258).curve, 3);

        let table = [
            0x00, 0x00, 0x02, 0x58, 0x7f, 0xff, // linear rise for 600 units
            0x00, 0x0e, 0x00, 0x00, 0x00, 0x00, // sustain
        ];
        let commands = parse_envelope_table(&table, 0).unwrap();
        let mut oscillator = VoiceOscillator::new(OscillatorSpec {
            kind: 0,
            rate: 0.5,
            attack: Some(Arc::new(commands)),
            release: None,
            width: 1.0,
            vertex: 0.0,
        });
        let mut midpoint = 0.0;
        for _ in 0..=OUTPUT_RATE as usize {
            midpoint = oscillator.next_value().0;
        }
        assert!((midpoint - 0.5).abs() < 0.001);
        assert_eq!(envelope_curve(0, 1.0, 0.0, 0.5), 0.5);
        assert!(envelope_curve(3, 1.0, 0.0, 0.5) < 0.12);
    }

    #[test]
    fn child_track_gain_matches_jaudio_squared_parent_chain() {
        let mut root = Track::new(0);
        root.volume = 0.75;
        let mut child = Track::new_child(1, 0, &root, 0);
        child.volume = 0.5;
        let tracks = vec![root, child];
        let modulations = vec![TrackModulation::default(); tracks.len()];
        let (gain, _) = effective_track_gain_and_pitch(&tracks, &modulations, 1);
        assert!((gain - 0.140625).abs() < f32::EPSILON);
    }

    #[test]
    fn track_vibrato_uses_jaudio_self_oscillator_parameters() {
        let mut track = Track::new(0);
        track.set_oscillator_parameter(9, 0.1);
        let mut minimum = f32::MAX;
        let mut maximum = f32::MIN;
        for _ in 0..5_200 {
            let pitch = track.next_self_modulation().pitch;
            minimum = minimum.min(pitch);
            maximum = maximum.max(pitch);
        }
        assert!(minimum < 0.951);
        assert!(maximum > 1.099);
    }

    #[test]
    fn sequence_adsr_is_routed_onto_spawned_voices() {
        let mut track = Track::new(0);
        track.set_simple_adsr(&[1, 1, 2000, 31_000, 200]);
        track.set_oscillator_route(0);
        let mut oscillators = vec![default_volume_oscillator()];
        track.apply_voice_oscillator_routes(&mut oscillators);

        assert!(oscillators[0].spec.attack.is_some());
        assert!(oscillators[0].spec.release.is_some());
        assert_eq!(oscillators[0].next_value().0, 0.0);
        oscillators[0].begin_release(None);
        assert!(matches!(
            oscillators[0].state,
            OscillatorState::Table { .. }
        ));
    }

    #[test]
    fn jaudio_note_release_encoding_expands_long_values() {
        assert_eq!(decode_note_release(0), 0);
        assert_eq!(decode_note_release(100), 100);
        assert_eq!(decode_note_release(101), 60);
        assert_eq!(decode_note_release(200), 2040);
    }

    #[test]
    fn sequence_interrupt_timer_preempts_and_restores_wait() {
        let mut track = Track::new(10);
        track.wait = 60;
        track.interrupt_offsets[6] = Some(123);
        track.set_interrupt_timer(0, 2);
        track.advance_interrupts();
        assert!(!track.begin_pending_interrupt());
        track.advance_interrupts();
        assert!(track.begin_pending_interrupt());
        assert_eq!(track.pc, 123);
        assert_eq!(track.wait, 0);

        let mut children = Vec::new();
        let mut voices = Vec::new();
        process_command(&[], &mut track, 0, 0xe3, 0, &mut children, &mut voices).unwrap();
        assert_eq!(track.pc, 10);
        assert_eq!(track.wait, 60);
    }

    #[test]
    fn timed_parameters_move_once_per_sequence_tick() {
        let mut track = Track::new(0);
        track.move_time_parameter(0, 0.5, 2);
        track.advance_timed_parameters();
        assert!((track.volume - 0.75).abs() < f32::EPSILON);
        track.advance_timed_parameters();
        assert!((track.volume - 0.5).abs() < f32::EPSILON);
        assert!(track.timed_moves[0].is_none());
    }

    #[test]
    fn sound_preview_peak_guard_prevents_hard_clipping() {
        let mut samples = [2.0, -1.0, 0.5];
        apply_preview_peak_guard(&mut samples);
        assert!((samples[0] - 0.95).abs() < f32::EPSILON);
        assert!((samples[1] + 0.475).abs() < f32::EPSILON);
    }

    #[test]
    #[ignore = "requires an extracted retail Sunshine base in SMS_BASE_ROOT"]
    fn renders_real_sunshine_bgm() {
        let root = std::env::var_os("SMS_BASE_ROOT").expect("SMS_BASE_ROOT");
        let samples = render_bgm_preview(Path::new(&root), 0x8001_0023, 1.0).unwrap();
        assert_eq!(samples.len(), OUTPUT_RATE as usize * 2);
        assert!(samples.iter().any(|sample| sample.abs() > 0.0001));
    }

    #[test]
    #[ignore = "requires an extracted retail Sunshine base in SMS_BASE_ROOT"]
    fn renders_real_sunshine_sound_effect() {
        let root = std::env::var_os("SMS_BASE_ROOT").expect("SMS_BASE_ROOT");
        let sound_id = std::env::var("SMS_SOUND_ID")
            .ok()
            .and_then(|value| u32::from_str_radix(value.trim_start_matches("0x"), 16).ok())
            .unwrap_or(0x19);
        let samples = render_sound_preview(Path::new(&root), sound_id, 8.0).unwrap();
        assert_eq!(samples.len(), OUTPUT_RATE as usize * 16);
        assert!(samples.iter().any(|sample| sample.abs() > 0.0001));
    }

    #[test]
    #[ignore = "requires an extracted retail Sunshine base in SMS_BASE_ROOT"]
    fn audits_retail_sunshine_sound_previews() {
        let root = PathBuf::from(std::env::var_os("SMS_BASE_ROOT").expect("SMS_BASE_ROOT"));
        let entries = crate::music_library::index_retail_sounds(&root).unwrap();
        let mut assets = AudioAssets::load(&root).unwrap();
        let dispatcher = assets.sequence_for_entry(0).unwrap().to_vec();
        let mut per_category = [0usize; 16];
        for entry in entries {
            let category = ((entry.sound_id >> 12) & 0xf) as usize;
            if per_category[category] >= 12 {
                continue;
            }
            per_category[category] += 1;
            let trigger = assets.sound_preview_trigger(entry.sound_id).unwrap();
            let samples =
                render_sequence_preview(&mut assets, dispatcher.clone(), 2.0, Some(trigger));
            match samples {
                Ok(samples) => {
                    let peak = samples
                        .iter()
                        .fold(0.0f32, |peak, value| peak.max(value.abs()));
                    let rms = (samples.iter().map(|value| value * value).sum::<f32>()
                        / samples.len().max(1) as f32)
                        .sqrt();
                    let clipped = samples.iter().filter(|value| value.abs() > 0.99).count();
                    eprintln!(
                        "{:04x} {:<40} peak={peak:.4} rms={rms:.4} clip={clipped}",
                        entry.sound_id, entry.symbol
                    );
                }
                Err(error) => {
                    eprintln!("{:04x} {:<40} ERROR {error}", entry.sound_id, entry.symbol)
                }
            }
        }
    }

    #[test]
    #[ignore = "requires an extracted retail Sunshine base in SMS_BASE_ROOT"]
    fn renders_every_decomp_mapped_sunshine_bgm() {
        let root = PathBuf::from(std::env::var_os("SMS_BASE_ROOT").expect("SMS_BASE_ROOT"));
        let source = fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../src/MSound/MSoundBGM.cpp"),
        )
        .unwrap();
        let mapping =
            regex::Regex::new(r"(?s)case\s+(0x[0-9A-Fa-f]+)\s*:\s*return\s+(0x[0-9A-Fa-f]+)\s*;")
                .unwrap();
        let mut failures = Vec::new();
        for captures in mapping.captures_iter(&source) {
            let bgm_id = u32::from_str_radix(captures[1].trim_start_matches("0x"), 16).unwrap();
            if bgm_id & 0xffff_0000 != 0x8001_0000 {
                continue;
            }
            match render_bgm_preview(&root, bgm_id, 2.0) {
                Ok(samples) if samples.iter().any(|sample| sample.abs() > 0.0001) => {}
                Ok(_) => failures.push(format!("0x{bgm_id:08X}: silent")),
                Err(error) => failures.push(format!("0x{bgm_id:08X}: {error}")),
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}
