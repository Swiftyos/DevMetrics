use std::path::PathBuf;
use std::collections::HashMap;
use git2::{Repository, Status, StatusOptions, Time};
use chrono::{DateTime, Utc, Local};
use structopt::StructOpt;
use notify::{Watcher, RecursiveMode, watcher};
use std::sync::mpsc::channel;
use std::time::Duration;
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use tokio;

#[derive(StructOpt)]
#[structopt(name = "git-loc-tracker", about = "Track LoC changes in git repositories")]
struct Opt {
    /// Paths to the git repositories to track.
    #[structopt(parse(from_os_str))]
    paths: Vec<PathBuf>,
    
    /// The author whose changes will be tracked.
    #[structopt(short, long)]
    author: String,
}

/// A struct representing a line of code change in a repository.
#[derive(Debug)]
struct LocChange {
    repo_name: String,
    timestamp: DateTime<Utc>,
    author: Option<String>,
    additions: i32,
    deletions: i32,
    is_committed: bool,
}

/// A struct to hold statistics about a repository's changes.
#[derive(Debug, Clone)]
struct RepoStats {
    committed_additions: i32,
    committed_deletions: i32,
    pending_additions: i32,
    pending_deletions: i32,
}

/// Sets up the database by creating the necessary table if it does not exist.
/// 
/// # Arguments
/// 
/// * `pool` - A reference to the SQLite connection pool.
async fn setup_database(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS loc_changes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            repo_name TEXT NOT NULL,
            timestamp TEXT NOT NULL,
            author TEXT,
            additions INTEGER NOT NULL,
            deletions INTEGER NOT NULL,
            is_committed BOOLEAN NOT NULL
        )
        "#
    )
    .execute(pool)
    .await?;

    Ok(())
}

/// Stores a line of code change in the database.
/// 
/// # Arguments
/// 
/// * `pool` - A reference to the SQLite connection pool.
/// * `change` - A reference to the LocChange struct containing the change details.
async fn store_change(pool: &SqlitePool, change: &LocChange) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO loc_changes 
        (repo_name, timestamp, author, additions, deletions, is_committed)
        VALUES ($1, $2, $3, $4, $5, $6)
        "#
    )
    .bind(&change.repo_name)
    .bind(&change.timestamp.to_rfc3339())
    .bind(&change.author)
    .bind(change.additions)
    .bind(change.deletions)
    .bind(change.is_committed)
    .execute(pool)
    .await?;

    Ok(())
}

/// Checks if a commit was made today.
/// 
/// # Arguments
/// 
/// * `commit_time` - A reference to the commit time.
/// 
/// # Returns
/// 
/// Returns true if the commit was made today, otherwise false.
fn is_commit_from_today(commit_time: &Time) -> bool {
    if let Some(dt) = DateTime::from_timestamp(commit_time.seconds(), 0) {
        let commit_date = dt.with_timezone(&Local).date_naive();
        let today = Local::now().date_naive();
        commit_date == today
    } else {
        false
    }
}

/// Counts the number of additions and deletions in the working directory of a repository.
/// 
/// # Arguments
/// 
/// * `repo` - A reference to the Repository object.
/// 
/// # Returns
/// 
/// A tuple containing the number of additions and deletions.
fn count_file_changes(repo: &Repository) -> (i32, i32) {
    let mut additions = 0;
    let mut deletions = 0;

    if let Ok(diff) = repo.diff_index_to_workdir(None, None) {
        if let Ok(stats) = diff.stats() {
            additions = stats.insertions() as i32;
            deletions = stats.deletions() as i32;
        }
    }

    (additions, deletions)
}

