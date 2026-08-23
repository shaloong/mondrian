#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct TextCompositionState {
    preedit: String,
}

impl TextCompositionState {
    pub(super) fn preedit(&self) -> &str {
        &self.preedit
    }

    pub(super) fn is_active(&self) -> bool {
        !self.preedit.is_empty()
    }

    pub(super) fn set_preedit(&mut self, preedit: String) {
        self.preedit = preedit;
    }

    pub(super) fn clear(&mut self) {
        self.preedit.clear();
    }
}
