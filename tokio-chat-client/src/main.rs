//! A chat client written to talk to tokio-chat-server and present messages in a TUI.
//!
//! To run this, first start tokio-chat-server in another window, then run
//!
//!     cargo run -- your_chat_username
//!
//! to connect to the server. If all goes well, you should see a textual chat-like interface
//! with the message `* your_chat_username connected`, and you should be able to type messages.
//! Start up another instance of this client (probably with a different username) in another
//! window to confirm messages are being broadcast to all clients.
//!
//! The 10,000-foot view archiecture of this client is that a thread is spawned to run a tokio
//! reactor with the client connection to the server, that thread is given a
//! `std::sync::mpsc::Sender` it can use to send messages back to the GUI thread, and the GUI
//! thread is given a `futures::mpsc::sync::Sender` to send the user's chat messages to the tokio
//! thread. See the comments in tokio-chat-server for a description of the client/server protocol.
//!
//! Note that the GUI code below is mostly unannotated except where it comes into contact with
//! tokio-like things.
extern crate futures;
extern crate tokio_core;
extern crate cursive;
extern crate unicode_width;
extern crate tokio_chat_common;

use cursive::Cursive;
use cursive::direction::Direction;
use cursive::event::{Event, Key};
use cursive::theme::Theme;
use cursive::traits::{Boxable, Identifiable, View};
use cursive::views::{EditView, LinearLayout};
use std::thread;

use std::net::SocketAddr;
use tokio_core::io::Io;
use tokio_core::reactor::Core;
use tokio_core::net::TcpStream;
use futures::{Stream, Sink, Future};
use futures::sync::mpsc;
use tokio_chat_common::{Handshake, HandshakeCodec, ClientMessage, ServerMessage,
                        ClientToServerCodec};

mod chat_view;
use self::chat_view::ChatView;

// GuiEventSender is a wrapper around an MPSC Sender (NOTE: This is a `std::sync::mpsc::Sender`,
// _not_ a `futures::sync::mpsc::Sender`!). This allows us to send closures to be run in the
// Cursive GUI context.
#[derive(Clone)]
struct GuiEventSender(std::sync::mpsc::Sender<Box<Fn(&mut Cursive) + Send>>);

impl GuiEventSender {
    fn send<F>(&self, f: F)
        where F: Fn(&mut GuiWrapper) + Send + 'static
    {
        self.0
            .send(Box::new(move |cursive| f(&mut GuiWrapper::new(cursive))))
            .expect("gui is gone?");
    }
}

struct GuiWrapper<'a>(&'a mut Cursive);

impl<'a> GuiWrapper<'a> {
    fn new(cursive: &mut Cursive) -> GuiWrapper {
        GuiWrapper(cursive)
    }

    // Build up the Cursive UI. `tx` is a `futures::sync::mpsc::Sender` that we use to send
    // client input to the thread managing the tokio connection to the server.
    fn build_ui(&mut self, tx: mpsc::Sender<ClientMessage>) -> GuiEventSender {
        self.0.add_layer(LinearLayout::vertical()
            .child(ChatView::new(500)
                .with_id("chat")
                .full_height())
            .child(EditView::new()
                .on_submit(move |cursive, s| {
                    // This is called whenever the user presses "enter" after entering text.
                    GuiWrapper::new(cursive).handle_entry_input(s, tx.clone());
                })
                .with_id("input")
                .full_width()));
        for k in &[Key::Home, Key::End, Key::Up, Key::Down, Key::PageDown, Key::PageUp] {
            let e = Event::Key(*k);
            self.0.add_global_callback(e, move |s| {
                {
                    let chat = s.find_id::<ChatView>("chat").unwrap();
                    chat.on_event(e);
                }

                s.find_id::<EditView>("input").unwrap().take_focus(Direction::front());
            });
        }
        self.0.set_fps(10);
        GuiEventSender(self.0.cb_sink().clone())
    }

