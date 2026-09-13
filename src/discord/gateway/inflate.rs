//! `compress=zlib-stream`, which is not per-message compression.
//!
//! Discord's gateway compresses the *whole connection* as one deflate stream
//! and then chops it into websocket frames. That means the decompressor holds a
//! dictionary built from every message so far: a frame cannot be inflated on
//! its own, frames cannot be reordered, and a decompressor that is reset
//! between messages produces garbage rather than an error. It is the single
//! most common thing to get wrong when hand-rolling this protocol, and the
//! symptom is a connection that works for thirty seconds and then dissolves.
//!
//! A message ends with the sync-flush marker `00 00 FF FF`. Frames are
//! accumulated until that appears at the end, and only then handed to the
//! shared decompressor.
//!
//! Two caps. Neither has ever been reached by Discord; both exist because the
//! bytes come off a socket and a zip bomb is four lines of Python.

use flate2::{Decompress, FlushDecompress, Status};

/// The marker a complete message ends with.
const SYNC_FLUSH: [u8; 4] = [0x00, 0x00, 0xFF, 0xFF];

/// How much compressed data one message may be built from.
///
/// A full READY for a heavily-joined account is a few megabytes compressed.
/// Sixteen is generous by an order of magnitude and still small enough that a
/// stream that never ends is caught rather than paged out.
pub const MAX_COMPRESSED: usize = 16 * 1024 * 1024;

