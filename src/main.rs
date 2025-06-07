use std::ffi::OsString;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TryRecvError;

use anyhow::{Error, Result};
use log::trace;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use tokio::task;

use clap::Parser;
use crossterm::event::{KeyCode, KeyEventKind};

const UNCHECKED: &str = "\u{2610}";
const CHECKED: &str = "\u{2611}";
const FAILED: &str = "\u{2612}";

#[derive(clap::Parser, Debug, Clone)]
#[command(version, about)]
struct Opt {
    /// Directory with workflow scripts.
    #[arg(long)]
    dir: std::path::PathBuf,

    /// Directory that tickbox should use as a starting working directory.
    #[arg(long, default_value = ".")]
    cwd: std::path::PathBuf,

    /// Only run steps (files) matching regex.
    #[arg(long, default_value = ".*")]
    matching: regex::Regex,

    /// Wait when done, even if successful.
    #[arg(long)]
    wait: bool,

    /// Optionally log to file.
    #[arg(long, default_value = "/dev/null")]
    log: String,

    /// Optionally disable TUI.
    #[arg(long)]
    disable_tui: bool,

    /// Enable parallel ranges.
    #[arg(long, num_args=1, value_delimiter=',', value_parser=parse_range)]
    parallel: Vec<(usize, usize)>,

    /// Maximum task concurrency.
    #[arg(long, default_value_t = 3)]
    max_concurrency: usize,
}

fn parse_range(s: &str) -> Result<(usize, usize), String> {
    let part = s;
    use std::str::FromStr;
    let parts: Vec<&str> = part.split('-').collect();
    if parts.len() != 2 {
        return Err(format!("Invalid range format: {s}"));
    }

    let start = usize::from_str(parts[0]).map_err(|_| format!("Invalid number: {}", parts[0]))?;
    let end = usize::from_str(parts[1]).map_err(|_| format!("Invalid number: {}", parts[1]))?;

    if start > end {
        return Err(format!("End must be less than start: {s}"));
    }

    Ok((start, end))
}

#[derive(Default)]
struct UiState {
    scroll: usize,
}

// Render the UI, once.
fn render(frame: &mut ratatui::Frame, out: &str, status: &[Line], state: &mut UiState) {
    use ratatui::layout::Layout;
    use ratatui::prelude::*;
    use ratatui::widgets::{Block, Paragraph};

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(frame.area());
    let top = chunks[0];
    let bottom = chunks[1];

    // Render top part.
    frame.render_widget(
        Paragraph::new(status.to_owned()).block(Block::bordered().title("Workflow")),
        top,
    );
    let nlines = out.lines().collect::<Vec<_>>().len();
    state.scroll = state.scroll.clamp(
        0,
        nlines.max(bottom.height as usize) - bottom.height as usize + 2,
    );

    // Render bottom part, the command output.
    use ansi_to_tui::IntoText;
    let out: Vec<Line> = out
        .lines()
        .rev()
        // Subtract top and bottom border.
        .skip(state.scroll)
        .take((bottom.height - 2).into())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .flat_map(|line| line.into_text().unwrap())
        .collect::<Vec<_>>();

    frame.render_widget(
        Paragraph::new(out).block(Block::bordered().title("Command output")),
        bottom,
    );
}

/// A task is one step in a workflow, and therefore one file on disk.
#[derive(Debug, Clone)]
struct Task {
    n: usize,
    id: usize,
    name: String,
    cmd: std::path::PathBuf,
    state: State,
}

/// The state of a task.
#[derive(Clone, Debug)]
enum State {
    Complete(Duration),
    Failed(Duration),
    Running(Instant),
    Pending,
    Skipped,
}

impl std::fmt::Display for State {
    fn fmt(&self, w: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        match self {
            State::Pending => write!(w, "Pending"),
            State::Running(_) => write!(w, "Running"),
            State::Failed(d) => write!(w, "Failed after {}", format_duration(*d)),
            State::Complete(d) => write!(w, "Succeeded after {}", format_duration(*d)),
            State::Skipped => write!(w, "Skipped"),
        }
    }
}

