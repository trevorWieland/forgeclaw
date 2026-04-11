//! SeaORM entity backing the `messages` table.

use sea_orm::entity::prelude::*;

/// Row model for `messages`.
///
/// The primary key `seq` is a store-owned monotonic ordinal assigned
/// by the database on insert. The caller-generated UUIDv7 `id` is a
/// unique correlation key only.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "messages")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub seq: i64,
    #[sea_orm(unique)]
    pub id: String,
    pub group_id: String,
    pub channel_id: String,
    pub sender: String,
    pub content: String,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
