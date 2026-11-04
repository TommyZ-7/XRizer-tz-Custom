//! Neutral XRU1 model container for the ML upscaler.
//!
//! The format is intentionally independent of any reference implementation:
//! a small header, a layer table, int8 weights, and int32 biases.

use crate::upscaler::math::MLP_FEATURE_COUNT;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::SystemTime;

pub const MAGIC: &[u8; 4] = b"XRU1";
pub const VERSION: u16 = 1;
const HEADER_SIZE: usize = 64;
const LAYER_ENTRY_SIZE: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelKind {
    Mlp,
    ResidualCnn,
    Offset,
}

impl ModelKind {
    fn from_u8(value: u8) -> Result<Self, String> {
        match value {
            0 => Ok(Self::Mlp),
            1 => Ok(Self::ResidualCnn),
            2 => Ok(Self::Offset),
            other => Err(format!("unknown model kind {other}")),
        }
    }

    fn as_u8(self) -> u8 {
        match self {
            Self::Mlp => 0,
            Self::ResidualCnn => 1,
            Self::Offset => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Layer {
    pub input_count: u32,
    pub output_count: u32,
    pub weight_scale: f32,
    pub input_scale: f32,
    pub output_scale: f32,
    pub weights: Vec<i8>,
    pub biases: Vec<i32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    pub kind: ModelKind,
    pub feature_count: u32,
    pub hidden: u16,
    pub channels: u16,
    pub layers: Vec<Layer>,
    pub input_scale: f32,
    pub activation_scale: f32,
    pub residual_clamp: f32,
    pub scale: f32,
}

impl Model {
    /// Deterministic, hand-authored smoke model. It is not trained and is only
    /// used to exercise the loader/DP4A path before user captures arrive.
    pub fn self_test_mlp(hidden: u16) -> Result<Self, String> {
        if !matches!(hidden, 8 | 16) {
            return Err(format!("MLP hidden size must be 8 or 16, got {hidden}"));
        }
        let hidden = hidden as u32;
        let mut first = Vec::with_capacity((MLP_FEATURE_COUNT as u32 * hidden) as usize);
        for output in 0..hidden {
            for input in 0..MLP_FEATURE_COUNT as u32 {
                let value = ((input as i32 * 7 + output as i32 * 13) % 31) - 15;
                first.push(value as i8);
            }
        }
        let mut second = Vec::with_capacity(hidden as usize);
        for input in 0..hidden {
            let value = ((input as i32 * 5 + 3) % 17) - 8;
            second.push(value as i8);
        }

        Ok(Self {
            kind: ModelKind::Mlp,
            feature_count: MLP_FEATURE_COUNT as u32,
            hidden: hidden as u16,
            channels: 0,
            layers: vec![
                Layer {
                    input_count: MLP_FEATURE_COUNT as u32,
                    output_count: hidden,
                    weight_scale: 1.0 / 127.0,
                    input_scale: 64.0,
                    output_scale: 1.0,
                    weights: first,
                    biases: vec![0; hidden as usize],
                },
                Layer {
                    input_count: hidden,
                    output_count: 1,
                    weight_scale: 1.0 / 127.0,
                    input_scale: 127.0,
                    output_scale: 1.0,
                    weights: second,
                    biases: vec![0; 1],
                },
            ],
            input_scale: 64.0,
            activation_scale: 1.0,
            residual_clamp: 0.15,
            scale: 1.0,
        })
    }

    /// Deterministic 3x3-receptive-field residual CNN smoke model.
    pub fn self_test_residual_cnn(channels: u16) -> Result<Self, String> {
        if !matches!(channels, 8 | 16 | 32) {
            return Err(format!(
                "residual CNN channels must be 8/16/32, got {channels}"
            ));
        }
        let channels = channels as u32;
        let mut first = Vec::new();
        for output in 0..channels {
            for input in 0..MLP_FEATURE_COUNT as u32 {
                first.push((((input as i32 * 11 + output as i32 * 7) % 29) - 14) as i8);
            }
        }
        let mut second = Vec::new();
        for output in 0..channels {
            for input in 0..channels {
                second.push((((input as i32 * 3 + output as i32 * 5) % 19) - 9) as i8);
            }
        }
        let mut third = Vec::new();
        for output in 0..3u32 {
            for input in 0..channels {
                third.push((((input as i32 * 7 + output as i32 * 2) % 13) - 6) as i8);
            }
        }
        Ok(Self {
            kind: ModelKind::ResidualCnn,
            feature_count: MLP_FEATURE_COUNT as u32,
            hidden: channels as u16,
            channels: channels as u16,
            layers: vec![
                Layer {
                    input_count: MLP_FEATURE_COUNT as u32,
                    output_count: channels,
                    weight_scale: 1.0 / 127.0,
                    input_scale: 64.0,
                    output_scale: 1.0,
                    weights: first,
                    biases: vec![0; channels as usize],
                },
                Layer {
                    input_count: channels,
                    output_count: channels,
                    weight_scale: 1.0 / 127.0,
                    input_scale: 127.0,
                    output_scale: 1.0,
                    weights: second,
                    biases: vec![0; channels as usize],
                },
                Layer {
                    input_count: channels,
                    output_count: 3,
                    weight_scale: 1.0 / 127.0,
                    input_scale: 127.0,
                    output_scale: 1.0,
                    weights: third,
                    biases: vec![0; 3],
                },
            ],
            input_scale: 64.0,
            activation_scale: 1.0,
            residual_clamp: 0.15,
            scale: 1.0,
        })
    }

    /// Deterministic SRPO-style offset predictor smoke model.
    pub fn self_test_srpo(channels: u16) -> Result<Self, String> {
        let mut model = Self::self_test_residual_cnn(channels)?;
        model.kind = ModelKind::Offset;
        model.scale = 1.5;
        Ok(model)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < HEADER_SIZE {
            return Err("XRU1 header is truncated".to_string());
        }
        if &bytes[0..4] != MAGIC {
            return Err("bad XRU1 magic".to_string());
        }
        let version = read_u16(bytes, 4)?;
        if version != VERSION {
            return Err(format!("unsupported XRU1 version {version}"));
        }
        let kind = ModelKind::from_u8(bytes[6])?;
        let feature_count = u32::from(read_u16(bytes, 8)?);
        let hidden = read_u16(bytes, 10)?;
        let channels = read_u16(bytes, 12)?;
        let layer_count = read_u16(bytes, 14)? as usize;
        let input_scale = read_f32(bytes, 16)?;
        let activation_scale = read_f32(bytes, 20)?;
        let residual_clamp = read_f32(bytes, 24)?;
        let scale = read_f32(bytes, 28)?;

        if layer_count == 0 {
            return Err("XRU1 model has no layers".to_string());
        }
        for (name, value) in [
            ("input_scale", input_scale),
            ("activation_scale", activation_scale),
            ("residual_clamp", residual_clamp),
            ("scale", scale),
        ] {
            if !value.is_finite() || value < 0.0 {
                return Err(format!("invalid {name}"));
            }
        }
        if residual_clamp > 1.0 {
            return Err("residual clamp must be <= 1.0".to_string());
        }
        if kind == ModelKind::Mlp {
            if feature_count != MLP_FEATURE_COUNT as u32 {
                return Err(format!(
                    "MLP expects {MLP_FEATURE_COUNT} features, got {feature_count}"
                ));
            }
            if !matches!(hidden, 8 | 16) {
                return Err(format!("unsupported MLP hidden size {hidden}"));
            }
        }

        let table_end = HEADER_SIZE
            .checked_add(layer_count * LAYER_ENTRY_SIZE)
            .ok_or_else(|| "layer table overflow".to_string())?;
        if bytes.len() < table_end {
            return Err("XRU1 layer table is truncated".to_string());
        }

        let mut descriptors = Vec::with_capacity(layer_count);
        let mut offset = HEADER_SIZE;
        for _ in 0..layer_count {
            let input_count = read_u32(bytes, offset)?;
            let output_count = read_u32(bytes, offset + 4)?;
            let weight_scale = read_f32(bytes, offset + 8)?;
            let layer_input_scale = read_f32(bytes, offset + 12)?;
            let layer_output_scale = read_f32(bytes, offset + 16)?;
            offset += LAYER_ENTRY_SIZE;
            if input_count == 0 || output_count == 0 {
                return Err("XRU1 layer has zero dimensions".to_string());
            }
            for (name, value) in [
                ("weight_scale", weight_scale),
                ("input_scale", layer_input_scale),
                ("output_scale", layer_output_scale),
            ] {
                if !value.is_finite() || value <= 0.0 {
                    return Err(format!("invalid layer {name}"));
                }
            }
            descriptors.push((
                input_count,
                output_count,
                weight_scale,
                layer_input_scale,
                layer_output_scale,
            ));
        }

        let mut layers = Vec::with_capacity(layer_count);
        for (input_count, output_count, weight_scale, layer_input_scale, layer_output_scale) in
            descriptors
        {
            let weight_count = (input_count as usize)
                .checked_mul(output_count as usize)
                .ok_or_else(|| "weight count overflow".to_string())?;
            let bias_bytes = (output_count as usize)
                .checked_mul(4)
                .ok_or_else(|| "bias count overflow".to_string())?;
            if offset + weight_count + bias_bytes > bytes.len() {
                return Err("XRU1 weights are truncated".to_string());
            }
            let weights = bytes[offset..offset + weight_count]
                .iter()
                .map(|value| *value as i8)
                .collect();
            offset += weight_count;
            let mut biases = Vec::with_capacity(output_count as usize);
            for _ in 0..output_count {
                biases.push(read_i32(bytes, offset)?);
                offset += 4;
            }
            layers.push(Layer {
                input_count,
                output_count,
                weight_scale,
                input_scale: layer_input_scale,
                output_scale: layer_output_scale,
                weights,
                biases,
            });
        }

        if offset != bytes.len() {
            return Err("XRU1 has trailing data".to_string());
        }
        if layers[0].input_count != feature_count {
            return Err("first layer input does not match feature count".to_string());
        }
        for pair in layers.windows(2) {
            if pair[0].output_count != pair[1].input_count {
                return Err("XRU1 layer dimensions are not contiguous".to_string());
            }
        }
        match kind {
            ModelKind::Mlp => {
                if layers.len() != 2 {
                    return Err("MLP must have exactly two layers".to_string());
                }
                if layers[0].output_count != u32::from(hidden) {
                    return Err("MLP first layer does not match hidden size".to_string());
                }
                if layers[1].input_count != u32::from(hidden) || layers[1].output_count != 1 {
                    return Err("MLP output layer has the wrong shape".to_string());
                }
            }
            ModelKind::ResidualCnn | ModelKind::Offset => {
                if layers.len() != 3 {
                    return Err("CNN/offset models must have exactly three layers".to_string());
                }
                if feature_count != MLP_FEATURE_COUNT as u32 {
                    return Err("CNN/offset models expect 28 input features".to_string());
                }
                if !matches!(channels, 8 | 16 | 32) {
                    return Err(format!("unsupported channel count {channels}"));
                }
                let channels = u32::from(channels);
                if layers[0].output_count != channels
                    || layers[1].input_count != channels
                    || layers[1].output_count != channels
                    || layers[2].input_count != channels
                    || layers[2].output_count != 3
                {
                    return Err("CNN/offset layer dimensions are unsupported".to_string());
                }
            }
        }

        Ok(Self {
            kind,
            feature_count,
            hidden,
            channels,
            layers,
            input_scale,
            activation_scale,
            residual_clamp,
            scale,
        })
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.push(self.kind.as_u8());
        bytes.push(0);
        bytes.extend_from_slice(&(self.feature_count as u16).to_le_bytes());
        bytes.extend_from_slice(&self.hidden.to_le_bytes());
        bytes.extend_from_slice(&self.channels.to_le_bytes());
        bytes.extend_from_slice(&(self.layers.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&self.input_scale.to_le_bytes());
        bytes.extend_from_slice(&self.activation_scale.to_le_bytes());
        bytes.extend_from_slice(&self.residual_clamp.to_le_bytes());
        bytes.extend_from_slice(&self.scale.to_le_bytes());
        bytes.resize(HEADER_SIZE, 0);
        for layer in &self.layers {
            bytes.extend_from_slice(&layer.input_count.to_le_bytes());
            bytes.extend_from_slice(&layer.output_count.to_le_bytes());
            bytes.extend_from_slice(&layer.weight_scale.to_le_bytes());
            bytes.extend_from_slice(&layer.input_scale.to_le_bytes());
            bytes.extend_from_slice(&layer.output_scale.to_le_bytes());
        }
        for layer in &self.layers {
            bytes.extend(layer.weights.iter().map(|value| *value as u8));
            for bias in &layer.biases {
                bytes.extend_from_slice(&bias.to_le_bytes());
            }
        }
        bytes
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path)
            .map_err(|error| format!("could not read model {}: {error}", path.display()))?;
        Self::parse(&bytes)
    }

    /// Pack an MLP into the exact layout consumed by `mlp_dp4a.comp`.
    /// Layer 1: hidden * ceil(features/4) packed weight words.
    /// Then hidden int32 biases. Then ceil(hidden/4) layer-2 packed weights.
    /// Then the single layer-2 bias.
    pub fn packed_mlp_weights(&self) -> Option<Vec<u32>> {
        if self.kind != ModelKind::Mlp || self.layers.len() != 2 {
            return None;
        }
        let first = &self.layers[0];
        let second = &self.layers[1];
        if second.output_count != 1 {
            return None;
        }
        let groups1 = first.input_count.div_ceil(4) as usize;
        let groups2 = second.input_count.div_ceil(4) as usize;
        let mut packed = Vec::with_capacity(
            first.output_count as usize * groups1 + first.output_count as usize + groups2 + 1,
        );
        for output in 0..first.output_count as usize {
            for group in 0..groups1 {
                let mut values = [0i8; 4];
                for (lane, value) in values.iter_mut().enumerate() {
                    let index = output * first.input_count as usize + group * 4 + lane;
                    if group * 4 + lane < first.input_count as usize {
                        *value = first.weights[index];
                    }
                }
                packed.push(pack_i8x4(values));
            }
        }
        for bias in &first.biases {
            packed.push(*bias as u32);
        }
        for group in 0..groups2 {
            let mut values = [0i8; 4];
            for (lane, value) in values.iter_mut().enumerate() {
                let index = group * 4 + lane;
                if index < second.input_count as usize {
                    *value = second.weights[index];
                }
            }
            packed.push(pack_i8x4(values));
        }
        packed.push(second.biases[0] as u32);
        Some(packed)
    }

    /// Pack a residual CNN into the exact layout consumed by
    /// `residual_cnn.comp`: layer weights and biases in layer order.
    pub fn packed_cnn_weights(&self) -> Option<Vec<u32>> {
        if self.kind != ModelKind::ResidualCnn || self.layers.len() != 3 {
            return None;
        }
        let mut packed = Vec::new();
        for layer in &self.layers {
            packed.extend(pack_layer(layer));
            packed.extend(layer.biases.iter().map(|bias| *bias as u32));
        }
        Some(packed)
    }

    pub fn packed_srpo_weights(&self) -> Option<Vec<u32>> {
        if self.kind != ModelKind::Offset || self.layers.len() != 3 {
            return None;
        }
        let mut packed = Vec::new();
        for layer in &self.layers {
            packed.extend(pack_layer(layer));
            packed.extend(layer.biases.iter().map(|bias| *bias as u32));
        }
        Some(packed)
    }

    /// Select the inference shader and packed weights for a model.
    pub fn packed_inference_weights(&self) -> Option<(Vec<u32>, ModelKind, u32, f32)> {
        match self.kind {
            ModelKind::Mlp => Some((
                self.packed_mlp_weights()?,
                ModelKind::Mlp,
                self.layers.first()?.output_count,
                self.mlp_output_scale(),
            )),
            ModelKind::ResidualCnn => Some((
                self.packed_cnn_weights()?,
                ModelKind::ResidualCnn,
                self.channels as u32,
                self.cnn_output_scale(),
            )),
            ModelKind::Offset => Some((
                self.packed_srpo_weights()?,
                ModelKind::Offset,
                self.channels as u32,
                self.srpo_output_scale(),
            )),
        }
    }

    /// Small output scale for the integer accumulator in the smoke model.
    pub fn mlp_output_scale(&self) -> f32 {
        1.0 / (127.0 * 127.0)
    }

    pub fn cnn_output_scale(&self) -> f32 {
        1.0 / (127.0 * 127.0)
    }

    pub fn srpo_output_scale(&self) -> f32 {
        1.0 / (127.0 * 127.0)
    }

    /// Float CPU reference for the self-test model. The shader uses integer
    /// DP4A; this helper is for sanity checks, not bit-exact training.
    #[allow(clippy::needless_range_loop)]
    pub fn eval_mlp(&self, features: &[f32; MLP_FEATURE_COUNT]) -> f32 {
        if self.kind != ModelKind::Mlp || self.layers.len() != 2 {
            return 0.0;
        }
        let first = &self.layers[0];
        let second = &self.layers[1];
        let mut hidden = vec![0.0f32; first.output_count as usize];
        for output in 0..first.output_count as usize {
            let mut acc = first.biases[output] as f32 * first.weight_scale;
            for input in 0..first.input_count as usize {
                let weight = first.weights[output * first.input_count as usize + input] as f32
                    * first.weight_scale;
                acc += weight * features[input];
            }
            hidden[output] = acc.clamp(-1.0, 1.0);
        }
        let mut output = second.biases[0] as f32 * second.weight_scale;
        for input in 0..second.input_count as usize {
            let weight = second.weights[input] as f32 * second.weight_scale;
            output += weight * hidden[input];
        }
        output.clamp(-self.residual_clamp, self.residual_clamp)
    }
}

pub fn default_model_path() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".config/xrizer/upscaler_model.xru")
    } else {
        PathBuf::from("/tmp/xrizer_upscaler_model.xru")
    }
}

