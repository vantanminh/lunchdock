//! Generated gRPC types for the agent session.
//!
//! The heavy operation payloads are JSON bytes inside [`agent::Command`] so
//! `rudder_core` stays the source of truth for request and response shapes.

pub mod agent {
    #![allow(clippy::all, clippy::pedantic, missing_docs, unused_imports)]
    tonic::include_proto!("rudder.agent.v1");
}

/// Re-export so callers can name the service without walking the module tree.
pub use agent::node_agent_client::NodeAgentClient;
pub use agent::node_agent_server::{NodeAgent, NodeAgentServer};
