//! Side-effect-free interaction rules. Repeats and busy presses never enqueue dictation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Down,
    Up,
    Escape,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    Start,
    Submit,
    Discard,
}

#[derive(Default, Debug)]
pub struct State {
    pub held: bool,
    pub recording: bool,
    pub busy: bool,
}

impl State {
    pub fn input(&mut self, input: Input) -> Action {
        match input {
            Input::Down if !self.held => {
                self.held = true;
                if self.busy {
                    Action::None
                } else {
                    self.recording = true;
                    Action::Start
                }
            }
            Input::Up => {
                self.held = false;
                if self.recording {
                    self.recording = false;
                    self.busy = true;
                    Action::Submit
                } else {
                    Action::None
                }
            }
            Input::Escape | Input::Cancel if self.recording => {
                self.recording = false;
                Action::Discard
            }
            _ => Action::None,
        }
    }
    pub fn complete(&mut self) {
        self.busy = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_release_submits() {
        let mut s = State::default();
        assert_eq!(s.input(Input::Down), Action::Start);
        for _ in 0..20 {
            assert_eq!(s.input(Input::Down), Action::None);
        }
        assert_eq!(s.input(Input::Up), Action::Submit);
        assert_eq!(s.input(Input::Up), Action::None);
    }
    #[test]
    fn escape_discards_and_repeat_cannot_restart() {
        let mut s = State::default();
        s.input(Input::Down);
        assert_eq!(s.input(Input::Escape), Action::Discard);
        assert_eq!(s.input(Input::Escape), Action::None);
        assert_eq!(s.input(Input::Down), Action::None);
        assert_eq!(s.input(Input::Up), Action::None);
        assert_eq!(s.input(Input::Down), Action::Start);
        assert_eq!(s.input(Input::Up), Action::Submit);
    }
    #[test]
    fn busy_press_does_not_start_after_completion() {
        let mut s = State::default();
        s.input(Input::Down);
        s.input(Input::Up);
        assert_eq!(s.input(Input::Down), Action::None);
        s.complete();
        assert_eq!(s.input(Input::Down), Action::None);
        assert_eq!(s.input(Input::Up), Action::None);
        assert_eq!(s.input(Input::Down), Action::Start);
    }
    #[test]
    fn stray_escape_or_release_does_nothing() {
        let mut s = State::default();
        assert_eq!(s.input(Input::Escape), Action::None);
        assert_eq!(s.input(Input::Up), Action::None);
    }
    #[test]
    fn limit_discards_instead_of_transcribing_partial_audio() {
        let mut s = State::default();
        s.input(Input::Down);
        assert_eq!(s.input(Input::Cancel), Action::Discard);
        assert_eq!(s.input(Input::Up), Action::None);
    }
}
