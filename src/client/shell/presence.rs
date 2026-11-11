//! Shared-session presence in the client shell.
//!
//! When several people attach to one endpoint, each shell publishes its display name and
//! pane-relative pointer, and draws everyone else's pointer and focused pane on top of the
//! composed pane surface. Presence is presentation only: it never changes input routing.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crossterm::event::MouseEvent;
use ratatui::style::{Color, Modifier};
use unicode_width::UnicodeWidthStr;

use super::{ClientEndpointId, ClientShellInput, ClientShellState};
use crate::protocol::endpoint::{
    EndpointPresencePeer, EndpointPresencePeers, EndpointPresencePointer, EndpointPresencePosition,
};
use crate::protocol::{ClientMessage, FrameData};

/// How long a peer's name stays beside its pointer after the pointer last moved.
pub(super) const PRESENCE_LABEL_TTL: Duration = Duration::from_millis(2500);

/// Distinct mid-luminance backgrounds that stay legible with dark text in light and dark
/// themes.
const PEER_COLORS: [Color; 6] = [
    Color::Rgb(0xff, 0x9e, 0x64),
    Color::Rgb(0x7d, 0xcf, 0xff),
    Color::Rgb(0xc0, 0x9a, 0xff),
    Color::Rgb(0x9e, 0xce, 0x6a),
    Color::Rgb(0xff, 0x7a, 0xa8),
    Color::Rgb(0xe0, 0xc0, 0x68),
];
const PEER_TEXT: Color = Color::Rgb(0x1a, 0x1b, 0x26);

/// Peers attached to one endpoint connection.
#[derive(Debug, Default)]
pub(crate) struct PeerPresence {
    generation: u64,
    peers: Vec<EndpointPresencePeer>,
    /// When each peer's pointer last moved, for the transient name label.
    moved_at: HashMap<u64, Instant>,
}

/// One cell to draw over the composed frame.
struct PresencePaint {
    x: u16,
    y: u16,
    symbol: Option<String>,
    color: Color,
}

fn color_slot(name: &str) -> usize {
    // FNV-1a keeps one person's color stable across reconnects and machines.
    let mut hash: u32 = 0x811c_9dc5;
    for byte in name.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash as usize % PEER_COLORS.len()
}

/// Assigns each peer a color from its name, moving collisions to the next free slot.
fn peer_colors(peers: &[EndpointPresencePeer]) -> HashMap<u64, Color> {
    let mut taken = [false; PEER_COLORS.len()];
    let mut colors = HashMap::new();
    for peer in peers {
        let preferred = color_slot(&peer.name);
        let slot = (0..PEER_COLORS.len())
            .map(|offset| (preferred + offset) % PEER_COLORS.len())
            .find(|slot| !taken[*slot])
            .unwrap_or(preferred);
        taken[slot] = true;
        colors.insert(peer.peer_id, PEER_COLORS[slot]);
    }
    colors
}

/// Name text limited to single-width characters so labels never split wide glyphs.
fn label_cells(name: &str, max: usize) -> Vec<String> {
    name.chars()
        .take(max)
        .map(|ch| {
            let symbol = ch.to_string();
            if symbol.width() == 1 {
                symbol
            } else {
                "?".to_owned()
            }
        })
        .collect()
}

fn cell_index(frame: &FrameData, x: u16, y: u16) -> Option<usize> {
    (x < frame.width && y < frame.height)
        .then(|| usize::from(y) * usize::from(frame.width) + usize::from(x))
}

fn apply_paint(frame: &mut FrameData, paint: &[PresencePaint]) {
    let replaced = paint
        .iter()
        .filter(|cell| cell.symbol.is_some())
        .map(|cell| (cell.x, cell.y))
        .collect::<std::collections::HashSet<_>>();
    let fg = crate::protocol::color_to_u32(PEER_TEXT);
    for cell in paint {
        let Some(index) = cell_index(frame, cell.x, cell.y) else {
            continue;
        };
        if let Some(symbol) = cell.symbol.as_ref() {
            // A wide glyph whose right half is about to be replaced must not be left behind.
            if cell.x > 0 && !replaced.contains(&(cell.x - 1, cell.y)) {
                if let Some(left) = cell_index(frame, cell.x - 1, cell.y) {
                    if frame.cells[left].symbol.width() > 1 {
                        frame.cells[left].symbol = " ".to_owned();
                    }
                }
            }
            let replaced_wide = frame.cells[index].symbol.width() > 1;
            frame.cells[index].symbol.clone_from(symbol);
            frame.cells[index].skip = false;
            frame.cells[index].hyperlink = None;
            // ...nor may the orphaned continuation of a wide glyph replaced here.
            if replaced_wide && !replaced.contains(&(cell.x.saturating_add(1), cell.y)) {
                if let Some(right) = cell_index(frame, cell.x.saturating_add(1), cell.y) {
                    if frame.cells[right].symbol.is_empty() || frame.cells[right].skip {
                        frame.cells[right].symbol = " ".to_owned();
                        frame.cells[right].skip = false;
                    }
                }
            }
        }
        frame.cells[index].fg = fg;
        frame.cells[index].bg = crate::protocol::color_to_u32(cell.color);
        frame.cells[index].modifier |= Modifier::BOLD.bits();
    }
}

