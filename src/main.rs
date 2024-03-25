use std::{
    path::{Path, PathBuf},
    process::Command,
};

use clap::Parser;
use git2::{build::TreeUpdateBuilder, ApplyLocation, DiffOptions, FileMode, Repository};

#[derive(Parser)]
struct Cli {
    /// The staged files to format.
    files: Vec<String>,

    /// The formatting command.
    #[clap(last = true)]
    command: Vec<String>,
}

fn main() {
    let cli = Cli::parse();

    if let Some((command, args)) = cli.command.split_first() {
        let repo_path = match search_upward_for_entry(".", ".git") {
            Some(path) => path,
            None => {
                eprintln!("error: not a Git repository");
                std::process::exit(1);
            }
        };

        match git_format_staged(&repo_path, &cli.files, command, args) {
            Ok(()) => {}
            Err(err) => {
                eprintln!("error: {}", err);
                std::process::exit(1);
            }
        }
    }
}

fn git_format_staged(
    repo_path: &Path,
    files: &[String],
    command: &str,
    args: &[String],
) -> Result<(), git2::Error> {
    let repo = Repository::open(repo_path)?;

    let mut index = repo.index()?;
    let mut bad_target = false;
    let targets: Vec<String> = files
        .iter()
        .filter_map(|arg| match index.get_path(arg.as_ref(), 0) {
            Some(index_entry) => {
                {
                    let from = arg;
                    let to = format!("{}.orig", arg);
                    std::fs::rename(from, &to).unwrap_or_else(|err| {
                        eprintln!("error: failed to rename {} to {}: {}", from, to, err);
                        std::process::exit(1);
                    });
                }

                {
                    let entry_blob = repo.find_blob(index_entry.id).unwrap_or_else(|err| {
                        eprintln!("error: failed to lookup blob for {}: {}", arg, err);
                        std::process::exit(1);
                    });

                    let content = entry_blob.content();

                    {
                        let path = format!("{}.pre", arg);
                        std::fs::write(&path, content).unwrap_or_else(|err| {
                            eprintln!("error: failed to write {}: {}", path, err);
                            std::process::exit(1);
                        });
                    }

                    {
                        let path = format!("{}.post", arg);
                        std::fs::write(&path, content).unwrap_or_else(|err| {
                            eprintln!("error: failed to write {}: {}", path, err);
                            std::process::exit(1);
                        });

                        Some(path)
                    }
                }
            }
            None => {
                eprintln!("error: {} is not staged", arg);
                bad_target = true;
                None
            }
        })
        .collect();

    if bad_target {
        std::process::exit(1);
    }

    fn cleanup(args: &[String]) {
        for arg in args {
            {
                let path = format!("{}.pre", arg);
                std::fs::remove_file(&path).unwrap_or_else(|err| {
                    eprintln!("error: failed to remove {}: {}", path, err);
                });
            }

            {
                let path = format!("{}.post", arg);
                std::fs::remove_file(&path).unwrap_or_else(|err| {
                    eprintln!("error: failed to remove {}: {}", path, err);
                });
            }
        }
    }

    fn restore(args: &[String]) {
        cleanup(args);

        for arg in args {
            let from = format!("{}.orig", arg);
            let to = arg;
            std::fs::rename(&from, to).unwrap_or_else(|err| {
                eprintln!("error: failed to rename {} to {}: {}", from, to, err);
            });
        }
    }

    let exit_status = Command::new(command)
        .args(args)
        .args(&targets)
        .status()
        .unwrap();
    if !exit_status.success() {
        restore(files);

        match exit_status.code() {
            Some(code) => std::process::exit(code),
            None => {
                eprintln!("error: {} was terminated by a signal", command);
                std::process::exit(1);
            }
        }
    }

    let index_tree_oid = index.write_tree()?;
    let index_tree = repo.find_tree(index_tree_oid)?;

    let mut tree_builder = TreeUpdateBuilder::new();
    for (arg, target) in files.iter().zip(targets.iter()) {
        let content = std::fs::read(target).unwrap();
        let blob_oid = repo.blob(&content)?;
        tree_builder.upsert(arg, blob_oid, FileMode::Blob);
    }

    let post_tree_oid = tree_builder.create_updated(&repo, &index_tree)?;
    let post_tree = repo.find_tree(post_tree_oid)?;

    let diff = repo.diff_tree_to_tree(
        Some(&index_tree),
        Some(&post_tree),
        Some(DiffOptions::new().context_lines(0)),
    )?;

    for file in files {
        let from = format!("{}.orig", file);
        let to = file;
        std::fs::copy(&from, to).unwrap_or_else(|err| {
            eprintln!("error: failed to copy {} to {}: {}", from, to, err);
            std::process::exit(1);
        });
    }
    repo.apply(&diff, ApplyLocation::WorkDir, None)?;

    index.read_tree(&post_tree)?;
    index.write()?;

    cleanup(files);

    for file in files {
        let path = format!("{}.orig", file);
        std::fs::remove_file(&path).unwrap_or_else(|err| {
            eprintln!("error: failed to remove {}: {}", path, err);
        });
    }

    Ok(())
}

fn search_upward_for_entry<P: AsRef<Path>>(cwd: P, entry: &str) -> Option<PathBuf> {
    let mut target_dir = std::fs::canonicalize(cwd.as_ref()).unwrap();
    let mut found = false;

    loop {
        let target_file_exists = {
            target_dir.push(entry);
            let result = target_dir.try_exists().unwrap();
            target_dir.pop();
            result
        };

        if target_file_exists {
            found = true;
            break;
        }

        if !target_dir.pop() {
            break;
        }
    }

    if found {
        Some(target_dir)
    } else {
        None
    }
}