/// Return `true` if this is a sync point, that stops parallel steps.
fn sync_point(
    task: &Task,
    running: &[&Task],
    opt_par: &[(usize, usize)],
    conf_par_re: &[regex::Regex],
) -> bool {
    if !opt_par.is_empty() {
        // If command line flag ranges are provided, then use that instead of
        // the config.
        if let Some(r) = opt_par.iter().find(|r| r.0 <= task.id && task.id <= r.1) {
            return !running.iter().all(|t| r.0 <= t.id && t.id <= r.1);
        }
        return true;
    }
    if let Some(r) = conf_par_re.iter().find(|r| r.is_match(&task.name)) {
        return !running.iter().all(|t| r.is_match(&t.name));
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;

    #[test]
    fn sync_test() -> Result<()> {
        let running = [
            &Task {
                n: 0,
                id: 1,
                name: "01-first".into(),
                cmd: "".into(),
                state: State::Pending,
            },
            &Task {
                n: 1,
                id: 2,
                name: "02-second".into(),
                cmd: "".into(),
                state: State::Pending,
            },
        ];
        let new = Task {
            n: 2,
            id: 3,
            name: "03-third".into(),
            cmd: "".into(),
            state: State::Pending,
        };
        for (a, b, out) in [
            (vec![], vec![], true),
            // Test command line.
            (vec![(0, 1)], vec![], true),
            (vec![(0, 2)], vec![], true),
            (vec![(0, 3)], vec![], false),
            (vec![(0, 4)], vec![], false),
            (vec![(1, 4)], vec![], false),
            (vec![(2, 4)], vec![], true),
            // Test config.
            (vec![], vec![Regex::new("XXX")?], true),
            (vec![], vec![Regex::new("^01-")?], true),
            (vec![], vec![Regex::new("^0[1-2]-")?], true),
            (vec![], vec![Regex::new("^0[1-3]-")?], false),
            (vec![], vec![Regex::new("^0[1-4]-")?], false),
            (vec![], vec![Regex::new("^0[2-4]-")?], true),
        ] {
            assert_eq!(
                sync_point(&new, &running, &a, &b),
                out,
                "failed for input {a:?} {b:?} => {out}"
            );
        }
        Ok(())
    }
}

/// A UIUpdate is sent to the UI thread whenever there's any news.
enum UIUpdate {
    /// Enable waiting when finished, even if all tasks succeed.
    Wait,

    /// Update the status window.
    Status(Task),

    /// Add a line to the stdout/stderr window.
    AddLine(String),
}

async fn run_raw(mut rx: mpsc::Receiver<UIUpdate>) -> Result<()> {
    let mut status = Vec::new();
    loop {
        match rx.try_recv() {
            Ok(UIUpdate::Wait) => {
                // Waiting only makes sense in TUI mode.
            }
            Ok(UIUpdate::AddLine(line)) => {
                println!("{line}");
            }
            Ok(UIUpdate::Status(st)) if st.n == status.len() => {
                status.push(st);
            }
            Ok(UIUpdate::Status(st)) => {
                status[st.n] = st.clone();
                let maxlen = status
                    .iter()
                    .map(|s| s.name.len())
                    .max()
                    .expect("no steps?");
                println!("=== Status ===");
                for task in &status {
                    println!("  {:>maxlen$} {}", task.name, task.state);
                }
            }
            Err(TryRecvError::Empty) => continue,
            Err(TryRecvError::Disconnected) => {
                return Ok(());
            }
        }
    }
}

/// Run the UI until the channel with UIUpdates ends.
async fn run_tui(mut rx: mpsc::Receiver<UIUpdate>) -> Result<()> {
    let mut terminal = ratatui::init();
    let mut out = String::new();
    let mut status = Vec::new();
    let mut do_wait = false;
    let mut state = UiState::default();
    'outer: loop {
        loop {
            match rx.try_recv() {
                Ok(UIUpdate::Wait) => {
                    do_wait = true;
                }
                Ok(UIUpdate::AddLine(line)) => {
                    out += &line;
                    out += "\n";
                }
                Ok(UIUpdate::Status(st)) if st.n == status.len() => {
                    status.push(st);
                }
                Ok(UIUpdate::Status(st)) => {
                    status[st.n] = st.clone();
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if do_wait {
                        break;
                    } else {
                        break 'outer;
                    }
                }
            }
        }
        let status_lines = make_status_update(&status);
        // TODO: get the actual output window height.
        let out_height = 10;
        terminal.draw(|frame| render(frame, &out, &status_lines, &mut state))?;
        // Handle input.
        if crossterm::event::poll(std::time::Duration::from_millis(50)).unwrap() {
            match crossterm::event::read().unwrap() {
                crossterm::event::Event::Key(key) if key.kind == KeyEventKind::Press => {
                    match key.code {
                        KeyCode::Char('j') | KeyCode::Down => {
                            state.scroll = state.scroll.saturating_sub(1)
                        }
                        KeyCode::PageDown => state.scroll = state.scroll.saturating_sub(out_height),
                        KeyCode::Char('k') | KeyCode::Up => state.scroll += 1,
                        KeyCode::PageUp => state.scroll += out_height,
                        KeyCode::Char('l') => terminal.clear()?,
                        KeyCode::Char('q') => break,
                        KeyCode::Char('Q') => break,
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
    let status = make_status_update(&status);
    out += "\n======== Exiting tickbox UI ==========";
    terminal
        .draw(|frame| render(frame, &out, &status, &mut state))
        .unwrap();
    ratatui::restore();
    Ok(())
}

/// Run a command, and wait for it to finish.
///
/// Returns `true` if the command exited with code 0.
async fn run_command(
    task: &Task,
    envs: &[(OsString, OsString)],
    tx: mpsc::Sender<UIUpdate>,
) -> Result<bool> {
    use tokio::io::AsyncBufReadExt;
    use tokio::io::BufReader;

    // TODO: Make this fixed width.
    tx.send(UIUpdate::AddLine(format!(
        "============ Running \"{}\" ================",
        task.name,
    )))
    .await
    .unwrap();

    let mut cmd = tokio::process::Command::new("bash")
        .arg("-c")
        .arg(task.cmd.clone())
        .envs(envs.iter().map(|(k, v)| (k.as_os_str(), v.as_os_str())))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("Failed to execute");
    let stdout = cmd.stdout.take().unwrap();
    let stderr = cmd.stderr.take().unwrap();
    let rout = BufReader::new(stdout);
    let mut lout = rout.lines();
    let rerr = BufReader::new(stderr);
    let mut lerr = rerr.lines();

    let mut out_open = true;
    let mut err_open = true;

    loop {
        trace!("Main loop iteration");
        tokio::select! {
            line = lerr.next_line(), if err_open => {
                trace!("Stderr line");
                match line? {
                    Some(line) => {
                        if tx.send(UIUpdate::AddLine(line)).await.is_err() {
                            cmd.kill().await?;
                            break;
                        }
                    }
                    None => err_open = false,
                }
            }
            line = lout.next_line(), if out_open => {
                trace!("Stdout line");
                match line? {
                    Some(line) => {
                        if tx.send(UIUpdate::AddLine(line)).await.is_err() {
                            cmd.kill().await?;
                            break;
                        }
                    }
                    None => out_open = false,
                }
            }

            status = cmd.wait() => {
                trace!("Command finished");
                let status = status?;
                tx.send(UIUpdate::AddLine("".to_string())).await.unwrap();
                use std::os::unix::process::ExitStatusExt;
                if let Some(code) = status.code() {
                    tx.send(UIUpdate::AddLine(format!(
                        "==> Command \"{}\" exited with code {code}",
                        task.name,
                    )))
                    .await
                    .unwrap();
                } else if let Some(sig) = status.signal() {
                    tx.send(UIUpdate::AddLine(format!(
                        "==> Command \"{}\" exited with signal {sig} ",
                        task.name
                    )))
                    .await
                    .unwrap();
                }
                return Ok(status.success());
            },
        };
    }
    Ok(false)
}

fn parse_usize_prefix(input: &str) -> Option<usize> {
    let digits_end = input
        .char_indices()
        .take_while(|(_, c)| c.is_ascii_digit())
        .map(|(i, _)| i + 1)
        .last()?;

    let (digits, _) = input.split_at(digits_end);
    let value = digits.parse::<usize>().ok()?;
    Some(value)
}

/// Load workflow (list of tasks) from directory.
fn load_tasks(path: &std::path::Path) -> Result<Vec<Task>> {
    use itertools::Itertools;
    Ok(std::fs::read_dir(path)
        .map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!("Failed to read directory {}: {e}", path.display()),
            )
        })?
        .flatten()
        .filter_map(|entry| {
            let cmd = entry.path();
            let name = cmd.file_name().unwrap().to_str().unwrap();

            if name.ends_with("~") // Don't join.
               || name.ends_with(".conf")
               || name.ends_with(".json")
               || name.starts_with(".")
            {
                return None;
            }
            let id = match parse_usize_prefix(name).ok_or(Error::msg(format!(
                "step file name doesn't start with a number: {name}"
            ))) {
                Ok(x) => x,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(Task {
                n: 0,
                id,
                name: name.to_string(),
                cmd,
                state: State::Pending,
            }))
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .sorted_by(|a, b| a.id.cmp(&b.id))
        .enumerate()
        .map(|(n, t)| {
            let mut t = t.clone();
            t.n = n;
            t
        })
        .collect())
}

fn format_duration(d: Duration) -> String {
    format!("{:7.1}s", d.as_secs_f64())
}

/// Take the tasks and turn them into something nicely formatted.
fn make_status_update(steps: &[Task]) -> Vec<Line<'static>> {
    let maxlen = steps.iter().map(|s| s.name.len()).max().expect("no steps?");
    let lines: Vec<_> = steps
        .iter()
        .map(|s| {
            let (pre, color, extra) = match s.state {
                State::Running(st) => (UNCHECKED, Color::Blue, format_duration(st.elapsed())),
                State::Complete(e) => (CHECKED, Color::Green, format_duration(e)),
                State::Failed(e) => (FAILED, Color::Red, format_duration(e)),
                State::Pending => (UNCHECKED, Color::Yellow, "".to_owned()),
                State::Skipped => (UNCHECKED, Color::Gray, "".to_owned()),
            };
            Line::from(vec![Span::styled(
                format!("{pre} {:<maxlen$} {extra}", s.name),
                Style::default().fg(color),
            )])
        })
        .collect();
    lines
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
        .collect()
}

#[derive(Default, serde::Deserialize, Clone)]
struct Config {
    #[serde(deserialize_with = "deserialize_envs")]
    envs: Vec<(OsString, OsString)>,
    #[serde(deserialize_with = "deserialize_regexes", default)]
    parallel_regex: Vec<regex::Regex>,
}

fn deserialize_regexes<'de, D>(deserializer: D) -> Result<Vec<regex::Regex>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    let strs = Vec::<String>::deserialize(deserializer)?;
    let mut regexes = Vec::with_capacity(strs.len());

    for s in strs {
        match regex::Regex::new(&s) {
            Ok(r) => regexes.push(r),
            Err(e) => {
                return Err(serde::de::Error::custom(format!(
                    "Invalid regex '{s}': {e}"
                )));
            }
        }
    }
    Ok(regexes)
}

fn deserialize_envs<'de, D>(deserializer: D) -> Result<Vec<(OsString, OsString)>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    use std::collections::HashMap;
    let map: HashMap<String, String> = HashMap::deserialize(deserializer)?;
    Ok(map
        .into_iter()
        .map(|(key, value)| (OsString::from(key), OsString::from(value)))
        .collect())
}

/// Load config in JSON format.
fn load_config(dir: &std::path::Path) -> Result<Config> {
    let filename = dir.join("tickbox.json");
    let contents = match std::fs::read_to_string(&filename) {
        Ok(data) => data,
        Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Config::default());
        }
        Err(e) => {
            return Err(std::io::Error::new(
                e.kind(),
                format!("Error reading {}: {}", filename.display(), e),
            )
            .into());
        }
    };
    serde_json::from_str(&contents).map_err(|e| Error::msg(format!("JSON parse: {e}")))
}

