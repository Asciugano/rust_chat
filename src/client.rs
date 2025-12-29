use crossterm::{
    QueueableCommand,
    cursor::MoveTo,
    event::{Event, KeyCode, KeyEventKind, KeyModifiers, poll, read},
    execute,
    style::{Color, Print, SetBackgroundColor, SetForegroundColor},
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use std::io::{ErrorKind, Read, Write, stdout};
use std::{cmp, thread};
use std::{io, net::TcpStream};
use std::{mem, time::Duration};

struct Rect {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
}

struct ScreenState;

impl ScreenState {
    fn enable() -> io::Result<Self> {
        execute!(stdout(), EnterAlternateScreen)?;
        terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for ScreenState {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode().map_err(|err| {
            eprintln!("[ERROR]: disable raw mode: {err}");
        });
        let _ = execute!(stdout(), LeaveAlternateScreen).map_err(|err| {
            eprintln!("[ERROR]: leave alternate screen: {err}");
        });
    }
}

fn sanitize_terminal_output(bytes: &[u8]) -> Option<String> {
    let bytes: Vec<u8> = bytes.iter().cloned().filter(|x| *x >= 32).collect();
    if let Ok(result) = str::from_utf8(&bytes) {
        Some(result.to_string())
    } else {
        None
    }
}

fn status_bar(buffer: &mut Buffer, label: &str, x: usize, y: usize, w: usize) {
    let label_chars: Vec<_> = label.chars().collect();
    let n = cmp::min(label_chars.len(), w);
    buffer.put_cells(x, y, &label_chars[..n], Color::Black, Color::White);
    for x in label.len()..w {
        buffer.put_cell(x, y, ' ', Color::Black, Color::White);
    }
}

fn parse_command<'a>(prompt: &'a [char]) -> Option<(&'a [char], &'a [char])> {
    let prompt = prompt.strip_prefix(&[':'])?;
    let mut iter = prompt.splitn(2, |x| *x == ' ');
    let a = iter.next().unwrap_or(prompt);
    let b = iter.next().unwrap_or(&[]);

    Some((a, b))
}

#[derive(Default)]
struct ChatLog {
    items: Vec<(String, Color)>,
}

#[derive(Debug, Clone, PartialEq)]
struct Cell {
    ch: char,
    fg: Color,
    bg: Color,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: Color::White,
            bg: Color::Black,
        }
    }
}

#[derive(Debug, Clone)]
struct Buffer {
    cells: Vec<Cell>,
    width: usize,
    height: usize,
}

struct Patch {
    cell: Cell,
    x: usize,
    y: usize,
}

impl Buffer {
    fn new(width: usize, height: usize) -> Self {
        let cells = vec![Cell::default(); width * height];
        Self {
            cells,
            width,
            height,
        }
    }

    fn resize(&mut self, width: usize, height: usize) {
        self.cells.resize(width * height, Cell::default());
        self.cells.fill(Cell::default());
        self.width = width;
        self.height = height;
    }

    fn diff(&self, other: &Self) -> Vec<Patch> {
        assert!(self.width == other.width && self.height == other.height);
        self.cells
            .iter()
            .zip(other.cells.iter())
            .enumerate()
            .filter(|(_, (a, b))| *a != *b)
            .map(|(i, (_, cell))| {
                let x = i % self.width;
                let y = i / self.width;
                Patch {
                    cell: cell.clone(),
                    x,
                    y,
                }
            })
            .collect()
    }

    fn clear(&mut self) {
        self.cells.fill(Cell::default());
    }

    fn put_cell(&mut self, x: usize, y: usize, ch: char, fg: Color, bg: Color) {
        if let Some(cell) = self.cells.get_mut(y * self.width + x) {
            *cell = Cell { ch, fg, bg }
        }
    }

    fn put_cells(&mut self, x: usize, y: usize, chs: &[char], fg: Color, bg: Color) {
        let start = y * self.width + x;
        for (offset, &ch) in chs.iter().enumerate() {
            if let Some(cell) = self.cells.get_mut(start + offset) {
                *cell = Cell { ch, fg, bg };
            } else {
                break;
            }
        }
    }

    fn flush(&self, qc: &mut impl Write) -> io::Result<()> {
        let mut fg_curr = Color::White;
        let mut bg_curr = Color::Black;
        qc.queue(Clear(ClearType::All))?;
        qc.queue(SetForegroundColor(fg_curr))?;
        qc.queue(SetBackgroundColor(bg_curr))?;
        qc.queue(MoveTo(0, 0))?;
        for Cell { ch, fg, bg } in self.cells.iter() {
            if fg_curr != *fg {
                fg_curr = *fg;
                qc.queue(SetForegroundColor(fg_curr))?;
            }
            if bg_curr != *bg {
                bg_curr = *bg;
                qc.queue(SetBackgroundColor(bg_curr))?;
            }
            qc.queue(Print(ch))?;
        }
        qc.flush()?;
        Ok(())
    }
}

