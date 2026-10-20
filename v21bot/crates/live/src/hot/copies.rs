//! The copies of the market channel (spec §5.2). Every copy carries the same frames, byte for byte
//! (the recording of 2026-10-19: 99.5%; the rest was lost in a reconnect of one copy), so a frame is
//! applied once, from the copy that brings it first, and is known by a 64-bit hash of its bytes —
//! without reading it. The `hash` field of an event cannot serve: several messages of one millisecond
//! share it.
//!
//! The same bytes can come twice in one copy, as two frames. So the n-th time a copy brings some bytes
//! is admitted only if no copy has brought them an n-th time yet.

use xxhash_rust::xxh3::xxh3_64;

/// The most copies a table counts.
const MAX_COPIES: usize = 4;

/// Slots looked at for one frame, from its hash's own on.
const PROBE: usize = 16;

/// A slot that holds no frame.
const EMPTY: i64 = i64::MIN;

#[derive(Clone, Copy)]
struct Slot {
    hash: u64,
    /// How many times each copy has brought these bytes.
    seen: [u16; MAX_COPIES],
    /// How many times they have been admitted: the most any copy has brought them.
    admitted: u16,
    /// When a copy last brought them; `EMPTY` for a free slot.
    at_ms: i64,
}

const FREE: Slot = Slot { hash: 0, seen: [0; MAX_COPIES], admitted: 0, at_ms: EMPTY };

/// The frames of the last few seconds, a table of fixed size: nothing is allocated after `new`.
pub struct FrameDedupe {
    slots: Box<[Slot]>,
    mask: usize,
    probe: usize,
    copies: usize,
    ttl_ms: i64,
    dropped: u64,
    overflows: u64,
}

impl FrameDedupe {
    /// A table of `capacity` slots (rounded up to a power of two) for `copies` copies (at most 4).
    /// An entry older than `ttl_ms` is a free slot again. The bot uses 262 144 slots, 2 copies and
    /// 30 000 ms: the copies drift apart by seconds (the recording of 2026-10-20: 4.6% of the frames
    /// over 3 s behind the other copy, none over 30 s), and 30 s of the busiest second seen (2 458
    /// frames) fill under a third of the table.
    pub fn new(capacity: usize, copies: u8, ttl_ms: i64) -> FrameDedupe {
        let n = capacity.max(1).next_power_of_two();
        assert!((1..=MAX_COPIES).contains(&usize::from(copies)), "1 to {MAX_COPIES} copies, got {copies}");
        FrameDedupe {
            slots: vec![FREE; n].into_boxed_slice(),
            mask: n - 1,
            probe: PROBE.min(n),
            copies: usize::from(copies),
            ttl_ms,
            dropped: 0,
            overflows: 0,
        }
    }

    /// Forgets every frame and the counts, where the table is: nothing is allocated.
    pub fn clear(&mut self) {
        self.slots.fill(FREE);
        self.dropped = 0;
        self.overflows = 0;
    }

    /// The frame `raw` came from `copy` at `now_ms`: true if it is to be applied, false if another
    /// copy has brought it already. With no free slot within the probe the frame is admitted and
    /// counted: a frame applied twice is harmless, a lost one is not.
    pub fn admit(&mut self, copy: u8, raw: &[u8], now_ms: i64) -> bool {
        let copy = usize::from(copy).min(self.copies - 1);
        let hash = xxh3_64(raw);
        // the low bits pick the slot; truncating the hash is the point
        let start = hash as usize & self.mask;
        let mut free = None;
        for i in 0..self.probe {
            let ix = (start + i) & self.mask;
            let slot = &mut self.slots[ix];
            let live = slot.at_ms != EMPTY && now_ms - slot.at_ms <= self.ttl_ms;
            if live && slot.hash == hash {
                slot.at_ms = now_ms;
                slot.seen[copy] = slot.seen[copy].saturating_add(1);
                if slot.seen[copy] > slot.admitted {
                    slot.admitted = slot.seen[copy];
                    return true;
                }
                self.dropped += 1;
                return false;
            }
            if !live && free.is_none() {
                free = Some(ix);
            }
        }
        match free {
            Some(ix) => {
                let mut slot = Slot { hash, at_ms: now_ms, admitted: 1, ..FREE };
                slot.seen[copy] = 1;
                self.slots[ix] = slot;
            }
            None => self.overflows += 1,
        }
        true
    }

    /// Frames not applied: another copy had brought them.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Frames admitted without a record, for want of a free slot.
    pub fn overflows(&self) -> u64 {
        self.overflows
    }
}
