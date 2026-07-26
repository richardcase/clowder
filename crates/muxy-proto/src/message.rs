use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize)]
pub struct PaneId(pub u64);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClientToDaemon {
    Attach { pane: PaneId },
    Input { pane: PaneId, bytes: Vec<u8> },
    Resize { pane: PaneId, cols: u16, rows: u16 },
    Detach,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DaemonToClient {
    Attached { pane: PaneId, cols: u16, rows: u16 },
    Output { pane: PaneId, bytes: Vec<u8> },
    PaneExited { pane: PaneId, code: Option<i32> },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_message_roundtrips_through_postcard() {
        let msg = ClientToDaemon::Input { pane: PaneId(7), bytes: b"ls\n".to_vec() };
        let bytes = postcard::to_stdvec(&msg).unwrap();
        let back: ClientToDaemon = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn daemon_message_roundtrips_through_postcard() {
        let msg = DaemonToClient::Output { pane: PaneId(7), bytes: b"file.txt\n".to_vec() };
        let bytes = postcard::to_stdvec(&msg).unwrap();
        let back: DaemonToClient = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(msg, back);
    }
}
