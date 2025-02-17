use std::io::BufReader;
use std::sync::mpsc::TryRecvError;

use anyhow::Result;

use crossterm::event::{KeyCode, KeyEventKind};

fn render(frame: &mut ratatui::Frame, out: &str) {
    use ratatui::layout::Constraint::Fill;
    use ratatui::layout::Layout;
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};
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

fn run_ui(rx: std::sync::mpsc::Receiver<String>) -> Result<()> {
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

fn run_command(task: &Task, tx: std::sync::mpsc::Sender<String>) -> Result<()> {
    let mut cmd = std::process::Command::new("bash")
        .arg("-c")
        .arg(task.cmd.clone())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("Failed to execute");
    std::thread::scope(|scope| -> Result<()> {
        let t1 = scope.spawn(|| -> Result<()> {
            if let Some(stdout) = cmd.stdout.take() {
                let reader = BufReader::new(stdout);
                let lines = std::io::BufRead::lines(reader);
                for line in lines {
                    tx.send(line?)?;
                }
            }
            Ok(())
        });
        let t2 = scope.spawn(|| -> Result<()> {
            if let Some(stderr) = cmd.stderr.take() {
                let reader = BufReader::new(stderr);
                let lines = std::io::BufRead::lines(reader);
                for line in lines {
                    tx.send(line?)?;
                }
            }
            Ok(())
        });
        t1.join().unwrap()?;
        t2.join().unwrap()
    })?;
    cmd.wait()?;
    // TODO: check for exit code 0
    Ok(())
}

fn main() -> Result<()> {
    println!("Hello world");

    let running = Task {
        name: "test".to_string(),
        cmd: "echo hello world && echo foo && cd ~/scm/rustradio && cargo test"
            .to_string(),
    };
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::scope(move |scope| {
        scope.spawn(move || {
            match run_command(&running, tx.clone()) {
                Ok(_) => {}
                Err(e) => {
                    tx.send(format!("Got an error: {e:?}\n")).unwrap();
                }
            }
        });
        run_ui(rx)
    })
}
