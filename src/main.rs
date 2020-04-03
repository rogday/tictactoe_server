//TODO: what is this 2018 idioms?
//#![warn(rust_2018_idioms)]

use tokio::{
    net::{TcpListener, TcpStream},
    sync::{mpsc, Mutex},
};

use tokio_util::codec::{Framed, LinesCodec, LinesCodecError};

use futures::{SinkExt, Stream, StreamExt};

use std::{
    collections::{BTreeSet, HashMap},
    env,
    error::Error,
    //hash::{Hash, Hasher},
    io,
    net::SocketAddr,
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
    //sync::Arc,
    task::{Context, Poll},
};

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
enum ClientMessage {
    Login { version: i64, login: String },
    Settings { quick_play: bool, side: Side },
    Room { room_id: i64 },
    //TODO: Ready button as soon as there are two players in a room
}

#[derive(Serialize, Deserialize, Debug, Clone)]
enum ServerMessage {
    //TODO: Server error in a string, really?
    Error(Option<String>),
    RoomInfo(SRoomInfo),
    Rooms(SRooms),
    Incoming(SClient),
}

//TODO: delete this "OnlyWhatever" shit and refactor your spaghetti who the fuck do you think you are?
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    //PreferX,
    //PreferO,
    OnlyX,
    OnlyO,
    Random,
    Spectator,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct SClient {
    login: String,
    side:  Side,
}

