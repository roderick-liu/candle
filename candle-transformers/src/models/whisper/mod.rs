pub mod audio;
pub mod model;
pub mod quantized_model;

use serde::Deserialize;

// The names in comments correspond to the original implementation:
// https://github.com/openai/whisper/blob/f572f2161ba831bae131364c3bffdead7af6d210/whisper/model.py#L17
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Config {
    pub num_mel_bins: usize,            // n_mels
    pub max_source_positions: usize,    // n_audio_ctx
    pub d_model: usize,                 // n_audio_state
    pub encoder_attention_heads: usize, // n_audio_head
    pub encoder_layers: usize,          // n_audio_layer
    pub vocab_size: usize,              // n_vocab
    pub max_target_positions: usize,    //  n_text_ctx
    // pub n_text_state: usize,
    pub decoder_attention_heads: usize, // n_text_head
    pub decoder_layers: usize,          // n_text_layer
    pub suppress_tokens: Vec<u32>,
}

pub const DTYPE: candle::DType = candle::DType::F32;

// Audio parameters.
pub const SAMPLE_RATE: usize = 16000;
pub const N_FFT: usize = 400;
pub const N_MELS: usize = 80;
pub const HOP_LENGTH: usize = 160;
pub const CHUNK_LENGTH: usize = 30;
pub const N_SAMPLES: usize = CHUNK_LENGTH * SAMPLE_RATE; // 480000 samples in a 30-second chunk
pub const N_FRAMES: usize = N_SAMPLES / HOP_LENGTH; // 3000 frames in a mel spectrogram input

pub const NO_SPEECH_THRESHOLD: f64 = 0.6;
pub const LOGPROB_THRESHOLD: f64 = -1.0;
pub const TEMPERATURES: [f64; 6] = [0.0, 0.2, 0.4, 0.6, 0.8, 1.0];
pub const COMPRESSION_RATIO_THRESHOLD: f64 = 2.4;

// Tokenizer dependent bits.
pub const SOT_TOKEN: &str = "<|startoftranscript|>";
pub const TRANSCRIBE_TOKEN: &str = "<|transcribe|>";
pub const TRANSLATE_TOKEN: &str = "<|translate|>";
pub const NO_TIMESTAMPS_TOKEN: &str = "<|notimestamps|>";
pub const EOT_TOKEN: &str = "<|endoftext|>";
pub const NO_SPEECH_TOKEN: &str = "<|nocaptions|>";
