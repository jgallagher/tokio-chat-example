//! A chat server that broadcasts messages to all connections.
//!
//! The server expects to send and receive messages via codecs provided by tokio-chat-common.
//! The message protocol between the client and server is:
//!
//! 1. A new client connects to the server. It must send a single `Handshake` message.
//! 2. After receiving the `Handshake`, the server broadcasts a `ServerMessage::UserConnected`
//!    message to all connected clients (including the new one that triggered this message).
//! 3. The client may send any number of `ClientMessage`s to the server. For each incoming
//!    `ClientMessage`, the server broadcasts a `ServerMessage::Message` to every connected
//!    client (including the client that sent this `ClientMessage`).
//! 4. When a client disconnects, the server broadcasts a `ServerMessage::UserDisconnected`
//!    message to all remaining connected clients. This step is skipped if the client disconnecting
//!    never completed the `Handshake` in step 1.
//!
//! To test this, run
//!
//!     cargo run
//!
//! in this project and then in another window run one or more instances the tokio-chat-client
//! binary.

extern crate futures;
extern crate tokio_core;
extern crate tokio_chat_common;

use std::cell::RefCell;
use std::rc::Rc;
use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use tokio_core::io::Io;
use tokio_core::reactor::Core;
use tokio_core::net::TcpListener;
use futures::{Stream, Sink, Future};
use futures::stream;
use futures::sync::mpsc;
use tokio_chat_common::{HandshakeCodec, ClientMessage, ServerMessage, ServerToClientCodec};

// For each client that connects, we hang on to an mpsc::Sender (to send the task managing
// that client messages) and the name they gave us during handshaking.
#[derive(Clone)]
struct Client {
    tx: mpsc::Sender<ServerMessage>,
    name: String,
}

impl Client {
    fn new<S: Into<String>>(tx: mpsc::Sender<ServerMessage>, name: S) -> Client {
        Client {
            tx: tx,
            name: name.into(),
        }
    }
}

// The server is single-threaded, so we can keep all clients in a single Rc<RefCell<HashMap<_>>>.
#[derive(Clone)]
struct ConnectedClients(Rc<RefCell<HashMap<SocketAddr, Client>>>);

impl ConnectedClients {
    fn new() -> ConnectedClients {
        ConnectedClients(Rc::new(RefCell::new(HashMap::new())))
    }

    // Called when a new client connects and sends us a `Handshake`.
    fn insert(&self, addr: SocketAddr, client: Client) {
        self.0.borrow_mut().insert(addr, client);
    }

    // Called when a client disconnects. The return value will be `Some(client)` if `addr` had
    // successfully sent us a `Handshake` and `None` otherwise.
    fn remove(&self, addr: &SocketAddr) -> Option<Client> {
        self.0.borrow_mut().remove(addr)
    }

    // Broadcast `message` to all clients. The return type of this method involves closures,
    // so we either have to Box it (as in this case) or wait for `impl Trait`. Note that the
    // broadcast performed here doesn't actually do any socket communication at all; it merely
    // sends the message along the `mpsc::Sender` associated with each connected `Client`.
    //
    // Note that the `Error` type of the returned future can be anything at all. This makes it
    // easier to insert calls to this method in other contexts. This method itself will never
    // fail. Perhaps the return type could change to `Box<Future<Item = (), Error = !>>` once
    // `!` lands?
    fn broadcast<E: 'static>(&self, message: ServerMessage) -> Box<Future<Item = (), Error = E>> {
        let client_map = self.0.borrow();

        // For each client, clone its `mpsc::Sender` (because sending consumes the sender) and
        // start sending a clone of `message`. This produces an iterator of Futures.
        let all_sends = client_map.values().map(|client| client.tx.clone().send(message.clone()));

        // Collect the futures into a stream. We don't care about:
        //
        //    1. what order they finish (hence `futures_unordered`)
        //    2. the result of any individual send (hence the `.then(|_| Ok(()))`. If the send
        //       succeeds we don't need the `Sender` back since we still have it in our hashmap.
        //       If the send fails its because the receiver is gone, so we don't need to broadcast
        //       to them anyway.
        let send_stream = stream::futures_unordered(all_sends).then(|_| Ok(()));

        // Convert the stream to a future that runs all the sends and box it up.
        Box::new(send_stream.for_each(|()| Ok(())))
    }
}

// Helper function for figuring out the types of futures. A common tool for getting the compiler
// to tell you the type of a variable is
//
//      let x: () = some_variable;
//
//  but this largely fails spectacularly with futures because their types are so involved. This
//  helper function lets you do this instead:
//
//      _debugf(some_future)
//
//  which will _usually_ fail to compile with a useful error message about the type of the
//  future's `Item` or `Error` being whatever it really is instead of `()`. This isn't perfect
//  because calling this function can sometimes interfere with type inference, but that can often
//  be worked around as well with some reordering or extra temporary variables.
fn _debugf<F: Future<Item = (), Error = ()>>(_: F) {}

// Like `_debugf` but for `Stream`s instead of `Future`s.
fn _debugs<S: Stream<Item = (), Error = ()>>(_: S) {}

