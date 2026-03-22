/// Linear interpolation downsample from `src_rate` to `dst_rate`.
/// Returns new sample vector at target rate.
pub fn resample(samples: &[f32], src_rate: u32, dst_rate: u32) -> Vec<f32> {
    if samples.is_empty() || src_rate == dst_rate {
        return samples.to_vec();
    }
    let ratio = src_rate as f64 / dst_rate as f64;
    let out_len = (samples.len() as f64 / ratio) as usize;
    (0..out_len)
        .map(|i| {
            let src = i as f64 * ratio;
            let idx = src as usize;
            let frac = (src - idx as f64) as f32;
            let a = samples[idx];
            let b = samples.get(idx + 1).copied().unwrap_or(a);
            a + (b - a) * frac
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_when_same_rate() {
        let input = vec![1.0, 2.0, 3.0, 4.0];
        let output = resample(&input, 16000, 16000);
        assert_eq!(output, input);
    }

    #[test]
    fn downsample_48k_to_16k() {
        // 48kHz -> 16kHz = 3:1 ratio
        // 9 input samples → 3 output samples
        let input: Vec<f32> = (0..9).map(|i| i as f32).collect();
        let output = resample(&input, 48000, 16000);
        assert_eq!(output.len(), 3);
        assert!((output[0] - 0.0).abs() < 1e-6);
        assert!((output[1] - 3.0).abs() < 1e-6);
        assert!((output[2] - 6.0).abs() < 1e-6);
    }

    #[test]
    fn empty_input() {
        let output = resample(&[], 48000, 16000);
        assert!(output.is_empty());
    }
}
