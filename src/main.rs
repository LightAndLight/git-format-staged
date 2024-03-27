use std::{
    path::{Path, PathBuf},
    process::Command,
};

use clap::Parser;
use git2::{
    build::TreeUpdateBuilder, ApplyLocation, DiffOptions, Error, FileMode, Index, IndexEntry,
    Repository, Tree,
};

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

    assert!(repo_path.is_absolute());
    assert!(cwd.is_absolute());
    let dir_prefix = cwd.strip_prefix(repo_path).unwrap();

    prepare_workdir(&repo, dir_prefix, files)?;

    let exit_status = Command::new(command)
        .args(args)
        .args(files)
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
        for file in files {
            // At this point the index hasn't been changed, so `.staged.orig` can be removed.
            remove_file(&format!("{}.staged.orig", file));

            // Restores the file to its original unstaged version.
            rename_file(&format!("{}.orig", file), file);
        }

        match exit_status.code() {
            Some(code) => std::process::exit(code),
            None => {
                eprintln!("error: {} was terminated by a signal", command);
                std::process::exit(1);
            }
        }
    }

    let index_tree = get_index_tree(&repo)?;
    let formatted_tree = build_formatted_tree(&repo, &index_tree, dir_prefix, files)?;
    let diff = repo.diff_tree_to_tree(
        Some(&index_tree),
        Some(&formatted_tree),
        Some(DiffOptions::new().context_lines(0)),
    )?;

    for file in files {
        copy_file(&format!("{}.orig", file), file);
    }
    repo.apply(&diff, ApplyLocation::WorkDir, None)?;

    // Formatting has succeeded and changes have been "backported" to
    // the unstaged files. The index can be safely updated.
    let mut index = repo.index()?;
    index.read_tree(&formatted_tree)?;
    index.write()?;

    // This run has succeeded. The backups can all be safely removed.
    for file in files {
        remove_file(&format!("{}.staged.orig", file));
        remove_file(&format!("{}.orig", file));
    }

    Ok(())
}

/** Creates backups of existing files and copies data out of the index.

* Each file `file` to be formatted is renamed to `file.orig`.
* The version of `file` in the index is written to the filesystem as `file.staged.orig`.
* The version of `file` in the index is also written to the filesystem as `file`.

  This is the file that will be formatted.

*/
fn prepare_workdir(repo: &Repository, dir_prefix: &Path, files: &[String]) -> Result<(), Error> {
    let index = repo.index()?;

    rename_originals(&index, dir_prefix, files);

    for file in files {
        match get_staged(&index, dir_prefix, file) {
            Some(index_entry) => {
                let entry_blob = repo.find_blob(index_entry.id).unwrap_or_else(|err| {
                    eprintln!("error: failed to lookup blob for {}: {}", file, err);
                    std::process::exit(1);
                });

                let content = entry_blob.content();
                write_file(&format!("{}.staged.orig", file), content);
                write_file(file, content);
            }
            None => {
                panic!("internal error: {} is not a staged file", file);
            }
        }
    }

    Ok(())
}

/** Rename the target files from `file` to `file.orig`.

The `.orig` files need to stick around until the very end of the program, in case
an unexpected failure happens.
*/
fn rename_originals(index: &Index, dir_prefix: &Path, files: &[String]) {
    // The user has passed a file that isn't actually staged
    let mut bad_file = false;

    let renamed_files: Vec<(&str, String)> = files
        .iter()
        .filter_map(|file| match get_staged(index, dir_prefix, file) {
            Some(_) => {
                let from = file.as_ref();
                let to = format!("{}.orig", file);
                rename_file(from, &to);
                Some((from, to))
            }
            None => {
                eprintln!("error: {} is not a staged file", file);
                bad_file = true;
                None
            }
        })
        .collect();

    if bad_file {
        for (from, to) in renamed_files {
            rename_file(&to, from);
        }
        std::process::exit(1);
    }
}

/// View the current index as a [`Tree`].
fn get_index_tree(repo: &Repository) -> Result<Tree, Error> {
    let oid = repo.index()?.write_tree()?;
    repo.find_tree(oid)
}

/** Create a [`Tree`], based on `index_tree`, but including the new contents of files
in `files` that have changed on disk.
*/
fn build_formatted_tree<'a>(
    repo: &'a Repository,
    index_tree: &Tree,
    dir_prefix: &Path,
    files: &[String],
) -> Result<Tree<'a>, Error> {
    let mut tree_builder = TreeUpdateBuilder::new();

    for file in files.iter() {
        let content = std::fs::read(file).unwrap();
        let blob_oid = repo.blob(&content)?;
        tree_builder.upsert(dir_prefix.join(file), blob_oid, FileMode::Blob);
    }

    let post_tree_oid = tree_builder.create_updated(repo, index_tree)?;
    repo.find_tree(post_tree_oid)
}

fn get_staged(index: &Index, dir_prefix: &Path, file: &str) -> Option<IndexEntry> {
    index.get_path(&dir_prefix.join(file), 0)
}

fn copy_file(from: &str, to: &str) -> u64 {
    std::fs::copy(from, to).unwrap_or_else(|err| {
        eprintln!("error: failed to copy {} to {}: {}", from, to, err);
        std::process::exit(1);
    })
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
