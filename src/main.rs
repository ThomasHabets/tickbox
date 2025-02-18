use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TryRecvError;

use anyhow::Result;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use tokio::task;

use crossterm::event::{KeyCode, KeyEventKind};

fn render(frame: &mut ratatui::Frame, out: &str) {
    use ratatui::layout::Constraint::Fill;
    use ratatui::layout::Layout;
    use ratatui::widgets::{Block, Paragraph};

    let [top, bottom] = Layout::vertical([Fill(1); 2]).areas(frame.area());

    let unchecked = "\u{2610}";
    let checked = "\u{2611}";
    let failed = "\u{2612}";

    let text = vec![
        Line::from(vec![Span::styled(
            format!("{checked} Step 1"),
            Style::default().fg(Color::Green),
        )]),
        Line::from(vec![Span::styled(
            format!("{failed} Step 2"),
            Style::default().fg(Color::Red),
        )]),
        Line::from(vec![Span::styled(
            format!("{unchecked} Step 3"),
            Style::default(),
        )]),
    ];

    frame.render_widget(
        Paragraph::new(text).block(Block::bordered().title("Workflow")),
        top,
    );

    let out = out
        .lines()
        .rev()
        .take((bottom.height - 5).into())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    frame.render_widget(
        Paragraph::new(out).block(Block::bordered().title("Command output")),
        bottom,
    );
}

struct Task {
    name: String,
    cmd: String,
}
enum Step {
    Complete(Task),
    Failed(Task),
    Pending(Task),
}

fn ansi_to_spans(ansi_str: &str) -> Vec<Span> {
    use ansi_parser::{AnsiParser, AnsiSequence, Output};
    let parsed = ansi_str.ansi_parse();
    let mut style = Style::default();
    let mut out = Vec::new();
    for fragment in parsed {
        match fragment {
            Output::TextBlock(txt) => out.push(Span::styled(txt, style)),
            Output::Escape(AnsiSequence::SetGraphicsMode(params)) => {
                for param in params {
                    style = match param {
                        0 => Style::default(),
                        1 => style.add_modifier(ratatui::style::Modifier::BOLD),
                        30 => style.fg(Color::Black),
                        31 => style.fg(Color::Red),
                        32 => style.fg(Color::Green),
                        33 => style.fg(Color::Yellow),
                        34 => style.fg(Color::Blue),
                        35 => style.fg(Color::Magenta),
                        36 => style.fg(Color::Cyan),
                        37 => style.fg(Color::White),
                        38 => style.fg(Color::Reset),
                        _ => style,
                    };
                }
            }
            _ => {}
        }
    }
    out
}
/*
fn ansi_color_to_ratatui(color: nu_ansi_term::Color) -> Color {
    match color {
        nu_ansi_term::Color::Black => Color::Black,
        nu_ansi_term::Color::Red => Color::Red,
        nu_ansi_term::Color::Green => Color::Green,
        nu_ansi_term::Color::Yellow => Color::Yellow,
        nu_ansi_term::Color::Blue => Color::Blue,
        nu_ansi_term::Color::Magenta => Color::Magenta,
        nu_ansi_term::Color::Cyan => Color::Cyan,
        nu_ansi_term::Color::White => Color::White,
        nu_ansi_term::Color::Fixed(i) => Color::Indexed(i),
        nu_ansi_term::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
        _ => panic!()
    }
}
*/
async fn run_ui(mut rx: mpsc::Receiver<String>) -> Result<()> {
    let mut terminal = ratatui::init();
    let mut out = String::new();
    'outer: loop {
        loop {
            match rx.try_recv() {
                Ok(line) => {
                    out += &line;
                    out += "\n";
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break 'outer,
            }
        }
        terminal.draw(|frame| render(frame, &out))?;
        // Handle input.
        if crossterm::event::poll(std::time::Duration::from_millis(50)).unwrap() {
            match crossterm::event::read().unwrap() {
                crossterm::event::Event::Key(key) if key.kind == KeyEventKind::Press => {
                    match key.code {
                        KeyCode::Char('q') => break,
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
    out += "\n========DONE==========";
    terminal.draw(|frame| render(frame, &out)).unwrap();
    std::thread::sleep(std::time::Duration::from_secs(1));
    ratatui::restore();
    Ok(())
}

async fn run_command(task: &Task, intx: mpsc::Sender<String>) -> Result<()> {
    use tokio::io::AsyncBufReadExt;
    use tokio::io::BufReader;
    let mut cmd = tokio::process::Command::new("bash")
        .arg("-c")
        .arg(task.cmd.clone())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("Failed to execute");
    // TODO: do a select lines read
    let mut tasks = tokio::task::JoinSet::new();
    let stdout = cmd.stdout.take().unwrap();
    let stderr = cmd.stderr.take().unwrap();
    {
        let tx = intx.clone();
        let reader = BufReader::new(stderr);
        let mut lines = reader.lines();
        tasks.spawn(async move {
            while let Ok(Some(line)) = lines.next_line().await {
                tx.send(line).await.unwrap()
            }
        });
    }
    {
        let tx = intx.clone();
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        tasks.spawn(async move {
            while let Ok(Some(line)) = lines.next_line().await {
                tx.send(line).await.unwrap()
            }
        });
    }
    cmd.wait().await?;
    // TODO: get exit code.
    tasks.join_all();

    // TODO: check for exit code 0
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("Hello world");

    let running = Task {
        name: "test".to_string(),
        cmd: "echo -e '\\x1b[1mhello world' >&2 && echo foo && cd ~/scm/rustradio && cargo test --color=always"
            .to_string(),
    };
    let (tx, rx) = mpsc::channel(500);
    task::spawn(async move {
        match run_command(&running, tx.clone()).await {
            Ok(_) => {}
            Err(e) => {
                tx.send(format!("Got an error: {e:?}\n")).await.unwrap();
            }
        }
    });
    run_ui(rx).await
}
