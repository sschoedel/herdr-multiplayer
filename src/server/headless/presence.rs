//! Shared-session presence: who else is attached, where their pointer is, and which pane
//! they are working in.
//!
//! Presence is a projection over per-connection state. Each shell publishes its display name
//! and pane-relative pointer; the server pairs that with the connection's own pane focus and
//! sends every other shell a full replacement whenever its view of its peers changes.

use super::*;
use crate::protocol::endpoint::{
    EndpointPresencePeer, EndpointPresencePeers, EndpointPresencePointer,
};

impl HeadlessServer {
    pub(super) fn apply_client_shell_presence(
        &mut self,
        client_id: u64,
        presence: EndpointPresencePointer,
    ) {
        let pointer = presence.pointer.and_then(|pointer| {
            let (workspace_index, pane_id) = self.app.parse_pane_id(&pointer.pane_id)?;
            Some(crate::protocol::endpoint::EndpointPresencePosition {
                pane_id: self.app.public_pane_id(workspace_index, pane_id)?,
                col: pointer.col,
                row: pointer.row,
            })
        });
        let Some(client) = self.clients.get_mut(&client_id) else {
            return;
        };
        if !client.is_shell_client() {
            return;
        }
        client.presence = Some(crate::server::clients::ClientPresence {
            name: crate::protocol::endpoint::sanitize_presence_name(&presence.name),
            pointer,
        });
        self.sync_presence();
    }

    fn presence_peer(&self, client_id: u64) -> Option<EndpointPresencePeer> {
        let client = self.clients.get(&client_id)?;
        let presence = client.presence.as_ref();
        let name = presence
            .map(|presence| presence.name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| format!("peer {client_id}"));
        // A pointer left behind in a window that lost focus, or over a pane this person
        // no longer shows, would point at nothing they are looking at.
        let pointer = presence
            .and_then(|presence| presence.pointer.clone())
            .filter(|_| client.outer_terminal_focus != Some(false))
            .filter(|pointer| {
                self.app.parse_pane_id(&pointer.pane_id).is_some_and(
                    |(workspace_index, pane_id)| {
                        self.shell_client_views_pane(client_id, workspace_index, pane_id)
                    },
                )
            });
        let focused_pane_id = self.shell_focus_target(client_id).and_then(|target| {
            self.app
                .public_pane_id(target.workspace_index, target.pane_id)
        });
        Some(EndpointPresencePeer {
            peer_id: client_id,
            name,
            pointer,
            focused_pane_id,
        })
    }

    /// Sends each shell its current peers when they differ from what it last received.
    ///
    /// Cost is one small projection per attached shell, and only while at least two shells
    /// share this endpoint or one still holds a stale non-empty peer list.
    pub(super) fn sync_presence(&mut self) {
        let peer_ids = self
            .clients
            .iter()
            .filter(|(_, client)| client.is_active_shell_client() && client.writer.is_some())
            .map(|(&client_id, _)| client_id)
            .collect::<Vec<_>>();
        let has_stale_peers = self.clients.values().any(|client| {
            client
                .presence_peers_sent
                .as_ref()
                .is_some_and(|sent| !sent.peers.is_empty())
        });
        if peer_ids.len() < 2 && !has_stale_peers {
            return;
        }
        let mut peers = peer_ids
            .iter()
            .filter_map(|&client_id| self.presence_peer(client_id))
            .collect::<Vec<_>>();
        peers.sort_by_key(|peer| peer.peer_id);

        let recipients = self
            .clients
            .iter()
            .filter(|(_, client)| client.is_shell_client() && client.writer.is_some())
            .map(|(&client_id, _)| client_id)
            .collect::<Vec<_>>();
        for client_id in recipients {
            let visible = EndpointPresencePeers {
                peers: peers
                    .iter()
                    .filter(|peer| peer.peer_id != client_id)
                    .cloned()
                    .collect(),
            };
            let Some(client) = self.clients.get_mut(&client_id) else {
                continue;
            };
            let unchanged = match client.presence_peers_sent.as_ref() {
                Some(sent) => *sent == visible,
                None => visible.peers.is_empty(),
            };
            if unchanged {
                continue;
            }
            let framed = match crate::protocol::endpoint::presence_peers_message(&visible)
                .map_err(|err| err.to_string())
                .and_then(|message| {
                    Self::frame_server_message(&message).map_err(|err| err.to_string())
                }) {
                Ok(framed) => framed,
                Err(err) => {
                    warn!(client_id, err = %err, "failed to encode presence peers");
                    continue;
                }
            };
            let Some(writer) = client.writer.as_ref() else {
                continue;
            };
            if writer.control.send(framed).is_ok() {
                client.presence_peers_sent = Some(visible);
            }
        }
    }
}
