use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TryRecvError;

use anyhow::Result;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use tokio::task;

use crossterm::event::{KeyCode, KeyEventKind};

const unchecked: &str = "\u{2610}";
const checked: &str = "\u{2611}";
const failed: &str = "\u{2612}";

fn render(frame: &mut ratatui::Frame, out: &str, status: &[Line]) {
    use ratatui::layout::Constraint::Fill;
    use ratatui::layout::Layout;
    use ratatui::widgets::{Block, Paragraph};

    let [top, bottom] = Layout::vertical([Fill(1); 2]).areas(frame.area());

    frame.render_widget(
        Paragraph::new(status.to_owned()).block(Block::bordered().title("Workflow")),
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

#[derive(Clone)]
struct Task {
    name: String,
    cmd: String,
    state: State,
}
#[derive(Clone)]
enum State {
    Complete,
    Failed,
    Pending,
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

enum UIUpdate {
    Status(Vec<Line<'static>>),
    AddLine(String),
}

async fn run_ui(mut rx: mpsc::Receiver<UIUpdate>) -> Result<()> {
    let mut terminal = ratatui::init();
    let mut out = String::new();
    let mut status = Vec::new();
    'outer: loop {
        loop {
            match rx.try_recv() {
                Ok(UIUpdate::AddLine(line)) => {
                    out += &line;
                    out += "\n";
                }
                Ok(UIUpdate::Status(st)) => {
                    status = st;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break 'outer,
            }
        }
        terminal.draw(|frame| render(frame, &out, &status))?;
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
    terminal.draw(|frame| render(frame, &out, &status)).unwrap();
    std::thread::sleep(std::time::Duration::from_secs(5));
    ratatui::restore();
    Ok(())
}

async fn run_command(task: &Task, intx: mpsc::Sender<UIUpdate>) -> Result<bool> {
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
                tx.send(UIUpdate::AddLine(line)).await.unwrap()
            }
        });
    }
    {
        let tx = intx.clone();
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        tasks.spawn(async move {
            while let Ok(Some(line)) = lines.next_line().await {
                tx.send(UIUpdate::AddLine(line)).await.unwrap()
            }
        });
    }
    let success = cmd.wait().await?.success();
    // TODO: get exit code.
    tasks.join_all();

    Ok(success)
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("Hello world");

    let mut steps = [
        Task {
            name: "test".to_string(),
            cmd: "echo -e '\\x1b[1mhello world\\x1b[0m' >&2 && echo foo && cd ~/scm/rustradio && echo cargo test --color=always"
                .to_string(),
            state: State::Pending,
        },
        Task {
            name: "false".to_string(),
            cmd: "false".to_string(),
            state: State::Pending,
        },
        Task {
            name: "Foo".to_string(),
            cmd: "echo third step".to_string(),
            state: State::Pending,
        },
    ];
    let (tx, rx) = mpsc::channel(500);
    task::spawn(async move {
        for (n, s) in steps.clone().iter_mut().enumerate() {
            tx.send(make_status_update(&steps)).await.unwrap();
            match run_command(&s, tx.clone()).await {
                Ok(true) => {
                    steps[n].state = State::Complete;
                }
                Ok(false) => {
                    steps[n].state = State::Failed;
                }
                Err(e) => {
                    tx.send(UIUpdate::AddLine(format!("Got an error: {e:?}\n")))
                        .await
                        .unwrap();
                }
            }
        }
    });

    run_ui(rx).await
}

fn make_status_update(steps: &[Task]) -> UIUpdate {
    let lines: Vec<_> = steps
        .iter()
        .map(|s| {
            let (pre, color) = match s.state {
                State::Complete => (checked, Color::Green),
                State::Failed => (failed, Color::Red),
                State::Pending => (unchecked, Color::Yellow),
            };
            Line::from(vec![Span::styled(
                format!("{pre} {}", s.name),
                Style::default().fg(color),
            )])
        })
        .collect();
    let owned_lines: Vec<Line> = lines
        .clone()
        .into_iter()
        .map(|line| {
            Line::from(
                line.spans
                    .into_iter()
                    .map(|span| Span::styled(span.content.to_string(), span.style))
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    UIUpdate::Status(owned_lines)
}
