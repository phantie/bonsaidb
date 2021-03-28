use std::{borrow::Cow, marker::PhantomData, ops::Range};

use async_trait::async_trait;
use serde::Serialize;

use crate::{
    document::{Document, Header},
    schema::{self, map},
    transaction::{self, Command, Operation, OperationResult, Transaction},
    Error,
};

/// Defines all interactions with a [`schema::Database`], regardless of whether it is local or remote.
#[async_trait]
pub trait Connection<'a>: Send + Sync {
    /// Accesses a collection for the connected [`schema::Database`].
    fn collection<C: schema::Collection + 'static>(
        &'a self,
    ) -> Result<Collection<'a, Self, C>, Error>
    where
        Self: Sized;

    /// Inserts a newly created document into the connected [`schema::Database`] for the [`Collection`] `C`.
    async fn insert<C: schema::Collection>(&self, contents: Vec<u8>) -> Result<Header, Error> {
        let mut tx = Transaction::default();
        tx.push(Operation {
            collection: C::id(),
            command: Command::Insert {
                contents: Cow::from(contents),
            },
        });
        let results = self.apply_transaction(tx).await?;
        if let OperationResult::DocumentUpdated { header, .. } = &results[0] {
            Ok(header.clone())
        } else {
            unreachable!(
                "apply_transaction on a single insert should yield a single DocumentUpdated entry"
            )
        }
    }

    /// Updates an existing document in the connected [`schema::Database`] for the
    /// [`Collection`] `C`. Upon success, `doc.revision` will be updated with
    /// the new revision.
    async fn update(&self, doc: &mut Document<'_>) -> Result<(), Error> {
        let mut tx = Transaction::default();
        tx.push(Operation {
            collection: doc.collection.clone(),
            command: Command::Update {
                header: Cow::Owned(doc.header.as_ref().clone()),
                contents: Cow::Owned(doc.contents.to_vec()),
            },
        });
        let results = self.apply_transaction(tx).await?;
        if let OperationResult::DocumentUpdated { header, .. } = &results[0] {
            doc.header = Cow::Owned(header.clone());
            Ok(())
        } else {
            unreachable!(
                "apply_transaction on a single update should yield a single DocumentUpdated entry"
            )
        }
    }

    /// Retrieves a stored document from [`Collection`] `C` identified by `id`.
    async fn get<C: schema::Collection>(&self, id: u64)
        -> Result<Option<Document<'static>>, Error>;

    /// Removes a `Document` from the database.
    async fn delete(&self, doc: &Document<'_>) -> Result<(), Error> {
        let mut tx = Transaction::default();
        tx.push(Operation {
            collection: doc.collection.clone(),
            command: Command::Delete {
                header: Cow::Owned(doc.header.as_ref().clone()),
            },
        });
        let results = self.apply_transaction(tx).await?;
        if let OperationResult::DocumentDeleted { .. } = &results[0] {
            Ok(())
        } else {
            unreachable!(
                "apply_transaction on a single update should yield a single DocumentUpdated entry"
            )
        }
    }

    /// Initializes [`ViewQuery`] for [`schema::View`] `V`.
    #[must_use]
    fn view<V: schema::View>(&'a self) -> View<'a, Self, V>
    where
        Self: Sized,
    {
        View::new(self)
    }

    /// Initializes [`ViewQuery`] for [`schema::View`] `V`.
    #[must_use]
    async fn query<'k, V: schema::View>(
        &self,
        query: View<'a, Self, V>,
    ) -> Result<Vec<map::Serialized>, Error>
    where
        Self: Sized;

    /// Applies a [`Transaction`] to the [`schema::Database`]. If any operation in the
    /// [`Transaction`] fails, none of the operations will be applied to the
    /// [`schema::Database`].
    async fn apply_transaction(
        &self,
        transaction: Transaction<'static>,
    ) -> Result<Vec<OperationResult>, Error>;

    /// Lists executed [`Transaction`]s from this [`schema::Database`]. By default, a maximum of
    /// 1000 entries will be returned, but that limit can be overridden by
    /// setting `result_limit`. A hard limit of 100,000 results will be
    /// returned. To begin listing after another known `transaction_id`, pass
    /// `transaction_id + 1` into `starting_id`.
    async fn list_executed_transactions(
        &self,
        starting_id: Option<u64>,
        result_limit: Option<usize>,
    ) -> Result<Vec<transaction::Executed<'static>>, Error>;
}

