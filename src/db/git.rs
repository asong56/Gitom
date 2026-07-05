use anyhow::{Result};
use git2::{BranchType, Delta, DiffOptions, ObjectType, Repository, Sort};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TreeEntry {
    pub name:    String,
    pub path:    String,
    pub kind:    String,
    pub size:    Option<u64>,
    pub message: Option<String>,
    pub sha:     Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CommitInfo {
    pub sha:        String,
    pub short_sha:  String,
    pub message:    String,
    pub subject:    String,
    pub author:     String,
    pub email:      String,
    pub timestamp:  i64,
    pub time_human: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DiffFile {
    pub old_path:  String,
    pub new_path:  String,
    pub status:    String,
    pub hunks:     Vec<DiffHunk>,
    pub is_binary: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DiffHunk {
    pub header: String,
    pub lines:  Vec<DiffLine>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DiffLine {
    pub origin:  char,
    pub content: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BranchInfo {
    pub name:    String,
    pub is_head: bool,
    pub sha:     String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TagInfo {
    pub name:    String,
    pub sha:     String,
    pub message: Option<String>,
}

pub fn list_tree(repo: &Repository, ref_name: &str, path: &str) -> Result<Vec<TreeEntry>> {
    let obj  = repo.revparse_single(ref_name)?;
    let tree = if path.is_empty() {
        obj.peel_to_tree()?
    } else {
        let tree  = obj.peel_to_tree()?;
        let entry = tree.get_path(std::path::Path::new(path))?;
        entry.to_object(repo)?.peel_to_tree()?
    };

    let mut entries = Vec::new();
    for entry in tree.iter() {
        let name = entry.name().unwrap_or("").to_string();
        let kind = match entry.kind() {
            Some(ObjectType::Tree) => "tree",
            _                      => "blob",
        };
        let full_path = if path.is_empty() { name.clone() } else { format!("{path}/{name}") };
        let size = if kind == "blob" {
            entry.to_object(repo).ok().and_then(|o| o.as_blob().map(|b| b.size() as u64))
        } else {
            None
        };
        entries.push(TreeEntry {
            name, path: full_path, kind: kind.to_string(), size, message: None,
            sha: Some(entry.id().to_string()),
        });
    }

    entries.sort_by(|a, b| match (a.kind.as_str(), b.kind.as_str()) {
        ("tree", "blob") => std::cmp::Ordering::Less,
        ("blob", "tree") => std::cmp::Ordering::Greater,
        _                => a.name.cmp(&b.name),
    });

    Ok(entries)
}

pub fn read_blob(repo: &Repository, ref_name: &str, path: &str) -> Result<(Vec<u8>, bool)> {
    let obj  = repo.revparse_single(&format!("{ref_name}:{path}"))?;
    let blob = obj.peel_to_blob()?;
    Ok((blob.content().to_vec(), blob.is_binary()))
}

/// Returns empty content with `is_binary=true` when the blob exceeds `limit` bytes,
/// allowing callers to show a "file too large" notice without loading it into memory.
pub fn read_blob_limited(repo: &Repository, ref_name: &str, path: &str, limit: u64) -> Result<(Vec<u8>, bool)> {
    let obj  = repo.revparse_single(&format!("{ref_name}:{path}"))?;
    let blob = obj.peel_to_blob()?;
    if blob.size() as u64 > limit {
        return Ok((Vec::new(), true));
    }
    Ok((blob.content().to_vec(), blob.is_binary()))
}

pub fn read_readme(repo: &Repository, ref_name: &str) -> Result<Option<String>> {
    for name in &["README.md", "README.rst", "README.txt", "README"] {
        if let Ok((bytes, false)) = read_blob(repo, ref_name, name) {
            return Ok(Some(String::from_utf8_lossy(&bytes).into_owned()));
        }
    }
    Ok(None)
}

pub fn list_commits(repo: &Repository, ref_name: &str, page: u32, per_page: u32) -> Result<Vec<CommitInfo>> {
    let obj = repo.revparse_single(ref_name)?;
    let mut walk = repo.revwalk()?;
    walk.push(obj.id())?;
    walk.set_sorting(Sort::TIME)?;

    // Bug fix: cast to u64 before multiplying to prevent u32 overflow when page
    // is a very large value (in release mode overflow silently wraps).
    let skip  = (page as u64 - 1).saturating_mul(per_page as u64) as usize;
    let limit = per_page as usize;
    let mut out = Vec::with_capacity(limit);
    for (i, oid) in walk.enumerate() {
        let oid = oid?;
        if i < skip { continue; }
        if out.len() >= limit { break; }
        out.push(commit_info(&repo.find_commit(oid)?));
    }
    Ok(out)
}

/// Returns commits that modified `path`. For the root directory returns the latest N commits.
/// Uses revwalk + per-commit tree diff to find commits that actually touched the path.
pub fn commits_for_tree(repo: &Repository, ref_name: &str, path: &str, limit: usize) -> Result<Vec<CommitInfo>> {
    if path.is_empty() {
        return list_commits(repo, ref_name, 1, limit as u32);
    }

    let obj = repo.revparse_single(ref_name)?;
    let mut walk = repo.revwalk()?;
    walk.push(obj.id())?;
    walk.set_sorting(Sort::TIME)?;

    let target = std::path::Path::new(path);
    let mut out = Vec::with_capacity(limit);

    for oid in walk {
        if out.len() >= limit { break; }
        let oid    = oid?;
        let commit = repo.find_commit(oid)?;
        let curr   = commit.tree()?;

        let touched = if commit.parent_count() == 0 {
            curr.get_path(target).is_ok()
        } else {
            let parent = commit.parent(0)?.tree()?;
            let mut opts = DiffOptions::new();
            opts.pathspec(path);
            let diff = repo.diff_tree_to_tree(Some(&parent), Some(&curr), Some(&mut opts))?;
            diff.deltas().count() > 0
        };

        if touched { out.push(commit_info(&commit)); }
    }
    Ok(out)
}

pub fn get_commit_with_diff(repo: &Repository, sha: &str) -> Result<(CommitInfo, Vec<DiffFile>)> {
    let oid    = git2::Oid::from_str(sha)?;
    let commit = repo.find_commit(oid)?;
    let info   = commit_info(&commit);
    let parent = commit.parent(0).ok().and_then(|p| p.tree().ok());
    let curr   = commit.tree()?;
    let diff   = repo.diff_tree_to_tree(parent.as_ref(), Some(&curr), Some(&mut DiffOptions::new()))?;
    Ok((info, diff_to_files(&diff)?))
}

pub fn list_branches(repo: &Repository) -> Result<Vec<BranchInfo>> {
    let head_sha = repo.head().ok()
        .and_then(|r| r.target())
        .map(|o| o.to_string());
    let mut out = Vec::new();
    for branch in repo.branches(Some(BranchType::Local))? {
        let (branch, _) = branch?;
        let name    = branch.name()?.unwrap_or("").to_string();
        let sha     = branch.get().target().map(|o| o.to_string()).unwrap_or_default();
        let is_head = head_sha.as_deref() == Some(&sha);
        out.push(BranchInfo { name, is_head, sha });
    }
    Ok(out)
}

pub fn list_tags(repo: &Repository) -> Result<Vec<TagInfo>> {
    let mut out = Vec::new();
    repo.tag_foreach(|oid, name| {
        let name = String::from_utf8_lossy(name).trim_start_matches("refs/tags/").to_string();
        // tag_foreach gives the tag object OID for annotated tags, not the commit OID.
        // Peel to commit so the displayed SHA matches `git show <tag>`.
        let commit_sha = repo.find_object(oid, None)
            .and_then(|obj| obj.peel(ObjectType::Commit))
            .map(|c| c.id().to_string())
            .unwrap_or_else(|_| oid.to_string());
        // message is only present on annotated tags; lightweight tags return None.
        let message = repo.find_tag(oid).ok().and_then(|t| t.message().map(|m| m.to_string()));
        out.push(TagInfo { name, sha: commit_sha, message });
        true
    })?;
    Ok(out)
}

pub fn init_bare(path: &str) -> Result<()> {
    std::fs::create_dir_all(path)?;
    git2::Repository::init_bare(path)?;
    Ok(())
}

fn commit_info(commit: &git2::Commit) -> CommitInfo {
    let sha       = commit.id().to_string();
    let short_sha = sha.get(..8).unwrap_or(&sha).to_string();
    let message   = commit.message().unwrap_or("").to_string();
    let subject   = message.lines().next().unwrap_or("").to_string();
    let sig       = commit.author();
    CommitInfo {
        sha, short_sha, message, subject,
        author:     sig.name().unwrap_or("Unknown").to_string(),
        email:      sig.email().unwrap_or("").to_string(),
        timestamp:  commit.time().seconds(),
        time_human: relative_time(commit.time().seconds()),
    }
}

fn relative_time(unix: i64) -> String {
    let now  = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    // Bug fix: clamp to 0 so that a future timestamp (e.g. due to clock skew on
    // the committer's machine) doesn't produce a negative value that falls through
    // to the last match arm and renders as "-5 days ago".
    let diff = (now - unix).max(0);
    match diff {
        0..=59       => "just now".into(),
        60..=3599    => format!("{} minutes ago", diff / 60),
        3600..=86399 => format!("{} hours ago", diff / 3600),
        _            => format!("{} days ago", diff / 86400),
    }
}

fn diff_to_files(diff: &git2::Diff) -> Result<Vec<DiffFile>> {
    let mut files: Vec<DiffFile> = Vec::new();

    diff.print(git2::DiffFormat::Patch, |delta, hunk, line| {
        let idx = files.len().saturating_sub(1);

        if hunk.is_none() && line.origin() == 'F' {
            let status = match delta.status() {
                Delta::Added    => "added",
                Delta::Deleted  => "deleted",
                Delta::Renamed  => "renamed",
                _               => "modified",
            };
            files.push(DiffFile {
                old_path:  delta.old_file().path().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
                new_path:  delta.new_file().path().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
                status:    status.to_string(),
                hunks:     Vec::new(),
                is_binary: delta.old_file().is_binary() || delta.new_file().is_binary(),
            });
            return true;
        }

        if let Some(h) = hunk {
            if !files.is_empty() {
                let header = String::from_utf8_lossy(h.header()).to_string();
                files[idx].hunks.push(DiffHunk { header, lines: Vec::new() });
            }
        } else {
            let origin = line.origin();
            if matches!(origin, '+' | '-' | ' ') && !files.is_empty() && !files[idx].hunks.is_empty() {
                let hi      = files[idx].hunks.len() - 1;
                let content = String::from_utf8_lossy(line.content()).to_string();
                files[idx].hunks[hi].lines.push(DiffLine { origin, content });
            }
        }
        true
    })?;

    Ok(files)
}
