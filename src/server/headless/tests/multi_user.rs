//! Several people attached to one session: independent pane focus and presence.

use super::*;
use crate::api::schema::{Method, PaneDirection, PaneFocusDirectionParams, PaneTarget};
use crate::protocol::endpoint::{
    EndpointPresencePeers, EndpointPresencePointer, EndpointPresencePosition,
};

/// One tab split into three side-by-side panes: left, middle, right (focused).
fn three_pane_server() -> (HeadlessServer, [crate::layout::PaneId; 3]) {
    let mut server = test_headless_server();
    let mut workspace = crate::workspace::Workspace::test_new("pairing");
    let left = workspace.tabs[0].root_pane;
    let middle = workspace.test_split(ratatui::layout::Direction::Horizontal);
    let right = workspace.test_split(ratatui::layout::Direction::Horizontal);
    server.app.state.workspaces = vec![workspace];
    server.app.state.ensure_test_terminals();
    server.app.state.active = Some(0);
    server.app.state.selected = 0;
    server.app.state.mode = crate::app::Mode::Terminal;
    (server, [left, middle, right])
}

fn shell_request(server: &mut HeadlessServer, client_id: u64, id: &str, method: Method) {
    let (respond_to, _response_rx) = std::sync::mpsc::channel();
    server.handle_client_shell_api_request(
        client_id,
        crate::api::ApiRequestMessage {
            request: crate::api::schema::Request {
                id: id.into(),
                method,
            },
            respond_to,
            response_write_complete: None,
        },
    );
}

fn public_request(server: &mut HeadlessServer, id: &str, method: Method) {
    let (respond_to, _response_rx) = std::sync::mpsc::channel();
    server.handle_api_request_with_shutdown_check(crate::api::ApiRequestMessage {
        request: crate::api::schema::Request {
            id: id.into(),
            method,
        },
        respond_to,
        response_write_complete: None,
    });
}

/// Drains a control channel and returns the newest snapshot and peer list it carried.
///
/// The test writer forwards through a thread, so wait briefly for in-flight messages.
fn drain_control(
    control: &std::sync::mpsc::Receiver<Vec<u8>>,
) -> (
    Option<crate::protocol::ClientShellSnapshot>,
    Option<EndpointPresencePeers>,
) {
    let mut snapshot = None;
    let mut peers = None;
    while let Ok(bytes) = control.recv_timeout(std::time::Duration::from_millis(50)) {
        if let ServerMessage::EndpointControl { kind, data } = read_server_message(bytes) {
            if kind == crate::protocol::endpoint::ENDPOINT_SNAPSHOT_KIND {
                snapshot = Some(serde_json::from_str(&data).expect("snapshot json"));
            } else if kind == crate::protocol::endpoint::PRESENCE_PEERS_KIND {
                peers = Some(serde_json::from_str(&data).expect("presence json"));
            }
        }
    }
    (snapshot, peers)
}

fn focused_pane(server: &HeadlessServer, client_id: u64) -> crate::layout::PaneId {
    server
        .shell_focus_target(client_id)
        .expect("focus target")
        .pane_id
}

