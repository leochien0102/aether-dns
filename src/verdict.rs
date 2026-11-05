use std::collections::HashMap;

/// Process-local wire-qname -> china membership, with arbitrary eviction.
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
        self.entries.get(qname).copied()
    }

    pub fn add(&mut self, qname: &[u8], verdict: bool) {
        if self.capacity == 0 {
            return;
        }
        if !self.entries.contains_key(qname)
            && self.entries.len() >= self.capacity
            && let Some(key) = self.entries.keys().next().cloned()
        {
            self.entries.remove(&key);
        }
        self.entries.insert(qname.to_vec(), verdict);
    }
}
