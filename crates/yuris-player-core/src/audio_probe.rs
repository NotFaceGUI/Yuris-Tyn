//! `SOUNDINFO` 查询桥：把 `yuris-audio` 的状态句柄适配上 `yuris-vm` 的
//! [`SoundStateProbe`](yuris_vm::audio_probe::SoundStateProbe) 边界。
//!
//! ## 为什么需要这层桥
//!
//! 分层约定：VM（L3）**不得**依赖音频层（L6），平台与内核之间只经
//! `RuntimeApi` 边界。`SOUNDINFO` 是查询命令，需要一个受控通道；于是
//! VM 定义 trait，播放器（同时持有 VM 与音频系统）提供实现 —— 与
//! `PacFileIndex`（`FILEINFO` 查询）完全同构。
//!
//! 桥本身只做**字段名 → 查询枚举**的映射，不改变任何语义：
//! [`yuris_audio::Mixer::query`] 已是引擎 v494 逐槽取证过的实现。

use yuris_audio::sound::{SoundInfoQuery, SoundInfoValue as AudioValue};
use yuris_vm::audio_probe::{SoundField, SoundInfoValue, SoundStateProbe};

/// 基于 [`yuris_audio::StateHandle`] 的查询桥。
pub struct AudioStateProbe {
    handle: yuris_audio::StateHandle,
}

impl AudioStateProbe {
    /// 用音频系统的状态句柄建立桥。
    pub fn new(handle: yuris_audio::StateHandle) -> Self {
        Self { handle }
    }
}

/// VM 字段 → 音频层查询。
///
/// 一对一；`None` = 该字段在本内核没有对应查询（不应发生，全部字段都在
/// 音频层有实现）。
fn map_query(field: SoundField) -> Option<SoundInfoQuery> {
    Some(match field {
        SoundField::Exist => SoundInfoQuery::Exist,
        SoundField::Enabled => SoundInfoQuery::Enabled,
        SoundField::Play => SoundInfoQuery::Play,
        SoundField::Pause => SoundInfoQuery::Pause,
        SoundField::Volume => SoundInfoQuery::Volume,
        SoundField::Pan => SoundInfoQuery::Pan,
        SoundField::Pitch => SoundInfoQuery::Pitch,
        SoundField::File => SoundInfoQuery::File,
        SoundField::FileType => SoundInfoQuery::FileType,
        SoundField::Start => SoundInfoQuery::Start,
        SoundField::Restart => SoundInfoQuery::Restart,
        SoundField::End => SoundInfoQuery::End,
        SoundField::Loop => SoundInfoQuery::Loop,
        SoundField::LoopCount => SoundInfoQuery::LoopCount,
        SoundField::Time => SoundInfoQuery::Time,
        SoundField::TotalTime => SoundInfoQuery::TotalTime,
        SoundField::Channel => SoundInfoQuery::Channel,
        SoundField::Bit => SoundInfoQuery::Bit,
        SoundField::Sampling => SoundInfoQuery::Sampling,
        SoundField::DataLeft => SoundInfoQuery::DataLeft,
        SoundField::DataRight => SoundInfoQuery::DataRight,
        SoundField::Codec => SoundInfoQuery::Codec,
    })
}

impl SoundStateProbe for AudioStateProbe {
    fn query(&self, layer_id: &str, field: SoundField) -> Option<SoundInfoValue> {
        let q = map_query(field)?;
        Some(match self.handle.query(layer_id, q) {
            AudioValue::Int(v) => SoundInfoValue::Int(v),
            AudioValue::Str(s) => SoundInfoValue::Str(s.into_bytes()),
            AudioValue::Unsupported => SoundInfoValue::Unsupported,
        })
    }
}

/// 给 VM 注入音频状态查询通道（播放器构造后调用一次）。
///
/// 幂等：重复调用只会替换句柄。
pub fn install_audio_probe(vm: &mut yuris_vm::GroupVm, audio: &yuris_audio::AudioSystem) {
    vm.set_audio_probe(std::sync::Arc::new(AudioStateProbe::new(
        audio.state_handle(),
    )));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个 VM 字段都必须映射到音频层查询——防止将来加字段时漏接。
    #[test]
    fn every_field_maps() {
        for f in [
            SoundField::Exist,
            SoundField::Enabled,
            SoundField::Play,
            SoundField::Pause,
            SoundField::Volume,
            SoundField::Pan,
            SoundField::Pitch,
            SoundField::File,
            SoundField::FileType,
            SoundField::Start,
            SoundField::Restart,
            SoundField::End,
            SoundField::Loop,
            SoundField::LoopCount,
            SoundField::Time,
            SoundField::TotalTime,
            SoundField::Channel,
            SoundField::Bit,
            SoundField::Sampling,
            SoundField::DataLeft,
            SoundField::DataRight,
            SoundField::Codec,
        ] {
            assert!(map_query(f).is_some(), "{f:?} 未映射到音频层查询");
        }
    }

    /// 声层不存在 → `EXIST=0`（引擎口径），不是"无真值"。
    #[test]
    fn missing_layer_answers_exist_zero() {
        let sys = yuris_audio::AudioSystem::new();
        let probe = AudioStateProbe::new(sys.state_handle());
        assert_eq!(
            probe.query("nope", SoundField::Exist),
            Some(SoundInfoValue::Int(0))
        );
    }
}