#[tokio::test]
async fn clicking_a_pane_does_not_move_another_clients_focus() {
    let (mut server, [left, _middle, right]) = three_pane_server();
    let left_id = server.app.public_pane_id(0, left).unwrap();
    let right_id = server.app.public_pane_id(0, right).unwrap();
    let (first_control, first_render) = connect_matching_test_shell(&mut server, 81);
    let (second_control, second_render) = connect_matching_test_shell(&mut server, 82);
    drain_control(&first_control);
    drain_control(&second_control);

    shell_request(
        &mut server,
        81,
        "click-left",
        Method::PaneFocus(PaneTarget {
            pane_id: left_id.clone(),
        }),
    );
    server.render_and_stream();

    assert_eq!(focused_pane(&server, 81), left);
    assert_eq!(focused_pane(&server, 82), right);
    let (first_snapshot, _) = drain_control(&first_control);
    let (second_snapshot, _) = drain_control(&second_control);
    assert_eq!(
        first_snapshot.expect("first snapshot").focused_pane_id,
        Some(left_id.clone())
    );
    // The second client's projection did not change, so it may not get a new snapshot;
    // if it does, it must still show its own pane.
    if let Some(snapshot) = second_snapshot {
        assert_eq!(snapshot.focused_pane_id, Some(right_id.clone()));
    }

    // Each client's surface highlights its own pane.
    let focused_in = |render: &std::sync::mpsc::Receiver<Vec<u8>>| {
        let mut focused = None;
        while let Ok(bytes) = render.recv_timeout(std::time::Duration::from_millis(50)) {
            if let ServerMessage::PaneSurface(surface) = read_server_message(bytes) {
                focused = surface
                    .panes
                    .iter()
                    .find(|pane| pane.focused)
                    .map(|pane| pane.pane_id.clone());
            }
        }
        focused
    };
    assert_eq!(focused_in(&first_render), Some(left_id));
    if let Some(focused) = focused_in(&second_render) {
        assert_eq!(focused, right_id);
    }
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn focus_relative_requests_act_on_the_requesting_clients_pane() {
    let (mut server, [left, middle, right]) = three_pane_server();
    let left_id = server.app.public_pane_id(0, left).unwrap();
    let (first_control, _) = connect_matching_test_shell(&mut server, 83);
    let (second_control, _) = connect_matching_test_shell(&mut server, 84);
    drain_control(&first_control);
    drain_control(&second_control);

    // The first client leaves the shared layout focus on the left pane...
    shell_request(
        &mut server,
        83,
        "click-left",
        Method::PaneFocus(PaneTarget { pane_id: left_id }),
    );
    assert_eq!(
        server.app.state.workspaces[0].tabs[0].layout.focused(),
        left
    );

    // ...but "focus left" from the second client starts from its own right pane.
    shell_request(
        &mut server,
        84,
        "focus-left",
        Method::PaneFocusDirection(PaneFocusDirectionParams {
            pane_id: None,
            direction: PaneDirection::Left,
        }),
    );
    assert_eq!(focused_pane(&server, 84), middle);
    assert_eq!(focused_pane(&server, 83), left);
    assert_ne!(right, middle);
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn public_pane_focus_still_moves_every_client() {
    let (mut server, [left, _middle, right]) = three_pane_server();
    let left_id = server.app.public_pane_id(0, left).unwrap();
    let right_id = server.app.public_pane_id(0, right).unwrap();
    let (first_control, _) = connect_matching_test_shell(&mut server, 85);
    let (second_control, _) = connect_matching_test_shell(&mut server, 86);
    drain_control(&first_control);
    drain_control(&second_control);
    shell_request(
        &mut server,
        85,
        "click-left",
        Method::PaneFocus(PaneTarget { pane_id: left_id }),
    );
    assert_eq!(focused_pane(&server, 85), left);

    // A script or agent focusing a pane is a request for everyone's attention.
    public_request(
        &mut server,
        "api-focus-right",
        Method::PaneFocus(PaneTarget { pane_id: right_id }),
    );
    assert_eq!(focused_pane(&server, 85), right);
    assert_eq!(focused_pane(&server, 86), right);
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn closing_another_clients_pane_falls_back_to_a_live_pane() {
    let (mut server, [left, middle, right]) = three_pane_server();
    let left_id = server.app.public_pane_id(0, left).unwrap();
    let right_id = server.app.public_pane_id(0, right).unwrap();
    let (first_control, _) = connect_matching_test_shell(&mut server, 87);
    let (second_control, _) = connect_matching_test_shell(&mut server, 88);
    drain_control(&first_control);
    drain_control(&second_control);
    shell_request(
        &mut server,
        87,
        "click-left",
        Method::PaneFocus(PaneTarget { pane_id: left_id }),
    );
    assert_eq!(focused_pane(&server, 88), right);

    shell_request(
        &mut server,
        87,
        "close-right",
        Method::PaneClose(PaneTarget { pane_id: right_id }),
    );
    assert_eq!(focused_pane(&server, 87), left);
    let fallback = focused_pane(&server, 88);
    assert!(fallback == left || fallback == middle);
    assert!(server.app.state.workspaces[0].tabs[0]
        .panes
        .contains_key(&fallback));
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn presence_reaches_peers_with_pointer_and_focus_but_not_the_sender() {
    let (mut server, [left, _middle, right]) = three_pane_server();
    let left_id = server.app.public_pane_id(0, left).unwrap();
    let right_id = server.app.public_pane_id(0, right).unwrap();
    // Render receivers stay alive: a dropped render lane disconnects its client.
    let (first_control, _first_render) = connect_matching_test_shell(&mut server, 91);
    let (second_control, _second_render) = connect_matching_test_shell(&mut server, 92);
    server.render_and_stream();
    drain_control(&first_control);
    drain_control(&second_control);

    server.handle_server_event(ServerEvent::ClientShellPresence {
        client_id: 91,
        presence: EndpointPresencePointer {
            name: " sam\u{7} ".into(),
            pointer: Some(EndpointPresencePosition {
                pane_id: left_id.clone(),
                col: 3,
                row: 2,
            }),
        },
    });

    let (_, first_peers) = drain_control(&first_control);
    let (_, second_peers) = drain_control(&second_control);
    let second_peers = second_peers.expect("peer sees presence");
    assert_eq!(second_peers.peers.len(), 1);
    let sam = &second_peers.peers[0];
    assert_eq!(sam.peer_id, 91);
    assert_eq!(sam.name, "sam");
    assert_eq!(
        sam.pointer,
        Some(EndpointPresencePosition {
            pane_id: left_id,
            col: 3,
            row: 2,
        })
    );
    assert_eq!(sam.focused_pane_id, Some(right_id));
    if let Some(first_peers) = first_peers {
        assert!(first_peers.peers.iter().all(|peer| peer.peer_id != 91));
    }

    // An unchanged projection is not resent.
    server.render_and_stream();
    let (_, resent) = drain_control(&second_control);
    assert!(resent.is_none());

    // A pointer is hidden while its window is in the background.
    server.handle_server_event(ServerEvent::ClientShellFocus {
        client_id: 91,
        focused: false,
    });
    server.render_and_stream();
    let (_, background) = drain_control(&second_control);
    assert_eq!(
        background.expect("background update").peers[0].pointer,
        None
    );

    // Leaving clears the peer.
    server.remove_client_and_resize_if_needed(91);
    server.render_and_stream();
    let (_, after_leave) = drain_control(&second_control);
    assert!(after_leave.expect("leave update").peers.is_empty());
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn a_lone_client_receives_no_presence_traffic() {
    let (mut server, [left, ..]) = three_pane_server();
    let left_id = server.app.public_pane_id(0, left).unwrap();
    let (control, _render) = connect_matching_test_shell(&mut server, 95);
    server.handle_server_event(ServerEvent::ClientShellPresence {
        client_id: 95,
        presence: EndpointPresencePointer {
            name: "solo".into(),
            pointer: Some(EndpointPresencePosition {
                pane_id: left_id,
                col: 0,
                row: 0,
            }),
        },
    });
    server.render_and_stream();
    let (_, peers) = drain_control(&control);
    assert!(peers.is_none());
    shutdown_test_runtimes(&mut server);
}

#[tokio::test]
async fn smallest_shared_tab_size_stops_alternating_input_from_resizing_panes() {
    use crate::config::SharedTabSizeConfig;

    for policy in [SharedTabSizeConfig::Smallest, SharedTabSizeConfig::Latest] {
        let mut server = test_headless_server();
        server.app.state.shared_tab_size = policy;
        let mut workspace = crate::workspace::Workspace::test_new("shared-size");
        let pane = workspace.tabs[0].root_pane;
        workspace.insert_test_runtime(
            pane,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b"SHARED"),
        );
        server.app.state.workspaces = vec![workspace];
        server.app.state.active = Some(0);
        server.app.state.selected = 0;
        server.app.state.mode = crate::app::Mode::Terminal;
        let pane_id = server.app.public_pane_id(0, pane).unwrap();
        let (_large_control, _large_render) = connect_test_shell(&mut server, 31, 100, 30);
        let (_small_control, _small_render) = connect_test_shell(&mut server, 32, 70, 20);

        let mut sizes = Vec::new();
        for client_id in [31, 32, 31, 32] {
            server.handle_server_event(ServerEvent::ClientShellPaneInput {
                client_id,
                pane_id: pane_id.clone(),
                events: vec![crate::protocol::ClientPaneInputEvent::TextCommit(
                    "x".into(),
                )],
            });
            sizes.push(server.app.state.workspaces[0].test_runtimes[&pane].current_size());
        }

        match policy {
            SharedTabSizeConfig::Smallest => {
                assert!(
                    sizes.windows(2).all(|pair| pair[0] == pair[1]),
                    "alternating input resized the shared pane: {sizes:?}"
                );
                let (rows, cols) = sizes[0];
                assert!(rows <= 20 && cols <= 70, "pane must fit the smaller viewer");

                // Once the smaller viewer leaves, the pane grows to fit who remains.
                server.remove_client_and_resize_if_needed(32);
                let (rows_after, cols_after) =
                    server.app.state.workspaces[0].test_runtimes[&pane].current_size();
                assert!(rows_after > rows && cols_after > cols);
            }
            SharedTabSizeConfig::Latest => {
                assert_ne!(sizes[0], sizes[1], "latest follows whoever typed");
                assert_eq!(sizes[0], sizes[2]);
            }
        }
        shutdown_test_runtimes(&mut server);
    }
}

#[tokio::test]
async fn a_script_closing_one_persons_pane_does_not_move_the_other() {
    let (mut server, [left, _middle, right]) = three_pane_server();
    let left_id = server.app.public_pane_id(0, left).unwrap();
    let right_id = server.app.public_pane_id(0, right).unwrap();
    let (first_control, _) = connect_matching_test_shell(&mut server, 101);
    let (second_control, _) = connect_matching_test_shell(&mut server, 102);
    drain_control(&first_control);
    drain_control(&second_control);
    shell_request(
        &mut server,
        101,
        "click-left",
        Method::PaneFocus(PaneTarget { pane_id: left_id }),
    );
    shell_request(
        &mut server,
        102,
        "click-right",
        Method::PaneFocus(PaneTarget {
            pane_id: right_id.clone(),
        }),
    );
    assert_eq!(
        server.app.state.workspaces[0].tabs[0].layout.focused(),
        right
    );

    public_request(
        &mut server,
        "script-close-right",
        Method::PaneClose(PaneTarget { pane_id: right_id }),
    );
    assert_eq!(focused_pane(&server, 101), left);
    let moved = focused_pane(&server, 102);
    assert_ne!(moved, right);
    assert!(server.app.state.workspaces[0].tabs[0]
        .panes
        .contains_key(&moved));
    shutdown_test_runtimes(&mut server);
}