pub fn pack_i8x4(values: [i8; 4]) -> u32 {
    (values[0] as u8 as u32)
        | ((values[1] as u8 as u32) << 8)
        | ((values[2] as u8 as u32) << 16)
        | ((values[3] as u8 as u32) << 24)
}

pub fn dot_packed4x8(a: u32, b: u32) -> i32 {
    let mut sum = 0;
    for shift in [0, 8, 16, 24] {
        let av = ((a >> shift) & 0xff) as u8 as i8 as i32;
        let bv = ((b >> shift) & 0xff) as u8 as i8 as i32;
        sum += av * bv;
    }
    sum
}

type ModelCache = HashMap<PathBuf, (SystemTime, Arc<Model>)>;

static MODEL_CACHE: LazyLock<Mutex<ModelCache>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Load a model, reusing the parsed result while the file's mtime is unchanged.
pub fn load_cached(path: &Path) -> Result<Arc<Model>, String> {
    let modified = std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .map_err(|error| format!("could not stat model {}: {error}", path.display()))?;
    let mut cache = MODEL_CACHE.lock().unwrap();
    if let Some((cached_modified, model)) = cache.get(path)
        && *cached_modified == modified
    {
        return Ok(model.clone());
    }
    let model = Arc::new(Model::load(path)?);
    cache.insert(path.to_path_buf(), (modified, model.clone()));
    Ok(model)
}

