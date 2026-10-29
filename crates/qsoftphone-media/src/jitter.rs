//! The jitter buffer: the frames that arrived, waiting to be played.
//!
//! ## Why this exists
//!
//! RTP is UDP: the packets can be late, they can be out of order, and
//! they can be lost. The sound card does not care — it wants the next
//! sample *now*, and it wants the one after that, and the one after
//! that.
//!
//! Playing what has arrived as it arrives sounds terrible: the peer
//! sends a frame every 20 ms, the card plays 10 ms of it, and then the
//! card asks for the next 10 ms — which has not arrived yet. The
//! speaker gets a burst of sound, then silence, then a burst. That is
//! the "dithering" a call sounds like without a jitter buffer.
//!
//! So the frames are **buffered** before they are played:
//!
//! * the **sequence number** says which frame comes next, so a frame
//!   that arrives late is put back in its place and a frame that never
//!   arrives is noticed;
//! * the **buffer's depth** is the playout delay — the time the audio
//!   waits in the machine before it reaches the speaker.
//!
//! ## The delay has to be paid
//!
//! A jitter buffer trades latency for continuity: the deeper it is,
//! the more jitter it absorbs, and the longer the delay. Four frames —
//! 80 ms — is the usual compromise, and it is what this build uses.
//! Asterisk's own default is in the same range.
//!
//! ## Late is not lost
//!
//! The difference matters. A frame that has not arrived yet may be on
//! its way, and a frame that has been *given up on* never will be. So
//! the buffer does not decide by itself: it hands out the frame it is
//! waiting for, and the caller comes back with
//! [`JitterBuffer::skip`] when it has run out of patience.
//!
//! The caller is the playout, and its patience is the audio that is
//! still buffered: while there is slack, a late frame is waited for.
//!
//! ## What happens when a frame is lost
//!
//! The decoder has to keep going: a gap in the stream would leave it
//! out of step with the encoder at the other end, and for G.722 — which
//! is ADPCM, and keeps a state — that means noise for the rest of the
//! call. So a lost frame is replaced by a **concealment frame**: the
//! codec's own way of saying "nothing here". For G.711 that is its
//! silence code; for G.722 it is the byte that tells both of its
//! bands "no change". The decoder takes its step and stays in step.

use std::collections::VecDeque;

/// A frame of audio that arrived from the peer.
#[derive(Debug, Clone)]
pub struct Frame {
    /// The RTP sequence number.
    pub sequence: u16,
    /// The RTP timestamp: the codec's clock, in samples.
    pub timestamp: u32,
    /// The media payload.
    pub payload: Vec<u8>,
}

/// The frames that arrived, in order.
#[derive(Debug)]
pub struct JitterBuffer {
    /// The frames, oldest first.
    frames: VecDeque<Frame>,
    /// The sequence number of the frame that comes next.
    expected: Option<u16>,
    /// How many frames may wait.
    capacity: usize,
}

impl JitterBuffer {
    /// A buffer that holds at most `capacity` frames.
    pub fn new(capacity: usize) -> Self {
        Self {
            frames: VecDeque::new(),
            expected: None,
            capacity: capacity.max(1),
        }
    }

    /// Take in a frame that arrived.
    ///
    /// Returns `false` when the frame was thrown away: it is a
    /// duplicate, or it is one that the playout has already passed.
    pub fn push(&mut self, frame: Frame) -> bool {
        // The first frame we see is the one we expect: we join the
        // stream in the middle, and what came before it is gone.
        let expected = match self.expected {
            Some(expected) => expected,
            None => {
                self.expected = Some(frame.sequence);
                self.insert(frame);
                return true;
            }
        };

        // A frame from before the playout: too late to be of any use.
        if !is_at_or_after(frame.sequence, expected) {
            return false;
        }

        // A frame we already have.
        if self
            .frames
            .iter()
            .any(|held| held.sequence == frame.sequence)
        {
            return false;
        }

        self.insert(frame);

        // The buffer is full: the oldest frame goes, so that the
        // playout catches up with the peer instead of falling further
        // and further behind.
        //
        // The frame that is expected is *not* skipped by this: it may
        // be one that never arrived, and the playout has to be told
        // that it was lost — a concealment frame for it, or the
        // decoder falls out of step with the encoder at the other end.
        while self.frames.len() > self.capacity {
            self.frames.pop_front();
        }

        true
    }

    /// The frame that is expected, if it has arrived.
    ///
    /// `None` means it has not: it may be late, or lost. The caller
    /// waits for it while it has slack, and gives up on it with
    /// [`JitterBuffer::skip`] when it has not.
    pub fn take(&mut self) -> Option<Frame> {
        let expected = self.expected?;
        let front = self.frames.front()?;
        if front.sequence != expected {
            // The frame at the front is newer than the one we want:
            // the one we want has not arrived.
            return None;
        }

        let frame = self.frames.pop_front().expect("a frame");
        self.expected = Some(expected.wrapping_add(1));
        Some(frame)
    }

    /// Give up on the frame that was expected.
    ///
    /// It is lost: the caller is to play a concealment frame for it
    /// and carry on. The frames behind it are played as they are.
    pub fn skip(&mut self) {
        if let Some(expected) = self.expected {
            self.expected = Some(expected.wrapping_add(1));
        }
    }

