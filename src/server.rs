use getrandom;
use mio::{
    Events, Interest, Poll, Token,
    net::{TcpListener, TcpStream},
};
use std::collections::HashMap;
use std::fmt::Write as FmtWrite;
use std::io::{Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr};
use std::time::{Duration, SystemTime};
use std::{fmt, io};
use std::{fs, result};

type Result<T> = result::Result<T, ()>;

const PORT: u16 = 4444;
const SAFE_MODE: bool = true;
const BAN_LIMIT: Duration = Duration::from_secs(10 * 60);
const MESSAGE_RATE: Duration = Duration::from_secs(1);
const SLOWORIS_LIMIT: Duration = Duration::from_millis(200);
const STRIKE_LIMIT: usize = 10;

struct Sensitive<T>(T);

impl<T: fmt::Display> fmt::Display for Sensitive<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(inner) = self;
        if SAFE_MODE {
            writeln!(f, "[REDACTED]")
        } else {
            inner.fmt(f)
        }
    }
}

enum Sinner {
    Striked(usize),
    Banned(SystemTime),
}

impl Sinner {
    fn new() -> Self {
        Self::Striked(0)
    }

    fn forgive(&mut self) {
        *self = Self::Striked(0)
    }

    fn strike(&mut self) -> bool {
        match self {
            Self::Striked(x) => {
                if *x >= STRIKE_LIMIT {
                    *self = Self::Banned(SystemTime::now());
                    true
                } else {
                    *x += 1;
                    false
                }
            }
            Self::Banned(_) => true,
        }
    }
}

struct Server {
    clients: HashMap<Token, Client>,
    sinners: HashMap<IpAddr, Sinner>,
    token: String,
}

impl Server {
    fn from_token(token: String) -> Self {
        Self {
            clients: HashMap::new(),
            sinners: HashMap::new(),
            token,
        }
    }

    fn client_connected(&mut self, mut author: TcpStream, author_addr: SocketAddr, token: Token) {
        let now = SystemTime::now();

        if let Some(sinner) = self.sinners.get_mut(&author_addr.ip()) {
            match sinner {
                Sinner::Banned(banned_at) => {
                    let diff = now.duration_since(*banned_at).unwrap_or_else(|err| {
                        eprintln!("[ERROR]: ban time check on client connection: the clock might have gone backwards: {err}");
                        Duration::ZERO
                    });
                    if diff < BAN_LIMIT {
                        let secs = (BAN_LIMIT - diff).as_secs_f32();
                        println!(
                            "[INFO]: Client {author} tried to connect, but is banned for {secs} s",
                            author = Sensitive(author_addr),
                        );
                        let _ =
                            writeln!(author, "You are banned: {secs} secs left").map_err(|err| {
                                eprintln!(
                                    "[ERROR]: Could not send banned message to {author}: {err}",
                                    author = Sensitive(author_addr),
                                    err = Sensitive(err)
                                );
                            });
                        let _ = author.shutdown(Shutdown::Both).map_err(|err| {
                            eprintln!(
                                "[ERROR]: Could not shutdown socket for {author}: {err}",
                                author = Sensitive(author_addr),
                                err = Sensitive(err)
                            );
                        });

                        return;
                    } else {
                        sinner.forgive();
                    }
                }
                Sinner::Striked(_) => {}
            }
        }

        println!(
            "[INFO]: Client {author} connected",
            author = Sensitive(author_addr)
        );
        self.clients.insert(
            token,
            Client {
                conn: author,
                last_message: now - 2 * MESSAGE_RATE,
                connected_at: now,
                authed: false,
                addr: author_addr,
            },
        );
    }

    fn client_read(&mut self, token: Token) {
        if let Some(author) = self.clients.get_mut(&token) {
            let author_addr: SocketAddr = author.addr.clone();
            let mut buffer = [0; 64];
            let bytes: Vec<_> = match author.conn.read(&mut buffer) {
                Ok(0) => {
                    println!(
                        "[INFO]: Client {author} disconnected",
                        author = Sensitive(author_addr)
                    );
                    self.clients.remove(&token);
                    return;
                }
                Ok(n) => buffer[0..n].iter().cloned().filter(|x| *x >= 32).collect(),
                Err(err) => {
                    if err.kind() != io::ErrorKind::WouldBlock {
                        eprintln!(
                            "[ERROR]: Could not read message from {author}: {err}",
                            author = Sensitive(author_addr),
                            err = Sensitive(err)
                        );
                        self.clients.remove(&token);
                    }
                    return;
                }
            };

            let now = SystemTime::now();
            let diff = now.duration_since(author.last_message).unwrap_or_else(|err| {
                eprintln!("[ERROR]: message rate check on new message: the clock might have gone backwards: {err}");
                Duration::from_secs(0)
            });
            if diff < MESSAGE_RATE {
                self.strike_ip(author_addr.ip());
                return;
            }
            let text = if let Ok(text) = str::from_utf8(&bytes) {
                text
            } else {
                return;
            };
            self.sinners
                .entry(author_addr.ip())
                .or_insert(Sinner::new())
                .forgive();
            author.last_message = now;
            if author.authed {
                println!(
                    "[INFO]: Client {author} sent message {bytes:?}",
                    author = Sensitive(author_addr)
                );
                for (client_token, client) in self.clients.iter_mut() {
                    if *client_token != token && client.authed {
                        let _ = writeln!(client.conn, "{text}").map_err(|err| {
                            eprintln!("[ERROR]: could not broadcast message to all the clients from {author}: {err}", author = Sensitive(author_addr), err = Sensitive(err));
                        });
                    }
                }
            } else {
                if text != self.token {
                    println!("[INFO]: {} failed authorization", Sensitive(author_addr));
                    let _ = writeln!(author.conn, "Invalid tokwn").map_err(|err| {
                        eprintln!(
                            "[ERROR]: could not notify the client {} about invalid token: {}",
                            Sensitive(author_addr),
                            Sensitive(err)
                        );
                    });

                    let _ = author.conn.shutdown(Shutdown::Both).map_err(|err| {
                        eprintln!(
                            "[ERROR]: could not shutdown {}: {}",
                            Sensitive(author_addr),
                            Sensitive(err)
                        );
                    });
                    self.clients.remove(&token);
                    self.strike_ip(author_addr.ip());
                    return;
                }

                author.authed = true;
                println!("[INFO]: {} authorized", Sensitive(author_addr));
                let _ = writeln!(author.conn, "Welcome to the club").map_err(|err| {
                    eprintln!(
                        "[ERROR]: could not send the welcome message to {}: {}",
                        Sensitive(author_addr),
                        Sensitive(err)
                    );
                });
            }
        }
    }

