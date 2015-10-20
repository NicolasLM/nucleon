extern crate mio;

use std::io;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use mio::*;
use mio::buf::ByteBuf;
use mio::tcp::{TcpListener, TcpStream};
use mio::util::Slab;

use std::collections::VecDeque;
use std::ops::Drop;

use backend::{RoundRobinBackend, GetBackend};

const BUFFER_SIZE: usize = 8192;
const MAX_BUFFERS_PER_CONNECTION: usize = 16;
const MAX_CONNECTIONS: usize = 512;
const CONNECT_TIMEOUT_MS: usize = 1000;

pub struct Proxy {
    // socket where incoming connections arrive
    pub listen_sock: TcpListener,

    // token of the listening socket
    token: Token,
    
    // backend containing server to proxify
    backend: Arc<Mutex<RoundRobinBackend>>,

    // slab of Connections (front and back ends)
    connections: Slab<Connection>,

    // queue of tokens waiting to be read
    readable_tokens: VecDeque<Token>
}

impl Proxy {

    /// Create an instance of our proxy
    pub fn new(listen_addr: &str, backend: Arc<Mutex<RoundRobinBackend>>) -> Proxy {
        let listen_addr: SocketAddr = FromStr::from_str(&listen_addr)
            .ok().expect("Failed to parse listen host:port string");
        let listen_sock = TcpListener::bind(&listen_addr).unwrap();
        info!("Now listening on {}", &listen_addr);
        Proxy {
            listen_sock: listen_sock,
            token: Token(1),
            backend: backend,
            connections: Slab::new_starting_at(Token(2), MAX_CONNECTIONS),
            readable_tokens: VecDeque::with_capacity(MAX_CONNECTIONS)
        }
    }

    /// Put a token in the list of readable tokens
    ///
    /// Once token is in the list, it is no longer interested in readable events
    /// from the event_loop.
    fn push_to_readable_tokens(&mut self, event_loop: &mut EventLoop<Proxy>, token: Token) {
        self.readable_tokens.push_back(token);
        self.connections[token].interest.remove(EventSet::readable());
        self.connections[token].reregister(event_loop).unwrap();
    }

    /// Read as much as it can of a token
    ///
    /// Stops when we read everything the kernel had for us or the other end send
    /// queue is full.
    fn read_token(&mut self, event_loop: &mut EventLoop<Proxy>,
                         token: Token) -> io::Result<bool> {
        let other_end_token = self.connections[token].end_token.unwrap();
        let buffers_to_read =
            MAX_BUFFERS_PER_CONNECTION - self.connections[other_end_token].send_queue.len();
        let (exhausted_kernel, messages) = try!(
            self.find_connection_by_token(token).read(buffers_to_read)
        );
        // let's not tell we have something to write if we don't
        if messages.is_empty() {
            return Ok(exhausted_kernel);
        }

        self.connections[other_end_token].send_messages(messages)
            .and_then(|_| {
                self.connections[other_end_token].interest.insert(EventSet::writable());
                self.connections[other_end_token].reregister(event_loop)
            })
            .unwrap_or_else(|e| {
                error!("Failed to queue message for {:?}: {:?}", other_end_token, e);
            });
        self.connections[token].reregister(event_loop).unwrap();
        Ok(exhausted_kernel)
    }

    /// Try to flush the list of readable tokens
    ///
    /// Loops on all readable tokens and remove them from the list if we flushed
    /// completely the kernel buffer.
    fn flush_readable_tokens(&mut self, event_loop: &mut EventLoop<Proxy>) {
        for _ in 0..self.readable_tokens.len() {
            match self.readable_tokens.pop_front() {
                Some(token) => {
                    match self.read_token(event_loop, token) {
                        Ok(exhausted_kernel) => {
                            if exhausted_kernel {
                                self.connections[token].interest.insert(EventSet::readable());
                                self.connections[token].reregister(event_loop).unwrap();
                            } else {
                                self.readable_tokens.push_back(token);
                            }
                        },
                        Err(e) => panic!("Error while reading {:?}: {}", token, e)
                    }
                },
                None => break
            }
        }
    }

