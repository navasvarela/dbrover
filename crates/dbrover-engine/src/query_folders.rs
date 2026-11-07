//! Listing query folders (VC2) on the blocking pool: a repository may hold thousands of
//! files, which would stall a frame if the app walked them.

use std::path::PathBuf;

use dbrover_core::describe_error;
use dbrover_store::{QueryFolderError, read_query_folder};

use crate::Event;
use crate::protocol::QueryFolderProblem;

/// The folder's tree, answering [`crate::Command::ReadQueryFolder`].
pub(crate) async fn read(path: PathBuf) -> Event {
    let root = path.clone();
    let result = match tokio::task::spawn_blocking(move || read_query_folder(&root)).await {
        Ok(Ok(listing)) => {
            for skipped in &listing.skipped {
                tracing::warn!("left out of the query folder {}: {skipped}", path.display());
            }
            Ok(listing.tree)
        }
        Ok(Err(QueryFolderError::Missing { .. })) => Err(QueryFolderProblem::Missing),
        Ok(Err(error)) => Err(failed(&path, &error)),
        Err(error) => Err(failed(&path, &error)),
    };
    Event::QueryFolderRead { path, result }
}

fn failed(path: &std::path::Path, error: &(dyn std::error::Error + 'static)) -> QueryFolderProblem {
    let described = describe_error(error);
    tracing::error!(
        "could not read the query folder {}: {described}",
        path.display()
    );
    QueryFolderProblem::Failed(described)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn answers_with_the_tree_or_why_there_is_none() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("sales.sql"), "select 1;").unwrap();
        let gone = dir.path().join("gone");

        let Event::QueryFolderRead { path, result } = read(dir.path().to_owned()).await else {
            panic!("not a query folder's answer");
        };
        assert_eq!(path, dir.path());
        assert_eq!(result.unwrap().files[0].name, "sales.sql");

        let Event::QueryFolderRead { result, .. } = read(gone).await else {
            panic!("not a query folder's answer");
        };
        assert_eq!(result, Err(QueryFolderProblem::Missing));
    }
}