/// Interacts with a collection over a `Connection`.
pub struct Collection<'a, Cn, Cl> {
    connection: &'a Cn,
    _phantom: PhantomData<Cl>, // allows for extension traits to be written for collections of specific types
}

impl<'a, Cn, Cl> Collection<'a, Cn, Cl>
where
    Cn: Connection<'a>,
    Cl: schema::Collection,
{
    /// Creates a new instance using `connection`.
    pub fn new(connection: &'a Cn) -> Self {
        Self {
            connection,
            _phantom: PhantomData::default(),
        }
    }

    /// Adds a new `Document<Cl>` with the contents `item`.
    pub async fn push<S: Serialize + Sync>(&self, item: &S) -> Result<Header, crate::Error> {
        let contents = serde_cbor::to_vec(item)?;
        Ok(self.connection.insert::<Cl>(contents).await?)
    }

    /// Retrieves a `Document<Cl>` with `id` from the connection.
    pub async fn get(&self, id: u64) -> Result<Option<Document<'static>>, Error> {
        self.connection.get::<Cl>(id).await
    }
}

/// Parameters to query a `schema::View`.
pub struct View<'a, Cn, V: schema::View> {
    connection: &'a Cn,

    /// Key filtering criteria.
    pub key: Option<QueryKey<V::MapKey>>,

    /// The view's data access policy. The default value is [`AccessPolicy::UpdateBefore`].
    pub access_policy: AccessPolicy,
}

impl<'a, Cn, V> View<'a, Cn, V>
where
    V: schema::View,
    Cn: Connection<'a>,
{
    fn new(connection: &'a Cn) -> Self {
        Self {
            connection,
            key: None,
            access_policy: AccessPolicy::UpdateBefore,
        }
    }

    /// Filters for entries in the view with `key`.
    #[must_use]
    pub fn with_key(mut self, key: V::MapKey) -> Self {
        self.key = Some(QueryKey::Matches(key));
        self
    }

    /// Filters for entries in the view with `keys`.
    #[must_use]
    pub fn with_keys(mut self, keys: Vec<V::MapKey>) -> Self {
        self.key = Some(QueryKey::Multiple(keys));
        self
    }

    /// Filters for entries in the view with the range `keys`.
    #[must_use]
    pub fn with_key_range(mut self, range: Range<V::MapKey>) -> Self {
        self.key = Some(QueryKey::Range(range));
        self
    }

    /// Sets the access policy for queries.
    pub fn with_access_policy(mut self, policy: AccessPolicy) -> Self {
        self.access_policy = policy;
        self
    }

    /// Executes the query and retrieves the results.
    pub async fn query(self) -> Result<Vec<map::Serialized>, Error> {
        self.connection.query(self).await
    }
}

/// Filters a [`View`] by key.
pub enum QueryKey<K> {
    /// Matches all entries with the key provided.
    Matches(K),

    /// Matches all entires with keys in the range provided.
    Range(Range<K>),

    /// Matches all entries that have keys that are included in the set provided.
    Multiple(Vec<K>),
}

/// Changes how the view's outdated data will be treated.
pub enum AccessPolicy {
    /// Update any changed documents before returning a response.
    UpdateBefore,

    /// Return the results, which may be out-of-date, and start an update job in
    /// the background. This pattern is useful when you want to ensure you
    /// provide consistent response times while ensuring the database is
    /// updating in the background.
    UpdateAfter,

    /// Returns the restuls, which may be out-of-date, and do not start any
    /// background jobs. This mode is useful if you're using a view as a cache
    /// and have a background process that is responsible for controlling when
    /// data is refreshed and updated. While the default `UpdateBefore`
    /// shouldn't have much overhead, this option removes all overhead related
    /// to view updating from the query.
    NoUpdate,
}