fn strip_newlines(os: OsString) -> OsString {
    match os.into_string() {
        Ok(s) => OsString::from(s.trim_end_matches(['\n', '\r'])),
        Err(e) => panic!("Branch name not valid UTF-8: {e:?}"),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let opt = Opt::parse();
    simplelog::WriteLogger::init(
        simplelog::LevelFilter::Info,
        simplelog::Config::default(),
        std::fs::File::create(&opt.log).unwrap(),
    )?;
    let mut conf = load_config(&opt.dir)?;
    let steps = load_tasks(&opt.dir)?;
    std::env::set_current_dir(&opt.cwd)?;
    let cwd = std::env::current_dir()?;
    let tmp_dir = tempfile::TempDir::new()?;
    conf.envs.extend(vec![
        ("TICKBOX_TEMPDIR".into(), tmp_dir.path().into()),
        ("TICKBOX_CWD".into(), cwd.to_str().unwrap().into()),
    ]);

    // If CWD is a git repository, put the branch name into an env.
    {
        let gitdir = cwd.join(".git");
        if gitdir.exists() && gitdir.is_dir() {
            let out = tokio::process::Command::new("git")
                .arg("branch")
                .arg("--show-current")
                .output()
                .await?;
            if !out.status.success() {
                return Err(Error::msg("git branch exec failed"));
            }
            let branch = strip_newlines(OsString::from_vec(out.stdout.clone()));
            use std::os::unix::ffi::OsStringExt;
            conf.envs.push(("TICKBOX_BRANCH".into(), branch));
        }
    }
    let (tx, rx) = mpsc::channel(500);
    if opt.wait {
        tx.send(UIUpdate::Wait).await.unwrap();
    }
    for s in steps.iter() {
        tx.send(UIUpdate::Status(s.clone())).await.unwrap();
    }
    let disable_tui = opt.disable_tui;
    let runner = task::spawn(async move {
        let mut success = true;
        let mut running: Vec<Task> = Vec::new();
        let mut handles: Vec<tokio::task::JoinHandle<bool>> = Vec::new();
        for (n, s) in steps.clone().iter_mut().enumerate() {
            if handles.len() >= opt.max_concurrency {
                let (res, idx, _rem) = futures::future::select_all(&mut handles).await;
                match res {
                    Ok(true) => {}
                    Ok(false) => return false,
                    Err(e) => panic!("{e}"),
                }
                handles.remove(idx);
                running.remove(idx);
            }
            let s = s.clone();
            let mut steps = steps.clone();
            let opt = opt.clone();
            let tx = tx.clone();
            let conf = conf.clone();
            let rs: Vec<&Task> = running.iter().collect();
            if sync_point(&s, &rs, &opt.parallel, &conf.parallel_regex) {
                for t in handles.iter_mut() {
                    if !t.await.unwrap() {
                        //success = false;
                        return false;
                    }
                }
                running.clear();
                handles.clear();
            }
            running.push(s.clone());
            handles.push(task::spawn(async move {
                if !opt.matching.is_match(&steps[n].name) {
                    steps[n].state = State::Skipped;
                    tx.send(UIUpdate::Status(steps[n].clone())).await.unwrap();
                    return true;
                }
                let now = Instant::now();
                steps[n].state = State::Running(now);
                tx.send(UIUpdate::Status(steps[n].clone())).await.unwrap();

                match run_command(&s, &conf.envs, tx.clone()).await {
                    Ok(true) => {
                        steps[n].state = State::Complete(now.elapsed());
                    }
                    Ok(false) => {
                        // This send() fails if the UI is gone, so nowhere to
                        // display it anyway.
                        let _ = tx.send(UIUpdate::Wait).await;
                        steps[n].state = State::Failed(now.elapsed());
                        let _ = tx.send(UIUpdate::Status(steps[n].clone())).await;
                        return false;
                    }
                    Err(e) => {
                        tx.send(UIUpdate::AddLine(format!("Got an error: {e:?}\n")))
                            .await
                            .unwrap();
                    }
                }
                let _ = tx.send(UIUpdate::Status(steps[n].clone())).await;
                true
            }));
        }
        for r in handles.into_iter() {
            if !r.await.unwrap() {
                success = false;
            }
        }
        success
    });

    if disable_tui {
        run_raw(rx).await?;
    } else {
        run_tui(rx).await?;
    }
    if !runner.await? {
        std::process::exit(1);
    }
    Ok(())
}
