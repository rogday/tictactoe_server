//#![warn(rust_2018_idioms)]

use tokio::{
    net::{TcpListener, TcpStream},
    sync::{mpsc, Mutex},
};

use tokio_util::codec::{Framed, LinesCodec, LinesCodecError};

use futures::{SinkExt, Stream, StreamExt};

use std::{
    collections::{HashMap, HashSet},
    env,
    error::Error,
    hash::{Hash, Hasher},
    io,
    net::SocketAddr,
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
    sync::Arc,
    task::{Context, Poll},
};

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
enum ClientMessage {
    Login { version: i64, login: String },
    Settings { quick_play: bool, side: Side },
    Room { room_id: i64 },
}

#[derive(Serialize, Deserialize, Debug)]
enum ServerMessage {
    Error(Option<String>),
    RoomInfo(SRoomInfo),
    Rooms(SRooms),
    Incoming(SClient),
}

#[derive(Serialize, Deserialize, Debug)]
enum Side {
    PreferX,
    PreferO,
    OnlyX,
    OnlyO,
    Random,
    Spectator,
}

#[derive(Serialize, Deserialize, Debug)]
struct SClient {
    login: String,
    side: Side,
}

// CSettings <- SRoomInfo | SRooms
#[derive(Serialize, Deserialize, Debug)]
struct SRoomInfo {
    room_id: u64,
    players: Vec<SClient>,
}

#[derive(Serialize, Deserialize, Debug)]
struct SRooms {
    rooms: Vec<SRoomInfo>,
}

#[derive(Debug)]
enum ClientState {
    Mute,
    Logined,
    InLobby,
    InRoom,
    Playing,
}