    /// Handle a write event from the event loop
    ///
    /// We assume that getting a write event means the TCP connection
    /// phase is finished.
    fn handle_write_event(&mut self, event_loop: &mut EventLoop<Proxy>, token: Token) {
        if !self.connections[token].connected {
            self.connections[token].connected = true; 
            match self.connections[token].sock.peer_addr() {
                Ok(addr) => info!("Connected to backend server {}", addr),
                Err(_) => warn!("Connected to unknonw backend server, this is odd")
            }
        }
        self.flush_send_queue(event_loop, token);
    }

    /// Flush the send queue of a token
    ///
    /// Drop the connection if we sent everything and other end is gone
    fn flush_send_queue(&mut self, event_loop: &mut EventLoop<Proxy>, token: Token) {
        match self.connections[token].write() {
            Ok(flushed_everything) => {
                if flushed_everything {
                    self.connections[token].interest.remove(EventSet::writable());
                    self.connections[token].reregister(event_loop).unwrap();
                }
            },
            Err(_) => {
                error!("Could not write on {:?}, dropping send queue", token);
                self.connections[token].send_queue.clear();
            }
        }

        // Terminate connection if other end is gone and send queue if flushed
        if self.connections[token].send_queue.is_empty() &&
           self.connections[token].end_token.is_none() {
           self.terminate_connection(event_loop, token);
        }
    }

    /// Accept all pending connections
    fn accept(&mut self, event_loop: &mut EventLoop<Proxy>) {
        loop {
            if !self.accept_one(event_loop) {
                break;
            }
        }
    }

    /// Accept a single pending connections
    ///
    /// Once connection is accepted, it creates a new connection to the backend
    /// and links both connections together.
    ///
    /// Returns true when a connection is accepted successfully,
    /// false when no more connection to accept or error happened.
    fn accept_one(&mut self, event_loop: &mut EventLoop<Proxy>) -> bool {
        let client_sock = match self.listen_sock.accept() {
            Ok(s) => {
                match s {
                    Some(sock) => sock,
                    None => {
                        debug!("No more socket to accept on this event");
                        return false;
                    }
                }
            },
            Err(e) => {
                error!("Failed to accept new socket, {:?}", e);
                return false;
            }
        };
        match client_sock.peer_addr() {
            Ok(client_addr) => info!("New client connection from {}", client_addr),
            Err(_) => info!("New client connection from unknown source")
        }
        let backend_sock = match self.connect_to_backend_server() {
            Ok(backend_sock) => {
                backend_sock
            },
            Err(e) => {
                error!("Could not connect to backend: {}", e);
                return false;
            }
        };
        let client_token = self.create_connection_from_sock(client_sock, true, event_loop);
        let backend_token = self.create_connection_from_sock(backend_sock, false, event_loop);
        match client_token {
            Some(client_token) => {
                match backend_token {
                    Some(backend_token) => {
                        self.link_connections_together(client_token, backend_token,
                                                       event_loop);
                        // Register a timeout on the backend server socket
                        let timeout = event_loop.timeout_ms(
                            backend_token.as_usize(),
                            CONNECT_TIMEOUT_MS as u64
                        ).unwrap();
                        self.connections[backend_token].timeout = Some(timeout);
                    },
                    None => {
                        error!("Cannot create backend Connection, dropping client");
                        self.connections.remove(client_token);
                        return false;
                    }
                }
            },
            None => {
                match backend_token {
                    Some(backend_token) => {
                        error!("Cannot create client Connection, dropping backend");
                        self.connections.remove(backend_token);
                        return false;
                    },
                    None => {
                        error!("Cannot create client nor backend Connection");
                        return false;
                    }
                }
            }
        };

        true

    }