impl ChatLog {
    fn push(&mut self, message: String, color: Color) {
        self.items.push((message, color))
    }

    fn render(&mut self, buffer: &mut Buffer, boundary: Rect) {
        let n = self.items.len();
        let m = n.checked_sub(boundary.h).unwrap_or(0);
        for (dy, (line, color)) in self.items.iter().skip(m).enumerate() {
            let line_chars: Vec<_> = line.chars().collect();
            buffer.put_cells(
                boundary.x,
                boundary.y + dy,
                line_chars.get(0..boundary.w).unwrap_or(&line_chars),
                *color,
                Color::Black,
            );
        }
    }
}

macro_rules! chat_msg {
    ($chat:expr, $($arg:tt)*) => {
        $chat.push(format!($($arg)*), Color::White)
    }
}

macro_rules! chat_error {
    ($chat:expr, $($arg:tt)*) => {
        $chat.push(format!($($arg)*), Color::Red)
    }
}

macro_rules! chat_info {
    ($chat:expr, $($arg:tt)*) => {
        $chat.push(format!($($arg)*), Color::Blue)
    }
}

#[derive(Default)]
struct Prompt {
    buffer: Vec<char>,
    cursor: usize,
    scroll: usize,
}

impl Prompt {
    fn sync_scroll_with_cursor(&mut self, w: usize) {
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        }
        if self.scroll + w <= self.cursor {
            self.scroll = self.cursor - w;
        }
    }

    fn sync_terminal_cursor(
        &mut self,
        qc: &mut impl Write,
        x: usize,
        y: usize,
        w: usize,
    ) -> io::Result<()> {
        if let Some(w) = w.checked_sub(2) {
            let x = x + 1;
            self.sync_scroll_with_cursor(w);
            let offset = self.cursor - self.scroll;
            let _ = qc.queue(MoveTo((x + offset) as u16, y as u16))?;
        }

        Ok(())
    }

    fn render(&mut self, buffer: &mut Buffer, x: usize, y: usize, w: usize) {
        if let Some(w) = w.checked_sub(2) {
            let x = x + 1;
            self.sync_scroll_with_cursor(w);
            let begin = self.scroll;
            let end = cmp::min(self.scroll + w, self.buffer.len());
            if let Some(window) = self.buffer.get(begin..end) {
                buffer.put_cells(x, y, window, Color::White, Color::Black);
                if self.scroll > 0 {
                    buffer.put_cell(x - 1, y, '<', Color::White, Color::Black);
                }
                if self.scroll + w < self.buffer.len() {
                    buffer.put_cell(x + w, y, '>', Color::White, Color::Black);
                }
            }
        }
    }

    fn insert(&mut self, x: char) {
        if self.cursor > self.buffer.len() {
            self.cursor = self.buffer.len()
        }
        self.buffer.insert(self.cursor, x);
        self.cursor += 1;
    }

    fn insert_str(&mut self, text: &str) {
        for x in text.chars() {
            self.insert(x);
        }
    }

    fn left_char(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    fn right_char(&mut self) {
        if self.cursor < self.buffer.len() {
            self.cursor += 1;
        }
    }

    fn at_cursor(&self) -> char {
        self.buffer.get(self.cursor).cloned().unwrap_or('\n')
    }

    fn left_word(&mut self) {
        while self.cursor > 0 && self.at_cursor().is_whitespace() {
            self.cursor -= 1;
        }
        while self.cursor > 0 && !self.at_cursor().is_whitespace() {
            self.cursor -= 1;
        }
    }

    fn right_word(&mut self) {
        while self.cursor < self.buffer.len() && self.at_cursor().is_whitespace() {
            self.cursor += 1;
        }

        while self.cursor < self.buffer.len() && !self.at_cursor().is_whitespace() {
            self.cursor += 1;
        }
    }

    fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.buffer.remove(self.cursor);
        }
    }

    fn before_cursor(&self) -> &[char] {
        &self.buffer[..self.cursor]
    }

    fn after_cursor(&self) -> &[char] {
        &self.buffer[self.cursor..]
    }

    fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
    }

    fn delete_until_end(&mut self) {
        while self.cursor < self.buffer.len() {
            self.buffer.pop();
        }
    }
}