/// Retrieves the changes for a repository made by a specific author.
/// 
/// # Arguments
/// 
/// * `repo` - A reference to the Repository object.
/// * `author` - A string slice representing the author's name.
/// 
/// # Returns
/// 
/// A Result containing RepoStats if successful, or a git2::Error if an error occurs.
fn get_repo_changes(repo: &Repository, author: &str) -> std::result::Result<RepoStats, git2::Error> {
    let mut stats = RepoStats {
        committed_additions: 0,
        committed_deletions: 0,
        pending_additions: 0,
        pending_deletions: 0,
    };

    // Get uncommitted changes
    let mut status_opts = StatusOptions::new();
    status_opts.include_untracked(true);
    let statuses = repo.statuses(Some(&mut status_opts))?;

    for status in statuses.iter() {
        if status.status() != Status::CURRENT {
            if let Some(_path) = status.path() {
                let (adds, dels) = count_file_changes(repo);
                stats.pending_additions += adds;
                stats.pending_deletions += dels;
            }
        }
    }

    // Get all commits from today
    let mut revwalk = repo.revwalk()?;
    revwalk.push_head()?;
    
    for oid in revwalk {
        let oid = oid?;
        let commit = repo.find_commit(oid)?;
        
        // Skip if not from today
        if !is_commit_from_today(&commit.time()) {
            break;
        }
        
        // Check author
        let commit_author = commit.author();
        let author_name = commit_author.name().unwrap_or_default();
        
        if author_name == author {
            // Get the parent commit
            if let Ok(parent) = commit.parent(0) {
                let parent_tree = parent.tree()?;
                let commit_tree = commit.tree()?;
                let diff = repo.diff_tree_to_tree(Some(&parent_tree), Some(&commit_tree), None)?;
                let diff_stats = diff.stats()?;
                
                stats.committed_additions += diff_stats.insertions() as i32;
                stats.committed_deletions += diff_stats.deletions() as i32;
            }
        }
    }

    Ok(stats)
}

/// Watches the specified repositories for changes and updates the database accordingly.
/// 
/// # Arguments
/// 
/// * `paths` - A vector of paths to the repositories.
/// * `author` - A string representing the author's name.
/// 
/// # Returns
/// 
/// A Result indicating success or failure.
async fn watch_repositories(paths: Vec<PathBuf>, author: String) -> Result<(), Box<dyn std::error::Error>> {
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect("sqlite:loc_stats.db")
        .await?;

    setup_database(&pool).await?;

    let (tx, rx) = channel();
    let mut watcher = watcher(tx, Duration::from_secs(300))?;

    for path in &paths {
        watcher.watch(path, RecursiveMode::Recursive)?;
    }

    let mut repo_stats: HashMap<String, RepoStats> = HashMap::new();

    loop {
        match rx.recv() {
            Ok(_) => {
                // Update stats for all repositories
                for path in &paths {
                    if let Ok(repo) = Repository::open(path) {
                        let repo_name = path.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned();

                        if let Ok(stats) = get_repo_changes(&repo, &author) {
                            let change = LocChange {
                                repo_name: repo_name.clone(),
                                timestamp: Utc::now(),
                                author: Some(author.clone()),
                                additions: stats.committed_additions + stats.pending_additions,
                                deletions: stats.committed_deletions + stats.pending_deletions,
                                is_committed: false,
                            };
                            
                            repo_stats.insert(repo_name, stats.clone());
                            
                            if let Err(e) = store_change(&pool, &change).await {
                                eprintln!("Error storing change: {}", e);
                            }
                        }
                    }
                }

                // Print current status
                let mut total_committed = 0;
                let mut total_pending = 0;

                for (repo_name, stats) in &repo_stats {
                    let committed_loc = stats.committed_additions + stats.committed_deletions;
                    let pending_loc = stats.pending_additions + stats.pending_deletions;
                    println!(
                        "{}: {} LoC committed, {} LoC In Progress",
                        repo_name, committed_loc, pending_loc
                    );
                    total_committed += committed_loc;
                    total_pending += pending_loc;
                }

                println!(
                    "\nTotal: {} LoC committed, {} LoC In Progress\n",
                    total_committed, total_pending
                );
            }
            Err(e) => eprintln!("Watch error: {:?}", e),
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let opt = Opt::from_args();
    watch_repositories(opt.paths, opt.author).await
}