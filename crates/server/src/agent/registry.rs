//! Connected agent sessions, keyed by server id.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use rudder_core::NodeDriver;
use uuid::Uuid;

use crate::driver::remote::{RemoteDriver, RemoteHandle};

#[derive(Default)]
pub struct AgentRegistry {
    sessions: RwLock<HashMap<Uuid, Arc<RemoteDriver>>>,
}

impl AgentRegistry {
    pub fn driver(&self, server_id: Uuid) -> Option<Arc<dyn NodeDriver>> {
        self.session(server_id)
            .map(|session| Arc::new(RemoteHandle(session)) as Arc<dyn NodeDriver>)
    }

    pub fn session(&self, server_id: Uuid) -> Option<Arc<RemoteDriver>> {
        self.sessions
            .read()
            .ok()
            .and_then(|sessions| sessions.get(&server_id).cloned())
            .filter(|session| session.is_open())
    }

    /// Register a new session, closing any older one for the same server.
    pub fn insert(&self, session: Arc<RemoteDriver>) {
        let previous = self
            .sessions
            .write()
            .ok()
            .and_then(|mut sessions| sessions.insert(session.server_id, session));
        if let Some(previous) = previous {
            previous.close();
        }
    }

    /// Remove a session, unless a newer one has already replaced it.
    pub fn remove(&self, server_id: Uuid, session_id: Uuid) -> bool {
        let Ok(mut sessions) = self.sessions.write() else {
            return false;
        };
        if sessions
            .get(&server_id)
            .is_some_and(|current| current.session_id == session_id)
        {
            sessions.remove(&server_id);
            return true;
        }
        false
    }

    /// Drop a server's session, for example when the server is deleted.
    pub fn disconnect(&self, server_id: Uuid) {
        let removed = self
            .sessions
            .write()
            .ok()
            .and_then(|mut sessions| sessions.remove(&server_id));
        if let Some(session) = removed {
            session.close();
        }
    }

    pub fn connected(&self) -> Vec<Uuid> {
        self.sessions
            .read()
            .map(|sessions| {
                sessions
                    .values()
                    .filter(|s| s.is_open())
                    .map(|s| s.server_id)
                    .collect()
            })
            .unwrap_or_default()
    }
}