    fn strike_ip(&mut self, ip: IpAddr) {
        let sinner = self.sinners.entry(ip).or_insert(Sinner::new());
        if sinner.strike() {
            println!("[INFO]: IP {ip} got banned", ip = Sensitive(ip));
            self.clients.retain(|_token, client| {
                let addr: SocketAddr = client.addr.clone();
                if addr.ip() == ip {
                    let _ = writeln!(client.conn, "You are banned").map_err(|err| {
                        eprintln!(
                            "[ERROR]: could not send banned message to {}: {}",
                            Sensitive(addr),
                            Sensitive(err)
                        );
                    });
                    let _ = client.conn.shutdown(Shutdown::Both).map_err(|err| {
                        eprintln!(
                            "[ERROR]: could not shutdown socket for {}: {}",
                            Sensitive(addr),
                            Sensitive(err)
                        );
                    });
                    return false;
                }
                true
            });
        }
    }

    fn update(&mut self, token: Token) {
        self.client_read(token);

        self.clients.retain(|_, client| {
            let addr = client.addr.clone();
            if !client.authed {
                let now = SystemTime::now();
                let diff = now.duration_since(client.connected_at).unwrap_or_else(|err| {
                    eprintln!("[ERROR]: slowloris time limit check: the clock might have gone backwards: {err}");
                    SLOWORIS_LIMIT
                });
                if diff >= SLOWORIS_LIMIT {
                    self.sinners.entry(addr.ip()).or_insert(Sinner::new()).strike();
                    let _ = client.conn.shutdown(Shutdown::Both).map_err(|err| {
                        eprintln!("[ERROR]: could not shutdown socket for {}: {}", Sensitive(addr), Sensitive(err));
                    });
                    return false;
                }
            }
            true
        });
    }
}

fn generate_token() -> Result<String> {
    let mut buffer = [0; 16];
    let _ = getrandom::fill(&mut buffer).map_err(|err| {
        eprintln!("[ERROR]: could not generate random access token: {err}");
    })?;

    let mut token = String::new();
    for x in buffer.iter() {
        let _ = write!(&mut token, "{x:02X}");
    }
    Ok(token)
}

struct Client {
    conn: TcpStream,
    last_message: SystemTime,
    connected_at: SystemTime,
    authed: bool,
    addr: SocketAddr,
}

fn main() -> Result<()> {
    let token = generate_token()?;
    let token_file_path = "./TOKEN";
    fs::write(token_file_path, token.as_bytes()).map_err(|err| {
        eprintln!("[ERROR]: could not create token file {token_file_path}: {err}");
    })?;

    println!("[INFO]: check {token_file_path} file for token");
    let address = format!("0.0.0.0:{PORT}");
    let mut listener = TcpListener::bind(address.parse().unwrap()).map_err(|err| {
        eprintln!(
            "[ERROR]: could not bind {}: {}",
            Sensitive(address.clone()),
            Sensitive(err)
        );
    })?;
    let mut poll = Poll::new().map_err(|err| {
        eprintln!("[ERROR]: could not create poll object: {err}");
    })?;
    let mut events = Events::with_capacity(1024);
    let mut counter = 0;

    poll.registry()
        .register(&mut listener, Token(counter), Interest::READABLE)
        .map_err(|err| {
            eprintln!("[ERROR]: could not register server soket in the poll object: {err}");
        })?;

    let mut server = Server::from_token(token);

    println!("[INFO]: listening to {}", Sensitive(address));
    loop {
        if let Err(err) = poll.poll(&mut events, None) {
            eprintln!("[ERROR]: Failed to poll: {err}");
            continue;
        }
        for token in events.iter().map(|e| e.token()) {
            match token {
                Token(0) => match listener.accept() {
                    Ok((mut stream, author_addr)) => {
                        counter += 1;
                        let token = Token(counter);
                        match poll
                            .registry()
                            .register(&mut stream, token, Interest::READABLE)
                        {
                            Ok(_) => server.client_connected(stream, author_addr, token),
                            Err(err) => eprintln!(
                                "[ERROR]: could not register client socket in the poll object: {err}"
                            ),
                        }
                    }
                    Err(err) => {
                        if err.kind() != io::ErrorKind::WouldBlock {
                            eprintln!("[ERROR]: could not accept connection: {err}");
                        }
                    }
                },
                token => server.update(token),
            }
        }
    }
}