    fn append_content<S: Into<String>>(&mut self, s: S) {
        let chat = self.0.find_id::<ChatView>("chat").unwrap();
        chat.append_content(s, false);
    }

    fn handle_entry_input(&mut self, s: &str, tx: mpsc::Sender<ClientMessage>) {
        if s == "/quit" {
            self.0.quit();
            return;
        }

        // Here we `wait` on the send to complete. If this fails, it's because the receiving half
        // managed by the tokio thread is gone because we've lost our connection to the server, so
        // just give up and quit altogether.
        if let Err(_) = tx.send(ClientMessage::new(s)).wait() {
            self.0.quit();
        }

        self.clear_entry();
        self.scroll_to_bottom();
    }

    fn scroll_to_bottom(&mut self) {
        let chat = self.0.find_id::<ChatView>("chat").unwrap();
        chat.scroll_to_bottom();
    }

    fn clear_entry(&mut self) {
        let ev = self.0.find_id::<EditView>("input").unwrap();
        ev.set_content("");
    }
}

fn main() {
    let name = {
        let mut args = std::env::args();
        let usage = format!("usage: {} username", args.nth(0).unwrap());
        args.nth(0).unwrap_or_else(|| {
            println!("{}", usage);
            std::process::exit(1);
        })
    };
    let mut cursive = Cursive::new();

    cursive.set_theme({
        let mut t = Theme::default();
        t.shadow = false;
        t
    });

    // Build the futures channel for sending user input from the GUI thread to the tokio thread.
    let (tx, rx) = mpsc::channel(1);

    // Construct the GUI, and get back the std::sync::mpsc::Sender for sending server messages from
    // the tokio thread to the GUI thread.
    let gui_events = GuiWrapper::new(&mut cursive).build_ui(tx);

    // Start the tokio thread.
    thread::spawn(move || run_client(name, gui_events, rx));

    // Run the GUI.
    cursive.run();
}

fn run_client(name: String, gui: GuiEventSender, rx: mpsc::Receiver<ClientMessage>) {
    let addr = "127.0.0.1:12345".parse::<SocketAddr>().unwrap();

    // Create the event loop and initiate the connection to the remote server
    let mut core = Core::new().unwrap();
    let handle = core.handle();
    let tcp = TcpStream::connect(&addr, &handle);

    // Once we connect, send a `Handshake` with our name.
    let handshake = tcp.and_then(|stream| {
        let handshake_io = stream.framed(HandshakeCodec::new());

        // After sending the handshake, convert the framed stream back into its inner socket.
        handshake_io.send(Handshake::new(name)).map(|handshake_io| handshake_io.into_inner())
    });

    // Once we've sent our `Handshake`, start listening for messages from either the server (to
    // send to the GUI thread) or the GUI thread (to send to the server).
    let client = handshake.and_then(|socket| {
        let (to_server, from_server) = socket.framed(ClientToServerCodec::new()).split();

        // For each incoming message...
        let reader = from_server.for_each(move |msg| {
            // ... convert it to a string for display in the GUI...
            let content = match msg {
                ServerMessage::Message(from, msg) => format!("{}: {}", from, msg),
                ServerMessage::UserConnected(user) => format!("* {} connected", user),
                ServerMessage::UserDisconnected(user) => format!("* {} disconnected", user),
            };

            // ... and send that string _to_ the GUI.
            gui.send(move |g| g.append_content(content.clone()));

            Ok(())
        });

        // For each incoming message from the GUI thread, send it along to the server. This
        // code is identical to the `writer` future in tokio-chat-server, but the `rx` here is
        // being fed from the GUI thread instead of from other futures.
        let writer = rx
            .map_err(|()| unreachable!("rx can't fail"))
            .fold(to_server, |to_server, msg| {
                to_server.send(msg)
            })
            .map(|_| ());

        // Use select to allow either the reading or writing half dropping to drop the other
        // half. The `map` and `map_err` here effectively force this drop.
        reader.select(writer).map(|_| ()).map_err(|(err, _)| err)
    });

    core.run(client).unwrap();
}