// CSettings <- SRoomInfo | SRooms
#[derive(Serialize, Deserialize, Debug, Clone)]
struct SRoomInfo {
    room_id: usize,
    players: Vec<SClient>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct SRooms {
    rooms: Vec<SRoomInfo>,
}

#[derive(Debug, Copy, Clone)]
enum ClientState {
    //TODO: Fresh, not Mute
    Mute,
    Logined,
    InLobby,
    InRoom,
    Playing,
}

async fn progress(peer_id: Token, event: &ClientMessage) {
    //TODO: server should send state back to client, so that it knows we're still up.
    let peer_state = SHARED_STATE.lock().await.clients[&peer_id].state;

    let playing_conditions = false;

    //TODO: function
    let playing_check = || {
        if playing_conditions {
            ClientState::Playing
        } else {
            ClientState::InRoom
        }
    };

    //TODO: the fuck is this dispatch, make functions for every arm
    let new_state = match (&peer_state, &event) {
        (ClientState::Mute, ClientMessage::Login { version, login }) => {
            //send(CError(format!("{}", version.is_recent())))
            if *version < 42 {
                //TODO: make enum variant for this case and don't send plain string wtf
                let _ = SHARED_STATE.lock().await.peers[&peer_id].send(ServerMessage::Error(Some(
                    format!("{}, your client version is less than required.", login),
                )));

                //TODO: why would you keep the connection with this client?
                ClientState::Mute
            } else {
                //TODO: discord-like logins will do
                SHARED_STATE.lock().await.clients.get_mut(&peer_id).unwrap().login = login.clone();
                ClientState::Logined
            }
        }
        (ClientState::Logined, ClientMessage::Settings { quick_play, side }) => {
            let mut guard = SHARED_STATE.lock().await;
            guard.clients.get_mut(&peer_id).unwrap().side = *side;
            if *quick_play {
                let mut rid = NULL_TOKEN;
                //TODO: marked for removal, choose any non-playing room

                for (room_id, clients) in guard.rooms.public.iter() {
                    for client in clients {
                        let other = guard.clients[client].side;

                        if (other == Side::OnlyO && (*side == Side::Random || *side == Side::OnlyX))
                            || (*side == Side::OnlyO
                                && (other == Side::Random || other == Side::OnlyX))
                        {
                            rid = *room_id;
                            break;
                        }
                    }
                }

                if rid != NULL_TOKEN {
                    //found playable room
                    //TODO: adding player to a room should be wrapped in function
                    guard.rooms.public.get_mut(&rid).unwrap().insert(peer_id);
                    guard.clients.get_mut(&peer_id).unwrap().room = rid;
                    let _ = guard.peers[&peer_id].send(ServerMessage::RoomInfo(SRoomInfo {
                        room_id: rid,
                        players: guard.rooms.public[&rid]
                            .iter()
                            .map(|id| SClient {
                                login: guard.clients[id].login.clone(),
                                side:  guard.clients[id].side,
                            })
                            .collect(),
                    }));
                    ClientState::Playing
                } else {
                    //TODO: room creation FUNCTION
                    let room_id = ROOM_COUNTER.fetch_add(1, Ordering::SeqCst);
                    let mut clients = BTreeSet::new();

                    clients.insert(peer_id);
                    guard.clients.get_mut(&peer_id).unwrap().room = room_id;

                    guard.rooms.public.insert(room_id, clients);

                    //TODO: room describing function
                    let _ = guard.peers[&peer_id].send(ServerMessage::RoomInfo(SRoomInfo {
                        room_id: room_id,
                        players: guard.rooms.public[&room_id]
                            .iter()
                            .map(|id| SClient {
                                login: guard.clients[id].login.clone(),
                                side:  guard.clients[id].side,
                            })
                            .collect(),
                    }));
                    ClientState::InRoom
                }

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
            //playing_check()
            } else {
                //Slow play

                //TODO: lobby describing function
                let _ = guard.peers[&peer_id].send(ServerMessage::Rooms(SRooms {
                    rooms: guard
                        .rooms
                        .public
                        .iter()
                        .chain(guard.rooms.private.iter())
                        .map(|(room_id, clients)| SRoomInfo {
                            room_id: *room_id,
                            players: clients
                                .iter()
                                .map(|id| SClient {
                                    login: guard.clients[id].login.clone(),
                                    side:  guard.clients[id].side,
                                })
                                .collect(),
                        })
                        .collect(),
                }));

                ClientState::InLobby
                //send SRooms
            }
        }

        (ClientState::InLobby, ClientMessage::Room { .. }) => {
            //check if this room_id exists, otherwise send CError
            //send SRoomInfo
            //TODO: Not playing check. You are waiting for client to send Ready from InLobby state with another player
            playing_check()
        }

        //TODO: not println, not eprintln, not even fucking panic. Use logging crate with lots of shittiest colors ffs.
        case => panic!("you're not handling case {:?}", case),
    };

    SHARED_STATE.lock().await.clients.get_mut(&peer_id).unwrap().state = new_state;
}

//TODO: What the actual fuck? Are you using 2015 version of rust or am I missing something?
#[macro_use]
extern crate lazy_static;

// Create the shared state. This is how all the peers communicate.
//
// The server task will hold a handle to this. For every new client, the
// `state` handle is cloned and passed into the task that processes the
// client connection.

//TODO: once_cell
lazy_static! {
    static ref SHARED_STATE: Mutex<Shared> = Mutex::new(Shared::new());
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    //TODO: structopt for args handling
    let addr = env::args().nth(1).unwrap_or("127.0.0.1:6142".to_string());

    // Bind a TCP listener to the socket address.
    //
    // Note that this is the Tokio TcpListener, which is fully async.
    let mut listener = TcpListener::bind(&addr).await?;

    //TODO: logging
    println!("server running on {}", addr);

    loop {
        // Asynchronously wait for an inbound TcpStream.
        let (stream, addr) = listener.accept().await?;

        // Clone a handle to the `Shared` state for the new connection.
        //let state = Arc::clone(&SHARED_STATE);

        // Spawn our handler to be run asynchronously.
        tokio::spawn(async move {
            if let Err(e) = process(stream, addr).await {
                //TODO: logging
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
//TODO: the fuck is this shit? Option<Token>
const NULL_TOKEN: Token = 0;

//TODO: factory with fucking local counter?????
static PEER_COUNTER: AtomicUsize = AtomicUsize::new(NULL_TOKEN + 1);
static ROOM_COUNTER: AtomicUsize = AtomicUsize::new(NULL_TOKEN + 1);

/// Data that is shared between all peers in the server.
///
/// This is the set of `Tx` handles for all connected clients. Whenever a
/// message is received from a client, it is broadcasted to all peers by
/// iterating over the `peers` entries and sending a copy of the message on each
/// `Tx`.

//TODO: fuck private rooms.
struct Rooms {
    //TODO: not a fucking BTreeSet. should be a custom struct with methods, containing this fucking tree
    private: HashMap<Token, BTreeSet<Token>>,
    public:  HashMap<Token, BTreeSet<Token>>,
}

struct Shared {
    rooms:   Rooms,
    peers:   HashMap<Token, Tx>,
    clients: HashMap<Token, Client>,
}

#[derive(Debug)]
struct Client {
    id: Token,

    //TODO: Maybe SmallString?
    login: String,
    state: ClientState,
    side:  Side,
    room:  Token,
}

impl PartialEq for Client {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for Client {}

/// The state for each connected client.
//#[derive(PartialEq, Eq, Debug)]
struct Peer {
    id: Token,

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
    rx: Rx,
}

impl Shared {
    /// Create a new, empty, instance of `Shared`.
    fn new() -> Self {
        Shared {
            rooms:   Rooms { private: HashMap::new(), public: HashMap::new() },
            peers:   HashMap::new(),
            clients: HashMap::new(),
        }
    }

    // Send a `LineCodec` encoded message to every peer, except
    // for the sender.
    //TODO: broadcast should be inside a room struct.
    async fn _broadcast(&mut self, room: &BTreeSet<Token>, sender: Token, message: ServerMessage) {
        for client_id in room.iter() {
            if *client_id != sender {
                //sending in Tx for Rx to recieve
                let _ = self.peers[client_id].send(message.clone());
            }
        }
    }
}

impl Peer {
    /// Create a new instance of `Peer`.
    async fn new(lines: Framed<TcpStream, LinesCodec>) -> io::Result<Peer> {
        // Get the client socket address
        //let addr = lines.get_ref().peer_addr()?;

        // Create a channel for this peer
        let (tx, rx) = mpsc::unbounded_channel();

        // Add an entry for this `Peer` in the shared state map.
        //state.lock().await.peers.insert(addr, tx);

        let peer_id = PEER_COUNTER.fetch_add(1, Ordering::SeqCst);

        SHARED_STATE.lock().await.peers.insert(peer_id, tx);
        SHARED_STATE.lock().await.clients.insert(
            peer_id,
            Client {
                id: peer_id,

                login: String::new(),
                side:  Side::Random,
                state: ClientState::Mute,
                room:  NULL_TOKEN,
            },
        );

        Ok(Peer { id: peer_id, lines, rx })
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
                    //TODO: whoe the fuck cares about this error?
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
async fn process(stream: TcpStream, addr: SocketAddr) -> Result<(), Box<dyn Error>> {
    //TODO: I can send you \n in login string and you will fuck off. Also I think json supports pretty print?
    let lines = Framed::new(stream, LinesCodec::new());

    // Register our peer with state which internally sets up some channels.
    let mut peer = Peer::new(lines).await?;

    // A client has connected, let's let everyone know.
    {
        //let mut state = state.lock().await;
        //TODO: logging
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
                //let msg = format!("{}: {}", username, msg);
                //let peer_state = &mut shared_state.clients.get_mut(&peer.id).unwrap().state;
                progress(peer.id, &msg).await;

                //TODO: logging
                println!(
                    "{} [{:?}] => {:?}",
                    addr,
                    msg,
                    SHARED_STATE.lock().await.clients[&peer.id].state
                );

                //println!("#broadcast: sending '{}' from {}", msg, addr);
                //state.broadcast(addr, msg).await;
            }
            // A message was received from a peer. Send it to the
            // current user.
            Ok(Message::ToClient(msg)) => {
                //println!("#recived: sending '{}' to socket of {}", msg, addr);
                //TODO: dummy shit for ncat output to be readable
                peer.lines
                    .send("server: ".to_owned() + &serde_json::to_string(&msg).unwrap())
                    .await?;
            }
            Err(e) => {
                //TODO: logging
                println!(
                    "an error occured while processing messages for {}; error = {:?}",
                    addr, e
                );
                break;
            }
        }
    }

    // If this section is reached it means that the client was disconnected!
    // Let's let everyone still connected know about it.
    {
        let mut guard = SHARED_STATE.lock().await;

        //TODO: peer remover, room leaver etc
        guard.clients.remove(&peer.id);
        guard.peers.remove(&peer.id);

        //TODO: logging
        let msg = format!("{} has left the server", addr);
        println!("{}", msg);
        //state.broadcast(addr, msg).await;
    }

    Ok(())
}