#[derive(Default)]
struct Client {
    stream: Option<TcpStream>,
    chat: ChatLog,
    quit: bool,
}

fn connect_command(client: &mut Client, argument: &str) {
    if client.stream.is_none() {
        let chunks: Vec<&str> = argument.split(' ').filter(|s| !s.is_empty()).collect();
        match &chunks[..] {
            &[ip, token] => {
                client.stream = TcpStream::connect(&format!("{ip}:4444"))
                    .and_then(|mut stream| {
                        stream.set_nonblocking(true)?;
                        stream.write(token.as_bytes())?;

                        Ok(stream)
                    })
                    .map_err(|err| {
                        chat_error!(&mut client.chat, "Could not connect to {ip}: {err}")
                    })
                    .ok();
            }
            _ => {
                chat_error!(
                    &mut client.chat,
                    "Inccorrect usage of connect command. Try :connect <ip> <token>"
                );
            }
        }
    } else {
        chat_error!(
            &mut client.chat,
            "You are already connected to a server. Disconnect with :disconnect first."
        );
    }
}

fn disconnect_command(client: &mut Client, _arguments: &str) {
    if client.stream.is_some() {
        client.stream = None;
        chat_info!(&mut client.chat, "Disconnected");
    } else {
        chat_info!(&mut client.chat, "You are already offline");
    }
}

fn quit_command(client: &mut Client, _arguments: &str) {
    client.quit = true;
}

fn help_command(client: &mut Client, argument: &str) {
    let name = argument.trim();
    if name.is_empty() {
        for Command {
            signature,
            description,
            ..
        } in COMMANDS.iter()
        {
            chat_info!(client.chat, "{signature} - {description}");
        }
    } else {
        if let Some(Command {
            signature,
            description,
            ..
        }) = find_command(name)
        {
            chat_info!(client.chat, "{signature} - {description}");
        } else {
            chat_error!(client.chat, "Unknown command `:{name}`");
        }
    }
}

struct Command {
    name: &'static str,
    description: &'static str,
    signature: &'static str,
    run: fn(&mut Client, &str),
}

const COMMANDS: &[Command] = &[
    Command {
        name: "connect",
        run: connect_command,
        description: "Connect to a server by <ip> with authorization <token>",
        signature: ":connect <ip> <token>",
    },
    Command {
        name: "disconnect",
        run: disconnect_command,
        description: "Disconnect from the server you are currently connected to",
        signature: ":disconnect",
    },
    Command {
        name: "quit",
        run: quit_command,
        description: "Close the chat",
        signature: ":quit",
    },
    Command {
        name: "help",
        run: help_command,
        description: "Print the help",
        signature: ":help [command]",
    },
];

fn find_command(name: &str) -> Option<&Command> {
    COMMANDS.iter().find(|command| command.name == name)
}

fn apply_patchers(qc: &mut impl QueueableCommand, patches: &[Patch]) -> io::Result<()> {
    let mut fg_curr = Color::White;
    let mut bg_curr = Color::Black;
    let mut x_prev = 0;
    let mut y_prev = 0;
    qc.queue(SetForegroundColor(fg_curr))?;
    qc.queue(SetBackgroundColor(bg_curr))?;
    for Patch {
        cell: Cell { ch, fg, bg },
        x,
        y,
    } in patches
    {
        if !(y_prev == *y && x_prev + 1 == *x) {
            qc.queue(MoveTo(*x as u16, *y as u16))?;
        }
        x_prev = *x;
        y_prev = *y;
        if fg_curr != *fg {
            fg_curr = *fg;

            qc.queue(SetForegroundColor(fg_curr))?;
        }
        if bg_curr != *bg {
            bg_curr = *bg;

            qc.queue(SetBackgroundColor(bg_curr))?;
        }
        qc.queue(Print(ch))?;
    }

    Ok(())
}