    /// Create a new TCP socket to a backend server
    fn connect_to_backend_server(&mut self) -> io::Result<TcpStream> {
        let backend_socket_addr = self.backend.lock().unwrap().get().unwrap();
        TcpStream::connect(&backend_socket_addr)
    }

    /// Create a Connection instance from a socket
    fn create_connection_from_sock(&mut self, sock: TcpStream, already_connected: bool,
                                   event_loop: &mut EventLoop<Proxy>) -> Option<Token> {
        self.connections.insert_with(|token| {
            info!("Creating Connection with {:?}", token);
            let mut connection = Connection::new(sock, token, already_connected);
            connection.register(event_loop).unwrap();
            connection
        })
    }

    /// Link two Connection together
    ///
    /// This makes it easy to know to which Connection send the data we
    /// receive on another Connection.
    /// A Connection is not interested in read events before being linked correctly
    fn link_connections_together(&mut self, client_token: Token, backend_token: Token,
                                 event_loop: &mut EventLoop<Proxy>) {
        self.connections[client_token].end_token = Some(backend_token);
        self.connections[backend_token].end_token = Some(client_token);
        // Now that we have two Connections linked with each other
        // we can register to Read events.
        self.connections[client_token].interest.insert(EventSet::readable());
        self.connections[backend_token].interest.insert(EventSet::readable());
        self.connections[backend_token].interest.insert(EventSet::writable());
        self.connections[client_token].reregister(event_loop).unwrap();
        self.connections[backend_token].reregister(event_loop).unwrap();
    }

    /// Terminate a connection as well as its other end
    ///
    /// Makes sure to flush all pending queues before dropping the other end
    /// of a connection.
    fn terminate_connection(&mut self, event_loop: &mut EventLoop<Proxy>, token: Token) {
        match self.connections[token].end_token {
            Some(end_token) => {
                if self.connections[end_token].send_queue.is_empty() {
                    // Nothing to write on the other end, we can drop it
                    self.connections[end_token].deregister(event_loop).unwrap();
                    self.connections.remove(end_token);
                } else {
                    // We still need to write things in the other end
                    // just stop reading it and we will terminate it 
                    // when we flushed its send_queue
                    // Todo: Is there a way to schedule a timeout?
                    self.connections[end_token].end_token = None;
                    self.connections[end_token].interest.remove(EventSet::readable());
                    self.connections[end_token].interest.insert(EventSet::writable());
                    self.connections[end_token].reregister(event_loop).unwrap();
                }
            },
            None => {}
        }
        self.connections[token].deregister(event_loop).unwrap();
        self.connections.remove(token);
    }

    /// Find a connection in the slab using the given token.
    fn find_connection_by_token<'a>(&'a mut self, token: Token) -> &'a mut Connection {
        &mut self.connections[token]
    }

    /// Try to connect to the next backend server
    ///
    /// Resets the timeout to prevent multiple timeouts to be set concurrently.
    fn try_next_server(&mut self, event_loop: &mut EventLoop<Proxy>, token: Token) {
        match self.connections[token].timeout {
            Some(timeout) => {
                event_loop.clear_timeout(timeout);
                self.connections[token].timeout = None;
            },
            None => {}
        };
        self.connections[token].deregister(event_loop).unwrap();
        self.connections[token].sock = match self.connect_to_backend_server() {
            Ok(backend_sock) => {
                backend_sock
            },
            Err(e) => {
                // Todo: drop connections
                error!("Could not connect to backend: {}", e);
                return;
            }
        };
        self.connections[token].register(event_loop).unwrap();
        // Register another timeout on the backend server socket
        // in case the new backend server is also unavailable
        let timeout = event_loop.timeout_ms(token.as_usize(),
                                            CONNECT_TIMEOUT_MS as u64).unwrap();
        self.connections[token].timeout = Some(timeout);
    }
}

impl Handler for Proxy {
    type Timeout = usize;
    type Message = ();

