#![feature(
    destructuring_assignment,
    generic_associated_types,
    trait_alias,
    type_alias_impl_trait
)]
#![allow(incomplete_features, clippy::unused_io_amount)]

mod changeset;
mod common;
mod dbutils;
mod ext;
mod interface;
mod models;
mod remote;
mod traits;

pub use changeset::ChangeSet;
pub use dbutils::SyncStage;
pub use ext::TransactionExt;
pub use remote::{kv_client::KvClient as RemoteKvClient, RemoteCursor, RemoteTransaction};
pub use traits::{
    ComparatorFunc, Cursor, CursorDupFixed, CursorDupFixed2, CursorDupSort, CursorDupSort2,
    MutableCursor, Transaction, Transaction2,
};