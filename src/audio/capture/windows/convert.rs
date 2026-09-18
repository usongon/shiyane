use super::super::{mixdown_interleaved, resample_to_target};

/// WASAPI mix-format 帧（交错 f32，任意声道数/采样率）→ 16k mono i16
pub(crate) fn frames_to_pcm_i16(
    samples: &[f32],
    channels: u16,
    source_rate: u32,
    resampler: &mut Option<rubato::Async<f32>>,
) -> Vec<i16> {
    let mono = mixdown_interleaved(samples, channels);
    resample_to_target(&mono, source_rate, resampler)
}