    /// Method called when a timeout is fired
    ///
    /// Only used for backend server connection timeout
    fn timeout(&mut self, event_loop: &mut EventLoop<Proxy>, timeout: usize) {
        let token = Token(timeout);
        if self.connections.contains(token) && !self.connections[token].connected {
            warn!("Connect to backend server timeout");
            self.try_next_server(event_loop, token);
        }
    }

    /// Method called when a event from the event loop is notified 
    fn ready(&mut self, event_loop: &mut EventLoop<Proxy>, token: Token, events: EventSet) {
        debug!("events [{:?}] on {:?}", events, token);
        
        // we are only interested in read events from the listening token
        // so we can safely assume this is a read event
        if token == self.token {
            self.accept(event_loop);
            info!("Accepted connection(s), now {} Connections",
                  self.connections.count());
            return;
        }
        
        if events.is_error() {
            debug!("Got an error on {:?}", token);
            if !self.connections[token].connected {
                // Got an error while connecting to a backend server?
                // Let's try the next one.
                warn!("Connect to backend server failed");
                self.try_next_server(event_loop, token);
            } else {
                self.terminate_connection(event_loop, token);
            }
            return;
        }

        if events.is_writable() {
            debug!("Got a write event on {:?}", token);
            self.handle_write_event(event_loop, token);
        }
        
        if events.is_hup() && events.is_readable() {
            debug!("Got a read hang up on {:?}", token);
            // bypass the readable tokens queue, let's read until kernel
            // is exhausted and drop the connection
            self.read_token(event_loop, token).unwrap();
            self.terminate_connection(event_loop, token);
        } else if events.is_readable() {
            debug!("Got a read event on {:?}", token);
            self.push_to_readable_tokens(event_loop, token);
        } else if events.is_hup() {
            debug!("Got a hup event on {:?}", token);
        }

        self.flush_readable_tokens(event_loop);

        debug!("Finished loop with {} Connections and {} readable tokens",
               self.connections.count(), self.readable_tokens.len());

    }
}

struct Connection {
    // handle to the accepted socket
    sock: TcpStream,

    // token used to register with the event loop
    token: Token,

    // set of events we are interested in
    interest: EventSet,

    // messages waiting to be sent out to sock
    send_queue: VecDeque<ByteBuf>,
   
    // other end of the tunnel
    end_token: Option<Token>,

    // is the socket connected already or waiting for answer?
    connected: bool,

    // store timeout when connecting to a server backend
    timeout: Option<mio::Timeout>
}

impl Drop for Connection {
    fn drop(&mut self) {
        info!("Dropping Connection with {:?}", self.token);
    }
}

impl Connection {
    fn new(sock: TcpStream, token: Token, connected: bool) -> Connection {
        Connection {
            sock: sock,
            token: token,

            // new connections are only listening for a hang up event when
            // they are first created. We always want to make sure we are 
            // listening for the hang up event. We will additionally listen
            // for readable and writable events later on.
            interest: EventSet::hup() | EventSet::error(),

            send_queue: VecDeque::with_capacity(MAX_BUFFERS_PER_CONNECTION),

            // When instanciated a Connection does not have yet an other end
            end_token: None,

            connected: connected,

            timeout: None
        }
    }

    /// Read buffers from the kernel
    ///
    /// Reads at most 'nb_buffers' buffers. 
    ///
    /// Returns a tuple of (bool, Vec) wrapped in a Result.
    /// True if kernel has no more data to give, false otherwise.
    /// Vec contains the actual data read by chunks of buffers.
    fn read(&mut self, nb_buffers: usize) -> io::Result<(bool, Vec<ByteBuf>)> {

        let mut recv_vec: Vec<ByteBuf> = Vec::with_capacity(nb_buffers);
        let mut exhausted_kernel: bool = false;

        for _ in 0..nb_buffers {
            let mut recv_buf = ByteBuf::mut_with_capacity(BUFFER_SIZE);
            match self.sock.try_read_buf(&mut recv_buf) {
                // the socket receive buffer is empty, so let's move on
                // try_read_buf internally handles WouldBlock here too
                Ok(None) => {
                    debug!("CONN : we read 0 bytes, exhausted kernel");
                    exhausted_kernel = true;
                    break;
                },
                Ok(Some(n)) => {
                    debug!("CONN : we read {} bytes", n);
                    if n == 0 {
                        // Reading on a closed socket never gives Ok(None)...
                        // Todo: check why
                        exhausted_kernel = true;
                        break;
                    }
                    if n > 0 {
                        // flip changes our type from MutByteBuf to ByteBuf
                        recv_vec.push(recv_buf.flip());
                    }
                },
                Err(e) => {
                    error!("Failed to read buffer for {:?}, error: {}", self.token, e);
                    return Err(e);
                }
            }
        }

        Ok((exhausted_kernel, recv_vec))
    }