fn progress(state: &mut ClientState, event: &ClientMessage) {
    let playing_conditions = false;

    let playing_check = || {
        if playing_conditions {
            ClientState::Playing
        } else {
            ClientState::InRoom
        }
    };

    *state = match (&state, &event) {
        (ClientState::Mute, ClientMessage::Login { .. }) => {
            //send(CError(format!("{}", version.is_recent())))
            ClientState::Logined
        }
        (ClientState::Logined, ClientMessage::Settings { ref quick_play, .. }) => {
            if *quick_play {
                //for i in rooms.public
                // if playing_conditions{
                //  addtoRoom()
                //  send(SRoomInfo)
                //  return ClientState::Playing
                // }
                //rooms.public.push(Room::new())
                //  addtoRoom()
                // return ClientState::InRoom
                //--------------
                playing_check()
            } else {
                ClientState::InLobby
                //send SRooms
            }
        }

        (ClientState::InLobby, ClientMessage::Room { .. }) => {
            //check if this room_id exists, otherwise send CError
            //send SRoomInfo
            playing_check()
        }
        _ => return,
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Create the shared state. This is how all the peers communicate.
    //
    // The server task will hold a handle to this. For every new client, the
    // `state` handle is cloned and passed into the task that processes the
    // client connection.
    let state = Arc::new(Mutex::new(Shared::new()));

    let addr = env::args().nth(1).unwrap_or("127.0.0.1:6142".to_string());

    // Bind a TCP listener to the socket address.
    //
    // Note that this is the Tokio TcpListener, which is fully async.
    let mut listener = TcpListener::bind(&addr).await?;

    println!("server running on {}", addr);

    loop {
        // Asynchronously wait for an inbound TcpStream.
        let (stream, addr) = listener.accept().await?;

        // Clone a handle to the `Shared` state for the new connection.
        let state = Arc::clone(&state);

        // Spawn our handler to be run asynchronously.
        tokio::spawn(async move {
            if let Err(e) = process(state, stream, addr).await {
                println!("an error occured; error = {:?}", e);
            }
        });
    }
}

/// Shorthand for the transmit half of the message channel.
type Tx = mpsc::UnboundedSender<ServerMessage>;

/// Shorthand for the receive half of the message channel.
type Rx = mpsc::UnboundedReceiver<ServerMessage>;

type Token = usize;
const NULL_TOKEN: Token = 0;

static PEER_COUNTER: AtomicUsize = AtomicUsize::new(NULL_TOKEN + 1);
static ROOM_COUNTER: AtomicUsize = AtomicUsize::new(NULL_TOKEN + 1);

/// Data that is shared between all peers in the server.
///
/// This is the set of `Tx` handles for all connected clients. Whenever a
/// message is received from a client, it is broadcasted to all peers by
/// iterating over the `peers` entries and sending a copy of the message on each
/// `Tx`.
struct Shared {
    rooms: HashMap<Token, HashSet<Token>>,
    clients: HashMap<Token, Peer>,
}

/// The state for each connected client.
struct Peer {
    /// The TCP socket wrapped with the `Lines` codec, defined below.
    ///
    /// This handles sending and receiving data on the socket. When using
    /// `Lines`, we can work at the line level instead of having to manage the
    /// raw byte operations.
    lines: Framed<TcpStream, LinesCodec>,

    /// Receive half of the message channel.
    ///
    /// This is used to receive messages from peers. When a message is received
    /// off of this `Rx`, it will be written to the socket.
    tx: Tx,
    rx: Rx,

    /// Peer state
    state: ClientState,

    /// peer id
    id: Token,

    /// What room is client in
    room: Token,
}

impl PartialEq for Peer {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for Peer {}

impl Hash for Peer {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl Shared {
    /// Create a new, empty, instance of `Shared`.
    fn new() -> Self {
        Shared {
            rooms: HashMap::new(),
            clients: HashMap::new(),
        }
    }

    // Send a `LineCodec` encoded message to every peer, except
    // for the sender.
    // async fn broadcast(&mut self, sender: SocketAddr, message: String) {
    //     for peer in self.peers.iter_mut() {
    //         if *peer.0 != sender {
    //             let _ = peer.1.send(message.clone());
    //         }
    //     }
    // }
}

impl Peer {
    /// Create a new instance of `Peer`.
    async fn new(
        state: Arc<Mutex<Shared>>,
        lines: Framed<TcpStream, LinesCodec>,
    ) -> io::Result<Token> {
        // Get the client socket address
        let addr = lines.get_ref().peer_addr()?;

        // Create a channel for this peer
        let (tx, rx) = mpsc::unbounded_channel();

        // Add an entry for this `Peer` in the shared state map.
        //state.lock().await.peers.insert(addr, tx);

        let p_id = PEER_COUNTER.fetch_add(1, Ordering::SeqCst);

        state.lock().await.clients.insert(
            p_id,
            Peer {
                id: p_id,
                room: NULL_TOKEN,
                lines: lines,
                rx: rx,
                tx: tx,
                state: ClientState::Mute,
            },
        );

        Ok(p_id)
    }
}

#[derive(Debug)]
enum Message {
    /// A message that should be broadcasted to others.
    //Broadcast(ClientMessage),

    /// A message that should be received by a client
    FromClient(ClientMessage),
    ToClient(ServerMessage),
}

// Peer implements `Stream` in a way that polls both the `Rx`, and `Framed` types.
// A message is produced whenever an event is ready until the `Framed` stream returns `None`.
impl Stream for Peer {
    type Item = Result<Message, LinesCodecError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // First poll the `UnboundedReceiver`.

        if let Poll::Ready(Some(v)) = self.rx.poll_next_unpin(cx) {
            return Poll::Ready(Some(Ok(Message::ToClient(v))));
        }

        // Secondly poll the `Framed` stream.
        let result: Option<_> = futures::ready!(self.lines.poll_next_unpin(cx));

        // Poll::Ready(result.map(|msg| msg.map(Message::Broadcast)))

        Poll::Ready(match result {
            // We've received a message we should broadcast to others.
            Some(Ok(message)) => {
                if let Ok(event) = serde_json::from_str::<ClientMessage>(&message) {
                    Some(Ok(Message::FromClient(event)))
                } else {
                    Some(Err(LinesCodecError::Io(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "cant parse input",
                    ))))
                }
            }

            // An error occured.
            Some(Err(e)) => Some(Err(e)),

            // The stream has been exhausted.
            None => None,
        })
    }
}

/// Process an individual client
async fn process(
    state: Arc<Mutex<Shared>>,
    stream: TcpStream,
    addr: SocketAddr,
) -> Result<(), Box<dyn Error>> {
    let lines = Framed::new(stream, LinesCodec::new());

    // Register our peer with state which internally sets up some channels.
    let peer_id = Peer::new(state.clone(), lines).await?;
    let peer = { &state.lock().await.clients[&peer_id] };

    // A client has connected, let's let everyone know.
    {
        //let mut state = state.lock().await;
        let msg = format!("{} has joined the server", addr);
        println!("{}", msg);
        //state.broadcast(addr, msg).await;
    }

    // Process incoming messages until our stream is exhausted by a disconnect.
    while let Some(result) = peer.next().await {
        match result {
            // A message was received from the current user, we should
            // broadcast this message to the other users.
            Ok(Message::FromClient(msg)) => {
                //let mut state = state.lock().await;
                //let msg = format!("{}: {}", username, msg);
                progress(&mut peer.state, &msg);
                println!("{} [{:?}] => {:?}", addr, msg, peer.state);

                //println!("#broadcast: sending '{}' from {}", msg, addr);
                //state.broadcast(addr, msg).await;
            }
            // A message was received from a peer. Send it to the
            // current user.
            Ok(Message::ToClient(msg)) => {
                //println!("#recived: sending '{}' to socket of {}", msg, addr);
                peer.lines
                    .send(serde_json::to_string(&msg).unwrap())
                    .await?;
            }
            Err(e) => {
                println!(
                    "an error occured while processing messages for {}; error = {:?}",
                    addr, e
                );
            }
        }
    }

    // If this section is reached it means that the client was disconnected!
    // Let's let everyone still connected know about it.
    {
        let mut state = state.lock().await;
        state.clients.remove(&peer_id);

        let msg = format!("{} has left the server", addr);
        println!("{}", msg);
        //state.broadcast(addr, msg).await;
    }

    Ok(())
}
