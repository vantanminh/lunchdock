//! Connected agent sessions, keyed by server id.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use rudder_core::NodeDriver;
use uuid::Uuid;

#[derive(Default)]
pub struct AgentRegistry {
    sessions: RwLock<HashMap<Uuid, Arc<dyn NodeDriver>>>,
}

impl AgentRegistry {
    pub fn driver(&self, server_id: Uuid) -> Option<Arc<dyn NodeDriver>> {
        self.sessions
            .read()
            .ok()
            .and_then(|sessions| sessions.get(&server_id).cloned())
    }

    pub fn insert(&self, server_id: Uuid, driver: Arc<dyn NodeDriver>) {
        if let Ok(mut sessions) = self.sessions.write() {
            sessions.insert(server_id, driver);
        }
    }

    pub fn remove(&self, server_id: Uuid) {
        if let Ok(mut sessions) = self.sessions.write() {
            sessions.remove(&server_id);
        }
    }

    pub fn connected(&self) -> Vec<Uuid> {
        self.sessions
            .read()
            .map(|sessions| sessions.keys().copied().collect())
            .unwrap_or_default()
    }
}
