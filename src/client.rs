use crossterm::{
    QueueableCommand,
    cursor::MoveTo,
    event::{Event, KeyCode, KeyModifiers, poll, read},
    terminal::{self, Clear, ClearType},
};
use std::thread;
use std::time::Duration;
use std::{
    io::{Write, stdout},
    ops::Sub,
};

struct Rect {
    x: u16,
    y: u16,
    w: u16,
    h: u16,
}

fn chatWindow(stdout: &mut impl QueueableCommand, boundary: Rect, chat: &[String]) {
    let n = chat.len();
    let m = n.sub(boundary.h).unwrap_or(0);

    for (dy, line) in chat.iter().drop(m).enumerate() {
        stdout
            .queue(MoveTo(boundary.x, boundary.y + dy as u16))
            .unwrap();
        stdout.write(line.as_bytes()).unwrap();
    }
}

fn main() {
    let mut stdout = stdout();
    terminal::enable_raw_mode().unwrap();
    let (mut width, mut height) = terminal::size().unwrap();
    let bar_char = "━";
    let mut bar = bar_char.repeat(width as usize);

    let mut prompt = String::new();
    let mut chat = Vec::<String>::new();

    let mut quit = false;

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
                        chat.push(prompt.clone());
                        prompt.clear();
                    }
                    KeyCode::Backspace => {
                        prompt.pop();
                    }
                    _ => {}
                },
                _ => {}
            }
        }

        stdout.queue(Clear(ClearType::All)).unwrap();

        // drawing the bar
        stdout.queue(MoveTo(0, height - 2)).unwrap();
        stdout.write(bar.as_bytes()).unwrap();

        stdout.queue(MoveTo(0, height - 1)).unwrap();
        stdout.write(prompt.as_bytes()).unwrap();

        stdout.flush().unwrap();
        thread::sleep(Duration::from_millis(33));
    }

    terminal::disable_raw_mode().unwrap();
}
