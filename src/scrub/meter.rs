//! Paying for the memory scrubbing adds before it is allocated.
use crate::heap;

const KIB: usize = 1024;

/// What scrubbing has added to a request's memory. A marker can be longer
/// than what it replaces, so a scrubbed record can outgrow the price paid
/// for it. Each allocation is paid before it is made, in whole KiB, and a
/// refused payment stops the scrub.
pub struct Meter<'a> {
    pay: Option<&'a mut dyn FnMut(usize) -> bool>,
    /// Bytes held beyond what the input held, which goes negative when
    /// markers are shorter than what they replace.
    live: isize,
    paid: usize,
    refused: bool,
}

impl<'a> Meter<'a> {
    /// A meter that pays with `pay`, which gets bytes in whole KiB and says
    /// whether it could pay them.
    pub fn new(pay: &'a mut dyn FnMut(usize) -> bool) -> Self {
        Self {
            pay: Some(pay),
            live: 0,
            paid: 0,
            refused: false,
        }
    }

    /// A meter that pays nothing, for callers without a budget.
    pub fn unmetered() -> Self {
        Self {
            pay: None,
            live: 0,
            paid: 0,
            refused: false,
        }
    }

    /// Pays for `bytes` about to be allocated. False when the payment was
    /// refused, after which the scrub stops.
    pub(super) fn grow(&mut self, bytes: usize) -> bool {
        if self.refused {
            return false;
        }
        self.live = self
            .live
            .saturating_add(isize::try_from(bytes).unwrap_or(isize::MAX));
        let owed = usize::try_from(self.live).unwrap_or(0);
        if owed > self.paid
            && let Some(pay) = &mut self.pay
        {
            let more = (owed - self.paid).div_ceil(KIB) * KIB;
            if !pay(more) {
                self.refused = true;
                return false;
            }
            self.paid += more;
        }
        true
    }

    /// Records that `bytes` were freed.
    pub(super) fn shrink(&mut self, bytes: usize) {
        self.live = self
            .live
            .saturating_sub(isize::try_from(bytes).unwrap_or(isize::MAX));
    }

    /// Whether a payment was refused, so the scrub stopped part way.
    pub fn refused(&self) -> bool {
        self.refused
    }

    /// What the scrubbed input holds beyond what it held before.
    pub fn grown(&self) -> usize {
        usize::try_from(self.live).unwrap_or(0)
    }

    /// What was paid beyond [`Meter::grown`], which the caller can give
    /// back: allocations that were freed again.
    pub fn excess(&self) -> usize {
        self.paid.saturating_sub(self.grown())
    }
}

/// A rewritten copy of a string, built span by span. Its buffer grows by
/// doubling, each step paid before it is allocated, so it holds the room it
/// paid for and no more.
pub(super) struct Rewrite<'t> {
    text: &'t str,
    out: Option<String>,
    kept: usize,
}

impl<'t> Rewrite<'t> {
    pub(super) fn new(text: &'t str) -> Self {
        Self {
            text,
            out: None,
            kept: 0,
        }
    }

    /// Replaces `start..end` of the text with `marker`. Spans come in
    /// order and do not overlap. False when the meter refused to pay.
    pub(super) fn replace(
        &mut self,
        start: usize,
        end: usize,
        marker: &str,
        meter: &mut Meter<'_>,
    ) -> bool {
        let Some(between) = self.text.get(self.kept..start) else {
            return true;
        };
        if !self.push(between, meter) || !self.push(marker, meter) {
            return false;
        }
        self.kept = end;
        true
    }

    fn push(&mut self, piece: &str, meter: &mut Meter<'_>) -> bool {
        if self.out.is_none() {
            if !meter.grow(heap::text(self.text.len())) {
                return false;
            }
            self.out = Some(String::with_capacity(self.text.len()));
        }
        let Some(out) = &mut self.out else {
            return false;
        };
        let need = out.len() + piece.len();
        if need > out.capacity() {
            let room = need.max(2 * out.capacity());
            if !meter.grow(heap::text(room)) {
                return false;
            }
            let before = out.capacity();
            out.reserve_exact(room - out.len());
            meter.shrink(heap::text(before));
        }
        out.push_str(piece);
        true
    }

    /// The rewritten text, once the rest of the original is copied.
    pub(super) fn finish(mut self, meter: &mut Meter<'_>) -> Done {
        if self.out.is_none() {
            return Done::Same;
        }
        let text = self.text;
        let pushed = self.push(&text[self.kept..], meter);
        match self.out.take() {
            Some(out) if pushed => Done::Changed(out),
            Some(out) => {
                meter.shrink(heap::text(out.capacity()));
                Done::Refused
            }
            None => Done::Refused,
        }
    }

    /// Drops a rewrite that will not be used.
    pub(super) fn abandon(mut self, meter: &mut Meter<'_>) {
        if let Some(out) = self.out.take() {
            meter.shrink(heap::text(out.capacity()));
        }
    }
}

/// How a [`Rewrite`] ended.
pub(super) enum Done {
    Same,
    Changed(String),
    Refused,
}

impl Done {
    /// Puts a changed text in `text`, which held the original. False when
    /// the meter refused to pay.
    pub(super) fn apply(self, text: &mut String, meter: &mut Meter<'_>) -> bool {
        match self {
            Self::Same => true,
            Self::Changed(out) => {
                meter.shrink(heap::text(text.capacity()));
                *text = out;
                true
            }
            Self::Refused => false,
        }
    }
}

/// Replaces all of `text` with `marker`, reusing its buffer when it is
/// large enough. False when the meter refused to pay.
pub(super) fn set(text: &mut String, marker: &str, meter: &mut Meter<'_>) -> bool {
    if text.capacity() >= marker.len() {
        marker.clone_into(text);
        return true;
    }
    if !meter.grow(heap::text(marker.len())) {
        return false;
    }
    let mut fresh = String::with_capacity(marker.len());
    fresh.push_str(marker);
    meter.shrink(heap::text(text.capacity()));
    *text = fresh;
    true
}
