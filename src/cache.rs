//! In-memory positive-answer cache. Wire bytes are patched, never re-encoded.
use crate::dns;
use std::{
    collections::{BTreeSet, HashMap},
    time::{Duration, Instant},
};

pub trait Clock: Send + Sync {
    fn now(&self) -> Duration;
}

pub struct MonotonicClock(Instant);
impl Default for MonotonicClock {
    fn default() -> Self {
        Self(Instant::now())
    }
}
impl Clock for MonotonicClock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
}

struct Entry {
    response: Vec<u8>,
    written: Duration,
    ttl: u32,
    offsets: Vec<usize>,
    used: u128,
}

pub struct Cache {
    capacity: u64,
    stale: u64,
    entries: HashMap<Vec<u8>, Entry>,
    lru: BTreeSet<(u128, Vec<u8>)>,
    sequence: u128,
}

impl Cache {
    pub fn new(capacity: Option<u64>, stale: Option<u64>) -> Self {
        Self {
            capacity: capacity.unwrap_or(0),
            stale: stale.unwrap_or(0),
            entries: HashMap::new(),
            lru: BTreeSet::new(),
            sequence: 0,
        }
    }

    pub fn get(&mut self, query: &[u8], now: Duration) -> Option<Vec<u8>> {
        let key = question(query)?;
        let entry = self.entries.get(key)?;
        let elapsed = now.saturating_sub(entry.written);
        let fresh = elapsed < Duration::from_secs(u64::from(entry.ttl));
        if !fresh
            && (self.stale == 0
                || elapsed >= Duration::from_secs(u64::from(entry.ttl).saturating_add(self.stale)))
        {
            self.remove(key);
            return None;
        }
        let entry = self.entries.get_mut(key)?;
        self.lru.remove(&(entry.used, key.to_vec()));
        self.sequence += 1;
        entry.used = self.sequence;
        self.lru.insert((entry.used, key.to_vec()));
        let mut response = entry.response.clone();
        for &at in &entry.offsets {
            let original = u32::from_be_bytes(response[at..at + 4].try_into().ok()?);
            let ttl = if fresh {
                original.saturating_sub(elapsed.as_secs().min(u64::from(u32::MAX)) as u32)
            } else {
                0
            };
            response[at..at + 4].copy_from_slice(&ttl.to_be_bytes());
        }
        dns::set_id(
            &mut response,
            u16::from_be_bytes(query[..2].try_into().ok()?),
        )
        .ok()?;
        Some(response)
    }

    /// Called only after forwarding normalization; retain the original TTLs.
    pub fn add(&mut self, query: &[u8], response: &[u8], now: Duration) {
        if self.capacity == 0 {
            return;
        }
        let Some(key) = question(query) else { return };
        let Ok(inspection) = dns::inspect(response) else {
            return;
        };
        if inspection.truncated
            || !matches!(response[3] & 15, 0 | 3)
            || inspection.answers.is_empty()
        {
            return;
        }
        let Some((ttl, offsets)) = ttl_fields(response) else {
            return;
        };
        let mut response = response.to_vec();
        if dns::set_id(&mut response, 0).is_err() {
            return;
        }
        self.remove(key);
        if self.entries.len() as u64 >= self.capacity
            && let Some((_, oldest)) = self.lru.first().cloned()
        {
            self.remove(&oldest);
        }
        self.sequence += 1;
        self.lru.insert((self.sequence, key.to_vec()));
        self.entries.insert(
            key.to_vec(),
            Entry {
                response,
                written: now,
                ttl,
                offsets,
                used: self.sequence,
            },
        );
    }

    fn remove(&mut self, key: &[u8]) {
        if let Some(entry) = self.entries.remove(key) {
            self.lru.remove(&(entry.used, key.to_vec()));
        }
    }
}

fn question(wire: &[u8]) -> Option<&[u8]> {
    let end = dns::skip_name(wire, 12).ok()?.checked_add(4)?;
    wire.get(12..end)
}

/// Scan existing wire RR boundaries; OPT's pseudo-TTL is never touched.
fn ttl_fields(wire: &[u8]) -> Option<(u32, Vec<usize>)> {
    let mut at = 12 + question(wire)?.len();
    let answers = u16::from_be_bytes(wire.get(6..8)?.try_into().ok()?);
    let authority = u16::from_be_bytes(wire.get(8..10)?.try_into().ok()?);
    let additional = u16::from_be_bytes(wire.get(10..12)?.try_into().ok()?);
    let mut minimum = None;
    let mut offsets = Vec::new();
    for i in 0..u32::from(answers) + u32::from(authority) + u32::from(additional) {
        let fields = dns::skip_name(wire, at).ok()?;
        let end = dns::skip_rr(wire, at).ok()?;
        let kind = u16::from_be_bytes(wire.get(fields..fields + 2)?.try_into().ok()?);
        if kind != 41 {
            let ttl = u32::from_be_bytes(wire.get(fields + 4..fields + 8)?.try_into().ok()?);
            offsets.push(fields + 4);
            if i < u32::from(answers) {
                minimum = Some(minimum.map_or(ttl, |old: u32| old.min(ttl)));
            }
        }
        at = end;
    }
    Some((minimum?, offsets))
}
