//! 音频解码：OGG Vorbis / PCM WAVE → 交错 `f32` PCM。
//!
//! 引擎面（官方手册 `sound/sound.html`）：只接受 OGG 与 WAVE，且明确不支持
//! “特殊コーデック形式の WAVE”。本模块因此只把 **PCM WAVE** 与
//! **OGG Vorbis** 视为可装载编码，其余显式报 [`AudioError::UnsupportedCodec`]。
//!
//! 采用全量解码：样本游戏的单个音频条目为数十 KB～数 MB，整曲解码进内存
//! 简化了循环点、`TIME`/`TOTALTIME` 与 `DATALEFT/RIGHT` 查询；流式解码留待
//! 长音轨优化时替换，不影响上层 API。

use std::io::Cursor;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{
    CodecType, CODEC_TYPE_ADPCM_G722, CODEC_TYPE_ADPCM_G726, CODEC_TYPE_ADPCM_G726LE,
    CODEC_TYPE_ADPCM_IMA_QT, CODEC_TYPE_ADPCM_IMA_WAV, CODEC_TYPE_ADPCM_MS, CODEC_TYPE_PCM_ALAW,
    CODEC_TYPE_PCM_F32BE, CODEC_TYPE_PCM_F32BE_PLANAR, CODEC_TYPE_PCM_F32LE,
    CODEC_TYPE_PCM_F32LE_PLANAR, CODEC_TYPE_PCM_F64BE, CODEC_TYPE_PCM_F64BE_PLANAR,
    CODEC_TYPE_PCM_F64LE, CODEC_TYPE_PCM_F64LE_PLANAR, CODEC_TYPE_PCM_MULAW, CODEC_TYPE_PCM_S16BE,
    CODEC_TYPE_PCM_S16BE_PLANAR, CODEC_TYPE_PCM_S16LE, CODEC_TYPE_PCM_S16LE_PLANAR,
    CODEC_TYPE_PCM_S24BE, CODEC_TYPE_PCM_S24BE_PLANAR, CODEC_TYPE_PCM_S24LE,
    CODEC_TYPE_PCM_S24LE_PLANAR, CODEC_TYPE_PCM_S32BE, CODEC_TYPE_PCM_S32BE_PLANAR,
    CODEC_TYPE_PCM_S32LE, CODEC_TYPE_PCM_S32LE_PLANAR, CODEC_TYPE_PCM_S8, CODEC_TYPE_PCM_S8_PLANAR,
    CODEC_TYPE_PCM_U16BE, CODEC_TYPE_PCM_U16BE_PLANAR, CODEC_TYPE_PCM_U16LE,
    CODEC_TYPE_PCM_U16LE_PLANAR, CODEC_TYPE_PCM_U24BE, CODEC_TYPE_PCM_U24BE_PLANAR,
    CODEC_TYPE_PCM_U24LE, CODEC_TYPE_PCM_U24LE_PLANAR, CODEC_TYPE_PCM_U32BE,
    CODEC_TYPE_PCM_U32BE_PLANAR, CODEC_TYPE_PCM_U32LE, CODEC_TYPE_PCM_U32LE_PLANAR,
    CODEC_TYPE_PCM_U8, CODEC_TYPE_PCM_U8_PLANAR, CODEC_TYPE_VORBIS,
};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::error::{AudioError, Result};
use crate::info::{AudioCodec, AudioInfo};

/// 解码结果：元信息 + 交错 PCM。
#[derive(Debug, Clone)]
pub struct DecodedAudio {
    /// 元信息（`total_frames` 已按实际解码帧数修正）。
    pub info: AudioInfo,
    /// 交错 `f32` 样本，范围 `[-1.0, 1.0]`。
    pub samples: Vec<f32>,
}

impl DecodedAudio {
    /// 每声道帧数。
    pub fn frames(&self) -> u64 {
        if self.info.channels == 0 {
            0
        } else {
            self.samples.len() as u64 / u64::from(self.info.channels)
        }
    }
}

/// 只探测元信息（当前实现为完整解码后取信息，保证时长精确）。
pub fn probe(bytes: &[u8]) -> Result<AudioInfo> {
    Ok(decode(bytes)?.info)
}

