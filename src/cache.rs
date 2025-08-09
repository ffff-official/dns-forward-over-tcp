use bytes::Bytes;
use std::{collections::HashMap, time::Instant};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DnsKey {
    pub name: String,
    pub qtype: dns_parser::QueryType,
}

#[derive(Debug, Clone)]
struct DnsEntry {
    buff: Bytes,
    expires_at: Instant,
}

#[derive(Debug, Clone)]
pub struct DnsCache {
    entries: HashMap<DnsKey, DnsEntry>,
}

impl DnsCache {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    pub fn insert(&mut self, record: DnsKey, buff: Bytes, ttl: u64) {
        let expires_at = Instant::now() + std::time::Duration::from_secs(ttl);
        let entry = DnsEntry { buff, expires_at };
        self.entries.insert(record, entry);
    }

    pub fn get(&self, record: &DnsKey) -> Option<Bytes> {
        if let Some(entry) = self.entries.get(record) {
            if entry.expires_at > Instant::now() {
                return Some(entry.buff.clone());
            }
        }
        None
    }
}
