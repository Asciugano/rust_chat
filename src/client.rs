use crossterm::{
    QueueableCommand,
    cursor::MoveTo,
    event::{Event, KeyCode, KeyModifiers, poll, read},
    terminal::{self, Clear, ClearType},
};
use std::net::TcpStream;
use std::thread;
use std::time::Duration;
use std::{
    env,
    io::{ErrorKind, Read, Write, stdout},
};

struct Rect {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
}

fn chat_window(stdout: &mut impl Write, chat: &[String], boundary: Rect) {
    let n = chat.len();
    let m = n.checked_sub(boundary.h).unwrap_or(0);

    for (dy, line) in chat.iter().skip(m).enumerate() {
        stdout
            .queue(MoveTo(boundary.x as u16, (boundary.y + dy) as u16))
            .unwrap();

        let bytes = line.as_bytes();
        stdout
            .write(bytes.get(0..boundary.w).unwrap_or(bytes))
            .unwrap();
    }
}

fn main() {
    let mut args = env::args();
    let _ = args.next().expect("program name");

    let ip = args.next().expect("provide an Ip");

    let mut stream = TcpStream::connect(&format!("{ip}:4444")).unwrap();
    stream.set_nonblocking(true).unwrap();

    let mut stdout = stdout();
    terminal::enable_raw_mode().unwrap();
    let (mut width, mut height) = terminal::size().unwrap();
    let bar_char = "━";
    let mut bar = bar_char.repeat(width as usize);

    let mut prompt = String::new();
    let mut chat = Vec::<String>::new();

    let mut quit = false;

    let mut buf = [0; 64];

    while !quit {
        while poll(Duration::ZERO).unwrap() {
            match read().unwrap() {
                Event::Resize(nw, nh) => {
                    width = nw;
                    height = nh;
                    bar = bar_char.repeat(width as usize);
                }
                Event::Key(event) => match event.code {
                    KeyCode::Char(x) => {
                        if x == 'c' && event.modifiers.contains(KeyModifiers::CONTROL) {
                            quit = true;
                        }
                        prompt.push(x)
                    }
                    KeyCode::Enter => {
                        stream.write(prompt.as_bytes()).unwrap();
                        if prompt.len() > 0 {
                            chat.push(prompt.clone());
                        }
                        prompt.clear();
                    }
                    KeyCode::Backspace => {
                        prompt.pop();
                    }
                    KeyCode::Esc => prompt.clear(),
                    _ => {}
                },
                Event::Paste(data) => {
                    prompt.push_str(&data);
                }
                _ => {}
            }
        }

        match stream.read(&mut buf) {
            Ok(n) => {
                if n > 0 {
                    chat.push(str::from_utf8(&buf[0..n]).unwrap().to_string())
                } else {
                    quit = true;
                }
            }
            Err(err) => {
                if err.kind() != ErrorKind::WouldBlock {
                    panic!("{err}");
                }
            }
        }

        stdout.queue(Clear(ClearType::All)).unwrap();

        chat_window(
            &mut stdout,
            chat.as_mut_slice(),
            Rect {
                x: 0,
                y: 0,
                w: width as usize,
                h: (height - 2) as usize,
            },
        );

        // drawing the bar
        stdout.queue(MoveTo(0, height - 2)).unwrap();
        stdout.write(bar.as_bytes()).unwrap();

        stdout.queue(MoveTo(0, height - 1)).unwrap();
        {
            let bytes = prompt.as_bytes();
            stdout
                .write(bytes.get(0..width as usize).unwrap_or(bytes))
                .unwrap();
        }

        stdout.flush().unwrap();
        thread::sleep(Duration::from_millis(33));
    }

    terminal::disable_raw_mode().unwrap();
}
