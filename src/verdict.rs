use std::collections::HashMap;

/// Process-local ASCII-case-insensitive wire-QNAME -> china membership, with arbitrary eviction.
/// Callers supply valid uncompressed QNAMEs: length/root bytes (0..=63) do not fold.
pub struct VerdictCache {
    capacity: usize,
    entries: HashMap<Vec<u8>, bool>,
}

impl VerdictCache {
    pub fn new(capacity: Option<u64>) -> Self {
        Self {
            capacity: capacity.unwrap_or(0).try_into().unwrap_or(usize::MAX),
            entries: HashMap::new(),
        }
    }

    pub fn get(&self, qname: &[u8]) -> Option<bool> {
        self.entries.get(&qname.to_ascii_lowercase()).copied()
    }

    pub fn add(&mut self, qname: &[u8], verdict: bool) {
        if self.capacity == 0 {
            return;
        }
        let key = qname.to_ascii_lowercase();
        if !self.entries.contains_key(&key)
            && self.entries.len() >= self.capacity
            && let Some(key) = self.entries.keys().next().cloned()
        {
            self.entries.remove(&key);
        }
        self.entries.insert(key, verdict);
    }
}
