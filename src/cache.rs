use crate::{ChatResponse, Error};
use async_trait::async_trait;
use std::{collections::HashMap, sync::Mutex, time::Duration};
use tokio::time::Instant;

#[async_trait]
pub trait Cache: Send + Sync {
    async fn get(&self, key: &str) -> Result<Option<ChatResponse>, Error>;
    async fn insert(&self, key: String, response: ChatResponse) -> Result<(), Error>;
}
pub struct MemoryCache {
    capacity: usize,
    ttl: Duration,
    entries: Mutex<HashMap<String, (Instant, ChatResponse)>>,
}
impl MemoryCache {
    pub fn new(capacity: usize, ttl: Duration) -> Result<Self, Error> {
        if capacity == 0 || ttl.is_zero() {
            return Err(crate::config::configuration());
        }
        Ok(Self {
            capacity,
            ttl,
            entries: Mutex::new(HashMap::new()),
        })
    }
}
#[async_trait]
impl Cache for MemoryCache {
    async fn get(&self, key: &str) -> Result<Option<ChatResponse>, Error> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| crate::config::configuration())?;
        entries.retain(|_, (at, _)| at.elapsed() < self.ttl);
        Ok(entries.get(key).map(|(_, r)| r.clone()))
    }
    async fn insert(&self, key: String, response: ChatResponse) -> Result<(), Error> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| crate::config::configuration())?;
        entries.retain(|_, (at, _)| at.elapsed() < self.ttl);
        if !entries.contains_key(&key)
            && entries.len() >= self.capacity
            && let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(k, _)| k.clone())
        {
            entries.remove(&oldest);
        }
        entries.insert(key, (Instant::now(), response));
        Ok(())
    }
}