fn main() -> io::Result<()> {
    let mut client = Client::default();
    let mut stdout = stdout();
    let _screen_state = ScreenState::enable();
    let (mut w, mut h) = terminal::size()?;
    let mut buf_curr = Buffer::new(w as usize, h as usize);
    let mut buf_prev = Buffer::new(w as usize, h as usize);
    let mut prompt = Prompt::default();
    let mut buf = [0; 64];
    help_command(&mut client, "");
    buf_prev.flush(&mut stdout)?;
    while !client.quit {
        while poll(Duration::ZERO)? {
            match read()? {
                Event::Resize(nw, nh) => {
                    w = nw;
                    h = nh;
                    buf_curr.resize(w as usize, h as usize);
                    buf_prev.resize(w as usize, h as usize);
                    buf_prev.flush(&mut stdout)?;
                }
                Event::Paste(data) => prompt.insert_str(&data),
                Event::Key(event) => {
                    if event.kind == KeyEventKind::Press {
                        match event.code {
                            KeyCode::Char(x) => {
                                if event.modifiers.contains(KeyModifiers::CONTROL) {
                                    match x {
                                        'c' => client.quit = true,
                                        'k' => prompt.delete_until_end(),
                                        _ => {}
                                    }
                                } else {
                                    prompt.insert(x);
                                }
                            }
                            KeyCode::Left => {
                                if event.modifiers.contains(KeyModifiers::CONTROL) {
                                    prompt.left_word();
                                } else {
                                    prompt.left_char();
                                }
                            }
                            KeyCode::Right => {
                                if event.modifiers.contains(KeyModifiers::CONTROL) {
                                    prompt.right_word();
                                } else {
                                    prompt.right_char();
                                }
                            }
                            KeyCode::Backspace => prompt.backspace(),
                            KeyCode::Tab => {
                                if let Some((prefix, &[])) = parse_command(prompt.before_cursor()) {
                                    let prefix = prefix.iter().collect::<String>();
                                    let rest = prompt.after_cursor().iter().collect::<String>();
                                    if let Some(command) = COMMANDS
                                        .iter()
                                        .find(|command| command.name.starts_with(&prefix))
                                    {
                                        prompt.clear();
                                        prompt.insert(':');
                                        prompt.insert_str(command.name);
                                        prompt.insert_str(&rest);
                                        prompt.cursor = command.name.len() + 1;
                                    }
                                }
                            }
                            KeyCode::Enter => {
                                if let Some((name, argument)) = parse_command(&prompt.buffer) {
                                    let name = name.iter().collect::<String>();
                                    let argument = argument.iter().collect::<String>();
                                    if let Some(command) = find_command(&name) {
                                        ((command.run)(&mut client, &argument));
                                    } else {
                                        chat_error!(&mut client.chat, "Unknown comman `:{name}");
                                    }
                                } else {
                                    if let Some(stream) = &mut client.stream {
                                        let prompt = prompt.buffer.iter().collect::<String>();
                                        stream.write(prompt.as_bytes())?;
                                        chat_msg!(&mut client.chat, "{text}", text = &prompt);
                                    } else {
                                        chat_info!(
                                            &mut client.chat,
                                            "You are offline. Use {signature} to connecto to a server",
                                            signature = find_command("connect")
                                                .expect("connect command")
                                                .signature
                                        );
                                    }
                                }
                                prompt.clear();
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }

        if let Some(s) = &mut client.stream {
            match s.read(&mut buf) {
                Ok(n) => {
                    if n > 0 {
                        if let Some(line) = sanitize_terminal_output(&buf[..n]) {
                            client.chat.push(line, Color::White);
                        }
                    } else {
                        client.stream = None;
                        chat_info!(&mut client.chat, "Server closed the connection");
                    }
                }
                Err(err) => {
                    if err.kind() != ErrorKind::WouldBlock {
                        client.stream = None;
                        chat_error!(&mut client.chat, "Connection Error: {err}");
                    }
                }
            }
        }

        buf_curr.clear();
        status_bar(&mut buf_curr, "chat", 0, 0, w.into());
        if let Some(h) = h.checked_sub(3) {
            client.chat.render(
                &mut buf_curr,
                Rect {
                    x: 0,
                    y: 1,
                    w: w as usize,
                    h: h as usize,
                },
            );
        }
        let status_label = if client.stream.is_some() {
            "Status: Online"
        } else {
            "Status: OffLine"
        };
        if let Some(h) = h.checked_sub(2) {
            status_bar(&mut buf_curr, status_label, 0, h as usize, w.into());
        }
        if let Some(y) = h.checked_sub(1) {
            let x = 1;
            if let Some(w) = w.checked_sub(1) {
                prompt.render(&mut buf_curr, x, y as usize, w as usize);
            }
            buf_curr.put_cell(0, y as usize, '-', Color::White, Color::Black);
        }

        apply_patchers(&mut stdout, &buf_prev.diff(&buf_curr))?;

        if let Some(y) = h.checked_sub(1) {
            let x = 1;
            if let Some(w) = w.checked_sub(1) {
                prompt.sync_terminal_cursor(&mut stdout, x, y as usize, w as usize)?;
            }
        }
        stdout.flush()?;
        mem::swap(&mut buf_curr, &mut buf_prev);

        thread::sleep(Duration::from_millis(16));
    }

    Ok(())
}
