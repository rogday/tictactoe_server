use tokio::{
    net::{TcpListener, TcpStream},
    sync::{mpsc, RwLock},
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

//logging
use log::LevelFilter;
use log::{debug, error, info, trace, warn};

#[derive(Serialize, Deserialize, Debug)]
enum ClientMessage {
    Login { version: i64, login: String },
    Settings { quick_play: bool },
    Room { room_id: i64 },
    Ready,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
enum ServerMessage {
    //TODO: Server error in a string, really?
    Error(Option<String>),
    RoomInfo(SRoomInfo),
    Rooms(SRooms),
    Incoming(SClient),
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    X,
    O,
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
    Fresh,
    Logined,
    InLobby,
    InRoom,
    Playing,
}

async fn version_check(peer_id: Token, version: &i64, login: &str) -> ClientState {
    if *version < 42 {
        //TODO: make enum variant for this case and don't send plain string wtf
        let _ = SHARED_STATE.write().await.peers[&peer_id].send(ServerMessage::Error(Some(
            format!("{}, your client version is less than required.", login),
        )));

        //TODO: why would you keep the connection with this client?
        ClientState::Fresh
    } else {
        //discord-like logins will do
        SHARED_STATE.write().await.clients.get_mut(&peer_id).unwrap().login = login.to_string();
        ClientState::Logined
    }
}

async fn progress(peer_id: Token, event: &ClientMessage) {
    //TODO: server should send state back to client, so that it knows we're still up.
    let peer_state = SHARED_STATE.read().await.clients[&peer_id].state;

    //TODO: function; if two clients in a room are ready, should be checked inside (InRoom, Ready)
    let _playing_check = || {};

    //TODO: the fuck is this dispatch, make functions for every arm
    let new_state = match (&peer_state, &event) {
        (ClientState::Fresh, ClientMessage::Login { version, login }) => {
            version_check(peer_id, version, login).await
        }
        (ClientState::Logined, ClientMessage::Settings { quick_play }) => {
            let mut guard = SHARED_STATE.write().await;

            let (state, msg) = if *quick_play {
                let room_id = guard.find_room_to_play().unwrap_or_else(|| guard.create_room());
                guard.insert_in_room(room_id, peer_id);

                (ClientState::InRoom, ServerMessage::RoomInfo(guard.describe_room(room_id)))
            } else {
                (ClientState::InLobby, ServerMessage::Rooms(guard.describe_lobby()))
            };

            guard.send(peer_id, msg);

            state
        }

        (ClientState::InLobby, ClientMessage::Room { .. }) => {
            //check if this room_id exists, otherwise send CError
            //send SRoomInfo

            ClientState::InRoom
        }

        (ClientState::InRoom, ClientMessage::Ready) => todo!(),

        case => {
            error!("you're not handling case {:?}", case);
            return;
        }
    };

    SHARED_STATE.write().await.clients.get_mut(&peer_id).unwrap().state = new_state;
}

use once_cell::sync::Lazy;
static SHARED_STATE: Lazy<RwLock<Shared>> = Lazy::new(|| RwLock::new(Shared::new()));

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    //TODO: structopt for args handling
    let addr = env::args().nth(1).unwrap_or("127.0.0.1:666".to_string());

    env_logger::builder()
        .filter(Some("tictactoe_server"), LevelFilter::Trace)
        .write_style(env_logger::WriteStyle::Always)
        .init();

    let mut listener = TcpListener::bind(&addr).await?;

    info!("server running on {}", addr);

    loop {
        let (stream, addr) = listener.accept().await?;

        tokio::spawn(async move {
            if let Err(e) = process(stream, addr).await {
                warn!("Error processing {}: {:?}", addr, e);
            }
        });
    }
}

/// Shorthand for the transmit half of the message channel.
type Tx = mpsc::UnboundedSender<ServerMessage>;

/// Shorthand for the receive half of the message channel.
type Rx = mpsc::UnboundedReceiver<ServerMessage>;

type Token = usize;

//TODO: factory with fucking local counter?????
static PEER_COUNTER: AtomicUsize = AtomicUsize::new(1);
static ROOM_COUNTER: AtomicUsize = AtomicUsize::new(1);

struct Rooms(HashMap<Token, BTreeSet<Token>>);

struct Shared {
    rooms:   Rooms,
    peers:   HashMap<Token, Tx>,
    clients: HashMap<Token, Client>,
}

impl Shared {
    fn new() -> Self {
        Shared { rooms: Rooms(HashMap::new()), peers: HashMap::new(), clients: HashMap::new() }
    }

    async fn _broadcast(&mut self, room: &BTreeSet<Token>, sender: Token, message: ServerMessage) {
        for client_id in room.iter() {
            if *client_id != sender {
                //sending in Tx for Rx to recieve
                let _ = self.peers[client_id].send(message.clone());
            }
        }
    }

    fn find_room_to_play(&self) -> Option<Token> {
        let mut ret = None;

        for (room_id, clients) in self.rooms.0.iter() {
            //if there are no players with X/O sides
            if clients
                .iter()
                .map(|client| self.clients[client].side)
                .filter(|side| side != &Side::Spectator)
                .count()
                == 0
            {
                //found a room with no active players
                ret = Some(*room_id);
                // should empty rooms be automatically removed?
                if clients.len() > 0 {
                    break;
                }
            }
        }

        ret
    }

    fn create_room(&mut self) -> Token {
        let room_id = ROOM_COUNTER.fetch_add(1, Ordering::SeqCst);
        self.rooms.0.insert(room_id, BTreeSet::new());

        room_id
    }

    fn send(&mut self, peer_id: Token, msg: ServerMessage) {
        let _ = self.peers[&peer_id].send(msg);
    }

    //TODO: distinct types for room id and client id to ensure compile-time error just in case
    fn insert_in_room(&mut self, room_id: Token, peer_id: Token) {
        self.rooms.0.get_mut(&room_id).unwrap().insert(peer_id);
        self.clients.get_mut(&peer_id).unwrap().room = Some(room_id);
    }

    ///Add client internally
    fn insert_client(&mut self, peer_id: Token, tx: Tx) {
        self.peers.insert(peer_id, tx);
        self.clients.insert(
            peer_id,
            Client {
                id: peer_id,

                login: String::new(),
                side:  Side::Spectator,
                state: ClientState::Fresh,
                room:  None,
            },
        );
    }

    fn remove_client(&mut self, peer_id: Token) {
        if let Some(room_id) = self.clients.get_mut(&peer_id).unwrap().room {
            self.rooms.0.get_mut(&room_id).unwrap().remove(&peer_id);
        }

        self.clients.remove(&peer_id);
        self.peers.remove(&peer_id);
    }

    fn describe_lobby(&self) -> SRooms {
        SRooms { rooms: self.rooms.0.keys().map(|room_id| self.describe_room(*room_id)).collect() }
    }

    fn describe_room(&self, rid: Token) -> SRoomInfo {
        SRoomInfo {
            room_id: rid,
            players: self.rooms.0[&rid]
                .iter()
                .map(|id| SClient {
                    login: self.clients[id].login.clone(),
                    side:  self.clients[id].side,
                })
                .collect(),
        }
    }
}

#[derive(Debug)]
struct Client {
    id: Token,

    //Maybe SmallString?
    login: String,
    state: ClientState,
    side:  Side,
    room:  Option<Token>,
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
    id:    Token,
    lines: Framed<TcpStream, LinesCodec>,
    rx:    Rx,
}

impl Peer {
    async fn new(lines: Framed<TcpStream, LinesCodec>) -> io::Result<Peer> {
        let (tx, rx) = mpsc::unbounded_channel();

        let peer_id = PEER_COUNTER.fetch_add(1, Ordering::SeqCst);

        SHARED_STATE.write().await.insert_client(peer_id, tx);

        Ok(Peer { id: peer_id, lines, rx })
    }
}

#[derive(Debug)]
enum Message {
    FromClient(ClientMessage),
    ToClient(ServerMessage),
}

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
                    warn!("unparseable message from client: '{}'", message);
                    //TODO: who the fuck cares about this error?
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
    //I can send you \n in login string and you will fuck off. Also I think json supports pretty print?
    //Let's say our protocol forbids \n inside a login
    let lines = Framed::new(stream, LinesCodec::new());

    let mut peer = Peer::new(lines).await?;

    trace!("{} has joined the server", addr);

    // Process incoming messages until our stream is exhausted by a disconnect.
    while let Some(result) = peer.next().await {
        match result {
            // A message was received from the current user, we should broadcast this message to the other users.
            Ok(Message::FromClient(msg)) => {
                progress(peer.id, &msg).await;

                debug!(
                    "{} [{:?}] => {:?}",
                    addr,
                    msg,
                    SHARED_STATE.read().await.clients[&peer.id].state
                );
            }
            // A message was received from a peer. Send it to the current user.
            Ok(Message::ToClient(msg)) => {
                //dummy shit for ncat output to be readable
                peer.lines
                    .send("server: ".to_owned() + &serde_json::to_string(&msg).unwrap())
                    .await?;
            }
            Err(e) => {
                warn!("Error processing {}: {:?}", addr, e);
                break;
            }
        }
    }

    // If this section is reached it means that the client was disconnected!
    // Let's let everyone still connected know about it.
    {
        SHARED_STATE.write().await.remove_client(peer.id);
        trace!("{} has left the server", addr);
    }

    Ok(())
}