impl ClientShellState {
    /// Replaces the peers attached to one endpoint connection. Returns whether the visible
    /// surface needs a repaint.
    pub(crate) fn set_endpoint_presence(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        peers: EndpointPresencePeers,
    ) -> bool {
        let now = Instant::now();
        let entry = self.peer_presence.entry(endpoint_id.clone()).or_default();
        if entry.generation != generation {
            *entry = PeerPresence {
                generation,
                ..PeerPresence::default()
            };
        }
        let mut moved_at = HashMap::new();
        for peer in &peers.peers {
            let previous = entry
                .peers
                .iter()
                .find(|previous| previous.peer_id == peer.peer_id);
            let moved = peer.pointer.is_some()
                && previous.is_none_or(|previous| previous.pointer != peer.pointer);
            if moved {
                moved_at.insert(peer.peer_id, now);
            } else if let Some(at) = entry.moved_at.get(&peer.peer_id) {
                moved_at.insert(peer.peer_id, *at);
            }
        }
        let changed = entry.peers != peers.peers;
        entry.peers = peers.peers;
        entry.moved_at = moved_at;
        changed && self.config.show_peer_cursors && endpoint_id == &self.active_endpoint_id
    }

    /// Records this shell's pointer and returns the presence update to publish, if any.
    pub(super) fn update_presence_pointer(
        &mut self,
        mouse: MouseEvent,
        outcome: &mut ClientShellInput,
    ) {
        let point = (mouse.column, mouse.row);
        let pointer = self
            .hits
            .panes
            .iter()
            .find(|hit| !hit.popup && super::contains(hit.inner_rect, point))
            .map(|hit| EndpointPresencePosition {
                pane_id: hit.pane_id.clone(),
                col: mouse.column.saturating_sub(hit.inner_rect.x),
                row: mouse.row.saturating_sub(hit.inner_rect.y),
            });
        self.presence_pointer = pointer;
        // Only endpoints that advertised presence are announced to; others never hear of it.
        let announced = self
            .presence_announced
            .as_ref()
            .is_some_and(|(endpoint_id, _)| *endpoint_id == self.active_endpoint_id);
        if !announced {
            return;
        }
        if let Some(message) = self.presence_message(false) {
            outcome.requests.push(message);
        }
    }

    /// Builds a presence update for the active endpoint, skipping unchanged pointers unless
    /// `force` asks to (re)announce this shell to a new connection.
    fn presence_message(&mut self, force: bool) -> Option<ClientMessage> {
        if !force && self.presence_pointer_sent.as_ref() == Some(&self.presence_pointer) {
            return None;
        }
        let message =
            crate::protocol::endpoint::presence_pointer_message(&EndpointPresencePointer {
                name: self.config.presence_name.clone(),
                pointer: self.presence_pointer.clone(),
            })
            .ok()?;
        self.presence_pointer_sent = Some(self.presence_pointer.clone());
        Some(message)
    }

    /// Announces this shell once per endpoint connection so peers learn its name before
    /// it first moves the pointer.
    pub(crate) fn presence_announcement(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
    ) -> Option<ClientMessage> {
        let connection = (endpoint_id.clone(), generation);
        if self.presence_announced.as_ref() == Some(&connection) {
            return None;
        }
        self.presence_announced = Some(connection);
        // A pointer over another endpoint's pane is dropped by this endpoint's server.
        self.presence_message(true)
    }

    /// Pretend every peer's pointer moved `by` earlier than it did.
    #[cfg(test)]
    pub(crate) fn age_peer_presence(&mut self, by: Duration) {
        for presence in self.peer_presence.values_mut() {
            for moved in presence.moved_at.values_mut() {
                *moved = moved.checked_sub(by).unwrap_or(*moved);
            }
        }
    }

