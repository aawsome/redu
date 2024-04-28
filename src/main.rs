#![feature(iter_intersperse)]
#![feature(try_blocks)]

use std::cmp;

use clap::{command, Parser};
use crossterm::event::{Event, EventStream, KeyCode};
use futures::TryStreamExt;
use ratatui::CompletedFrame;
use ratatui::style::Stylize;
use ratatui::widgets::{List, ListItem, Widget};

use crate::cache::Cache;
use crate::restic::Restic;
use crate::state::State;
use crate::tui::Tui;
use crate::types::Snapshot;

mod cache;
mod restic;
mod types;
mod tui;
mod state;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    #[arg(short = 'r', long)]
    repo: String,
    #[arg(long)]
    password_command: Option<String>,
}

fn render<'a>(
    tui: &'a mut Tui,
    state: &'_ State,
) -> std::io::Result<CompletedFrame<'a>>
{
    tui.draw(|frame| {
        let area = frame.size();
        let buf = frame.buffer_mut();
        let items = state.files
            .iter()
            .enumerate()
            .map(|(index, (name, size))| {
                let item = ListItem::new(format!("{name} : {size}"));
                if Some(index) == state.selected {
                    item.black().on_white()
                } else {
                    item
                }
            });
        List::new(items).render(area, buf);
    })
}

#[tokio::main]
async fn main() {
    env_logger::init();
    let cli = Cli::parse();
    let restic = Restic::new(&cli.repo, cli.password_command.as_ref().map(|s| s.as_str()));
    eprintln!("Getting restic config");
    let repo_id = restic.config().await.0.unwrap().id;
    let mut cache = Cache::open(repo_id.as_str()).unwrap();

    eprintln!("Using cache file '{}'", cache.filename());

    // Figure out what snapshots we need to update
    let snapshots: Vec<Snapshot> = {
        eprintln!("Fetching restic snapshot list");
        let restic_snapshots = restic.snapshots().await.0.unwrap();

        // Delete snapshots from the DB that were deleted on Restic
        for snapshot in cache.get_snapshots().unwrap() {
            if ! restic_snapshots.contains(&snapshot) {
                eprintln!("Deleting DB Snapshot {:?} (missing from restic)", snapshot.id);
                cache.delete_snapshot(&snapshot.id).unwrap();
            }
        }

        let db_snapshots = cache.get_snapshots().unwrap();
        restic_snapshots.into_iter().filter(|s| ! db_snapshots.contains(s)).collect()
    };

    // Update snapshots
    if snapshots.len() > 0 {
        eprintln!("Need to fetch {} snapshot(s)", snapshots.len());
        for (snapshot, i) in snapshots.iter().zip(1..) {
            eprintln!("Fetching snapshot {:?} [{}/{}]", &snapshot.id, i, snapshots.len());
            let (mut files, _) = restic.ls(&snapshot.id).await;
            let handle = cache.start_snapshot(&snapshot.id).unwrap();
            while let Some(f) = files.try_next().await.unwrap() {
                handle.insert_file(&f.path, f.size).unwrap()
            }
            handle.finish().unwrap();
        }
    } else {
        eprintln!("Snapshots up to date");
    }

    // UI
    let mut tui = Tui::new().unwrap();
    let mut terminal_events = EventStream::new();
    let mut state = State {
        path: Some("/".into()),
        files: cache.get_max_file_sizes(Some("/".into())).unwrap(),
        selected: None,
    };

    render(&mut tui, &state).unwrap();
    while let Some(event) = terminal_events.try_next().await.unwrap() {
        match event {
            Event::Key(k) => match k.code {
                KeyCode::Char('q') => break,
                KeyCode::Down => state.move_selection(1),
                KeyCode::Up => state.move_selection(-1),
                _ => {},
            }
            _ => {}
        }
        render(&mut tui, &state).unwrap();
    }
}