/// How much one message may inflate to.
///
/// The ratio matters more than the number: deflate reaches about 1000:1 on
/// repetitive input, so a 64 MiB ceiling on the output is what stops 64 KiB of
/// carefully-chosen compressed bytes from becoming an allocation failure.
pub const MAX_INFLATED: usize = 64 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum InflateError {
    #[error("the compressed message exceeded {MAX_COMPRESSED} bytes")]
    CompressedTooLarge,
    #[error("the message inflated past {MAX_INFLATED} bytes")]
    InflatedTooLarge,
    #[error("the deflate stream is corrupt: {0}")]
    Corrupt(#[source] flate2::DecompressError),
    #[error("the deflate stream stalled without consuming or producing anything")]
    Stalled,
}

/// One per connection. Never reset, never shared between connections.
pub struct Inflater {
    stream: Decompress,
    /// Compressed bytes accumulated since the last complete message.
    pending: Vec<u8>,
    /// The inflated message, reused between messages to avoid reallocating a
    /// multi-megabyte buffer on every READY.
    out: Vec<u8>,
}

impl Default for Inflater {
    fn default() -> Self {
        Self::new()
    }
}

impl Inflater {
    pub fn new() -> Self {
        Self {
            // `true` is the zlib header, which Discord's stream has.
            stream: Decompress::new(true),
            pending: Vec::new(),
            out: Vec::new(),
        }
    }

    /// Feed one websocket frame.
    ///
    /// Returns the inflated message when this frame completed one, and `None`
    /// when the message continues in the next frame.
    pub fn push(&mut self, frame: &[u8]) -> Result<Option<&[u8]>, InflateError> {
        if self.pending.len() + frame.len() > MAX_COMPRESSED {
            // Drop what has accumulated. Keeping it would mean the next frame
            // trips the same cap forever.
            self.pending.clear();
            return Err(InflateError::CompressedTooLarge);
        }
        self.pending.extend_from_slice(frame);

        if !self.pending.ends_with(&SYNC_FLUSH) {
            return Ok(None);
        }

        // Taken out so the decompressor can borrow `self` mutably, then given
        // back so the allocation is reused: a READY-sized buffer is not worth
        // reallocating once per message.
        let input = std::mem::take(&mut self.pending);
        let outcome = self.inflate_into(&input);
        self.pending = input;
        self.pending.clear();
        outcome?;
        Ok(Some(&self.out))
    }

    fn inflate_into(&mut self, input: &[u8]) -> Result<(), InflateError> {
        self.out.clear();
        // A first guess, not a limit. Two-to-one is well under deflate's usual
        // ratio on JSON and saves the first few doublings.
        self.out.reserve(input.len() * 2);

        let mut consumed = 0usize;
        loop {
            let before_in = self.stream.total_in();
            let before_out = self.stream.total_out();

            if self.out.len() == self.out.capacity() {
                if self.out.capacity() >= MAX_INFLATED {
                    return Err(InflateError::InflatedTooLarge);
                }
                let want = (self.out.capacity() * 2).clamp(8192, MAX_INFLATED);
                self.out.reserve(want - self.out.len());
            }

            let status = self
                .stream
                .decompress_vec(&input[consumed..], &mut self.out, FlushDecompress::Sync)
                .map_err(InflateError::Corrupt)?;

            consumed += (self.stream.total_in() - before_in) as usize;
            let produced = self.stream.total_out() - before_out;

            if self.out.len() > MAX_INFLATED {
                return Err(InflateError::InflatedTooLarge);
            }

            match status {
                // The stream never ends before the connection does, so
                // StreamEnd means Discord closed it mid-message.
                Status::StreamEnd => break,
                Status::Ok | Status::BufError => {
                    if consumed == input.len() && produced == 0 {
                        // Everything went in and nothing more is coming out:
                        // the sync flush has been processed.
                        break;
                    }
                    if produced == 0 && self.stream.total_in() == before_in {
                        // Neither side moved and there is input left. Looping
                        // again would spin forever.
                        return Err(InflateError::Stalled);
                    }
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write as _;

    /// A sender: one encoder for the whole connection, sync-flushed per
    /// message, exactly as the gateway does it.
    struct Encoder(ZlibEncoder<Vec<u8>>);

    impl Encoder {
        fn new() -> Self {
            Self(ZlibEncoder::new(Vec::new(), Compression::default()))
        }

        fn message(&mut self, text: &str) -> Vec<u8> {
            self.0.write_all(text.as_bytes()).unwrap();
            self.0.flush().unwrap();
            std::mem::take(self.0.get_mut())
        }
    }

    #[test]
    fn a_message_round_trips() {
        let mut encoder = Encoder::new();
        let mut inflater = Inflater::new();
        let payload = r#"{"op":0,"t":"READY","s":1,"d":{"hello":"world"}}"#;
        let frame = encoder.message(payload);
        assert!(frame.ends_with(&SYNC_FLUSH));
        let out = inflater
            .push(&frame)
            .unwrap()
            .expect("one frame, one message");
        assert_eq!(std::str::from_utf8(out).unwrap(), payload);
    }

    /// The shared dictionary is the whole reason this type exists. The second
    /// message compresses to almost nothing *because* the first one is still in
    /// the decompressor's window; inflating it with a fresh decompressor
    /// produces rubbish rather than an error.
    #[test]
    fn the_dictionary_is_shared_across_messages() {
        let mut encoder = Encoder::new();
        let mut inflater = Inflater::new();

        let first =
            r#"{"op":0,"t":"MESSAGE_CREATE","d":{"content":"the same long sentence again"}}"#;
        let second =
            r#"{"op":0,"t":"MESSAGE_CREATE","d":{"content":"the same long sentence again"}}"#;

        let frame_one = encoder.message(first);
        let frame_two = encoder.message(second);
        assert!(
            frame_two.len() < frame_one.len() / 2,
            "the second message did not benefit from the shared window, so this \
             test is not testing what it claims: {} vs {}",
            frame_one.len(),
            frame_two.len()
        );

        assert_eq!(
            std::str::from_utf8(inflater.push(&frame_one).unwrap().unwrap()).unwrap(),
            first
        );
        assert_eq!(
            std::str::from_utf8(inflater.push(&frame_two).unwrap().unwrap()).unwrap(),
            second
        );

        let mut fresh = Inflater::new();
        assert!(
            fresh.push(&frame_two).is_err(),
            "a fresh decompressor inflated a frame that depends on the window, \
             which means the shared-context assumption is not being tested"
        );
    }

    #[test]
    fn a_message_split_at_every_boundary_still_arrives() {
        let payload = r#"{"op":0,"t":"GUILD_CREATE","d":{"id":"1","name":"a guild with a reasonably long name"}}"#;

        for split in 1..64 {
            let mut encoder = Encoder::new();
            let mut inflater = Inflater::new();
            let frame = encoder.message(payload);
            if split >= frame.len() {
                break;
            }

            let (head, tail) = frame.split_at(split);
            assert!(
                inflater.push(head).unwrap().is_none(),
                "a partial message was treated as complete at split {split}"
            );
            let out = inflater
                .push(tail)
                .unwrap()
                .expect("the message did not complete");
            assert_eq!(std::str::from_utf8(out).unwrap(), payload, "split {split}");
        }
    }

    #[test]
    fn a_large_message_inflates() {
        let payload = format!(r#"{{"d":"{}"}}"#, "x".repeat(2_000_000));
        let mut encoder = Encoder::new();
        let mut inflater = Inflater::new();
        let frame = encoder.message(&payload);
        assert!(
            frame.len() < payload.len() / 100,
            "the fixture is not compressible"
        );
        let out = inflater.push(&frame).unwrap().unwrap();
        assert_eq!(out.len(), payload.len());
    }

    #[test]
    fn rubbish_is_an_error_rather_than_a_panic() {
        let mut inflater = Inflater::new();
        let mut frame = vec![0xffu8; 64];
        frame.extend_from_slice(&SYNC_FLUSH);
        assert!(inflater.push(&frame).is_err());
    }

    #[test]
    fn an_endless_stream_is_refused_before_it_is_buffered() {
        let mut inflater = Inflater::new();
        // Never ends with the marker, so it accumulates until the cap.
        let chunk = vec![0x42u8; 1024 * 1024];
        let mut pushes = 0;
        loop {
            match inflater.push(&chunk) {
                Ok(None) => pushes += 1,
                Ok(Some(_)) => panic!("that was not a complete message"),
                Err(InflateError::CompressedTooLarge) => break,
                Err(e) => panic!("unexpected {e}"),
            }
            assert!(pushes < 64, "the compressed cap did not hold");
        }
    }

    proptest::proptest! {
        /// Bytes off a socket. The inflater may refuse them; it may not panic,
        /// may not hang, and may not allocate without bound.
        #[test]
        fn arbitrary_frames_never_panic(
            frames in proptest::collection::vec(
                proptest::collection::vec(proptest::prelude::any::<u8>(), 0..512),
                1..8,
            ),
        ) {
            let mut inflater = Inflater::new();
            for frame in &frames {
                if let Ok(Some(out)) = inflater.push(frame) {
                    proptest::prop_assert!(out.len() <= MAX_INFLATED);
                }
            }
        }

        /// Any real message, split anywhere, arrives whole.
        #[test]
        fn any_payload_survives_any_split(
            payload in "[ -~]{0,400}",
            split in 1usize..64,
        ) {
            let mut encoder = Encoder::new();
            let mut inflater = Inflater::new();
            let frame = encoder.message(&payload);
            let split = split.min(frame.len().saturating_sub(1)).max(1);
            let (head, tail) = frame.split_at(split);
            // A head that happens to end with the marker is a complete message
            // in its own right; that is a property of the bytes, not a bug, and
            // the remaining case is the one worth asserting on.
            if inflater.push(head).unwrap().is_none() {
                let out = inflater.push(tail).unwrap().expect("never completed").to_vec();
                proptest::prop_assert_eq!(String::from_utf8(out).unwrap(), payload);
            }
        }
    }
}