    /// Whether a peer name label just expired and the frame needs a repaint.
    pub(crate) fn tick_peer_presence(&mut self, now: Instant) -> bool {
        if self
            .presence_label_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.presence_label_deadline = None;
            return true;
        }
        false
    }

    /// Draws peers' focused-pane tags and pointers over the composed pane surface and
    /// returns when the earliest drawn name label expires.
    pub(super) fn render_peer_presence(
        &self,
        frame: &mut FrameData,
        occlusion: &mut crate::kitty_graphics::surface::Occlusion,
    ) -> Option<Instant> {
        if !self.config.show_peer_cursors {
            return None;
        }
        let presence = self
            .peer_presence
            .get(&self.active_endpoint_id)
            .filter(|presence| Some(presence.generation) == self.active_snapshot_generation)?;
        if presence.peers.is_empty() {
            return None;
        }
        let now = Instant::now();
        let colors = peer_colors(&presence.peers);
        let mut paint = Vec::new();
        let mut label_deadline: Option<Instant> = None;

        // Focus tags sit right-aligned on the pane's top border, stacking leftward.
        let mut tag_right_edge = HashMap::<&str, u16>::new();
        for peer in &presence.peers {
            let Some(pane_id) = peer.focused_pane_id.as_deref() else {
                continue;
            };
            let Some(hit) = self
                .hits
                .panes
                .iter()
                .find(|hit| !hit.popup && hit.pane_id == pane_id)
            else {
                continue;
            };
            if hit.inner_rect.y <= hit.rect.y || hit.rect.width < 6 {
                continue;
            }
            let color = colors.get(&peer.peer_id).copied().unwrap_or(PEER_COLORS[0]);
            let right = tag_right_edge
                .get(pane_id)
                .copied()
                .unwrap_or_else(|| hit.rect.right().saturating_sub(2));
            let left_limit = hit.rect.x.saturating_add(2);
            let available = usize::from(right.saturating_sub(left_limit));
            if available < 3 {
                continue;
            }
            let mut cells = vec![" ".to_owned()];
            cells.extend(label_cells(&peer.name, available.saturating_sub(2)));
            cells.push(" ".to_owned());
            let start = right.saturating_sub(cells.len() as u16);
            for (offset, symbol) in cells.into_iter().enumerate() {
                paint.push(PresencePaint {
                    x: start + offset as u16,
                    y: hit.rect.y,
                    symbol: Some(symbol),
                    color,
                });
            }
            occlusion.cover(ratatui::layout::Rect::new(
                start,
                hit.rect.y,
                right.saturating_sub(start),
                1,
            ));
            tag_right_edge.insert(pane_id, start.saturating_sub(1));
        }

        // Pointers draw last so they stay visible over tags and each other's labels.
        for peer in &presence.peers {
            let Some(pointer) = peer.pointer.as_ref() else {
                continue;
            };
            let Some(hit) = self
                .hits
                .panes
                .iter()
                .find(|hit| !hit.popup && hit.pane_id == pointer.pane_id)
            else {
                continue;
            };
            let inner = hit.inner_rect;
            if pointer.col >= inner.width || pointer.row >= inner.height {
                continue;
            }
            let x = inner.x + pointer.col;
            let y = inner.y + pointer.row;
            let color = colors.get(&peer.peer_id).copied().unwrap_or(PEER_COLORS[0]);
            paint.push(PresencePaint {
                x,
                y,
                symbol: None,
                color,
            });
            occlusion.cover(ratatui::layout::Rect::new(x, y, 1, 1));

            let label_until = presence
                .moved_at
                .get(&peer.peer_id)
                .map(|moved| *moved + PRESENCE_LABEL_TTL)
                .filter(|until| *until > now);
            let Some(until) = label_until else {
                continue;
            };
            label_deadline = Some(label_deadline.map_or(until, |current| current.min(until)));
            let max = usize::from(inner.width.saturating_sub(1)).min(16);
            let mut cells = vec![" ".to_owned()];
            cells.extend(label_cells(&peer.name, max.saturating_sub(2)));
            cells.push(" ".to_owned());
            let width = cells.len() as u16;
            if width < 3 || width >= inner.width {
                continue;
            }
            // Beside the pointer, flipping to its left at the pane's right edge.
            let start = if x + 1 + width <= inner.right() {
                x + 1
            } else {
                x.saturating_sub(width).max(inner.x)
            };
            for (offset, symbol) in cells.into_iter().enumerate() {
                let cell_x = start + offset as u16;
                if cell_x == x {
                    continue;
                }
                paint.push(PresencePaint {
                    x: cell_x,
                    y,
                    symbol: Some(symbol),
                    color,
                });
            }
            occlusion.cover(ratatui::layout::Rect::new(start, y, width, 1));
        }

        // Keep each pointer cell on top of any label drawn later in the list.
        paint.sort_by_key(|cell| cell.symbol.is_none());
        apply_paint(frame, &paint);
        label_deadline
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(peer_id: u64, name: &str) -> EndpointPresencePeer {
        EndpointPresencePeer {
            peer_id,
            name: name.into(),
            pointer: None,
            focused_pane_id: None,
        }
    }

    #[test]
    fn peer_colors_are_stable_per_name_and_avoid_collisions() {
        let peers = (0..PEER_COLORS.len() as u64)
            .map(|id| peer(id, "same"))
            .collect::<Vec<_>>();
        let colors = peer_colors(&peers);
        let distinct = colors
            .values()
            .map(|color| format!("{color:?}"))
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(distinct.len(), PEER_COLORS.len());

        let alone = peer_colors(&[peer(9, "alex")]);
        let again = peer_colors(&[peer(42, "alex")]);
        assert_eq!(alone[&9], again[&42]);
    }

    #[test]
    fn labels_keep_single_width_cells() {
        assert_eq!(label_cells("al漢x", 8), vec!["a", "l", "?", "x"]);
        assert_eq!(label_cells("abcdef", 3), vec!["a", "b", "c"]);
    }
}
