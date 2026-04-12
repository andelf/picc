use crate::session::SessionMode;

pub const RIGHT_COMMAND_MASK: u64 = 0x10;
pub const LEFT_COMMAND_MASK: u64 = 0x08;

#[derive(Debug, Clone, Copy)]
pub struct HotkeyPolicy {
    pub accept_left_command: bool,
    pub enable_correction_mode: bool,
    pub enable_short_tap_cancel: bool,
    pub tap_ms: u64,
    pub correction_gap_ms: u64,
}

impl HotkeyPolicy {
    pub const fn dictation() -> Self {
        Self {
            accept_left_command: false,
            enable_correction_mode: false,
            enable_short_tap_cancel: false,
            tap_ms: 300,
            correction_gap_ms: 300,
        }
    }

    pub const fn voice_correct() -> Self {
        Self {
            accept_left_command: false,
            enable_correction_mode: true,
            enable_short_tap_cancel: true,
            tap_ms: 300,
            correction_gap_ms: 300,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeySignal {
    Start(SessionMode),
    Stop,
    Cancel,
    ClearPendingStart,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct HotkeyRuntime {
    pub is_recording: bool,
    pub cancel_pending: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct HotkeyState {
    was_down: bool,
    press_ms: u64,
    last_tap_release_ms: u64,
}

impl HotkeyState {
    pub const fn new() -> Self {
        Self {
            was_down: false,
            press_ms: 0,
            last_tap_release_ms: 0,
        }
    }

    pub fn handle_flags_changed(
        &mut self,
        device_flags: u64,
        now_ms: u64,
        runtime: HotkeyRuntime,
        policy: HotkeyPolicy,
    ) -> Vec<HotkeySignal> {
        let command_mask = if policy.accept_left_command {
            RIGHT_COMMAND_MASK | LEFT_COMMAND_MASK
        } else {
            RIGHT_COMMAND_MASK
        };
        let pressed = (device_flags & command_mask) != 0;

        if pressed && !self.was_down {
            self.was_down = true;
            self.press_ms = now_ms;

            if !runtime.is_recording || runtime.cancel_pending {
                let mode = if policy.enable_correction_mode
                    && now_ms.saturating_sub(self.last_tap_release_ms) < policy.correction_gap_ms
                {
                    SessionMode::Correction
                } else {
                    SessionMode::Dictation
                };
                return vec![HotkeySignal::Start(mode)];
            }
            return Vec::new();
        }

        if !pressed && self.was_down {
            self.was_down = false;
            let hold_ms = now_ms.saturating_sub(self.press_ms);
            let is_short_tap = hold_ms < policy.tap_ms;

            if is_short_tap {
                self.last_tap_release_ms = now_ms;
            }

            if runtime.is_recording {
                if policy.enable_short_tap_cancel && is_short_tap && !policy.enable_correction_mode
                {
                    return vec![HotkeySignal::Cancel];
                }
                return vec![HotkeySignal::Stop];
            }

            if is_short_tap {
                return vec![HotkeySignal::ClearPendingStart];
            }
        }

        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{HotkeyPolicy, HotkeyRuntime, HotkeySignal, HotkeyState, RIGHT_COMMAND_MASK};
    use crate::session::SessionMode;

    #[test]
    fn dictation_policy_starts_and_stops() {
        let mut state = HotkeyState::new();
        let start = state.handle_flags_changed(
            RIGHT_COMMAND_MASK,
            100,
            HotkeyRuntime::default(),
            HotkeyPolicy::dictation(),
        );
        assert_eq!(start, vec![HotkeySignal::Start(SessionMode::Dictation)]);

        let stop = state.handle_flags_changed(
            0,
            500,
            HotkeyRuntime {
                is_recording: true,
                cancel_pending: false,
            },
            HotkeyPolicy::dictation(),
        );
        assert_eq!(stop, vec![HotkeySignal::Stop]);
    }

    #[test]
    fn voice_correct_policy_enters_correction_after_tap_gap() {
        let mut state = HotkeyState::new();
        let _ = state.handle_flags_changed(
            RIGHT_COMMAND_MASK,
            100,
            HotkeyRuntime::default(),
            HotkeyPolicy::voice_correct(),
        );
        let _ = state.handle_flags_changed(
            0,
            150,
            HotkeyRuntime::default(),
            HotkeyPolicy::voice_correct(),
        );
        let signal = state.handle_flags_changed(
            RIGHT_COMMAND_MASK,
            300,
            HotkeyRuntime::default(),
            HotkeyPolicy::voice_correct(),
        );
        assert_eq!(signal, vec![HotkeySignal::Start(SessionMode::Correction)]);
    }
}