fn pack_layer(layer: &Layer) -> Vec<u32> {
    let groups = layer.input_count.div_ceil(4) as usize;
    let mut packed = Vec::with_capacity(layer.output_count as usize * groups);
    for output in 0..layer.output_count as usize {
        for group in 0..groups {
            let mut values = [0i8; 4];
            for (lane, value) in values.iter_mut().enumerate() {
                let index = output * layer.input_count as usize + group * 4 + lane;
                if group * 4 + lane < layer.input_count as usize {
                    *value = layer.weights[index];
                }
            }
            packed.push(pack_i8x4(values));
        }
    }
    packed
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, String> {
    bytes
        .get(offset..offset + 2)
        .and_then(|slice| slice.try_into().ok())
        .map(u16::from_le_bytes)
        .ok_or_else(|| "truncated u16".to_string())
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    bytes
        .get(offset..offset + 4)
        .and_then(|slice| slice.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| "truncated u32".to_string())
}

fn read_i32(bytes: &[u8], offset: usize) -> Result<i32, String> {
    bytes
        .get(offset..offset + 4)
        .and_then(|slice| slice.try_into().ok())
        .map(i32::from_le_bytes)
        .ok_or_else(|| "truncated i32".to_string())
}

fn read_f32(bytes: &[u8], offset: usize) -> Result<f32, String> {
    bytes
        .get(offset..offset + 4)
        .and_then(|slice| slice.try_into().ok())
        .map(f32::from_le_bytes)
        .ok_or_else(|| "truncated f32".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mlp_hidden_8_and_16() {
        for hidden in [8u16, 16] {
            let model = Model::self_test_mlp(hidden).unwrap();
            let bytes = model.to_bytes();
            let parsed = Model::parse(&bytes).unwrap();
            assert_eq!(parsed.kind, ModelKind::Mlp);
            assert_eq!(parsed.feature_count as usize, 28);
            assert_eq!(parsed.hidden, hidden);
            assert_eq!(parsed.layers.len(), 2);
            assert_eq!(parsed.layers[0].input_count, 28);
            assert_eq!(parsed.layers[0].output_count as u16, hidden);
            assert_eq!(parsed.layers[1].input_count as u16, hidden);
            assert_eq!(parsed.layers[1].output_count, 1);
            assert_eq!(parsed, model);
        }
    }

    #[test]
    fn reject_bad_magic_version_and_truncation() {
        let bytes = Model::self_test_mlp(16).unwrap().to_bytes();
        assert!(Model::parse(b"nope").is_err());
        let mut bad = bytes.clone();
        bad[4..6].copy_from_slice(&99u16.to_le_bytes());
        assert!(Model::parse(&bad).is_err());
        assert!(Model::parse(&bytes[..32]).is_err());
        assert!(Model::parse(&bytes[..bytes.len() - 1]).is_err());
    }

    #[test]
    fn cpu_reference_matches_dp4a_packing() {
        let weights = [1i8, -2, 3, -4];
        let activations = [5i8, 6, -7, 8];
        let packed_weights = pack_i8x4(weights);
        let packed_activations = pack_i8x4(activations);
        let scalar: i32 = weights
            .iter()
            .zip(activations.iter())
            .map(|(w, a)| i32::from(*w) * i32::from(*a))
            .sum();
        assert_eq!(dot_packed4x8(packed_weights, packed_activations), scalar);
        assert_eq!(packed_weights, 0xfc03_fe01);
        assert_eq!(packed_activations, 0x08f9_0605);
    }

    #[test]
    fn self_test_model_is_deterministic_and_bounded() {
        let model = Model::self_test_mlp(16).unwrap();
        assert!(model.residual_clamp > 0.0);
        assert_eq!(model.to_bytes(), model.to_bytes());
    }

    #[test]
    fn eval_mlp_is_bounded_by_residual_clamp() {
        let model = Model::self_test_mlp(16).unwrap();
        let features = [1.0f32; MLP_FEATURE_COUNT];
        let value = model.eval_mlp(&features);
        assert!(value.abs() <= model.residual_clamp + 1e-6);
    }

    #[test]
    fn packed_mlp_weights_match_shader_layout() {
        for hidden in [8u16, 16] {
            let model = Model::self_test_mlp(hidden).unwrap();
            let packed = model.packed_mlp_weights().unwrap();
            let groups1 = 28usize.div_ceil(4);
            let groups2 = hidden as usize / 4;
            assert_eq!(
                packed.len(),
                hidden as usize * groups1 + hidden as usize + groups2 + 1
            );
            assert_eq!(
                packed[0],
                pack_i8x4([
                    model.layers[0].weights[0],
                    model.layers[0].weights[1],
                    model.layers[0].weights[2],
                    model.layers[0].weights[3]
                ])
            );
        }
    }

    #[test]
    fn loader_cache_reuses_unchanged_model() {
        let path = std::env::temp_dir().join("xrizer_xru1_cache_test.xru");
        std::fs::write(&path, Model::self_test_mlp(16).unwrap().to_bytes()).unwrap();
        let first = load_cached(&path).unwrap();
        let second = load_cached(&path).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn parse_residual_cnn_and_offset_kinds() {
        for (kind, model) in [
            (
                ModelKind::ResidualCnn,
                Model::self_test_residual_cnn(16).unwrap(),
            ),
            (ModelKind::Offset, Model::self_test_srpo(16).unwrap()),
        ] {
            let parsed = Model::parse(&model.to_bytes()).unwrap();
            assert_eq!(parsed, model);
            assert_eq!(parsed.kind, kind);
            assert_eq!(parsed.channels, 16);
        }
    }

    #[test]
    fn srpo_packing_and_offset_scale() {
        let model = Model::self_test_srpo(16).unwrap();
        assert_eq!(model.kind, ModelKind::Offset);
        assert_eq!(model.scale, 1.5);
        assert!(model.packed_srpo_weights().is_some());
        let (_, kind, hidden, _) = model.packed_inference_weights().unwrap();
        assert_eq!(kind, ModelKind::Offset);
        assert_eq!(hidden, 16);
    }

    #[test]
    fn reject_invalid_residual_clamp() {
        let mut bytes = Model::self_test_mlp(16).unwrap().to_bytes();
        bytes[24..28].copy_from_slice(&2.0f32.to_le_bytes());
        assert!(Model::parse(&bytes).is_err());
    }

    #[test]
    fn default_model_path_is_upscaler_model_xru() {
        let path = default_model_path();
        assert!(path.ends_with("upscaler_model.xru"));
    }
}