    /// How many frames are waiting.
    pub fn depth(&self) -> usize {
        self.frames.len()
    }

    /// The sequence number that comes next, if it is known.
    pub fn expected(&self) -> Option<u16> {
        self.expected
    }

    /// Put a frame in its place.
    ///
    /// The frames arrive in order almost always, so the place is
    /// usually the end — but a packet that took a detour has to be put
    /// back where it belongs.
    fn insert(&mut self, frame: Frame) {
        let position = self
            .frames
            .iter()
            .position(|existing| is_after(existing.sequence, frame.sequence))
            .unwrap_or(self.frames.len());
        self.frames.insert(position, frame);
    }
}

/// Whether `sequence` comes after `other`.
///
/// The sequence number is 16 bits and wraps, so the comparison is done
/// on the difference: a number that is less than half the space away
/// is later, and one that is more is earlier.
fn is_after(sequence: u16, other: u16) -> bool {
    sequence.wrapping_sub(other) < 0x8000 && sequence != other
}

/// Whether `sequence` comes after `other`, or is `other`.
///
/// This is what a frame that is *late* looks like: it is the one the
/// playout is waiting for, and it turns up after the playout has
/// already moved on — which is not the same as being too late.
fn is_at_or_after(sequence: u16, other: u16) -> bool {
    sequence.wrapping_sub(other) < 0x8000
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(sequence: u16) -> Frame {
        Frame {
            sequence,
            timestamp: u32::from(sequence) * 160,
            payload: vec![sequence as u8; 160],
        }
    }

    #[test]
    fn the_frames_come_out_in_order() {
        let mut buffer = JitterBuffer::new(10);
        assert!(buffer.push(frame(1)));
        assert!(buffer.push(frame(2)));
        assert!(buffer.push(frame(3)));

        for expected in 1..=3 {
            let f = buffer.take().expect("a frame");
            assert_eq!(f.sequence, expected);
        }
        assert!(buffer.take().is_none(), "the buffer is empty");
    }

    #[test]
    fn a_frame_that_arrives_out_of_order_is_put_back_in_its_place() {
        let mut buffer = JitterBuffer::new(10);
        buffer.push(frame(1));
        buffer.push(frame(3));
        buffer.push(frame(2));

        for expected in 1..=3 {
            let f = buffer.take().expect("a frame");
            assert_eq!(f.sequence, expected);
        }
    }

    #[test]
    fn a_frame_that_is_late_is_still_handed_out() {
        // The playout is waiting for frame 2, and frame 3 has turned
        // up first. Frame 2 is late, not lost: it is still the one
        // that is wanted.
        let mut buffer = JitterBuffer::new(10);
        buffer.push(frame(1));
        buffer.push(frame(3));

        let f = buffer.take().expect("frame 1");
        assert_eq!(f.sequence, 1);

        // Frame 2 has not arrived.
        assert!(buffer.take().is_none(), "frame 2 was not there yet");

        // And now it is.
        assert!(buffer.push(frame(2)), "the late frame was thrown away");
        let f = buffer.take().expect("frame 2");
        assert_eq!(f.sequence, 2);
        // And the stream carries on.
        let f = buffer.take().expect("frame 3");
        assert_eq!(f.sequence, 3);
    }

    #[test]
    fn a_lost_frame_is_skipped() {
        let mut buffer = JitterBuffer::new(10);
        buffer.push(frame(1));
        buffer.push(frame(3));

        let f = buffer.take().expect("frame 1");
        assert_eq!(f.sequence, 1);

        // Frame 2 never arrives: the caller gives up on it.
        assert!(buffer.take().is_none());
        buffer.skip();

        // And the stream carries on.
        let f = buffer.take().expect("frame 3");
        assert_eq!(f.sequence, 3);
    }

    #[test]
    fn a_frame_that_is_too_late_is_thrown_away() {
        let mut buffer = JitterBuffer::new(10);
        buffer.push(frame(1));
        buffer.push(frame(2));
        // The playout has taken frame 1.
        assert!(buffer.take().is_some());
        // And now frame 1 turns up again.
        assert!(!buffer.push(frame(1)), "a late frame must be dropped");
        assert!(!buffer.push(frame(2)), "a duplicate must be dropped");
    }

    #[test]
    fn the_buffer_does_not_grow_without_bound() {
        let mut buffer = JitterBuffer::new(4);
        for sequence in 1..=20 {
            buffer.push(frame(sequence));
        }
        assert_eq!(buffer.depth(), 4);
        // The frames that were dropped are *not* skipped silently:
        // the playout is still waiting for the first one, and it will
        // be told that it was lost.
        assert_eq!(buffer.expected(), Some(1));
        assert!(buffer.take().is_none(), "the first frame was not there");
        buffer.skip();
    }

    #[test]
    fn the_sequence_number_may_wrap() {
        let mut buffer = JitterBuffer::new(10);
        buffer.push(frame(65535));
        buffer.push(frame(0));
        buffer.push(frame(1));

        for expected in [65535u16, 0, 1] {
            let f = buffer.take().expect("a frame");
            assert_eq!(f.sequence, expected);
        }
    }
}
