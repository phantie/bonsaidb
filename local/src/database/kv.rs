use async_trait::async_trait;
use pliantdb_core::{
    kv::{Command, KeyCheck, KeyOperation, KeyStatus, Kv, Output, Timestamp},
    schema::Schema,
};
use serde::{Deserialize, Serialize};
use sled::IVec;

use crate::{error::ResultExt as _, storage::kv::ExpirationUpdate, Database};

#[derive(Serialize, Deserialize)]
pub struct Entry {
    pub value: Vec<u8>,
    pub expiration: Option<Timestamp>,
}

#[async_trait]
impl<DB> Kv for Database<DB>
where
    DB: Schema,
{
    async fn execute_key_operation(
        &self,
        op: KeyOperation,
    ) -> Result<Output, pliantdb_core::Error> {
        let task_self = self.clone();
        tokio::task::spawn_blocking(move || match op.command {
            Command::Set {
                value,
                expiration,
                keep_existing_expiration,
                check,
                return_previous_value,
            } => execute_set_operation(
                &key_tree(&task_self.data.name, op.namespace),
                op.key,
                value,
                expiration,
                keep_existing_expiration,
                check,
                return_previous_value,
                &task_self,
            ),
            Command::Get { delete } => execute_get_operation(
                &key_tree(&task_self.data.name, op.namespace),
                &op.key,
                delete,
                &task_self,
            ),
            Command::Delete => execute_delete_operation(
                &key_tree(&task_self.data.name, op.namespace),
                op.key,
                &task_self,
            ),
        })
        .await
        .unwrap()
    }
}

fn key_tree(database: &str, namespace: Option<String>) -> String {
    format!("{}::kv::{}", database, namespace.unwrap_or_default())
}

#[allow(clippy::too_many_arguments)]
fn execute_set_operation<DB: Schema>(
    tree_name: &str,
    key: String,
    value: Vec<u8>,
    expiration: Option<Timestamp>,
    keep_existing_expiration: bool,
    check: Option<KeyCheck>,
    return_previous_value: bool,
    db: &Database<DB>,
) -> Result<Output, pliantdb_core::Error> {
    let kv_tree = db
        .data
        .storage
        .sled()
        .open_tree(tree_name.as_bytes())
        .map_err_to_core()?;

    let mut entry = Entry { value, expiration };
    let mut inserted = false;
    let mut updated = false;
    let previous_value = kv_tree
        .fetch_and_update(key.as_bytes(), |existing_value| {
            let should_update = match check {
                Some(KeyCheck::OnlyIfPresent) => existing_value.is_some(),
                Some(KeyCheck::OnlyIfVacant) => existing_value.is_none(),
                None => true,
            };
            if should_update {
                updated = true;
                inserted = existing_value.is_none();
                if keep_existing_expiration && !inserted {
                    if let Ok(previous_entry) =
                        bincode::deserialize::<Entry>(existing_value.unwrap())
                    {
                        println!(
                            "updated expiration for key {:?}: {:?}",
                            key, previous_entry.expiration
                        );
                        entry.expiration = previous_entry.expiration;
                    }
                }
                let entry_vec = bincode::serialize(&entry).unwrap();
                Some(IVec::from(entry_vec))
            } else {
                // TODO Investigate if this actually copies, I think IVec
                // optimizes this under the hood. Ultimately, fetch_and_update
                // isn't the exact right choice here, but it is implemented as a
                // loop calling compare_swap. It'd be a lot better for us to
                // write our own function, and be able to exit without updating
                // the key.
                existing_value.map(IVec::from)
            }
        })
        .map_err_to_core()?;

    if updated {
        db.data.storage.update_key_expiration(ExpirationUpdate {
            tree_key: TreeKey {
                tree: tree_name.to_string(),
                key,
            },
            expiration: entry.expiration,
        });
        if return_previous_value {
            if let Some(Ok(entry)) = previous_value.map(|v| bincode::deserialize::<Entry>(&v)) {
                Ok(Output::Value(Some(entry.value)))
            } else {
                Ok(Output::Value(None))
            }
        } else if inserted {
            Ok(Output::Status(KeyStatus::Inserted))
        } else {
            Ok(Output::Status(KeyStatus::Updated))
        }
    } else {
        Ok(Output::Status(KeyStatus::NotChanged))
    }
}

fn execute_get_operation<DB: Schema>(
    tree_name: &str,
    key: &str,
    delete: bool,
    db: &Database<DB>,
) -> Result<Output, pliantdb_core::Error> {
    let tree = db
        .data
        .storage
        .sled()
        .open_tree(tree_name.as_bytes())
        .map_err_to_core()?;
    let entry = if delete {
        let entry = tree.remove(key.as_bytes()).map_err_to_core()?;
        if entry.is_some() {
            db.data.storage.update_key_expiration(ExpirationUpdate {
                tree_key: TreeKey::new(&db.data.name, tree_name, key.to_string()),
                expiration: None,
            });
        }
        entry
    } else {
        tree.get(key.as_bytes()).map_err_to_core()?
    };

    let entry = entry
        .map(|e| bincode::deserialize::<Entry>(&e))
        .transpose()
        .map_err_to_core()?
        .map(|e| e.value);
    Ok(Output::Value(entry))
}

fn execute_delete_operation<DB: Schema>(
    tree_name: &str,
    key: String,
    db: &Database<DB>,
) -> Result<Output, pliantdb_core::Error> {
    let tree = db
        .data
        .storage
        .sled()
        .open_tree(tree_name.as_bytes())
        .map_err_to_core()?;
    let value = tree.remove(&key).map_err_to_core()?;
    if value.is_some() {
        db.data.storage.update_key_expiration(ExpirationUpdate {
            tree_key: TreeKey::new(&db.data.name, tree_name, key),
            expiration: None,
        });

        Ok(Output::Status(KeyStatus::Deleted))
    } else {
        Ok(Output::Status(KeyStatus::NotChanged))
    }
}

#[derive(Hash, Eq, PartialEq, Debug, Clone)]
pub struct TreeKey {
    pub tree: String,
    pub key: String,
}

impl TreeKey {
    pub fn new(database: &str, tree: &str, key: String) -> Self {
        Self {
            tree: format!("{}::{}", database, tree),
            key,
        }
    }
}