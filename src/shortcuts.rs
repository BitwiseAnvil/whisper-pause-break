//! State for swallowing a complete Win+H press, including repeats and either release order.

#[derive(Clone, Copy, Debug, Default)]
pub struct Modifiers {
    pub windows: bool,
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Pass,
    Block,
    BlockAndMaskStart,
}

#[derive(Default)]
pub struct VoiceTypingBlocker {
    blocked: bool,
    forwarded_down: bool,
}

impl VoiceTypingBlocker {
    pub fn new(h_already_down: bool) -> Self {
        Self {
            forwarded_down: h_already_down,
            ..Self::default()
        }
    }

    /// Called only for H events. Windows/modifier events themselves always pass through.
    pub fn h_event(&mut self, down: bool, modifiers: Modifiers) -> Decision {
        if !down {
            let block = self.blocked && !self.forwarded_down;
            *self = Self::default();
            return if block {
                Decision::Block
            } else {
                Decision::Pass
            };
        }
        if self.blocked {
            return Decision::Block;
        }
        if modifiers.windows && !modifiers.control && !modifiers.alt && !modifiers.shift {
            self.blocked = true;
            return Decision::BlockAndMaskStart;
        }
        self.forwarded_down = true;
        Decision::Pass
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win() -> Modifiers {
        Modifiers {
            windows: true,
            ..Modifiers::default()
        }
    }

    #[test]
    fn blocked_h_never_leaks_after_windows_is_released_first() {
        let mut blocker = VoiceTypingBlocker::default();
        assert_eq!(blocker.h_event(true, win()), Decision::BlockAndMaskStart);
        assert_eq!(blocker.h_event(true, win()), Decision::Block);
        assert_eq!(blocker.h_event(true, Modifiers::default()), Decision::Block);
        assert_eq!(
            blocker.h_event(false, Modifiers::default()),
            Decision::Block
        );
        assert_eq!(blocker.h_event(true, Modifiers::default()), Decision::Pass);
        assert_eq!(blocker.h_event(false, Modifiers::default()), Decision::Pass);
    }

    #[test]
    fn h_can_be_released_and_pressed_again_while_windows_stays_held() {
        let mut blocker = VoiceTypingBlocker::default();
        for _ in 0..2 {
            assert_eq!(blocker.h_event(true, win()), Decision::BlockAndMaskStart);
            assert_eq!(blocker.h_event(false, win()), Decision::Block);
        }
    }

    #[test]
    fn normal_typing_and_other_h_shortcuts_keep_both_key_events() {
        let mut blocker = VoiceTypingBlocker::default();
        for modifiers in [
            Modifiers::default(),
            Modifiers {
                control: true,
                ..Modifiers::default()
            },
            Modifiers {
                control: true,
                ..win()
            },
            Modifiers { alt: true, ..win() },
            Modifiers {
                shift: true,
                ..win()
            },
        ] {
            assert_eq!(blocker.h_event(true, modifiers), Decision::Pass);
            assert_eq!(blocker.h_event(true, modifiers), Decision::Pass);
            assert_eq!(blocker.h_event(false, modifiers), Decision::Pass);
        }
    }

    #[test]
    fn h_pressed_before_windows_keeps_its_matching_release() {
        let mut blocker = VoiceTypingBlocker::default();
        assert_eq!(blocker.h_event(true, Modifiers::default()), Decision::Pass);
        assert_eq!(blocker.h_event(true, win()), Decision::BlockAndMaskStart);
        assert_eq!(blocker.h_event(false, win()), Decision::Pass);
        assert_eq!(blocker.h_event(true, Modifiers::default()), Decision::Pass);
    }

    #[test]
    fn h_held_when_the_app_starts_keeps_its_matching_release() {
        let mut blocker = VoiceTypingBlocker::new(true);
        assert_eq!(blocker.h_event(true, win()), Decision::BlockAndMaskStart);
        assert_eq!(blocker.h_event(false, win()), Decision::Pass);
    }
}
