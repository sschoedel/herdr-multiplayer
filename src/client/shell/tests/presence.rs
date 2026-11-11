use super::*;
use crate::protocol::endpoint::{
    EndpointPresencePeer, EndpointPresencePeers, EndpointPresencePointer, EndpointPresencePosition,
    PRESENCE_POINTER_KIND,
};

const GENERATION: u64 = 7;

fn wide_surface() -> PaneSurfaceFrame {
    let mut surface = surface();
    let buffer = Buffer::with_lines(["shared pane content ", "second row          "]);
    surface.frame = FrameData::from_ratatui_buffer_with_hyperlinks(&buffer, None, &[]);
    let rect = SurfaceRect {
        x: 0,
        y: 0,
        width: 20,
        height: 2,
    };
    surface.panes[0].rect = rect;
    surface.panes[0].inner_rect = rect;
    surface
}

fn presence_state() -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_endpoint_snapshot_for_generation(
        &ClientEndpointId::Local,
        GENERATION,
        Box::new(snapshot()),
    );
    state.set_pane_surface(wide_surface());
    state.compose(106, 20).unwrap();
    state
}

fn pane_mouse(state: &ClientShellState, col: u16, row: u16) -> MouseEvent {
    let pane = &state.hits.panes[0];
    MouseEvent {
        kind: MouseEventKind::Moved,
        column: pane.inner_rect.x + col,
        row: pane.inner_rect.y + row,
        modifiers: KeyModifiers::NONE,
    }
}

fn published(outcome: &ClientShellInput) -> Vec<EndpointPresencePointer> {
    outcome
        .requests
        .iter()
        .filter_map(|request| match request {
            ClientMessage::EndpointControl { kind, data } if kind == PRESENCE_POINTER_KIND => {
                Some(serde_json::from_str(data).expect("presence json"))
            }
            _ => None,
        })
        .collect()
}

fn peer_at(col: u16, row: u16) -> EndpointPresencePeers {
    EndpointPresencePeers {
        peers: vec![EndpointPresencePeer {
            peer_id: 2,
            name: "alex".into(),
            pointer: Some(EndpointPresencePosition {
                pane_id: "pane_1".into(),
                col,
                row,
            }),
            focused_pane_id: Some("pane_1".into()),
        }],
    }
}

#[test]
fn pointer_is_published_only_after_announcement_and_once_per_cell() {
    let mut state = presence_state();
    let quiet = state.handle_raw_events(vec![RawInputEvent::Mouse(pane_mouse(&state, 2, 0))]);
    assert!(
        published(&quiet).is_empty(),
        "endpoints that never advertised presence must not receive it"
    );

    let announcement = state
        .presence_announcement(&ClientEndpointId::Local, GENERATION)
        .expect("first connection announces");
    let ClientMessage::EndpointControl { data, .. } = announcement else {
        panic!("announcement should be an endpoint control");
    };
    let announced: EndpointPresencePointer = serde_json::from_str(&data).unwrap();
    assert!(!announced.name.is_empty());
    assert!(state
        .presence_announcement(&ClientEndpointId::Local, GENERATION)
        .is_none());

    let moved = state.handle_raw_events(vec![RawInputEvent::Mouse(pane_mouse(&state, 5, 1))]);
    assert_eq!(
        published(&moved)
            .into_iter()
            .map(|update| update.pointer)
            .collect::<Vec<_>>(),
        vec![Some(EndpointPresencePosition {
            pane_id: "pane_1".into(),
            col: 5,
            row: 1,
        })]
    );
    let same_cell = state.handle_raw_events(vec![RawInputEvent::Mouse(pane_mouse(&state, 5, 1))]);
    assert!(published(&same_cell).is_empty());

    let sidebar = MouseEvent {
        kind: MouseEventKind::Moved,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    };
    assert!(!contains(state.hits.panes[0].inner_rect, (0, 0)));
    let left = state.handle_raw_events(vec![RawInputEvent::Mouse(sidebar)]);
    assert_eq!(
        published(&left)
            .into_iter()
            .map(|update| update.pointer)
            .collect::<Vec<_>>(),
        vec![None]
    );
}

#[test]
fn peer_pointer_and_name_are_drawn_over_the_pane_then_the_name_fades() {
    let mut state = presence_state();
    assert!(state.set_endpoint_presence(&ClientEndpointId::Local, GENERATION, peer_at(3, 1)));
    let frame = state.compose(106, 20).unwrap().frame;
    let pane = state.hits.panes[0].inner_rect;
    let cell = |col: u16| {
        &frame.cells[usize::from(pane.y + 1) * usize::from(frame.width) + usize::from(pane.x + col)]
    };
    let text = crate::protocol::color_to_u32(ratatui::style::Color::Rgb(0x1a, 0x1b, 0x26));
    // The pointer cell keeps the pane's text and gains the peer's color.
    assert_eq!(cell(3).symbol, "o");
    assert_eq!(cell(3).fg, text);
    assert_ne!(cell(3).bg, cell(0).bg);
    // The name follows the pointer.
    let label = (4..10)
        .map(|col| cell(col).symbol.clone())
        .collect::<String>();
    assert_eq!(label, " alex ");

    let deadline = state
        .presence_label_deadline
        .expect("visible name schedules its own removal");
    assert!(!state.tick_peer_presence(deadline - std::time::Duration::from_millis(1)));
    assert!(state.tick_peer_presence(deadline));
    state.age_peer_presence(super::super::presence::PRESENCE_LABEL_TTL);
    let faded = state.compose(106, 20).unwrap().frame;
    assert!(state.presence_label_deadline.is_none());
    let faded_cell = |col: u16| {
        &faded.cells[usize::from(pane.y + 1) * usize::from(faded.width) + usize::from(pane.x + col)]
    };
    // Once the pointer rests, only the colored cell remains over the pane text.
    assert_eq!(faded_cell(3).fg, text);
    assert_eq!(
        (4..10)
            .map(|col| faded_cell(col).symbol.clone())
            .collect::<String>(),
        "nd row"
    );
}

#[test]
fn presence_from_another_connection_or_with_cursors_disabled_is_not_drawn() {
    let mut state = presence_state();
    let baseline = state.compose(106, 20).unwrap().frame;

    assert!(state.set_endpoint_presence(&ClientEndpointId::Local, GENERATION + 1, peer_at(3, 1)));
    assert_eq!(state.compose(106, 20).unwrap().frame.cells, baseline.cells);

    let mut config = Config::default();
    config.ui.show_peer_cursors = false;
    let mut hidden = ClientShellState::new(ClientShellConfig::from_config(&config));
    hidden.set_endpoint_snapshot_for_generation(
        &ClientEndpointId::Local,
        GENERATION,
        Box::new(snapshot()),
    );
    hidden.set_pane_surface(wide_surface());
    let hidden_baseline = hidden.compose(106, 20).unwrap().frame;
    assert!(!hidden.set_endpoint_presence(&ClientEndpointId::Local, GENERATION, peer_at(3, 1)));
    assert_eq!(
        hidden.compose(106, 20).unwrap().frame.cells,
        hidden_baseline.cells
    );
}
