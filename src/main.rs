use std::{
    path::{Path, PathBuf},
    process::Command,
};

use clap::Parser;
use git2::{Error, Index, IndexEntry, Repository, Status};

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

        let repo_path = repo_path.canonicalize().unwrap();
        let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
        match git_format_staged(&repo_path, &cwd, &cli.files, command, args) {
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
    cwd: &Path,
    files: &[String],
    command: &str,
    args: &[String],
) -> Result<(), git2::Error> {
    let repo = Repository::open(repo_path)?;

    assert!(
        repo_path.is_absolute(),
        "internal error: {} is not an absolute path",
        repo_path.display()
    );
    assert!(
        cwd.is_absolute(),
        "internal error: {} is not an absolute path",
        cwd.display()
    );
    let dir_prefix = cwd.strip_prefix(repo_path).unwrap();

    let to_format = prepare_workdir(&repo, dir_prefix, files)?;

    let exit_status = Command::new(command)
        .args(args)
        .args(to_format.iter().flat_map(|file| match file {
            TargetFile::UnstagedAndStaged { unstaged, staged } => {
                vec![*unstaged, staged.as_ref()].into_iter()
            }
            TargetFile::StagedOnly(file) => vec![*file].into_iter(),
        }))
        .status()
        .unwrap_or_else(|err| {
            eprintln!(
                "error: command `{command}{}{}{}{}` failed: {err}",
                if args.is_empty() { "" } else { " " },
                args.join(" "),
                if files.is_empty() { "" } else { " " },
                files.join(" "),
            );
            std::process::exit(1);
        });
    if !exit_status.success() {
        for file in to_format {
            if let TargetFile::UnstagedAndStaged { staged, .. } = file {
                remove_file(&staged);
            }
        }

        match exit_status.code() {
            Some(code) => std::process::exit(code),
            None => {
                eprintln!("error: {} was terminated by a signal", command);
                std::process::exit(1);
            }
        }
    }

    let mut index = repo.index()?;
    for file in to_format {
        match file {
            TargetFile::UnstagedAndStaged { unstaged, staged } => {
                let formatted = format!("{}.formatted", unstaged);
                // `file` -> `file.formatted`
                rename_file(unstaged, &formatted);

                // `file.staged` -> `file`
                rename_file(&staged, unstaged);

                index.add_path(&dir_prefix.join(unstaged))?;

                // `file.formatted` -> `file`
                rename_file(&formatted, unstaged);

                // `.staged` files are not longer in the file system after this
                // series of renamings.
            }
            TargetFile::StagedOnly(staged) => {
                index.add_path(&dir_prefix.join(staged))?;
            }
        }
    }
    index.write()?;

    Ok(())
}

/// A (logical) staged file to be formatted.
enum TargetFile<'a> {
    /// The working tree version of a file differs from the version in the index.
    /// Both versions need to be formatted independently.
    UnstagedAndStaged { unstaged: &'a str, staged: String },

    /// The working tree version of a file is the same as the version in the index.
    /// Only one physical file needs to be formatted.
    StagedOnly(&'a str),
}

/** Copies files out of the index where needed.

Only succeeds when all the `files` are in the index.

* If a file is in the index and is unchanged from its working tree version, then nothing is copied.
  The on-disk file will be formatted and then re-added to the index.

* If a file is in the index and is different to its working tree version, then its data in the index
  is copied to the file system and given the suffix `.staged`. Both the staged and unstaged versions
  will be passed to the formatting command, and afterward the staged version will be re-added to the
  index and removed from the file system.
*/
fn prepare_workdir<'a>(
    repo: &Repository,
    dir_prefix: &Path,
    files: &'a [String],
) -> Result<Vec<TargetFile<'a>>, Error> {
    let index = repo.index()?;

    check_files_staged(&index, dir_prefix, files);

    let to_format = files
        .iter()
        .map(|file| {
            let status = repo.status_file(&dir_prefix.join(file))?;
            assert!(
                status.intersects(
                    Status::INDEX_NEW
                        | Status::INDEX_MODIFIED
                        | Status::INDEX_RENAMED
                        | Status::INDEX_TYPECHANGE
                ),
                "internal error: {} is not in the index",
                file
            );

            match get_staged(&index, dir_prefix, file) {
                Some(index_entry) => {
                    if status.intersects(
                        Status::WT_NEW
                            | Status::WT_MODIFIED
                            | Status::WT_RENAMED
                            | Status::WT_TYPECHANGE,
                    ) {
                        let entry_blob = repo.find_blob(index_entry.id).unwrap_or_else(|err| {
                            eprintln!("error: failed to lookup blob for {}: {}", file, err);
                            std::process::exit(1);
                        });

                        let content = entry_blob.content();

                        let staged_path = format!("{}.staged", file);
                        write_file(&staged_path, content);

                        Ok(TargetFile::UnstagedAndStaged {
                            unstaged: file,
                            staged: staged_path,
                        })
                    } else {
                        Ok(TargetFile::StagedOnly(file))
                    }
                }
                None => {
                    unreachable!();
                }
            }
        })
        .collect::<Result<Vec<_>, Error>>()?;

    Ok(to_format)
}

/** Check that the target files are actually staged.

Reports all files that aren't in the index and exits with failure if so.
*/
fn check_files_staged(index: &Index, dir_prefix: &Path, files: &[String]) {
    let mut bad_file = false;

    for file in files {
        if get_staged(index, dir_prefix, file).is_none() {
            eprintln!("error: {} is not a staged file", file);
            bad_file = true;
        }
    }

    if bad_file {
        std::process::exit(1);
    }
}

fn get_staged(index: &Index, dir_prefix: &Path, file: &str) -> Option<IndexEntry> {
    index.get_path(&dir_prefix.join(file), 0)
}

fn write_file(path: &str, content: &[u8]) {
    std::fs::write(path, content).unwrap_or_else(|err| {
        eprintln!("error: failed to write {}: {}", path, err);
        std::process::exit(1);
    })
}

fn rename_file(from: &str, to: &str) {
    std::fs::rename(from, to).unwrap_or_else(|err| {
        eprintln!("error: failed to rename {} to {}: {}", from, to, err);
        std::process::exit(1);
    })
}

fn remove_file(path: &str) {
    std::fs::remove_file(path).unwrap_or_else(|err| {
        eprintln!("error: failed to remove {}: {}", path, err);
    })
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