/// 解码 OGG Vorbis / PCM WAVE。
pub fn decode(bytes: &[u8]) -> Result<DecodedAudio> {
    if bytes.is_empty() {
        return Err(AudioError::Decode("empty audio payload".into()));
    }
    let mss = MediaSourceStream::new(Box::new(Cursor::new(bytes.to_vec())), Default::default());
    let probed = symphonia::default::get_probe()
        .format(
            &Hint::new(),
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| AudioError::Decode(format!("probe: {e}")))?;

    let track = probed
        .format
        .default_track()
        .ok_or_else(|| AudioError::Decode("no default audio track".into()))?;
    let track_id = track.id;
    let params = track.codec_params.clone();
    let codec = classify(params.codec);
    if !codec.is_supported() {
        return Err(AudioError::UnsupportedCodec(format!(
            "{codec} (codec id {:?})",
            params.codec
        )));
    }
    let channels = params
        .channels
        .map(|c| c.count())
        .filter(|c| *c > 0)
        .ok_or_else(|| AudioError::Decode("missing channel layout".into()))?;
    let sample_rate = params
        .sample_rate
        .filter(|r| *r > 0)
        .ok_or_else(|| AudioError::Decode("missing sample rate".into()))?;
    let channels_u16 = u16::try_from(channels)
        .map_err(|_| AudioError::Decode(format!("implausible channel count {channels}")))?;
    // OGG Vorbis 无固定位深：引擎按 16 位 PCM 处理（见 AudioInfo 文档）。
    let bits_per_sample = match codec {
        AudioCodec::OggVorbis => 16,
        _ => params.bits_per_sample.unwrap_or(16).clamp(1, 64) as u16,
    };

    let mut format = probed.format;
    let mut decoder = symphonia::default::get_codecs()
        .make(&params, &Default::default())
        .map_err(|e| AudioError::Decode(format!("decoder: {e}")))?;

    let mut samples: Vec<f32> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                break;
            }
            Err(SymphoniaError::ResetRequired) => break,
            Err(SymphoniaError::DecodeError(msg)) => {
                // 单包损坏不终止整曲；与大多数播放器一致，跳过该包。
                yuris_log::warn!("跳过损坏的音频包: {msg}");
                continue;
            }
            Err(e) => return Err(AudioError::Decode(format!("read packet: {e}"))),
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buffer) => {
                let spec = *buffer.spec();
                let capacity = buffer.capacity() as u64;
                let mut conv = SampleBuffer::<f32>::new(capacity, spec);
                conv.copy_interleaved_ref(buffer);
                samples.extend_from_slice(conv.samples());
            }
            Err(SymphoniaError::DecodeError(msg)) => {
                yuris_log::warn!("跳过无法解码的音频帧: {msg}");
            }
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                break;
            }
            Err(e) => return Err(AudioError::Decode(format!("decode: {e}"))),
        }
    }

    let total_frames = samples.len() as u64 / u64::from(channels_u16);
    if total_frames == 0 {
        return Err(AudioError::Decode("decoded zero audio frames".into()));
    }

    Ok(DecodedAudio {
        info: AudioInfo {
            codec,
            channels: channels_u16,
            sample_rate,
            bits_per_sample,
            total_frames,
        },
        samples,
    })
}

fn classify(codec: CodecType) -> AudioCodec {
    if codec == CODEC_TYPE_VORBIS {
        return AudioCodec::OggVorbis;
    }
    if is_pcm(codec) {
        return AudioCodec::PcmWav;
    }
    if matches!(
        codec,
        CODEC_TYPE_ADPCM_MS
            | CODEC_TYPE_ADPCM_IMA_WAV
            | CODEC_TYPE_ADPCM_IMA_QT
            | CODEC_TYPE_ADPCM_G722
            | CODEC_TYPE_ADPCM_G726
            | CODEC_TYPE_ADPCM_G726LE
            | CODEC_TYPE_PCM_ALAW
            | CODEC_TYPE_PCM_MULAW
    ) {
        return AudioCodec::AcmWav;
    }
    // 其余容器/编码（FLAC/MP3/Opus…）不在引擎声明面内。
    AudioCodec::Other
}

fn is_pcm(codec: CodecType) -> bool {
    matches!(
        codec,
        CODEC_TYPE_PCM_S8
            | CODEC_TYPE_PCM_S8_PLANAR
            | CODEC_TYPE_PCM_U8
            | CODEC_TYPE_PCM_U8_PLANAR
            | CODEC_TYPE_PCM_S16LE
            | CODEC_TYPE_PCM_S16LE_PLANAR
            | CODEC_TYPE_PCM_S16BE
            | CODEC_TYPE_PCM_S16BE_PLANAR
            | CODEC_TYPE_PCM_U16LE
            | CODEC_TYPE_PCM_U16LE_PLANAR
            | CODEC_TYPE_PCM_U16BE
            | CODEC_TYPE_PCM_U16BE_PLANAR
            | CODEC_TYPE_PCM_S24LE
            | CODEC_TYPE_PCM_S24LE_PLANAR
            | CODEC_TYPE_PCM_S24BE
            | CODEC_TYPE_PCM_S24BE_PLANAR
            | CODEC_TYPE_PCM_U24LE
            | CODEC_TYPE_PCM_U24LE_PLANAR
            | CODEC_TYPE_PCM_U24BE
            | CODEC_TYPE_PCM_U24BE_PLANAR
            | CODEC_TYPE_PCM_S32LE
            | CODEC_TYPE_PCM_S32LE_PLANAR
            | CODEC_TYPE_PCM_S32BE
            | CODEC_TYPE_PCM_S32BE_PLANAR
            | CODEC_TYPE_PCM_U32LE
            | CODEC_TYPE_PCM_U32LE_PLANAR
            | CODEC_TYPE_PCM_U32BE
            | CODEC_TYPE_PCM_U32BE_PLANAR
            | CODEC_TYPE_PCM_F32LE
            | CODEC_TYPE_PCM_F32LE_PLANAR
            | CODEC_TYPE_PCM_F32BE
            | CODEC_TYPE_PCM_F32BE_PLANAR
            | CODEC_TYPE_PCM_F64LE
            | CODEC_TYPE_PCM_F64LE_PLANAR
            | CODEC_TYPE_PCM_F64BE
            | CODEC_TYPE_PCM_F64BE_PLANAR
    )
}