fn main() {
    let addr = "0.0.0.0:12345".parse().unwrap();

    // Create the event loop and TCP listener we'll accept connections on.
    let mut core = Core::new().unwrap();
    let handle = core.handle();
    let listener = TcpListener::bind(&addr, &handle).unwrap();

    // Create our (currently empty) stash of clients.
    let clients = ConnectedClients::new();

    let server = listener.incoming().for_each(move |(socket, addr)| {
        // Frame the socket in a codec that will give us a `Handshake`.
        let handshake_io = socket.framed(HandshakeCodec::new());

        // `handshake_io` is a stream, but we just want to read a single `Handshake` off of it
        // then convert the socket into a different kind of stream. `.into_future()` lets us
        // do exactly that, giving us a `Future<Item=(Option<Handshake>, S)>` where `S` is the
        // `handshake_io` itself.
        //
        // If an error occurs, we just want the error and can discard the stream.
        let handshake = handshake_io.into_future()
            .map_err(|(err, _)| err)
            .and_then(move |(h, io)| {
                // `h` here is an `Option<Handshake>`. If we did not get a `Handshake`, throw
                // an error. This can happen if a client connects then disconnects, for example.
                // If we did get a handshake, log the client's name and return both the handshake
                // and the unframed socket. (`io.into_inner()` removes the framing and gives back
                // the underlying `Io` handle, which is `socket` in this case.
                h.map_or_else(|| Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
                              move |h| {
                                  println!("CONNECTED from {:?} with name {}", addr, h.name);
                                  Ok((h, io.into_inner()))
                              })
            });

        // If the handshake succeeds, the next step is to broadcast the `UserConnected` message.
        let clients_inner = clients.clone();
        let announce_connect = handshake.and_then(move |(handshake, socket)| {
            let clients = clients_inner.clone();
            let name = handshake.name;

            // Create the Sender/Receiver pair for this newly-connected client, and store them
            // in our HashMap.
            let (tx, rx) = mpsc::channel(8);
            clients.insert(addr, Client::new(tx, name.clone()));

            // Broadcast the message, and send this client's name, `mpsc::Receiver`, and socket
            // as the `Item` of this future.
            clients.broadcast(ServerMessage::UserConnected(name.clone()))
                .map(|()| (name, rx, socket))
        });

        // After broadcasting the announcment, the next step is to set up the futures that
        // sit on top of the reading/writing of the socket.
        let clients_inner = clients.clone();
        let connection = announce_connect.and_then(|(name, rx, socket)| {
            // Frame the socket in a codec that lets us receive `ClientMessage`s and send
            // `ServerMessage`s.
            let (to_client, from_client) = socket.framed(ServerToClientCodec::new()).split();

            // For each incoming `ClientMessage`, attach the sending client's `name` and
            // broadcast the resulting `ServerMessage::Message` to all connected clients.
            let reader = from_client.for_each(move |ClientMessage(msg)| {
                let msg = ServerMessage::Message(name.clone(), msg);
                clients_inner.broadcast(msg)
            });

            // Writing to the socket involves receiving messages on the channel that was
            // initially created in `announce_connect` above and sent to us as part of the
            // `Item` type of that future.
            let writer = rx
                .map_err(|()| unreachable!("rx can't fail"))

                // `fold` seems to be the most straightforward way to handle this. It takes
                // an initial value of `to_client` (the sending half of the framed socket);
                // for each message, it tries to send the message, and the future returned
                // by `to_client.send` gives back `to_client` itself on success, ready for the
                // next step of the fold.
                .fold(to_client, |to_client, msg| {
                    to_client.send(msg)
                })

                // Once the rx stream is exhausted (because the sender has been dropped), we
                // no longer need the writing half of the socket, so discard it.
                .map(|_| ());

            // Use select to allow either the reading or writing half dropping to drop the other
            // half. The `map` and `map_err` here effectively force this drop.
            reader.select(writer).map(|_| ()).map_err(|(err, _)| err)
        });

        // Finally, spawn off the connection.
        let clients_inner = clients.clone();
        handle.spawn(connection.then(move |r| {
            println!("DISCONNECTED from {:?} with result {:?}", addr, r);

            // When a client disconnects, we want to send a message to all remaining clients. This
            // is a little tricky since `msg` here is an `Option<Client>` (and will be `None`
            // if this disconnection is a client who never sent a `Handshake`). The natural thing
            // to try to write here is something like
            //
            //      if let Some(msg) = msg {
            //          clients_inner.broadcast(msg)
            //      } else {
            //          // ... now what? has to have the same return type as the `if branch`
            //      }
            //
            // but we can sidestep this by taking advantage of the fact that `Option` can also
            // act as an `Iterator` over its single (or no) element, convert that to a `Stream`
            // via `stream::iter`, then `fold` over the 0-or-1 long stream to send the message.
            let msg = clients_inner.remove(&addr)
                .map(|client| ServerMessage::UserDisconnected(client.name));
            stream::iter(msg.map(|m| Ok(m))).fold((), move |(), m| clients_inner.broadcast(m))
        }));

        Ok(())
    });

    // Don't forget to actually execute the server!
    core.run(server).unwrap();
}
