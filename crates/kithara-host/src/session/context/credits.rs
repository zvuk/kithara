/// Reservations for the Host's sole writer of Firewheel graph messages.
pub(super) struct Credits {
    capacity: u32,
    sent: u32,
    progress: Option<(i64, bool)>,
}

impl Credits {
    pub(super) const fn new(capacity: u32) -> Self {
        Self {
            capacity,
            sent: 0,
            progress: None,
        }
    }

    pub(super) fn returned(&mut self) {
        self.sent = 0;
        self.progress = None;
    }

    pub(super) fn observe(&mut self, frame: i64) {
        match self.progress {
            Some((last, first)) if frame > last => {
                // Poll precedes clock publication. The first observed advance may race the push;
                // another observed advance proves a subsequent poll consumed every older message.
                if first {
                    self.sent = 0;
                }
                self.progress = Some((frame, !first));
            }
            Some((last, _)) if frame == last => {}
            _ => self.progress = Some((frame, false)),
        }
    }

    pub(super) fn available(&self) -> bool {
        self.sent < self.capacity
    }

    pub(super) fn empty(&self) -> bool {
        self.sent == 0
    }

    pub(super) fn sent(&mut self, frame: i64) {
        self.sent = self
            .sent
            .checked_add(1)
            .expect("a reserved graph slot fits its capacity");
        self.progress = Some((frame, false));
    }
}
