use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TryRecvError;

use anyhow::Result;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use tokio::task;

use clap::Parser;
use crossterm::event::{KeyCode, KeyEventKind};

const UNCHECKED: &str = "\u{2610}";
const CHECKED: &str = "\u{2611}";
const FAILED: &str = "\u{2612}";

#[derive(clap::Parser, Debug)]
#[command(version, about)]
struct Opt {
    #[arg(long)]
    dir: std::path::PathBuf,
    #[arg(long)]
    cwd: std::path::PathBuf,
}

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
    Running,
    Pending,
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
                        KeyCode::Char('Q') => break,
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

async fn run_command(
    task: &Task,
    cwd: &std::path::Path,
    tmpdir: &std::path::Path,
    intx: mpsc::Sender<UIUpdate>,
) -> Result<bool> {
    use tokio::io::AsyncBufReadExt;
    use tokio::io::BufReader;
    let mut cmd = tokio::process::Command::new("bash")
        .arg("-c")
        .arg(task.cmd.clone())
        .env("TICKBOX_TEMPDIR", tmpdir.as_os_str())
        .env("TICKBOX_CWD", cwd.as_os_str())
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
    tasks.join_all().await;

    Ok(success)
}

fn load_tasks(path: &std::path::Path) -> Result<Vec<Task>> {
    let entries = std::fs::read_dir(path)?;
    let mut tasks = Vec::new();
    for entry in entries.flatten() {
        if entry.path().display().to_string().ends_with("~") {
            continue;
        }
        tasks.push(Task {
            name: entry
                .path()
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .to_string(),
            cmd: entry.path().display().to_string(),
            state: State::Pending,
        });
    }
    tasks.sort_by(|a, b| a.cmd.cmp(&b.cmd));
    Ok(tasks)
}

#[tokio::main]
async fn main() -> Result<()> {
    let opt = Opt::parse();
    let temp_dir = tempfile::TempDir::new()?;
    let mut steps = load_tasks(&opt.dir)?;
    std::env::set_current_dir(&opt.cwd)?;
    let cwd = std::env::current_dir()?;
    let (tx, rx) = mpsc::channel(500);
    let runner = task::spawn(async move {
        let mut success = true;
        for (n, s) in steps.clone().iter_mut().enumerate() {
            steps[n].state = State::Running;
            tx.send(make_status_update(&steps)).await.unwrap();
            match run_command(s, &cwd, temp_dir.path(), tx.clone()).await {
                Ok(true) => {
                    steps[n].state = State::Complete;
                }
                Ok(false) => {
                    success = false;
                    steps[n].state = State::Failed;
                    tx.send(make_status_update(&steps)).await.unwrap();
                    break;
                }
                Err(e) => {
                    tx.send(UIUpdate::AddLine(format!("Got an error: {e:?}\n")))
                        .await
                        .unwrap();
                }
            }
            tx.send(make_status_update(&steps)).await.unwrap();
        }
        success
    });

    run_ui(rx).await?;
    if runner.await? == false {
        std::process::exit(1);
    }
    Ok(())
}

fn make_status_update(steps: &[Task]) -> UIUpdate {
    let lines: Vec<_> = steps
        .iter()
        .map(|s| {
            let (pre, color) = match s.state {
                State::Running => (UNCHECKED, Color::Blue),
                State::Complete => (CHECKED, Color::Green),
                State::Failed => (FAILED, Color::Red),
                State::Pending => (UNCHECKED, Color::Yellow),
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
