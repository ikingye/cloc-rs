use std::fs;
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex, RwLock};

use crate::config::{Config, Info};
use crate::detail::Detail;
use crate::error::ClocError;
use crate::executor::ThreadPoolExecutor;
use crate::ClocResult;

#[derive(Debug)]
pub struct Engine {
    config: Config,
    entry: PathBuf,
    total_text_files: AtomicUsize,
    ignored_files: AtomicUsize,
}

impl Engine {
    pub fn new(entry: PathBuf) -> Self {
        Self {
            config: Config::default(),
            entry,
            total_text_files: AtomicUsize::new(0),
            ignored_files: AtomicUsize::new(0),
        }
    }

    pub fn calculate(self) -> (Vec<Detail>, usize, usize) {
        let executor = ThreadPoolExecutor::new();
        let Engine {
            config,
            entry,
            total_text_files,
            ignored_files,
        } = self;

        let config = Arc::new(RwLock::new(config));
        let total_text_files = Arc::new(total_text_files);
        let ignored_files = Arc::new(ignored_files);
        let (sender, receiver) = sync_channel(1024);
        let receiver = Arc::new(Mutex::new(receiver));

        executor.submit(move || explore(entry, &sender));

        let details = Arc::new(Mutex::new(Vec::new()));
        for _ in 0..(executor.capacity() - 1) {
            let receiver = Arc::clone(&receiver);
            let config = Arc::clone(&config);
            let total_text_files = Arc::clone(&total_text_files);
            let ignored_files = Arc::clone(&ignored_files);
            let details = Arc::clone(&details);

            executor.submit(move || {
                for path in receiver.lock().unwrap().recv() {
                    total_text_files.fetch_add(1, Ordering::SeqCst);

                    let info = match config.read().unwrap().get_by_extension(path.extension()) {
                        Some(info) => info.clone(),
                        None => {
                            ignored_files.fetch_add(1, Ordering::SeqCst);
                            continue;
                        }
                    };

                    if let Ok(detail) = calculate(path, info) {
                        details.lock().unwrap().push(detail);
                    } else {
                        ignored_files.fetch_add(1, Ordering::SeqCst);
                    }
                }
            });
        }
        mem::drop(executor);

        (
            Arc::try_unwrap(details).unwrap().into_inner().unwrap(),
            Arc::try_unwrap(total_text_files).unwrap().into_inner(),
            Arc::try_unwrap(ignored_files).unwrap().into_inner(),
        )
    }
}

fn explore(dir: PathBuf, sender: &SyncSender<PathBuf>) {
    // TODO: refactor
    if dir.is_file() {
        sender.send(dir).unwrap();
    } else if dir.is_dir() {
        let entries = fs::read_dir(dir).unwrap();
        for entry in entries {
            let entry = entry.unwrap();

            let path = entry.path();
            if path.is_file() {
                // TODO: remove unwrap
                sender.send(path).unwrap();
            } else if path.is_dir() {
                explore(path, sender);
            }
        }
    }
}

fn is_text_file<P: AsRef<Path>>(path: P) -> bool {
    // TODO
    // check:
    // 1. permission (can access)
    // 2. ascii text
    // 3. symlink
    let path = path.as_ref();
    if let Ok(metadata) = path.metadata() {
        let file_type = metadata.file_type();

        if file_type.is_symlink() {
            return false;
        }
    }

    true
}

fn calculate(path: PathBuf, info: Info) -> ClocResult<Detail> {
    let Info {
        name, single, multi, ..
    } = info;

    if !is_text_file(&path) {
        return Err(ClocError::NonTextFile);
    }

    let content = fs::read_to_string(path)?;
    let mut blank = 0;
    let mut comment = 0;
    let mut code = 0;
    let mut in_comment: Option<(&str, &str)> = None;

    'here: for line in content.lines() {
        let line = line.trim();

        // empty line
        if line.is_empty() {
            blank += 1;
            continue;
        }

        // match single line comments
        for single in &single {
            if line.starts_with(single) {
                comment += 1;
                continue 'here;
            }
        }

        // match multi line comments
        for (start, end) in &multi {
            if let Some(d) = in_comment {
                if d != (start, end) {
                    continue;
                }
            }

            // multi line comments maybe in one line
            let mut same_line = false;
            if line.starts_with(start) {
                in_comment = match in_comment {
                    Some(_) => {
                        comment += 1;
                        in_comment = None;
                        continue 'here;
                    }
                    None => {
                        same_line = true;
                        Some((start, end))
                    }
                }
            }

            // This line is in comments
            if in_comment.is_some() {
                comment += 1;
                if line.ends_with(end) {
                    if same_line {
                        if line.len() >= (start.len() + end.len()) {
                            in_comment = None;
                        }
                    } else {
                        in_comment = None;
                    }
                }
                continue 'here;
            }
        }

        code += 1;
    }

    Ok(Detail::new(name.as_str(), blank, comment, code))
}