    /// Try to flush all send queue
    ///
    /// Returns true when everything is flushed, false otherwise
    fn write(&mut self) -> io::Result<bool> {
        while !self.send_queue.is_empty() {
            let wrote_everything = try!(self.write_one_buf());
            if !wrote_everything {
                // Kernel did not accept all our data, let's keep
                // interest on write events so we get notified when
                // kernel is ready to accept our data in the future.
                return Ok(false);
            }
        }

        Ok(true)
    }

    /// Write one buffer to the socket
    ///
    /// Returns true if the totality of the buffer was sent,
    /// false if the kernel did not accept everything.
    fn write_one_buf(&mut self) -> io::Result<bool> {

        self.send_queue.pop_front()
            .ok_or(io::Error::new(io::ErrorKind::Other, "Could not pop send queue"))
            .and_then(|mut buf| {
                match self.sock.try_write_buf(&mut buf) {
                    Ok(None) => {
                        debug!("client flushing buf; WouldBlock");

                        // put message back into the queue so we can try again
                        self.send_queue.push_front(buf);
                        Ok(false)
                    },
                    Ok(Some(n)) => {
                        debug!("CONN : we wrote {} bytes", n);
                        if buf.has_remaining() {
                            self.send_queue.push_front(buf);
                            Ok(false)
                        } else {
                            Ok(true)
                        }
                    },
                    Err(e) => {
                        error!("Failed to send buffer for {:?}, error: {}", self.token, e);
                        Err(e)
                    }
                }
            })

    }

    /// Queue an outgoing message to the client
    ///
    /// This will cause the connection to register interests in write
    /// events with the event loop.
    fn send_messages(&mut self, messages: Vec<ByteBuf>) -> io::Result<()> {
        // Todo: use Vec.append() but not in Rust stable
        self.send_queue.extend(messages.into_iter());
        self.interest.insert(EventSet::writable());
        Ok(())
    }

    /// Register interest in read events with the event_loop
    ///
    /// This will let the event loop get notified on events happening on
    /// this connection.
    fn register(&mut self, event_loop: &mut EventLoop<Proxy>) -> io::Result<()> {
        event_loop.register_opt(
            &self.sock,
            self.token,
            self.interest, 
            PollOpt::edge() | PollOpt::oneshot()
        ).or_else(|e| {
            error!("Failed to register {:?}, {:?}", self.token, e);
            Err(e)
        })
    }

    /// Re-register interest in events with the event_loop
    fn reregister(&mut self, event_loop: &mut EventLoop<Proxy>) -> io::Result<()> {
        event_loop.reregister(
            &self.sock,
            self.token,
            self.interest,
            PollOpt::edge() | PollOpt::oneshot()
        ).or_else(|e| {
            error!("Failed to reregister {:?}, {:?}", self.token, e);
            Err(e)
        })
    }

    /// De-register every interest in events for this connection
    fn deregister(&mut self, event_loop: &mut EventLoop<Proxy>) -> io::Result<()> {
        event_loop.deregister(&self.sock).or_else(|e| {
            error!("Failed to deregister {:?}, {:?}", self.token, e);
            Err(e)
        })
    }
}
