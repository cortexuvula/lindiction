use std::collections::VecDeque;

/// Fixed-capacity ring buffer over f32 audio samples.
///
/// Used by the push-to-talk event loop to carry the last ~300 ms of
/// pre-press mic audio into each utterance. Without this, human reaction
/// time between "start speaking" and "press released-enough-to-register"
/// clips the first phoneme of most utterances — whisper then mishears
/// "Say hello" as "ay hello".
///
/// Capacity is in samples (not bytes / not ms); callers convert.
/// A zero-capacity PreRoll is valid and behaves as a no-op — `push`
/// silently discards, `drain_into` moves nothing. This is the "pre-roll
/// disabled" configuration.
pub struct PreRoll {
    cap: usize,
    buf: VecDeque<f32>,
}

impl PreRoll {
    pub fn new(cap_samples: usize) -> Self {
        Self {
            cap: cap_samples,
            buf: VecDeque::with_capacity(cap_samples),
        }
    }

    /// Append `chunk` to the ring, evicting the oldest samples so the
    /// total length never exceeds `cap`. For `cap == 0` this is a no-op.
    pub fn push(&mut self, chunk: &[f32]) {
        if self.cap == 0 {
            return;
        }
        // If the incoming chunk alone exceeds cap, only the tail of the
        // chunk is retained. Keeping the full chunk then trimming would
        // do the same work with more allocations.
        let start = chunk.len().saturating_sub(self.cap);
        let keep = &chunk[start..];
        // Evict to make room for `keep`.
        let overflow = (self.buf.len() + keep.len()).saturating_sub(self.cap);
        if overflow > 0 {
            self.buf.drain(..overflow);
        }
        self.buf.extend(keep);
    }

    /// Move the entire ring contents (oldest-first) to the front of
    /// `target`, emptying the ring. Order within `target` is:
    /// [preroll samples …][existing target samples …].
    pub fn drain_into(&mut self, target: &mut Vec<f32>) {
        if self.buf.is_empty() {
            return;
        }
        // Splice the ring in front of the existing target contents.
        // For the PTT case, `target` is always empty on press, so the
        // cost is a single extend; the generic case is covered anyway.
        let mut prepend: Vec<f32> = self.buf.drain(..).collect();
        prepend.append(target);
        *target = prepend;
    }

    pub fn clear(&mut self) {
        self.buf.clear();
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_within_cap_retains_all_in_order() {
        let mut r = PreRoll::new(8);
        r.push(&[1.0, 2.0, 3.0]);
        r.push(&[4.0, 5.0]);
        let mut out = Vec::new();
        r.drain_into(&mut out);
        assert_eq!(out, vec![1.0, 2.0, 3.0, 4.0, 5.0]);
        assert!(r.is_empty());
    }

    #[test]
    fn push_over_cap_evicts_oldest() {
        let mut r = PreRoll::new(4);
        r.push(&[1.0, 2.0, 3.0]);
        r.push(&[4.0, 5.0, 6.0]);
        let mut out = Vec::new();
        r.drain_into(&mut out);
        // Oldest two (1.0, 2.0) evicted; newest four preserved in order.
        assert_eq!(out, vec![3.0, 4.0, 5.0, 6.0]);
    }

    #[test]
    fn single_chunk_larger_than_cap_keeps_tail() {
        let mut r = PreRoll::new(3);
        r.push(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        assert_eq!(r.len(), 3);
        let mut out = Vec::new();
        r.drain_into(&mut out);
        assert_eq!(out, vec![3.0, 4.0, 5.0]);
    }

    #[test]
    fn drain_prepends_to_existing_target() {
        let mut r = PreRoll::new(4);
        r.push(&[1.0, 2.0]);
        let mut out = vec![10.0, 11.0];
        r.drain_into(&mut out);
        assert_eq!(out, vec![1.0, 2.0, 10.0, 11.0]);
    }

    #[test]
    fn drain_empty_is_noop() {
        let mut r = PreRoll::new(4);
        let mut out = vec![7.0];
        r.drain_into(&mut out);
        assert_eq!(out, vec![7.0]);
    }

    #[test]
    fn clear_empties_ring() {
        let mut r = PreRoll::new(4);
        r.push(&[1.0, 2.0, 3.0]);
        r.clear();
        assert!(r.is_empty());
        let mut out = Vec::new();
        r.drain_into(&mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn zero_cap_is_valid_and_noop() {
        let mut r = PreRoll::new(0);
        r.push(&[1.0, 2.0, 3.0]);
        assert!(r.is_empty());
        let mut out = vec![9.0];
        r.drain_into(&mut out);
        assert_eq!(out, vec![9.0]);
    }

    #[test]
    fn drain_leaves_ring_reusable() {
        let mut r = PreRoll::new(4);
        r.push(&[1.0, 2.0]);
        let mut out = Vec::new();
        r.drain_into(&mut out);
        r.push(&[5.0, 6.0, 7.0]);
        let mut out2 = Vec::new();
        r.drain_into(&mut out2);
        assert_eq!(out2, vec![5.0, 6.0, 7.0]);
    }
}
