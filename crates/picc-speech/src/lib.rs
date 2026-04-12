//! Shared speech subsystem container for PICC binaries.
//!
//! This crate starts as a boundary-enforcing shell for the speech migration.
//! Functionality is intentionally minimal at first so existing binaries can be
//! moved over incrementally without a large risky rewrite.

pub mod audio;
pub mod errors;
pub mod focus;
pub mod hotkey;
pub mod models;
pub mod postprocess;
pub mod session;
pub mod text_sink;

pub use audio::{resample_linear, AudioCaptureConfig, AudioEngineManager};
pub use errors::SpeechError;
pub use focus::{
    char_before_cursor, frontmost_bundle_id, read_focused_text, FocusedText, SPACE_AFTER_PUNCT,
};
pub use hotkey::{
    HotkeyPolicy, HotkeyRuntime, HotkeySignal, HotkeyState, LEFT_COMMAND_MASK, RIGHT_COMMAND_MASK,
};
pub use models::{
    ensure_tar_bz2_model, resolve_repo_parakeet_de_paths, resolve_repo_sensevoice_paths,
    ModelArchiveSpec, ModelPaths, TransducerModelPaths,
};
pub use postprocess::{apply_dictation_transforms, auto_insert_spaces, DictationOptions};
pub use session::{
    begin_requested_session, clear_recording_state, take_cancel_while_recording,
    take_stop_while_recording, SessionMode, SessionSignals,
};
pub use text_sink::{
    clipboard_replace_is_safe, should_skip_ax_read_for_bundle, should_use_clipboard_for_bundle,
};
