use getrandom;
use std::collections::HashMap;
use std::fmt;
use std::fmt::Write as FmtWrite;
use std::io::{Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::result;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::{Duration, SystemTime};

type Result<T> = result::Result<T, ()>;

const SAFE_MODE: bool = true;
const BAN_LIMIT: Duration = Duration::from_secs(10 * 60);
const MESSAGE_RATE: Duration = Duration::from_secs(1);
const STRIKE_LIMIT: u16 = 10;

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

enum Message {
    ClientConnected {
        author: Arc<TcpStream>,
    },
    ClientDisconected {
        author_addr: SocketAddr,
    },
    NewMessage {
        author_addr: SocketAddr,
        bytes: Vec<u8>,
    },
}

struct Client {
    conn: Arc<TcpStream>,
    last_message: SystemTime,
    strike_count: u16,
    authed: bool,
}

fn server(messages: Receiver<Message>) -> Result<()> {
    let mut clients = HashMap::<SocketAddr, Client>::new();
    let mut banned_mfs = HashMap::<IpAddr, SystemTime>::new();
    loop {
        let msg = messages.recv().expect("The server reciver is not hunng up");
        match msg {
            Message::ClientConnected { author } => {
                let author_addr = author.peer_addr().expect("TODO: cache the peer_addr");
                let mut banned_at = banned_mfs.get(&author_addr.ip());
                let now = SystemTime::now();

                banned_at = banned_at.and_then(|banned_at| {
                    let diff = now
                        .duration_since(*banned_at)
                        .expect("TODO: don't crash if the clock went backwards");

                    if diff >= BAN_LIMIT {
                        None
                    } else {
                        Some(banned_at)
                    }
                });

                if let Some(banned_at) = banned_at {
                    let diff = now
                        .duration_since(*banned_at)
                        .expect("TODO: don't crash if the clock went backwards");

                    banned_mfs.insert(author_addr.ip().clone(), *banned_at);

                    let mut author = author.as_ref();
                    let secs = (BAN_LIMIT - diff).as_secs_f32();
                    println!(
                        "[INFO]: Client {addr} try to connect but is banned, for {secs} secs",
                        addr = Sensitive(author_addr)
                    );
                    let _ = writeln!(author, "You are banned: {secs} secs left",);
                    let _ = author.shutdown(Shutdown::Both);
                    clients.remove(&author_addr);
                } else {
                    println!(
                        "[INFO]: Client {addr} connected",
                        addr = Sensitive(author_addr)
                    );
                    clients.insert(
                        author_addr.clone(),
                        Client {
                            conn: author.clone(),
                            last_message: now,
                            strike_count: 0,
                            authed: false,
                        },
                    );

                    let _ = writeln!(author.as_ref(), "Token: ").map_err(|err| {
                        eprintln!(
                            "[ERROR]: Could not send Token prompt to {}: {}",
                            Sensitive(author_addr),
                            Sensitive(err)
                        );
                    });
                }
            }
            Message::ClientDisconected { author_addr } => {
                println!(
                    "[INFO]: Client {addr} disconnected",
                    addr = Sensitive(author_addr)
                );
                clients.remove(&author_addr);
            }
            Message::NewMessage { author_addr, bytes } => {
                if let Some(author) = clients.get_mut(&author_addr) {
                    let now = SystemTime::now();

                    let diff = now
                        .duration_since(author.last_message)
                        .expect("TODO: don't crash if the clock went backwards");

                    if diff >= MESSAGE_RATE {
                        if str::from_utf8(&bytes).is_ok() {
                            println!(
                                "[INFO]: Client {addr} sent message: {bytes:?}",
                                addr = Sensitive(author_addr),
                            );
                            if author.authed {
                                for (addr, client) in clients.iter() {
                                    if *addr != author_addr && client.authed {
                                        let _ = client.conn.as_ref().write(&bytes).map_err(|err| {
                                            eprintln!(
                                                "[ERROR]: Could not broadcast message to all the clients from {addr}: {err}",
                                                addr = Sensitive(author_addr),
                                                err = Sensitive(err)
                                            )
                                        });
                                    }
                                }
                            } else {
                            }
                        } else {
                            author.strike_count += 1;

                            if author.strike_count >= STRIKE_LIMIT {
                                println!(
                                    "[INFO]: Client {addr} got banned",
                                    addr = Sensitive(author_addr),
                                );
                                banned_mfs.insert(author_addr.ip().clone(), now);
                                let _ = writeln!(author.conn.as_ref(), "You are banned").map_err(|err| {
                                    eprintln!(
                                        "[ERROR]: Could not send banned message to {addr}: {err}",
                                        addr = Sensitive(author_addr),
                                        err = Sensitive(err)
                                    )
                                });
                                let _ = author.conn.shutdown(Shutdown::Both).map_err(|err| {
                                    eprintln!(
                                        "[ERROR]: Could not shutdown socket for {addr}: {err}",
                                        addr = Sensitive(author_addr),
                                        err = Sensitive(err)
                                    )
                                });
                            }
                        }
                    } else {
                        author.strike_count += 1;

                        if author.strike_count >= STRIKE_LIMIT {
                            banned_mfs.insert(author_addr.ip().clone(), now);
                            println!(
                                "[INFO]: Client {addr} got banned",
                                addr = Sensitive(author_addr),
                            );
                            let _ =
                                writeln!(author.conn.as_ref(), "You are banned").map_err(|err| {
                                    eprintln!(
                                        "[ERROR]: Could not send banned message to {addr}: {err}",
                                        addr = Sensitive(author_addr),
                                        err = Sensitive(err)
                                    )
                                });
                            let _ = author.conn.shutdown(Shutdown::Both).map_err(|err| {
                                eprintln!(
                                    "[ERROR]: Could not shutdown socket for {addr}: {err}",
                                    addr = Sensitive(author_addr),
                                    err = Sensitive(err)
                                )
                            });
                        }
                    }
                }
            }
        }
    }
}

fn authorize(stream: &Arc<TcpStream>, addr: &SocketAddr, token: &str) -> Result<()> {
    let mut buf: [u8; 32] = [0; 32];
    let n = stream.as_ref().read(&mut buf).map_err(|err| {
        eprintln!(
            "[ERROR]: Clould not read authorization token from {}: {}",
            Sensitive(addr),
            Sensitive(err)
        );
    })?;

    if n < buf.len() {
        eprintln!("[ERROR: Didn't fully read the auth token: only {n} bytes");

        return Err(());
    }

    let user_token = str::from_utf8(&buf[0..n]).map_err(|err| {
        eprintln!("[ERROR]: token is not a valid UTF8: {err}");
    })?;

    if user_token != token {
        eprintln!("[ERROR]: user provide invalid token");
        return Err(());
    }

    Ok(())
}

fn client(stream: Arc<TcpStream>, messages: Sender<Message>, token: String) -> Result<()> {
    let author_addr = stream.peer_addr().map_err(|err| {
        eprintln!("[ERROR]: Could not get peer_addr: {}", Sensitive(err));
    })?;

    // let _ = writeln!(stream.as_ref(), "Token: ").map_err(|err| {
    //     eprintln!(
    //         "[ERROR]: Could not send Token prompt to {}: {}",
    //         Sensitive(author_addr),
    //         Sensitive(err)
    //     );
    // });
    //
    // authorize(&stream, &author_addr, &token).map_err(|()| {
    //     let _ = writeln!(stream.as_ref(), "Invalid Token").map_err(|err| {
    //         eprintln!(
    //             "[ERROR]: Could not notify the client {} about invalid token: {}",
    //             Sensitive(author_addr),
    //             Sensitive(err)
    //         );
    //     });
    //     eprintln!("[ERROR]: failed to authorized");
    //     let _ = stream.shutdown(Shutdown::Both).map_err(|err| {
    //         eprintln!(
    //             "[ERROR]: Could not shutdown {}: {}",
    //             Sensitive(author_addr),
    //             Sensitive(err)
    //         );
    //     });
    // })?;
    // println!("[INFO]: {} authorized", Sensitive(author_addr));
    // let _ = writeln!(stream.as_ref(), "Welcome to the club").map_err(|err| {
    //     eprintln!(
    //         "[ERROR]: Could not send the welcome message to {}: {}",
    //         Sensitive(author_addr),
    //         Sensitive(err)
    //     );
    // })?;

    messages
        .send(Message::ClientConnected {
            author: stream.clone(),
        })
        .map_err(|err| {
            eprintln!(
                "[ERROR]: Clould not send message to the server thread: {}",
                Sensitive(err)
            );
        })?;
    let mut buffer = Vec::new();
    buffer.resize(64, 0);
    loop {
        let n = stream.as_ref().read(&mut buffer).map_err(|err| {
            eprintln!(
                "[ERROR]: Could not read from the client: {}",
                Sensitive(err)
            );
            let _ = messages
                .send(Message::ClientDisconected { author_addr })
                .map_err(|err| {
                    eprintln!(
                        "[ERROR]: Clould not send message to the server thread: {}",
                        Sensitive(err)
                    );
                });
        })?;
        if n > 0 {
            let mut bytes = Vec::new();
            for x in &buffer[0..n] {
                if *x >= 32 {
                    bytes.push(*x);
                }
            }
            let _ = messages
                .send(Message::NewMessage { bytes, author_addr })
                .map_err(|err| {
                    eprintln!(
                        "[ERROR]: Could not send the message to the server thread: {}",
                        Sensitive(err)
                    );
                })?;
        } else {
            let _ = messages
                .send(Message::ClientDisconected { author_addr })
                .map_err(|err| {
                    eprintln!(
                        "[ERROR]: Clould not send message to the server thread: {}",
                        Sensitive(err)
                    );
                });
            break;
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let mut buffer = [0; 16];
    let _ = getrandom::fill(&mut buffer).map_err(|err| {
        eprintln!(
            "[ERROR]: Could not generate random random access buffer: {}",
            Sensitive(err)
        );
    });

    let mut token = String::new();
    for x in buffer {
        let _ = write!(&mut token, "{x:02X}");
    }

    println!("[TOKEN]: {token}");

    let address = "0.0.0.0:4444";
    let listener = TcpListener::bind(address).map_err(|err| {
        eprintln!(
            "[ERROR]: Could not bind {}: {}",
            Sensitive(address),
            Sensitive(err)
        )
    })?;

    println!("[INFO]: Listening on: {address}");
    let (message_sender, message_reciver) = channel();
    thread::spawn(|| server(message_reciver));

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let stream = Arc::new(stream);
                let message_sender = message_sender.clone();
                let token = token.clone();
                thread::spawn(|| client(stream, message_sender, token));
            }
            Err(err) => {
                eprintln!("[ERROR]: Could not accept connection: {}", Sensitive(err));
            }
        }
    }

    Ok(())
}
