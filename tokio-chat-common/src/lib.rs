//! tokio-chat-common provides the tokio codecs for client/server communication.
//!
//! This crate should be straightforward; see tokio-chat-server for a description of the
//! client/server protocol.
#[macro_use]
extern crate serde_derive;

extern crate serde;
extern crate serde_json;
extern crate tokio_core;
extern crate byteorder;

mod codec;

// Handshake message sent from a client to a server when it first connects, identifying the
// username of the client.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Handshake {
    pub name: String,
}

impl Handshake {
    pub fn new<S: Into<String>>(name: S) -> Handshake {
        Handshake { name: name.into() }
    }
}

pub type HandshakeCodec = codec::LengthPrefixedJson<Handshake, Handshake>;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ClientMessage(pub String);

impl ClientMessage {
    pub fn new<S: Into<String>>(message: S) -> ClientMessage {
        ClientMessage(message.into())
    }
}

// Enumerate possible messages the server can send to clients.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ServerMessage {
    // A message from a client (first String) containing arbitrary content (second String).
    Message(String, String),

    // Notification of a new user connection. The associated String is the name that user provided
    // in their Handshake.
    UserConnected(String),
    //
    // Notification of user disconnection. The associated String is the name that user provided
    // in their Handshake.
    UserDisconnected(String),
}

pub type ServerToClientCodec = codec::LengthPrefixedJson<ClientMessage, ServerMessage>;
pub type ClientToServerCodec = codec::LengthPrefixedJson<ServerMessage, ClientMessage>;
