use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SessionMode {
    None = 0,
    Dictation = 1,
    Correction = 2,
}

impl SessionMode {
    pub fn from_raw(value: u8) -> Self {
        match value {
            1 => Self::Dictation,
            2 => Self::Correction,
            _ => Self::None,
        }
    }
}

pub struct SessionSignals {
    start_pending: AtomicBool,
    stop_pending: AtomicBool,
    cancel_pending: AtomicBool,
    recording: AtomicBool,
    mode: AtomicU8,
}

impl SessionSignals {
    pub const fn new() -> Self {
        Self {
            start_pending: AtomicBool::new(false),
            stop_pending: AtomicBool::new(false),
            cancel_pending: AtomicBool::new(false),
            recording: AtomicBool::new(false),
            mode: AtomicU8::new(SessionMode::None as u8),
        }
    }

    pub fn request_start(&self, mode: SessionMode) {
        self.mode.store(mode as u8, Ordering::Relaxed);
        self.start_pending.store(true, Ordering::Relaxed);
    }

    pub fn clear_pending_start(&self) {
        self.start_pending.store(false, Ordering::Relaxed);
    }

    pub fn request_stop(&self) {
        self.stop_pending.store(true, Ordering::Relaxed);
    }

    pub fn request_cancel(&self) {
        self.cancel_pending.store(true, Ordering::Relaxed);
    }

    pub fn take_start(&self) -> Option<SessionMode> {
        self.start_pending
            .swap(false, Ordering::Relaxed)
            .then(|| SessionMode::from_raw(self.mode.load(Ordering::Relaxed)))
    }

    pub fn take_stop(&self) -> bool {
        self.stop_pending.swap(false, Ordering::Relaxed)
    }

    pub fn take_cancel(&self) -> bool {
        self.cancel_pending.swap(false, Ordering::Relaxed)
    }

    pub fn start_pending(&self) -> bool {
        self.start_pending.load(Ordering::Relaxed)
    }

    pub fn stop_pending(&self) -> bool {
        self.stop_pending.load(Ordering::Relaxed)
    }

    pub fn cancel_pending(&self) -> bool {
        self.cancel_pending.load(Ordering::Relaxed)
    }

    pub fn is_recording(&self) -> bool {
        self.recording.load(Ordering::Relaxed)
    }

    pub fn set_recording(&self, recording: bool) {
        self.recording.store(recording, Ordering::Relaxed);
    }

    pub fn mode(&self) -> SessionMode {
        SessionMode::from_raw(self.mode.load(Ordering::Relaxed))
    }

    pub fn mode_raw(&self) -> u8 {
        self.mode.load(Ordering::Relaxed)
    }
}

pub fn begin_requested_session(
    signals: &SessionSignals,
    is_recording: &Cell<bool>,
) -> Option<SessionMode> {
    if is_recording.get() {
        return None;
    }
    let mode = signals.take_start()?;
    is_recording.set(true);
    signals.set_recording(true);
    Some(mode)
}

pub fn take_stop_while_recording(signals: &SessionSignals, is_recording: &Cell<bool>) -> bool {
    if !is_recording.get() {
        return false;
    }
    if !signals.take_stop() {
        return false;
    }
    is_recording.set(false);
    signals.set_recording(false);
    true
}

pub fn take_cancel_while_recording(signals: &SessionSignals, is_recording: &Cell<bool>) -> bool {
    if !is_recording.get() {
        return false;
    }
    if !signals.take_cancel() {
        return false;
    }
    is_recording.set(false);
    signals.set_recording(false);
    true
}

pub fn clear_recording_state(signals: &SessionSignals, is_recording: &Cell<bool>) {
    is_recording.set(false);
    signals.set_recording(false);
}

#[cfg(test)]
mod tests {
    use super::{
        begin_requested_session, clear_recording_state, take_cancel_while_recording,
        take_stop_while_recording, SessionMode, SessionSignals,
    };
    use std::cell::Cell;

    #[test]
    fn start_round_trip_preserves_mode() {
        let signals = SessionSignals::new();
        signals.request_start(SessionMode::Correction);
        assert_eq!(signals.take_start(), Some(SessionMode::Correction));
        assert_eq!(signals.take_start(), None);
    }

    #[test]
    fn stop_and_cancel_are_latched() {
        let signals = SessionSignals::new();
        signals.request_stop();
        signals.request_cancel();
        assert!(signals.take_stop());
        assert!(!signals.take_stop());
        assert!(signals.take_cancel());
        assert!(!signals.take_cancel());
    }

    #[test]
    fn coordinator_marks_recording_lifecycle() {
        let signals = SessionSignals::new();
        let recording = Cell::new(false);
        signals.request_start(SessionMode::Dictation);
        assert_eq!(
            begin_requested_session(&signals, &recording),
            Some(SessionMode::Dictation)
        );
        assert!(recording.get());
        signals.request_stop();
        assert!(take_stop_while_recording(&signals, &recording));
        assert!(!recording.get());
        clear_recording_state(&signals, &recording);
        assert!(!signals.is_recording());
    }

    #[test]
    fn cancel_requires_active_recording() {
        let signals = SessionSignals::new();
        let recording = Cell::new(false);
        signals.request_cancel();
        assert!(!take_cancel_while_recording(&signals, &recording));
    }
}
