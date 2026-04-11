//! SeaORM entity backing the `messages` table.

use sea_orm::entity::prelude::*;

/// Row model for `messages`.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "messages")]
pub struct Model {
    /// UUIDv7 primary key (caller-generated).
    #[sea_orm(primary_key, auto_increment = false)]
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
