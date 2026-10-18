//! cpal 声卡输出后端。
//!
//! 回调里只做“锁混音器 → 渲染一块 → 写设备缓冲”，不分配新的持久内存
//! （scratch 缓冲在首次回调时增长后复用）。命令/解码在锁外完成。

use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};

use crate::error::{AudioError, Result};
use crate::mixer::Mixer;

/// 已打开的声卡输出流（drop 即停止）。
pub(crate) struct OutputDevice {
    _stream: cpal::Stream,
}

/// 打开默认输出设备，并把混音器采样率对齐到设备默认配置。
pub(crate) fn open(mixer: Arc<Mutex<Mixer>>) -> Result<OutputDevice> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| AudioError::Device("no default output device".into()))?;
    let supported = device
        .default_output_config()
        .map_err(|e| AudioError::Device(format!("default_output_config: {e}")))?;
    let sample_format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let channels = config.channels as usize;
    if channels == 0 {
        return Err(AudioError::Device("device reports zero channels".into()));
    }
    {
        let mut m = mixer
            .lock()
            .map_err(|_| AudioError::Internal("mixer mutex poisoned".into()))?;
        m.set_output_rate(config.sample_rate.0);
    }
    let rate = config.sample_rate.0;
    yuris_log::debug!("音频输出已打开 rate={rate} channels={channels} format={sample_format:?}");

    let err_fn = |err| yuris_log::warn!("音频输出流错误: {err}");
    let stream = match sample_format {
        SampleFormat::F32 => {
            let mut scratch = Vec::new();
            device
                .build_output_stream(
                    &config,
                    move |data: &mut [f32], _| fill(data, channels, &mixer, &mut scratch),
                    err_fn,
                    None,
                )
                .map_err(|e| AudioError::Device(format!("build f32 stream: {e}")))?
        }
        SampleFormat::I16 => {
            let mut scratch = Vec::new();
            device
                .build_output_stream(
                    &config,
                    move |data: &mut [i16], _| fill(data, channels, &mixer, &mut scratch),
                    err_fn,
                    None,
                )
                .map_err(|e| AudioError::Device(format!("build i16 stream: {e}")))?
        }
        SampleFormat::U16 => {
            let mut scratch = Vec::new();
            device
                .build_output_stream(
                    &config,
                    move |data: &mut [u16], _| fill(data, channels, &mixer, &mut scratch),
                    err_fn,
                    None,
                )
                .map_err(|e| AudioError::Device(format!("build u16 stream: {e}")))?
        }
        SampleFormat::I32 => {
            let mut scratch = Vec::new();
            device
                .build_output_stream(
                    &config,
                    move |data: &mut [i32], _| fill(data, channels, &mixer, &mut scratch),
                    err_fn,
                    None,
                )
                .map_err(|e| AudioError::Device(format!("build i32 stream: {e}")))?
        }
        SampleFormat::F64 => {
            let mut scratch = Vec::new();
            device
                .build_output_stream(
                    &config,
                    move |data: &mut [f64], _| fill(data, channels, &mixer, &mut scratch),
                    err_fn,
                    None,
                )
                .map_err(|e| AudioError::Device(format!("build f64 stream: {e}")))?
        }
        other => {
            return Err(AudioError::Device(format!(
                "unsupported output sample format {other}"
            )))
        }
    };
    stream
        .play()
        .map_err(|e| AudioError::Device(format!("start stream: {e}")))?;
    Ok(OutputDevice { _stream: stream })
}

fn fill<T>(data: &mut [T], channels: usize, mixer: &Mutex<Mixer>, scratch: &mut Vec<f32>)
where
    T: SizedSample + FromSample<f32>,
{
    if channels == 0 {
        return;
    }
    let frames = data.len() / channels;
    scratch.resize(frames * 2, 0.0);
    match mixer.lock() {
        Ok(mut m) => m.render(scratch),
        Err(_) => scratch.fill(0.0),
    }
    for frame in 0..frames {
        let l = scratch[frame * 2];
        let r = scratch[frame * 2 + 1];
        for ch in 0..channels {
            let value = match ch {
                0 => l,
                1 => r,
                _ => 0.0,
            };
            data[frame * channels + ch] = T::from_sample(value);
        }
    }
}
